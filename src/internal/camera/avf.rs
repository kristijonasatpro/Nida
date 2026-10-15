//! Cameras on a Mac, straight from `AVFoundation`: the devices plugged in,
//! the sizes and rates each offers — at the rates they really run at — and a
//! capture session for one of them.
//!
//! The camera library the other platforms use (nokhwa) is not used here. It
//! cuts a device's rates down to whole numbers and then refuses to open one a
//! hair under a whole number (an ATEM at 60p says 59.9999, taken as 59); its
//! `frame()` never returns once a device is unplugged, so removing the input
//! hung Nida; and it ignores the padding at the end of each row, which shears
//! a 720-wide picture.
//!
//! Frames arrive on a dispatch queue of their own, are turned into RGBA
//! there — padding, colour matrix and range as the device tags them — and are
//! left in the camera's [`Sink`] for the render loop. A [`Capture`] is made,
//! watched and dropped on one thread, the camera's own (see
//! [`super::CameraManager`]), so a device that stalls never holds up the
//! render loop.

#![allow(unsafe_code)]

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use dispatch2::{DispatchQueue, DispatchRetained};
use nokhwa::utils::FrameFormat;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, Bool, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, AllocAnyThread, ClassType, DefinedClass};
use objc2_av_foundation::{
    AVCaptureCenterStageControlMode, AVCaptureSystemUserInterface,
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDeviceFormat,
    AVCaptureDeviceInput, AVCaptureDevicePosition, AVCaptureDeviceTypeBuiltInWideAngleCamera,
    AVCaptureDeviceTypeContinuityCamera, AVCaptureDeviceTypeExternal, AVCaptureOutput,
    AVCaptureSession, AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate,
    AVMediaTypeVideo,
};
use objc2_core_foundation::CFString;
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetDimensions,
};
use objc2_core_video::{
    kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_2020,
    kCVImageBufferYCbCrMatrix_ITU_R_601_4, kCVImageBufferYCbCrMatrix_ITU_R_709_2,
    kCVPixelBufferPixelFormatTypeKey, CVPixelBuffer, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};
use yuvutils_rs::{
    uyvy422_to_rgba, yuv_nv12_to_rgba, yuyv422_to_rgba, YuvBiPlanarImage, YuvConversionMode,
    YuvPackedImage, YuvRange, YuvStandardMatrix,
};

use super::{Frame, Rate, Sink};
use crate::engine::value::camera::{CameraColour, CameraMatrix, CameraRange};

const fn fourcc(code: [u8; 4]) -> u32 {
    u32::from_be_bytes(code)
}

/// 4:2:0, luma and chroma planes, video range: most cameras and capture
/// devices, the ATEM among them.
const NV12_VIDEO: u32 = fourcc(*b"420v");
/// The same, full range.
const NV12_FULL: u32 = fourcc(*b"420f");
/// 4:2:2 packed, chroma first (UYVY).
const UYVY: u32 = fourcc(*b"2vuy");
/// 4:2:2 packed, luma first (YUY2).
const YUYV: u32 = fourcc(*b"yuvs");
/// Blue, green, red, alpha.
const BGRA: u32 = fourcc(*b"BGRA");

/// A camera plugged in: who it is, and what it offers.
pub struct Device {
    /// Kept by macOS for the device across unplugging and restarts: what
    /// tells two of the same model apart.
    pub unique_id: String,
    pub name: String,
    pub rates: Vec<Rate>,
}

/// The cameras plugged in: built in, plugged in (USB, Thunderbolt, capture
/// devices) and iPhones used as cameras.
pub fn devices() -> Vec<Device> {
    autoreleasepool(|_| {
        discovered()
            .iter()
            .map(|device| Device {
                unique_id: unsafe { device.uniqueID() }.to_string(),
                name: unsafe { device.localizedName() }.to_string(),
                rates: rates_of(&device),
            })
            .collect()
    })
}

/// Whether this macOS has the `AVCaptureDevice` class method `selector`.
fn device_class_has(selector: Sel) -> bool {
    let class = AVCaptureDevice::class();
    // SAFETY: `respondsToSelector:` sent to the class object asks about its
    // class methods; it takes a selector and returns a BOOL.
    let has: Bool = unsafe { msg_send![class, respondsToSelector: selector] };
    has.as_bool()
}

/// The macOS Video Effects switched on for this app (the menu bar's camera
/// icon → Video Effects). macOS applies them inside the app to every camera
/// it opens, so whatever is on goes to air with the camera. Each is read
/// only where this macOS has it; Edge Light (macOS 26.2) is newer than the
/// bindings, so it is asked for by name.
pub fn video_effects_on() -> Vec<&'static str> {
    let mut on = Vec::new();
    // SAFETY: plain BOOL class properties, each checked to exist first.
    unsafe {
        if device_class_has(sel!(isPortraitEffectEnabled)) && AVCaptureDevice::isPortraitEffectEnabled() {
            on.push("Portrait");
        }
        if device_class_has(sel!(isStudioLightEnabled)) && AVCaptureDevice::isStudioLightEnabled() {
            on.push("Studio Light");
        }
        if device_class_has(sel!(isEdgeLightEnabled)) {
            let edge: Bool = msg_send![AVCaptureDevice::class(), isEdgeLightEnabled];
            if edge.as_bool() {
                on.push("Edge Light");
            }
        }
        if device_class_has(sel!(isBackgroundReplacementEnabled))
            && AVCaptureDevice::isBackgroundReplacementEnabled()
        {
            on.push("Background");
        }
        if device_class_has(sel!(reactionEffectGesturesEnabled))
            && AVCaptureDevice::reactionEffectGesturesEnabled()
        {
            on.push("Reactions gestures");
        }
    }
    on
}

/// Open Control Center's Video Effects for this app, where those effects
/// are switched: the app may read them, not change them. System UI, so on
/// the main thread — sent there, not waited for, from the engine thread.
pub fn show_video_effects() {
    dispatch2::DispatchQueue::main().exec_async(|| {
        // SAFETY: a class method taking a plain enum value, on the main thread.
        unsafe {
            AVCaptureDevice::showSystemUserInterface(AVCaptureSystemUserInterface::VideoEffects);
        }
    });
}

/// Keep Center Stage — a camera that pans and zooms after people by itself —
/// off, under the app's control, so no Control Center switch turns it on
/// mid-show. Only cameras that have it (an iPhone, a Studio Display) care.
pub fn keep_center_stage_off() {
    if !device_class_has(sel!(setCenterStageControlMode:)) {
        return;
    }
    // SAFETY: class setters taking plain values; in App mode the enable
    // switch may be set.
    unsafe {
        AVCaptureDevice::setCenterStageControlMode(AVCaptureCenterStageControlMode::App);
        AVCaptureDevice::setCenterStageEnabled(false);
    }
}

fn discovered() -> Retained<NSArray<AVCaptureDevice>> {
    let types = unsafe {
        NSArray::from_slice(&[
            AVCaptureDeviceTypeBuiltInWideAngleCamera,
            AVCaptureDeviceTypeExternal,
            AVCaptureDeviceTypeContinuityCamera,
        ])
    };
    unsafe {
        AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &types,
            AVMediaTypeVideo,
            AVCaptureDevicePosition::Unspecified,
        )
        .devices()
    }
}

/// The rates a device plugged in offers now, found by its unique ID; `None`
/// when it is not plugged in.
pub fn rates_now(unique_id: &str) -> Option<Vec<Rate>> {
    autoreleasepool(|_| {
        let device =
            unsafe { AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(unique_id)) }?;
        Some(rates_of(&device))
    })
}

fn rates_of(device: &AVCaptureDevice) -> Vec<Rate> {
    let mut rates: Vec<Rate> = Vec::new();
    for format in &unsafe { device.formats() } {
        let Some((width, height, code)) = picture_of(&format) else {
            continue;
        };
        for (fps, _) in format_rates(&format) {
            let rate = Rate {
                width,
                height,
                format: frame_format(code),
                fps,
                code,
            };
            let listed = rates.iter().any(|r| {
                (r.width, r.height, r.code) == (width, height, code) && (r.fps - fps).abs() < 1e-3
            });
            if !listed {
                rates.push(rate);
            }
        }
    }
    rates
}

/// A video format's size and pixel format; `None` for one that is not video.
fn picture_of(format: &AVCaptureDeviceFormat) -> Option<(u32, u32, u32)> {
    let video = unsafe { AVMediaTypeVideo }?;
    if *unsafe { format.mediaType() } != *video {
        return None;
    }
    let description = unsafe { format.formatDescription() };
    let size = unsafe { CMVideoFormatDescriptionGetDimensions(&description) };
    let code = unsafe { description.media_sub_type() };
    let width = u32::try_from(size.width).ok().filter(|w| *w > 0)?;
    let height = u32::try_from(size.height).ok().filter(|h| *h > 0)?;
    Some((width, height, code))
}

/// Each rate a format offers, with the frame duration that asks for it.
fn format_rates(format: &AVCaptureDeviceFormat) -> Vec<(f64, CMTime)> {
    let mut out = Vec::new();
    for range in &unsafe { format.videoSupportedFrameRateRanges() } {
        let (slowest, fastest) = unsafe { (range.minFrameRate(), range.maxFrameRate()) };
        let (shortest, longest) = unsafe { (range.minFrameDuration(), range.maxFrameDuration()) };
        for fps in super::rates_in_range(slowest, fastest) {
            // The ends exactly as the device gives them: 59.99988 is
            // 1000000/16667, not a rate rounded and refused.
            let duration = if (fps - fastest).abs() < 1e-6 {
                shortest
            } else if (fps - slowest).abs() < 1e-6 {
                longest
            } else {
                whole_frames(fps)
            };
            out.push((fps, duration));
        }
    }
    out
}

/// The frame duration of a rate between the ends of a range: a whole number
/// of frames, or a broadcast rate (29.97 is 1001/30000).
fn whole_frames(fps: f64) -> CMTime {
    #[allow(clippy::cast_possible_truncation)]
    let (value, timescale) = if (fps - fps.round()).abs() < 1e-3 {
        (1, fps.round() as i32)
    } else {
        (1001, (fps * 1001.0).round() as i32)
    };
    CMTime {
        value,
        timescale,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

/// How dear a pixel format is to turn into a picture, for choosing among the
/// same size and rate.
fn frame_format(code: u32) -> FrameFormat {
    match code {
        NV12_VIDEO | NV12_FULL => FrameFormat::NV12,
        UYVY | YUYV => FrameFormat::YUYV,
        c if c == fourcc(*b"dmb1") || c == fourcc(*b"jpeg") => FrameFormat::MJPEG,
        BGRA => FrameFormat::RAWBGR,
        _ => FrameFormat::RAWRGB,
    }
}

struct OutputIvars {
    sink: Arc<Sink>,
    /// The device's name, for the log.
    name: String,
    /// Whether its frames have been described in the log.
    described: AtomicBool,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - The class does not implement Drop.
    // - It is not main-thread-only: AVFoundation calls it on the dispatch
    //   queue given to `setSampleBufferDelegate:queue:`.
    #[unsafe(super(NSObject))]
    #[name = "NidaCameraFrameOutput"]
    #[ivars = OutputIvars]
    struct FrameOutput;

    unsafe impl NSObjectProtocol for FrameOutput {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for FrameOutput {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        unsafe fn did_output(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            let called = crate::host_clock::host_seconds();
            let ivars = self.ivars();
            let colour = ivars.sink.colour();
            if !ivars.described.swap(true, Ordering::Relaxed) {
                if let Some(image) = unsafe { sample_buffer.image_buffer() } {
                    log::info!(
                        "Camera '{}' frames: {}",
                        ivars.name,
                        unsafe { describe(&image, colour) }
                    );
                }
            }
            let sink = &ivars.sink;
            let mut data = sink.buffer();
            let fresh = data.capacity() == 0;
            match unsafe { picture(sample_buffer, &mut data, colour) } {
                Some((width, height)) => {
                    let (captured, stamped) = unsafe { captured_at(sample_buffer) };
                    sink.put_called(
                        Frame {
                            data,
                            width,
                            height,
                        },
                        captured,
                        Some(super::CallBack {
                            called,
                            stamped,
                            fresh,
                        }),
                    );
                }
                None => sink.recycle(data),
            }
        }
    }
);

impl FrameOutput {
    fn new(sink: Arc<Sink>, name: String) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars {
            sink,
            name,
            described: AtomicBool::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// One camera capturing: its session, and what keeps the frames coming.
/// Stopped when dropped.
pub struct Capture {
    session: Retained<AVCaptureSession>,
    device: Retained<AVCaptureDevice>,
    output: Retained<AVCaptureVideoDataOutput>,
    _delegate: Retained<FrameOutput>,
    _queue: DispatchRetained<DispatchQueue>,
    /// The size and rate it runs at.
    pub rate: Rate,
}

impl Capture {
    /// Start the device with this unique ID at `rate`, its frames into
    /// `sink`. Blocks until it runs, or has failed to.
    ///
    /// # Errors
    ///
    /// When the device is not plugged in, no longer offers that size and
    /// rate, is refused (no camera access), or would not start.
    pub fn open(unique_id: &str, rate: &Rate, sink: Arc<Sink>) -> Result<Self> {
        autoreleasepool(|_| Self::open_in_pool(unique_id, rate, sink))
    }

    fn open_in_pool(unique_id: &str, rate: &Rate, sink: Arc<Sink>) -> Result<Self> {
        let device = unsafe { AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(unique_id)) }
            .context("it is not plugged in")?;
        let (format, duration) = unsafe { device.formats() }
            .iter()
            .find_map(|format| {
                let (width, height, code) = picture_of(&format)?;
                if (width, height, code) != (rate.width, rate.height, rate.code) {
                    return None;
                }
                let (_, duration) = format_rates(&format)
                    .into_iter()
                    .find(|(fps, _)| (fps - rate.fps).abs() < 1e-3)?;
                Some((format, duration))
            })
            .with_context(|| format!("it no longer offers {}", rate.label()))?;
        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;

        let output = unsafe { AVCaptureVideoDataOutput::new() };
        // The device's own pixel format where it is one turned into a picture
        // here; any other (MJPEG, say) AVFoundation decodes to 4:2:0 first.
        let pixels = if matches!(rate.code, NV12_VIDEO | NV12_FULL | UYVY | YUYV) {
            rate.code
        } else {
            NV12_VIDEO
        };
        // CFString and NSString are the same object (toll-free bridged).
        let key: &NSString =
            unsafe { &*std::ptr::from_ref::<CFString>(kCVPixelBufferPixelFormatTypeKey).cast() };
        let value: Retained<AnyObject> = NSNumber::new_u32(pixels).into();
        let settings = NSDictionary::from_retained_objects(&[key], &[value]);
        unsafe {
            output.setVideoSettings(Some(&settings));
            output.setAlwaysDiscardsLateVideoFrames(true);
        }
        let name = unsafe { device.localizedName() }.to_string();
        let delegate = FrameOutput::new(sink, name);
        let queue = DispatchQueue::new("lt.proconf.nida.camera", None);
        unsafe {
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
        }

        let session = unsafe { AVCaptureSession::new() };
        unsafe { session.beginConfiguration() };
        let added = unsafe {
            session.canAddInput(&input) && {
                session.addInput(&input);
                session.canAddOutput(&output)
            }
        };
        if !added {
            unsafe { session.commitConfiguration() };
            anyhow::bail!("it cannot be captured (in use by another app?)");
        }
        unsafe { session.addOutput(&output) };
        if let Err(e) = unsafe { device.lockForConfiguration() } {
            unsafe { session.commitConfiguration() };
            anyhow::bail!("it cannot be set up: {}", e.localizedDescription());
        }
        // Its rate fixed at both ends: the rate asked for, not one the device
        // lowers in the dark. Kept locked until it runs, so the session does
        // not put back a format of its own choosing.
        unsafe {
            device.setActiveFormat(&format);
            device.setActiveVideoMinFrameDuration(duration);
            device.setActiveVideoMaxFrameDuration(duration);
            session.commitConfiguration();
            session.startRunning();
            device.unlockForConfiguration();
        }
        if !unsafe { session.isRunning() } {
            anyhow::bail!("it would not start");
        }
        Ok(Self {
            session,
            device,
            output,
            _delegate: delegate,
            _queue: queue,
            rate: *rate,
        })
    }

    /// Whether it is still capturing: running, and its device plugged in.
    pub fn running(&self) -> bool {
        unsafe { self.session.isRunning() && self.device.isConnected() }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            self.session.stopRunning();
            self.output.setSampleBufferDelegate_queue(None, None);
        }
    }
}

/// A frame's picture as RGBA into `out`, read into colours as `colour`
/// says, and its size; `None` for a frame without one, or in a pixel format
/// not read here.
///
/// # Safety
///
/// `sample_buffer` must be a live video sample buffer.
unsafe fn picture(
    sample_buffer: &CMSampleBuffer,
    out: &mut Vec<u8>,
    colour: CameraColour,
) -> Option<(u32, u32)> {
    let image = unsafe { sample_buffer.image_buffer() }?;
    let buffer: &CVPixelBuffer = &image;
    let lock = CVPixelBufferLockFlags::ReadOnly;
    if unsafe { CVPixelBufferLockBaseAddress(buffer, lock) } != 0 {
        return None;
    }
    // Everything below reaches the unlock: no `?` past here.
    let size = unsafe { read_locked(buffer, out, colour) };
    unsafe { CVPixelBufferUnlockBaseAddress(buffer, lock) };
    size
}

/// The range a frame's pixel format says it spans: full for '420f', limited
/// for every other YUV format read here.
fn range_said(code: u32) -> YuvRange {
    if code == NV12_FULL {
        YuvRange::Full
    } else {
        YuvRange::Limited
    }
}

/// # Safety
///
/// `buffer`'s base address must be locked.
unsafe fn read_locked(
    buffer: &CVPixelBuffer,
    out: &mut Vec<u8>,
    colour: CameraColour,
) -> Option<(u32, u32)> {
    let code = CVPixelBufferGetPixelFormatType(buffer);
    let (width, height) = (
        CVPixelBufferGetWidth(buffer),
        CVPixelBufferGetHeight(buffer),
    );
    let (w, h) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    if w == 0 || h == 0 {
        return None;
    }
    out.resize(width * height * 4, 0);
    // As the camera says, unless set by hand (see `CameraColour`).
    let matrix = super::picture_matrix(colour, unsafe { matrix_of(buffer, h) });
    let range = super::picture_range(colour, range_said(code));
    // A plane as bytes: its rows, each `stride` long, padding and all.
    let plane = |base: *mut std::ffi::c_void, stride: usize, rows: usize| {
        (!base.is_null())
            .then(|| unsafe { std::slice::from_raw_parts(base.cast::<u8>(), stride * rows) })
    };
    let ok = match code {
        NV12_VIDEO | NV12_FULL => {
            let y_stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
            let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1);
            let y = plane(
                CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
                y_stride,
                height,
            )?;
            let uv_rows = height
                .div_ceil(2)
                .min(CVPixelBufferGetHeightOfPlane(buffer, 1));
            let uv_row = width.div_ceil(2) * 2;
            let uv = plane(
                CVPixelBufferGetBaseAddressOfPlane(buffer, 1),
                uv_stride,
                uv_rows,
            )?;
            let uv = tight(uv, uv_stride, uv_row, uv_rows)?;
            let image = YuvBiPlanarImage {
                y_plane: y,
                y_stride: u32::try_from(y_stride).ok()?,
                uv_plane: &uv,
                uv_stride: u32::try_from(uv_row).ok()?,
                width: w,
                height: h,
            };
            yuv_nv12_to_rgba(
                &image,
                out,
                w * 4,
                range,
                matrix,
                YuvConversionMode::Balanced,
            )
            .is_ok()
        }
        UYVY | YUYV => {
            let stride = CVPixelBufferGetBytesPerRow(buffer);
            let row = width.div_ceil(2) * 4;
            let pixels = plane(CVPixelBufferGetBaseAddress(buffer), stride, height)?;
            let pixels = tight(pixels, stride, row, height)?;
            let packed = YuvPackedImage {
                yuy: &pixels,
                yuy_stride: u32::try_from(row).ok()?,
                width: w,
                height: h,
            };
            if code == UYVY {
                uyvy422_to_rgba(&packed, out, w * 4, range, matrix).is_ok()
            } else {
                yuyv422_to_rgba(&packed, out, w * 4, range, matrix).is_ok()
            }
        }
        BGRA => {
            let stride = CVPixelBufferGetBytesPerRow(buffer);
            let pixels = plane(CVPixelBufferGetBaseAddress(buffer), stride, height)?;
            for (row, dst) in out.chunks_exact_mut(width * 4).enumerate() {
                let src = &pixels[row * stride..row * stride + width * 4];
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src.as_chunks::<4>().0)
                {
                    *d = [s[2], s[1], s[0], 255];
                }
            }
            true
        }
        _ => false,
    };
    ok.then_some((w, h))
}

/// A plane's rows without the padding at the end of each: the plane itself
/// when it has none. yuvutils-rs refuses a padded chroma plane, or a padded
/// 4:2:2 picture, outright; `None` for rows longer than their stride.
fn tight(plane: &[u8], stride: usize, row: usize, rows: usize) -> Option<Cow<'_, [u8]>> {
    if row > stride || plane.len() < stride * rows {
        return None;
    }
    if row == stride {
        return Some(Cow::Borrowed(&plane[..row * rows]));
    }
    let mut packed = Vec::with_capacity(row * rows);
    for line in plane.chunks_exact(stride).take(rows) {
        packed.extend_from_slice(&line[..row]);
    }
    Some(Cow::Owned(packed))
}

/// When a frame was captured, host seconds: its sample time, which on a Mac
/// is on the host clock — or, when it is not (in the future, or more than a
/// second ago), when it came; and whether it was its sample time.
///
/// # Safety
///
/// `sample_buffer` must be a live sample buffer.
unsafe fn captured_at(sample_buffer: &CMSampleBuffer) -> (f64, bool) {
    let now = crate::host_clock::host_seconds();
    let sampled = unsafe { sample_buffer.presentation_time_stamp().seconds() };
    if sampled.is_finite() && (0.0..1.0).contains(&(now - sampled)) {
        (sampled, true)
    } else {
        (now, false)
    }
}

/// What a camera's frames are, for the log: pixel format, size, colour
/// matrix (tagged or assumed) and the range they are read in — each marked
/// when set by hand. A picture with more contrast than the camera's own
/// output — darks darker, brights brighter — is one read in the wrong range.
///
/// # Safety
///
/// `buffer` must be a live pixel buffer.
unsafe fn describe(buffer: &CVPixelBuffer, colour: CameraColour) -> String {
    let code = CVPixelBufferGetPixelFormatType(buffer);
    let (width, height) = (
        CVPixelBufferGetWidth(buffer),
        CVPixelBufferGetHeight(buffer),
    );
    let tag = unsafe { buffer.attachment(kCVImageBufferYCbCrMatrixKey, std::ptr::null_mut()) }
        .and_then(|v| v.downcast_ref::<CFString>().map(ToString::to_string));
    let said = unsafe { matrix_of(buffer, u32::try_from(height).unwrap_or(0)) };
    let matrix = super::picture_matrix(colour, said);
    let tagged = tag.map_or_else(|| "untagged".to_owned(), |t| format!("tagged {t}"));
    let by_hand = |set: bool| if set { " (set by hand)" } else { "" };
    let range = match code {
        NV12_VIDEO | NV12_FULL | UYVY | YUYV => {
            let read = match super::picture_range(colour, range_said(code)) {
                YuvRange::Full => "full range",
                YuvRange::Limited => "limited range",
            };
            format!("{read}{}", by_hand(colour.range != CameraRange::Auto))
        }
        _ => "RGB".to_owned(),
    };
    let matrix_note = if colour.matrix == CameraMatrix::Auto {
        tagged
    } else {
        format!("set by hand; {tagged}")
    };
    format!(
        "'{}' {width}×{height}, matrix {matrix:?} ({matrix_note}), read as {range}",
        String::from_utf8_lossy(&code.to_be_bytes()),
    )
}

/// The colour matrix a frame is tagged with — or, untagged, the one its
/// size implies: BT.601 below 720 lines, BT.709 from there.
///
/// # Safety
///
/// `buffer` must be a live pixel buffer.
unsafe fn matrix_of(buffer: &CVPixelBuffer, height: u32) -> YuvStandardMatrix {
    let tagged = unsafe { buffer.attachment(kCVImageBufferYCbCrMatrixKey, std::ptr::null_mut()) };
    let name = tagged.and_then(|v| v.downcast_ref::<CFString>().map(ToString::to_string));
    let known = unsafe {
        [
            (
                kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                YuvStandardMatrix::Bt709,
            ),
            (
                kCVImageBufferYCbCrMatrix_ITU_R_601_4,
                YuvStandardMatrix::Bt601,
            ),
            (
                kCVImageBufferYCbCrMatrix_ITU_R_2020,
                YuvStandardMatrix::Bt2020,
            ),
        ]
    };
    name.and_then(|name| {
        known
            .iter()
            .find(|(tag, _)| tag.to_string() == name)
            .map(|(_, matrix)| *matrix)
    })
    .unwrap_or_else(|| super::untagged_matrix(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_between_the_ends_is_asked_for_in_whole_frames() {
        let t = whole_frames(25.0);
        assert_eq!((t.value, t.timescale), (1, 25));
        let t = whole_frames(29.97);
        assert_eq!((t.value, t.timescale), (1001, 30000));
    }

    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{CVAttachmentMode, CVPixelBufferCreate};
    use std::ptr::NonNull;

    /// A 4:2:0 picture as a device would send it: luma `y` and chroma
    /// `cb`, `cr` wherever `paint(x, y)` says, black elsewhere, and the
    /// padding at the end of each row full of 255s.
    fn nv12(
        width: usize,
        height: usize,
        paint: impl Fn(usize, usize) -> Option<(u8, u8, u8)>,
    ) -> CFRetained<CVPixelBuffer> {
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                NV12_VIDEO,
                None,
                NonNull::from(&mut out),
            )
        };
        assert_eq!(status, 0);
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(out).unwrap()) };
        let unlocked = CVPixelBufferLockFlags(0);
        assert_eq!(
            unsafe { CVPixelBufferLockBaseAddress(&buffer, unlocked) },
            0
        );
        let plane = |i: usize, rows: usize| {
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, i);
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, i).cast::<u8>();
            (
                unsafe { std::slice::from_raw_parts_mut(base, stride * rows) },
                stride,
            )
        };
        let (luma, y_stride) = plane(0, height);
        luma.fill(255);
        for (row, line) in luma.chunks_exact_mut(y_stride).enumerate() {
            for (x, v) in line[..width].iter_mut().enumerate() {
                *v = paint(x, row).map_or(16, |p| p.0);
            }
        }
        let (chroma, uv_stride) = plane(1, height / 2);
        chroma.fill(255);
        for (row, line) in chroma.chunks_exact_mut(uv_stride).enumerate() {
            for (x, pair) in line[..width].as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let (_, cb, cr) = paint(x * 2, row * 2).unwrap_or((16, 128, 128));
                *pair = [cb, cr];
            }
        }
        unsafe { CVPixelBufferUnlockBaseAddress(&buffer, unlocked) };
        buffer
    }

    fn read(buffer: &CVPixelBuffer) -> (Vec<u8>, (u32, u32)) {
        read_as(buffer, CameraColour::default())
    }

    fn read_as(buffer: &CVPixelBuffer, colour: CameraColour) -> (Vec<u8>, (u32, u32)) {
        let mut rgba = Vec::new();
        let lock = CVPixelBufferLockFlags::ReadOnly;
        assert_eq!(unsafe { CVPixelBufferLockBaseAddress(buffer, lock) }, 0);
        let size = unsafe { read_locked(buffer, &mut rgba, colour) };
        unsafe { CVPixelBufferUnlockBaseAddress(buffer, lock) };
        (rgba, size.expect("a picture"))
    }

    /// A camera tagged limited range whose picture is really full range
    /// (an ATEM's USB picture, next to its HDMI output) is read right when
    /// its range is set to Full: video white, 235, stays a light grey
    /// instead of being stretched to white, and video black, 16, a dark
    /// grey instead of black. Left on Auto, or set to Limited, it is read
    /// as tagged.
    #[test]
    fn a_range_set_by_hand_overrides_the_one_the_camera_says() {
        // White on the left half, black on the right, tagged '420v'.
        let buffer = nv12(64, 480, |x, _| (x < 32).then_some((235, 128, 128)));
        let level = |colour: CameraColour| {
            let (rgba, _) = read_as(&buffer, colour);
            // Green, away from the edge: white at x 8, black at x 56.
            (rgba[8 * 4 + 1], rgba[56 * 4 + 1])
        };
        let with = |range: CameraRange| CameraColour {
            range,
            ..CameraColour::default()
        };
        let (white, black) = level(with(CameraRange::Auto));
        assert!(white > 250 && black < 5, "as tagged: {white} {black}");
        assert_eq!(level(with(CameraRange::Limited)), (white, black));
        let (white, black) = level(with(CameraRange::Full));
        assert!((230..=240).contains(&white), "full range: white {white}");
        assert!((12..=20).contains(&black), "full range: black {black}");
    }

    /// A matrix set by hand overrides the tag: a picture tagged BT.709
    /// read as BT.601 comes out as the BT.601 one does.
    #[test]
    fn a_matrix_set_by_hand_overrides_the_tag() {
        let tagged = |matrix: &CFString| {
            let buffer = nv12(64, 480, |_, _| Some((81, 90, 240)));
            unsafe {
                buffer.set_attachment(
                    kCVImageBufferYCbCrMatrixKey,
                    matrix,
                    CVAttachmentMode::ShouldPropagate,
                );
            }
            buffer
        };
        let as_601 = CameraColour {
            matrix: CameraMatrix::Bt601,
            ..CameraColour::default()
        };
        let bt601 = read(&tagged(unsafe { kCVImageBufferYCbCrMatrix_ITU_R_601_4 })).0;
        let forced = read_as(&tagged(unsafe { kCVImageBufferYCbCrMatrix_ITU_R_709_2 }), as_601).0;
        assert_eq!(&forced[..4], &bt601[..4]);
    }

    /// The log line says what the camera sends and how it is read, marking
    /// what was set by hand.
    #[test]
    fn a_cameras_frames_are_described_as_read() {
        let buffer = nv12(64, 480, |_, _| None);
        let auto = unsafe { describe(&buffer, CameraColour::default()) };
        assert_eq!(auto, "'420v' 64×480, matrix Bt601 (untagged), read as limited range");
        let full = CameraColour {
            range: CameraRange::Full,
            matrix: CameraMatrix::Bt709,
        };
        let by_hand = unsafe { describe(&buffer, full) };
        assert_eq!(
            by_hand,
            "'420v' 64×480, matrix Bt709 (set by hand; untagged), read as full range (set by hand)"
        );
    }

    /// A 720-wide picture comes with its rows padded out: read row by row it
    /// stands upright. Read as if packed, each row would start further along
    /// than the last, and the picture shear.
    #[test]
    fn a_picture_with_padded_rows_is_read_upright() {
        let (width, height) = (720, 480);
        // The right half white.
        let buffer = nv12(width, height, |x, _| {
            (x >= width / 2).then_some((235, 128, 128))
        });
        assert!(
            CVPixelBufferGetBytesPerRowOfPlane(&buffer, 0) > width,
            "rows padded"
        );
        let (rgba, size) = read(&buffer);
        assert_eq!(size, (720, 480));
        for row in [0, height / 2, height - 1] {
            let red = |x: usize| rgba[(row * width + x) * 4];
            assert!(
                red(width / 2 - 2) < 5,
                "row {row}: black left of the middle"
            );
            assert!(red(width / 2 + 1) > 250, "row {row}: white right of it");
            assert!(
                red(width - 1) > 250,
                "row {row}: to the end, no padding shown"
            );
        }
    }

    /// Colour is read with the matrix the picture is tagged with — the same
    /// numbers are a purer red in BT.601 than in BT.709 — and an untagged SD
    /// picture as BT.601.
    #[test]
    fn colour_follows_the_matrix_a_picture_is_tagged_with() {
        let red = |matrix: Option<&CFString>| {
            let buffer = nv12(64, 480, |_, _| Some((81, 90, 240)));
            if let Some(matrix) = matrix {
                unsafe {
                    buffer.set_attachment(
                        kCVImageBufferYCbCrMatrixKey,
                        matrix,
                        CVAttachmentMode::ShouldPropagate,
                    );
                }
            }
            let (rgba, _) = read(&buffer);
            [rgba[0], rgba[1], rgba[2]]
        };
        let bt601 = red(Some(unsafe { kCVImageBufferYCbCrMatrix_ITU_R_601_4 }));
        let bt709 = red(Some(unsafe { kCVImageBufferYCbCrMatrix_ITU_R_709_2 }));
        assert!(bt601[0] > 245 && bt601[1] < 6 && bt601[2] < 6, "{bt601:?}");
        assert!((18..=30).contains(&bt709[1]), "{bt709:?}");
        assert_eq!(red(None), bt601, "SD untagged: BT.601");
    }

    /// How long a scan takes, by hand:
    /// `cargo test --lib avf::tests::timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "a timing, not a test"]
    fn timing_of_a_scan() {
        let _ = devices();
        let start = std::time::Instant::now();
        for _ in 0..10 {
            let _ = devices();
        }
        println!("scan: {:.2} ms", start.elapsed().as_secs_f64() * 100.0);
    }

    #[test]
    fn pixel_formats_are_ranked_by_what_they_cost() {
        assert_eq!(frame_format(NV12_VIDEO), FrameFormat::NV12);
        assert_eq!(frame_format(fourcc(*b"yuvs")), FrameFormat::YUYV);
        assert_eq!(frame_format(fourcc(*b"dmb1")), FrameFormat::MJPEG);
    }

    /// Lists the cameras plugged in and what they offer — and with
    /// `NIDA_OPEN_CAMERAS=1`, opens each briefly at its best (which sets the
    /// device's format for every app using it). Touches real devices, so
    /// only by hand: `cargo test --lib avf::tests::real -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a camera plugged in, and camera access"]
    fn real_cameras_open_at_their_best() {
        for device in devices() {
            println!("{} ({})", device.name, device.unique_id);
            for rate in &device.rates {
                println!("  {} {:?} {:.5}", rate.label(), rate.format, rate.fps);
            }
            let modes = super::super::CameraDeviceInfo::modes_of(&device.rates);
            println!("  modes {modes:?}");
            let Some(best) = super::super::choose(&device.rates, None) else {
                continue;
            };
            if std::env::var_os("NIDA_OPEN_CAMERAS").is_none() {
                continue;
            }
            let sink = Arc::new(Sink::default());
            let capture = Capture::open(&device.unique_id, &best, Arc::clone(&sink));
            println!("  opened at {}: {:?}", best.label(), capture.as_ref().err());
            std::thread::sleep(std::time::Duration::from_secs(2));
            println!("  {} frames", sink.frames());
            drop(capture);
        }
    }
}
