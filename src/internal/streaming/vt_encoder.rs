//! The stream's video encoded in Nida itself, on the Mac's media engine
//! (`VideoToolbox`), so its bit rate can change while it is live. ffmpeg's
//! encoders take theirs once, when they start: a stream whose upload could
//! not carry it lost parts of itself until the operator stopped it. Changed
//! here, the frames after come out at the new rate within a second and none
//! is lost — measured on a recording of a show, 6000 → 1500 → 800 → 6000
//! kbps, H.264 and HEVC alike.
//!
//! Each frame submitted ends in exactly one [`Encoded`]: the frame, word that
//! the engine dropped it, or that encoding failed — sent from the engine's
//! thread without ever waiting, so a callback the engine makes from inside
//! `encode` cannot hold it up. The stream's writer bounds how many frames are
//! out at once (see [`VtEncoder::in_flight`]).

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use crossbeam_channel::Sender;

use anyhow::{bail, Context};
use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC, CMFormatDescription,
    CMSampleBuffer, CMTime,
};
use objc2_core_video::{
    kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVPixelBufferHeightKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBuffer,
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ConstantBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, VTCompressionSession,
    VTEncodeInfoFlags, VTSessionSetProperty,
};

use super::VideoCodec;

/// What became of one frame submitted to the engine.
#[derive(Debug)]
pub enum Encoded {
    /// The frame for `slot`: its units behind their lengths, as FLV carries
    /// them, and — with a keyframe — the codec's configuration record.
    Frame {
        slot: u64,
        key: bool,
        units: Vec<u8>,
        config: Option<Vec<u8>>,
    },
    /// The engine dropped the frame for `slot` instead of encoding it.
    Dropped { slot: u64 },
    /// Encoding failed; nothing after it can be trusted.
    Failed(String),
}

/// Which frames have been told of, so each is told of once: a frame the
/// engine drops inside `encode` is told of there, unless its callback ran
/// already — and its callback, if it comes after, keeps quiet. Any number
/// may be dropped so before their callbacks come.
#[derive(Debug, Default)]
struct Told {
    /// The callbacks have told of every slot before this.
    upto: u64,
    /// Slots `encode` told of itself, their callbacks yet to come, if at all.
    skip: std::collections::BTreeSet<u64>,
}

impl Told {
    /// The callback for `slot`: whether it is the one to tell of it.
    fn callback(&mut self, slot: u64) -> bool {
        let told_already = self.skip.remove(&slot);
        // Callbacks come in the frames' order: none comes now for a slot
        // before this one.
        self.skip.retain(|&s| s > slot);
        self.upto = self.upto.max(slot + 1);
        !told_already
    }

    /// `encode` found `slot` dropped there and then: whether it is the one
    /// to tell of it.
    fn dropped_in_encode(&mut self, slot: u64) -> bool {
        self.upto <= slot && self.skip.insert(slot)
    }
}

/// What the media engine's callback shares with the encoder.
struct Shared {
    codec: VideoCodec,
    out: Sender<Encoded>,
    told: Mutex<Told>,
    /// Frames ended one way or another (encoded, dropped or failed).
    ended: AtomicU64,
    failed: AtomicBool,
}

impl Shared {
    /// Tell of `slot`'s end, unless `encode` already did.
    fn end(&self, slot: u64, encoded: Encoded) {
        let tells = self
            .told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .callback(slot);
        if tells {
            self.record(encoded);
        }
    }

    fn record(&self, encoded: Encoded) {
        if let Encoded::Failed(why) = &encoded {
            if !self.failed.swap(true, Ordering::Relaxed) {
                log::error!("Stream encoder: {why}");
            }
        }
        self.ended.fetch_add(1, Ordering::Relaxed);
        // The receiver gone is the stream stopping: nothing to tell.
        let _ = self.out.send(encoded);
    }
}

/// One stream's encoder on the media engine. Used from one thread — the
/// stream's writer — which hands it frames and changes its rate between them.
pub struct VtEncoder {
    session: CFRetained<VTCompressionSession>,
    pool: CFRetained<CVPixelBufferPool>,
    /// Read by the callback until the session is invalidated, so dropped only
    /// after it (see [`Self::finish`]).
    shared: Box<Shared>,
    width: usize,
    height: usize,
    fps: i32,
    kbps: u32,
    /// Whether the rate is held constant (the media engine takes that on
    /// Apple silicon) rather than averaged.
    constant: bool,
    /// Frames submitted so far.
    submitted: u64,
    finished: bool,
}

// SAFETY: the session and its pool are CoreFoundation objects the media
// engine allows from any thread; the encoder is used from one thread at a
// time (every call takes `&mut self`), and what its callback reads is in
// `Shared`, which is `Sync`.
unsafe impl Send for VtEncoder {}

impl VtEncoder {
    /// An encoder for `width` × `height` NV12 frames (BT.709, video levels, as
    /// the GPU makes them) at `fps`, `kbps` held constant where the engine can
    /// (`constant`), a keyframe every `keyframe_secs` and no B-frames, what
    /// becomes of each frame sent to `out`. Only on the media engine: no
    /// software encoder stands in, so a Mac without one gets an error and the
    /// stream ffmpeg's encoder instead.
    ///
    /// # Errors
    ///
    /// When the media engine cannot make the session or refuses a setting
    /// the stream needs.
    pub fn new(
        codec: VideoCodec,
        (width, height): (u32, u32),
        fps: u32,
        (kbps, constant): (u32, bool),
        keyframe_secs: u32,
        out: Sender<Encoded>,
    ) -> anyhow::Result<Self> {
        let w = i32::try_from(width).context("frame too wide")?;
        let h = i32::try_from(height).context("frame too tall")?;
        let fps = i32::try_from(fps.max(1)).context("frame rate too high")?;
        let shared = Box::new(Shared {
            codec,
            out,
            told: Mutex::new(Told::default()),
            ended: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        });
        let source = CFDictionary::<CFString, CFType>::from_slices(
            // SAFETY: CoreVideo's constant keys.
            unsafe {
                &[
                    kCVPixelBufferPixelFormatTypeKey,
                    kCVPixelBufferWidthKey,
                    kCVPixelBufferHeightKey,
                ]
            },
            &[
                #[allow(clippy::cast_possible_wrap)] // A four-character code.
                &CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32),
                &CFNumber::new_i32(w),
                &CFNumber::new_i32(h),
            ],
        );
        let hardware = CFDictionary::<CFString, CFType>::from_slices(
            // SAFETY: VideoToolbox's constant key.
            unsafe { &[kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder] },
            &[CFBoolean::new(true)],
        );
        let codec_type = match codec {
            VideoCodec::H264 => kCMVideoCodecType_H264,
            VideoCodec::Hevc => kCMVideoCodecType_HEVC,
        };
        let mut raw: *mut VTCompressionSession = std::ptr::null_mut();
        // SAFETY: the dictionaries hold CF types as VideoToolbox documents;
        // the callback's context is `shared`, which outlives the session's
        // callbacks (see `finish`).
        let status = unsafe {
            VTCompressionSession::create(
                None,
                w,
                h,
                codec_type,
                Some(hardware.as_ref()),
                Some(source.as_ref()),
                None,
                Some(on_encoded),
                std::ptr::from_ref::<Shared>(&shared).cast_mut().cast(),
                NonNull::from(&mut raw),
            )
        };
        let Some(raw) = NonNull::new(raw).filter(|_| status == 0) else {
            bail!("the media engine could not start a {} encoder ({status})", codec.label());
        };
        // SAFETY: a session made by `create`, which returns it retained.
        let session = unsafe { CFRetained::from_raw(raw) };
        let constant = configure(&session, codec, fps, keyframe_secs, (kbps, constant))?;
        // Made by the session once it is prepared.
        // SAFETY: a valid session.
        let pool = unsafe { session.pixel_buffer_pool() }
            .context("the media engine gave no frame buffers")?;
        Ok(Self {
            session,
            pool,
            shared,
            width: width as usize,
            height: height as usize,
            fps,
            kbps,
            constant,
            submitted: 0,
            finished: false,
        })
    }

    /// Change the rate while live: the frames after this come out at `kbps`.
    ///
    /// # Errors
    ///
    /// When the media engine refuses it; the rate before stays.
    pub fn set_kbps(&mut self, kbps: u32) -> anyhow::Result<()> {
        if kbps == self.kbps {
            return Ok(());
        }
        // Live, the rate is held constant or not changed: an averaged rate
        // was not found to follow a change back up.
        set_rate(&self.session, kbps, self.constant, false)?;
        self.kbps = kbps;
        Ok(())
    }

    /// The rate asked for now.
    pub fn kbps(&self) -> u32 {
        self.kbps
    }

    /// Whether the rate is held constant, as the stream's rate control needs:
    /// an averaged rate was not found to follow a change back up.
    pub fn constant(&self) -> bool {
        self.constant
    }

    /// Frames submitted and not yet ended: what the engine holds.
    pub fn in_flight(&self) -> u64 {
        self.submitted
            .saturating_sub(self.shared.ended.load(Ordering::Relaxed))
    }

    /// Whether encoding has failed.
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }

    /// Encode one NV12 frame — the Y plane, then the half-height interleaved
    /// U V plane, rows unpadded — for the stream's `slot`: its time is `slot`
    /// frames from the start, later than any frame before.
    ///
    /// # Errors
    ///
    /// When the frame is the wrong size, or the media engine takes it not.
    pub fn encode(&mut self, nv12: &[u8], slot: u64) -> anyhow::Result<()> {
        let (w, h) = (self.width, self.height);
        if nv12.len() != w * h * 3 / 2 {
            bail!("an NV12 frame of {} bytes for {w}x{h}", nv12.len());
        }
        let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: a valid pool; `raw` receives a retained buffer.
        let status =
            unsafe { CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut raw)) };
        let Some(raw) = NonNull::new(raw).filter(|_| status == 0) else {
            bail!("no frame buffer from the media engine ({status})");
        };
        // SAFETY: made by the pool, retained for us.
        let buffer = unsafe { CFRetained::from_raw(raw) };
        // SAFETY: the buffer is locked while its planes are written, each row
        // within the plane's own stride; the planes are as the pool made them,
        // NV12 at this size.
        unsafe {
            if CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0)) != 0 {
                bail!("the frame buffer could not be written");
            }
            for (plane, rows, from) in [(0, h, 0), (1, h / 2, w * h)] {
                let to = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
                let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
                if to.is_null() || stride < w {
                    CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
                    bail!("the frame buffer has no room for the picture");
                }
                for row in 0..rows {
                    std::ptr::copy_nonoverlapping(
                        nv12.as_ptr().add(from + row * w),
                        to.add(row * stride),
                        w,
                    );
                }
            }
            CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        }
        let at = i64::try_from(slot).context("too many frames")?;
        // SAFETY: CMTime's constructor.
        let (at, length) = unsafe { (CMTime::new(at, self.fps), CMTime::new(1, self.fps)) };
        let mut flags = VTEncodeInfoFlags::empty();
        // SAFETY: a valid session and buffer; the frame's context is its
        // slot, handed back to the callback as a number, never dereferenced.
        let status = unsafe {
            self.session.encode_frame(
                &buffer,
                at,
                length,
                None,
                slot as usize as *mut c_void,
                &raw mut flags,
            )
        };
        self.submitted += 1;
        if status != 0 {
            // No callback comes for a frame the engine refused.
            self.shared
                .record(Encoded::Failed(format!("the media engine refused a frame ({status})")));
            bail!("the media engine refused a frame ({status})");
        }
        if flags.contains(VTEncodeInfoFlags::FrameDropped) {
            // Dropped there and then: told of here unless its callback ran
            // already, and kept quiet about if it comes later.
            let tells = self
                .shared
                .told
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .dropped_in_encode(slot);
            if tells {
                self.shared.record(Encoded::Dropped { slot });
            }
        }
        Ok(())
    }

    /// End every frame still being encoded, then the session. Its callback
    /// is not called after this, so `shared` may go.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        // SAFETY: a valid session; an invalid time completes every frame.
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
        }
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        self.finish();
    }
}

/// The stream's settings on a new session, then prepared for the first
/// frame: whether its rate is held constant (see [`set_rate`]).
fn configure(
    session: &VTCompressionSession,
    codec: VideoCodec,
    fps: i32,
    keyframe_secs: u32,
    (kbps, constant): (u32, bool),
) -> anyhow::Result<bool> {
    let keyframe_secs = keyframe_secs.max(1);
    let gop = CFNumber::new_i64(i64::from(fps) * i64::from(keyframe_secs));
    let gop_secs = CFNumber::new_f64(f64::from(keyframe_secs));
    let rate = CFNumber::new_i32(fps);
    // SAFETY: VideoToolbox's and CoreVideo's constant keys and values.
    let settings: [(&str, &CFString, &CFType); 9] = unsafe {
        [
            ("real time", kVTCompressionPropertyKey_RealTime, CFBoolean::new(true)),
            (
                "profile",
                kVTCompressionPropertyKey_ProfileLevel,
                match codec {
                    VideoCodec::H264 => kVTProfileLevel_H264_High_AutoLevel,
                    VideoCodec::Hevc => kVTProfileLevel_HEVC_Main_AutoLevel,
                },
            ),
            // No B-frames: what platforms ask of a live stream.
            (
                "frame order",
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false),
            ),
            ("keyframes", kVTCompressionPropertyKey_MaxKeyFrameInterval, &gop),
            (
                "keyframe time",
                kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                &gop_secs,
            ),
            ("frame rate", kVTCompressionPropertyKey_ExpectedFrameRate, &rate),
            // The GPU's NV12 is BT.709, as the stream says it is.
            (
                "primaries",
                kVTCompressionPropertyKey_ColorPrimaries,
                kCVImageBufferColorPrimaries_ITU_R_709_2,
            ),
            (
                "transfer",
                kVTCompressionPropertyKey_TransferFunction,
                kCVImageBufferTransferFunction_ITU_R_709_2,
            ),
            (
                "matrix",
                kVTCompressionPropertyKey_YCbCrMatrix,
                kCVImageBufferYCbCrMatrix_ITU_R_709_2,
            ),
        ]
    };
    for (name, key, value) in settings {
        // SAFETY: a valid session, and each value of the key's type.
        let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
        if status != 0 {
            bail!("the media engine refused the {name} setting ({status})");
        }
    }
    let constant = set_rate(session, kbps, constant, true)?;
    // SAFETY: a valid session.
    let status = unsafe { session.prepare_to_encode_frames() };
    if status != 0 {
        bail!("the media engine could not prepare the encoder ({status})");
    }
    Ok(constant)
}

/// Ask `session` for `kbps` from the next frame on: held constant where the
/// engine takes that (`constant`, and it does), else — where it may
/// (`fallback`) — as the average; never with a data-rate limit, under which
/// the engine plays safe far below it (see `StreamEncoder::video_args`).
/// Whether it is held constant.
fn set_rate(
    session: &VTCompressionSession,
    kbps: u32,
    constant: bool,
    fallback: bool,
) -> anyhow::Result<bool> {
    let bits = CFNumber::new_i64(i64::from(kbps) * 1000);
    let key = if constant {
        // SAFETY: VideoToolbox's constant key.
        unsafe { kVTCompressionPropertyKey_ConstantBitRate }
    } else {
        // SAFETY: VideoToolbox's constant key.
        unsafe { kVTCompressionPropertyKey_AverageBitRate }
    };
    // SAFETY: a valid session; the rate is a number of bits a second.
    let status = unsafe { VTSessionSetProperty(session, key, Some(&bits)) };
    if status == 0 {
        return Ok(constant);
    }
    if constant && fallback {
        log::warn!("The media engine holds no constant rate here ({status}); averaging {kbps} kbps");
        return set_rate(session, kbps, false, false);
    }
    bail!("the media engine refused {kbps} kbps ({status})")
}

/// The media engine's callback: one encoded frame, or word that it was
/// dropped or failed. Runs on the engine's thread — or inside `encode` — in
/// the frames' order, and never waits.
unsafe extern "C-unwind" fn on_encoded(
    context: *mut c_void,
    frame: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: the context is the encoder's `Shared`, alive until its session
    // is invalidated, after which this is not called.
    let shared = unsafe { &*context.cast::<Shared>() };
    // The frame's context is its slot, as a number.
    let slot = frame as usize as u64;
    // A panic must not unwind into the media engine.
    let encoded = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if status != 0 {
            return Encoded::Failed(format!("encoding failed ({status})"));
        }
        if sample.is_null() || flags.contains(VTEncodeInfoFlags::FrameDropped) {
            return Encoded::Dropped { slot };
        }
        // SAFETY: a sample the engine hands us for the length of this call.
        unsafe { frame_of(&*sample, slot, shared.codec) }
            .unwrap_or_else(|| Encoded::Failed("an encoded frame could not be read".into()))
    }))
    .unwrap_or_else(|_| Encoded::Failed("the encoder's callback panicked".into()));
    shared.end(slot, encoded);
}

/// One encoded frame, copied out of the engine's sample: its units behind
/// their lengths and, for a keyframe, the configuration record. `None` when
/// the sample cannot be read.
///
/// # Safety
///
/// `sample` is a valid sample the media engine encoded with `codec`.
unsafe fn frame_of(sample: &CMSampleBuffer, slot: u64, codec: VideoCodec) -> Option<Encoded> {
    // SAFETY: a valid sample.
    let block = unsafe { sample.data_buffer() }?;
    // SAFETY: a valid block buffer.
    let len = unsafe { block.data_length() };
    let mut units = vec![0u8; len];
    // Copied across however many pieces the block is made of.
    // SAFETY: `units` has room for `len` bytes.
    let copied = unsafe { block.copy_data_bytes(0, len, NonNull::new(units.as_mut_ptr().cast())?) };
    if copied != 0 {
        return None;
    }
    // SAFETY: a valid sample.
    let key = unsafe { is_keyframe(sample) };
    let config = if key {
        // SAFETY: a valid sample.
        let description = unsafe { sample.format_description() }?;
        // SAFETY: a valid format description of `codec`.
        Some(unsafe { configuration_record(&description, codec) }?)
    } else {
        None
    };
    Some(Encoded::Frame {
        slot,
        key,
        units,
        config,
    })
}

/// The codec's configuration record in a format description — H.264's
/// `avcC`, HEVC's `hvcC` — as FLV carries it ahead of the frames.
///
/// # Safety
///
/// `description` is a valid video format description of `codec`.
unsafe fn configuration_record(description: &CMFormatDescription, codec: VideoCodec) -> Option<Vec<u8>> {
    // SAFETY: a valid description; CoreMedia's constant key.
    let atoms = unsafe {
        description.extension(kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms)
    }?;
    // The extension is a dictionary of atoms by name.
    let atoms = atoms.downcast_ref::<CFDictionary>()?;
    let name = CFString::from_static_str(match codec {
        VideoCodec::H264 => "avcC",
        VideoCodec::Hevc => "hvcC",
    });
    // SAFETY: a dictionary of CF types; the key is a CFString.
    let record = unsafe { atoms.value(std::ptr::from_ref::<CFString>(&name).cast()) };
    if record.is_null() {
        return None;
    }
    // SAFETY: the dictionary holds the record, alive while we hold the
    // description; it is checked to be CFData before it is read.
    let record = unsafe { &*record.cast::<CFType>() }.downcast_ref::<CFData>()?;
    Some(record.to_vec())
}

/// Whether a sample is a keyframe: one not marked as depending on others.
///
/// # Safety
///
/// `sample` is a valid sample.
unsafe fn is_keyframe(sample: &CMSampleBuffer) -> bool {
    // SAFETY: a valid sample.
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
        return true;
    };
    if attachments.count() == 0 {
        return true;
    }
    // SAFETY: the array holds one dictionary per sample.
    let first = unsafe { attachments.value_at_index(0) }.cast::<CFDictionary>();
    if first.is_null() {
        return true;
    }
    // SAFETY: a dictionary the sample holds; the key is CoreMedia's.
    let not_sync = unsafe {
        (*first).value(std::ptr::from_ref::<CFString>(kCMSampleAttachmentKey_NotSync).cast())
    };
    // SAFETY: the attachment's value is a CFBoolean when present.
    not_sync.is_null() || !unsafe { &*not_sync.cast::<CFBoolean>() }.as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each frame is told of once, however the engine's word of a drop
    /// comes: from `encode`, from the callback, or both.
    #[test]
    fn each_frame_is_told_of_once() {
        let mut told = Told::default();
        // Two dropped inside `encode`, their callbacks after, in order.
        assert!(told.dropped_in_encode(10));
        assert!(told.dropped_in_encode(11));
        assert!(!told.callback(10));
        assert!(!told.callback(11));
        assert!(told.callback(12));
        // A callback inside `encode`, before `encode` saw the drop.
        assert!(told.callback(13));
        assert!(!told.dropped_in_encode(13));
        // A drop whose callback never comes: forgotten once a later one does.
        assert!(told.dropped_in_encode(14));
        assert!(told.callback(15));
        assert!(told.skip.is_empty());
    }

    /// A gradient that moves, as NV12: something to encode.
    fn frame(width: usize, height: usize, n: usize) -> Vec<u8> {
        let mut nv12 = vec![128u8; width * height * 3 / 2];
        for (i, px) in nv12[..width * height].iter_mut().enumerate() {
            let (x, y) = (i % width, i / width);
            #[allow(clippy::cast_possible_truncation)] // Wrapped on purpose.
            let value = (x + y + n * 4) as u8;
            *px = value;
        }
        nv12
    }

    /// The media engine encodes every frame, in order, the first a keyframe
    /// with its configuration record, and takes a new rate while it runs.
    /// Skipped where there is no media engine, unless the tests are told a
    /// GPU is there.
    #[test]
    fn the_media_engine_encodes_each_frame_and_changes_its_rate() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (width, height) = (640, 360);
        let mut encoder =
            match VtEncoder::new(VideoCodec::H264, (width, height), 30, (2000, true), 1, tx) {
                Ok(encoder) => encoder,
                Err(e) if std::env::var_os("VARDA_REQUIRE_GPU").is_none() => {
                    eprintln!("no media engine here ({e}); skipping");
                    return;
                }
                Err(e) => panic!("the media engine would not start: {e}"),
            };
        for n in 0..60 {
            if n == 30 {
                encoder.set_kbps(500).expect("a new rate");
            }
            // Every other slot: the times need not follow on.
            encoder.encode(&frame(640, 360, n), n as u64 * 2).expect("a frame");
        }
        encoder.finish();
        assert_eq!(encoder.kbps(), 500);
        assert!(!encoder.failed());
        assert_eq!(encoder.in_flight(), 0, "every frame ended");
        drop(encoder);
        let ended: Vec<Encoded> = rx.iter().collect();
        assert_eq!(ended.len(), 60, "one end for each frame");
        let slots: Vec<u64> = ended
            .iter()
            .map(|e| match e {
                Encoded::Frame { slot, .. } | Encoded::Dropped { slot } => *slot,
                Encoded::Failed(why) => panic!("failed: {why}"),
            })
            .collect();
        assert_eq!(slots, (0..60).map(|n| n * 2).collect::<Vec<u64>>(), "in order, each its slot");
        let Encoded::Frame { key, config, units, .. } = &ended[0] else {
            panic!("the first frame dropped");
        };
        assert!(key, "a keyframe first");
        let config = config.as_ref().expect("with its configuration record");
        assert_eq!(config[0], 1, "an avcC record, version 1");
        assert_eq!(config[4] & 3, 3, "units behind four-byte lengths");
        let first = u32::from_be_bytes(units[..4].try_into().unwrap()) as usize;
        assert!(first + 4 <= units.len(), "units behind their lengths");
    }
}
