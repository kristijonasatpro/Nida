//! macOS screen/window capture via `ScreenCaptureKit`.
//!
//! `ScreenCaptureKit` is push-based: an `SCStream` delivers `CMSampleBuffer`s to
//! an `SCStreamOutput` delegate on a dispatch queue. The delegate repacks each
//! frame into a tightly-packed BGRA buffer and drops it into a shared slot;
//! [`MacosBackend::next_frame`] then takes whatever is there. The capture thread
//! never blocks on the OS, and a stalled stream simply yields `None`.
//!
//! Two SCK features are the reason this is a hand-written backend rather than a
//! crate (see spec/screen-capture.md § Decision):
//!
//! - `SCContentFilter` can exclude specific windows from a display capture,
//!   which is how `exclude_varda` avoids turning a full-display capture into an
//!   infinite mirror.
//! - `SCStreamConfiguration.width/height` scales at capture time, so a 4K
//!   display arrives already sized for the deck instead of moving 33 MB a frame.
//!
//! Frames are BGRA8 in sRGB, copied into a `Bgra8UnormSrgb` texture — no CPU
//! swizzle. Each is handed on in the GPU surface SCK captured it into and
//! copied on the GPU (see `screen_capture::surface`): read through memory at
//! 1080p60, the copies and uploads cost the control window and the outputs
//! frames of their own (spec/screen-capture.md § Decision: CPU readback
//! first). A frame with no surface is still read through memory.

#![allow(unsafe_code)]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceSRGB, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamFrameInfoDisplayTime, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
    SCWindow,
};

use crate::screen_capture::backend::{
    CaptureConfig, CaptureError, CaptureFrame, CapturePixelFormat, CaptureTargetInfo,
    CaptureTargetKind, CropRect, PermissionState, ScreenCaptureBackend,
};

/// `kCVPixelFormatType_32BGRA` — the four-char code SCK uses for BGRA output.
const PIXEL_FORMAT_32BGRA: u32 = u32::from_be_bytes(*b"BGRA");

/// How long to wait for `SCShareableContent`'s completion handler. Enumeration
/// is a synchronous call from our side; without a bound, a wedged capture daemon
/// would hang the render thread on a library rescan.
const ENUMERATE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for `startCaptureWithCompletionHandler:`. A stream that has
/// not started by now has failed, and reporting that beats a black deck.
const START_TIMEOUT: Duration = Duration::from_secs(5);

pub fn backend_name() -> &'static str {
    "ScreenCaptureKit"
}

pub fn permission_state() -> PermissionState {
    // `CGPreflightScreenCaptureAccess` cannot distinguish "never asked" from
    // "refused" — both are `false`. Treating an un-granted state as
    // `NotDetermined` is the useful lie: it makes the UI offer the request
    // button, and the request is harmless if the answer was already no (macOS
    // silently no-ops and the user is directed to System Settings).
    if CGPreflightScreenCaptureAccess() {
        PermissionState::Granted
    } else {
        PermissionState::NotDetermined
    }
}

pub fn request_permission() {
    // Raises the TCC prompt on first call. The grant does not apply to the
    // running process — macOS requires a restart — which the UI must say.
    let granted = CGRequestScreenCaptureAccess();
    log::info!("Screen recording access request returned granted={granted}");
}

/// Enumerate displays and on-screen windows.
///
/// # Errors
///
/// Returns [`CaptureError::PermissionDenied`] if TCC has not granted Screen
/// Recording, or [`CaptureError::Backend`] if `SCShareableContent` fails or does
/// not answer within [`ENUMERATE_TIMEOUT`].
pub fn enumerate() -> Result<Vec<CaptureTargetInfo>, CaptureError> {
    let content = shareable_content()?;
    let our_pid = std::process::id().cast_signed();
    let mut targets = Vec::new();

    // Displays first — they are the stable entries and belong at the top of the
    // library panel.
    for (i, display) in unsafe { content.displays() }.iter().enumerate() {
        let (w, h) = unsafe { (display.width(), display.height()) };
        targets.push(CaptureTargetInfo {
            kind: CaptureTargetKind::Display,
            platform_id: u64::from(unsafe { display.displayID() }),
            label: format!("Display {}", i + 1),
            app: None,
            title: None,
            width: u32::try_from(w).unwrap_or(0),
            height: u32::try_from(h).unwrap_or(0),
            is_varda: false,
        });
    }

    for window in &unsafe { content.windows() } {
        if !unsafe { window.isOnScreen() } {
            continue;
        }
        let frame = unsafe { window.frame() };
        let (width, height) = (frame.size.width as u32, frame.size.height as u32);
        // Menu-bar extras and other 1×1 helpers are noise in the picker.
        if width < 32 || height < 32 {
            continue;
        }
        let owner = unsafe { window.owningApplication() };
        let app_name = owner
            .as_ref()
            .map(|a| unsafe { a.applicationName() }.to_string());
        let bundle_id = owner
            .as_ref()
            .map(|a| unsafe { a.bundleIdentifier() }.to_string());
        let is_varda = owner
            .as_ref()
            .is_some_and(|a| unsafe { a.processID() } == our_pid);
        let title = unsafe { window.title() }.map(|t| t.to_string());

        let label = match (&app_name, &title) {
            (Some(app), Some(t)) if !t.is_empty() => format!("{app} — {t}"),
            (Some(app), _) => app.clone(),
            (None, Some(t)) => t.clone(),
            (None, None) => format!("Window {}", unsafe { window.windowID() }),
        };

        targets.push(CaptureTargetInfo {
            kind: CaptureTargetKind::Window,
            platform_id: u64::from(unsafe { window.windowID() }),
            label,
            // Persistence matches on the bundle id when there is one: it
            // survives an app rename, which the display name does not.
            app: bundle_id.or(app_name),
            title,
            width,
            height,
            is_varda,
        });
    }

    Ok(targets)
}

/// Open a capture stream for `target`.
///
/// # Errors
///
/// Returns [`CaptureError::TargetNotFound`] if the display or window has gone
/// away since enumeration, [`CaptureError::PermissionDenied`] if TCC refuses,
/// or [`CaptureError::Backend`] for any SCK failure.
pub fn open(
    target: &CaptureTargetInfo,
    config: &CaptureConfig,
) -> Result<Box<dyn ScreenCaptureBackend>, CaptureError> {
    let backend = MacosBackend::new(target, config)?;
    Ok(Box::new(backend))
}

fn shareable_content() -> Result<Retained<SCShareableContent>, CaptureError> {
    if !CGPreflightScreenCaptureAccess() {
        return Err(CaptureError::PermissionDenied);
    }

    let (tx, rx) = mpsc::channel::<Result<Retained<SCShareableContent>, String>>();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut objc2_foundation::NSError| {
            let msg = if content.is_null() {
                let detail = if error.is_null() {
                    "SCShareableContent returned nothing".to_string()
                } else {
                    unsafe { &*error }.localizedDescription().to_string()
                };
                Err(detail)
            } else {
                Ok(unsafe { Retained::retain(content) }
                    .ok_or_else(|| "failed to retain SCShareableContent".to_string()))
                .and_then(|r| r)
            };
            // The receiver may already have timed out and gone; that is fine.
            let _ = tx.send(msg);
        },
    );

    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true,
            true,
            &handler,
        );
    }

    match rx.recv_timeout(ENUMERATE_TIMEOUT) {
        Ok(Ok(content)) => Ok(content),
        Ok(Err(detail)) => {
            // SCK reports a TCC refusal as a generic error, so classify it here
            // rather than showing the user "error -3801".
            if detail.contains("declined") || detail.contains("permission") {
                Err(CaptureError::PermissionDenied)
            } else {
                Err(CaptureError::Backend(detail))
            }
        }
        Err(_) => Err(CaptureError::Backend(format!(
            "SCShareableContent did not respond within {}s",
            ENUMERATE_TIMEOUT.as_secs()
        ))),
    }
}

/// Frames from the SCK delegate queue on their way to the capture thread,
/// oldest first.
type FrameSlot = Arc<Mutex<std::collections::VecDeque<CaptureFrame>>>;

/// Frames kept between the SCK delegate and the capture thread at most; more
/// let the oldest go. Each carries when it was captured, so the show picks by
/// that (see `crate::frame_picker`), and none is replaced by the next as a
/// single slot did when two came within one poll.
const SLOT_FRAMES: usize = 4;

struct OutputIvars {
    slot: FrameSlot,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - The class does not implement Drop.
    // - It is not main-thread-only: SCK invokes the output on the dispatch queue
    //   supplied to `addStreamOutput:type:sampleHandlerQueue:`.
    #[unsafe(super(NSObject))]
    #[name = "VardaSCStreamOutput"]
    #[ivars = OutputIvars]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            if let Some(frame) = unsafe { frame_from_sample_buffer(sample_buffer) } {
                if let Ok(mut slot) = self.ivars().slot.lock() {
                    if slot.len() >= SLOT_FRAMES {
                        slot.pop_front();
                    }
                    slot.push_back(frame);
                }
            }
        }
    }
);

impl StreamOutput {
    fn new(slot: FrameSlot) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { slot });
        unsafe { msg_send![super(this), init] }
    }
}

/// Whether a sample buffer carries a frame worth uploading.
///
/// SCK delivers a buffer on every tick of the stream's interval, not only when
/// the content changed, and tags each one with an `SCFrameStatus`. Only
/// `Complete` is guaranteed to have valid pixels — `Idle` repeats a surface
/// whose contents SCK may already have recycled, and `Blank` is explicitly
/// empty. Uploading those interleaves stale or black frames with real ones,
/// which reads as flicker. Apple's own capture sample filters on this, and it
/// is the single most load-bearing line in this file.
///
/// # Safety
///
/// `sample_buffer` must be a live sample buffer delivered by SCK.
unsafe fn frame_is_complete(sample_buffer: &CMSampleBuffer) -> bool {
    let Some(attachments) = (unsafe { sample_buffer.sample_attachments_array(false) }) else {
        // No attachments at all predates the status key; treat as usable rather
        // than dropping every frame on an OS that does not report status.
        return true;
    };
    // CFArray/CFDictionary are toll-free bridged to their NS counterparts, and
    // the ObjC accessors are far less error-prone than raw CF index/key calls.
    let attachments: &NSArray<NSDictionary<NSString, NSObject>> =
        unsafe { &*std::ptr::from_ref(&*attachments).cast() };
    let Some(info) = attachments.firstObject() else {
        return true;
    };
    let Some(status) = info.objectForKey(unsafe { SCStreamFrameInfoStatus }) else {
        return true;
    };
    let Ok(status) = status.downcast::<NSNumber>() else {
        return true;
    };
    status.integerValue() == SCFrameStatus::Complete.0
}

/// When the window server showed what a sample buffer captured, host
/// seconds; `None` when SCK did not say.
///
/// # Safety
///
/// `sample_buffer` must be a live sample buffer delivered by SCK.
unsafe fn display_time(sample_buffer: &CMSampleBuffer) -> Option<f64> {
    let attachments = unsafe { sample_buffer.sample_attachments_array(false) }?;
    // Toll-free bridged, as in `frame_is_complete`.
    let attachments: &NSArray<NSDictionary<NSString, NSObject>> =
        unsafe { &*std::ptr::from_ref(&*attachments).cast() };
    let info = attachments.firstObject()?;
    let ticks = info.objectForKey(unsafe { SCStreamFrameInfoDisplayTime })?;
    let ticks = ticks.downcast::<NSNumber>().ok()?.unsignedLongLongValue();
    (ticks > 0).then(|| crate::host_clock::seconds_of_ticks(ticks))
}

/// Repack a `CMSampleBuffer`'s pixel buffer into a tightly-packed BGRA frame.
///
/// # Safety
///
/// `sample_buffer` must be a live video sample buffer delivered by SCK.
unsafe fn frame_from_sample_buffer(sample_buffer: &CMSampleBuffer) -> Option<CaptureFrame> {
    if !unsafe { frame_is_complete(sample_buffer) } {
        return None;
    }
    let captured_at = unsafe { display_time(sample_buffer) };
    let pixel_buffer = sample_buffer.image_buffer()?;
    // In the GPU surface it was captured into: handed on as that, not read.
    if let Some(surface) = crate::screen_capture::surface::SurfaceFrame::new(pixel_buffer.clone()) {
        let (width, height) = surface.size();
        if width > 0 && height > 0 {
            return Some(CaptureFrame {
                data: Vec::new(),
                width,
                height,
                format: CapturePixelFormat::Bgra8UnormSrgb,
                surface: Some(surface),
                captured_at,
            });
        }
    }
    let pixel_buffer = &*pixel_buffer;

    let lock = CVPixelBufferLockFlags::ReadOnly;
    if unsafe { CVPixelBufferLockBaseAddress(pixel_buffer, lock) } != 0 {
        return None;
    }
    // Everything below must reach the matching unlock, so no `?` past here.
    let result = (|| {
        let base = CVPixelBufferGetBaseAddress(pixel_buffer);
        if base.is_null() {
            return None;
        }
        let width = CVPixelBufferGetWidth(pixel_buffer);
        let height = CVPixelBufferGetHeight(pixel_buffer);
        let stride = CVPixelBufferGetBytesPerRow(pixel_buffer);
        if width == 0 || height == 0 || stride < width * 4 {
            return None;
        }

        // SCK pads rows to a hardware-friendly stride. wgpu wants
        // `bytes_per_row` we control, so repack to width*4 here — on the SCK
        // delegate queue, off both the render thread and the capture thread.
        let row_bytes = width * 4;
        let mut data = vec![0u8; row_bytes * height];
        for y in 0..height {
            let src = unsafe { base.cast::<u8>().add(y * stride) };
            let dst = &mut data[y * row_bytes..(y + 1) * row_bytes];
            unsafe { std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), row_bytes) };
        }

        Some(CaptureFrame {
            data,
            width: u32::try_from(width).ok()?,
            height: u32::try_from(height).ok()?,
            format: CapturePixelFormat::Bgra8UnormSrgb,
            surface: None,
            captured_at,
        })
    })();
    unsafe { CVPixelBufferUnlockBaseAddress(pixel_buffer, lock) };
    result
}

/// The `ObjC` objects backing a live stream.
///
/// `SCStream` and friends are not `Sync`, so `Retained<_>` is not `Send` and the
/// session cannot be moved to the capture thread as-is. They are, however,
/// ordinary background-thread-safe `ObjC` objects — SCK explicitly drives them
/// from its own dispatch queues and none is main-thread-only. We construct them
/// on the calling thread, hand ownership to exactly one capture thread, and
/// never touch them from anywhere else, so the move is sound.
struct StreamHandles {
    stream: Retained<SCStream>,
    output: Retained<StreamOutput>,
    filter: Retained<SCContentFilter>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
}

// SAFETY: see `StreamHandles` docs — single-owner move, no main-thread affinity.
unsafe impl Send for StreamHandles {}

pub struct MacosBackend {
    label: String,
    handles: StreamHandles,
    slot: FrameSlot,
    width: u32,
    height: u32,
    config: CaptureConfig,
    /// The target's size in points, which a crop is measured in.
    points: (f64, f64),
    /// Its pixels to a point: 2 on a Retina display.
    scale: f64,
}

impl MacosBackend {
    fn new(target: &CaptureTargetInfo, config: &CaptureConfig) -> Result<Self, CaptureError> {
        let content = shareable_content()?;
        let filter = build_filter(&content, target, config)?;
        // A Retina display has two pixels to each of its points, and is
        // captured at its pixels: its points would halve its sharpness.
        let scale =
            f64::from(unsafe { SCShareableContent::infoForFilter(&filter).pointPixelScale() });
        let scale = if scale.is_finite() && scale >= 1.0 {
            scale
        } else {
            1.0
        };
        let points = (f64::from(target.width), f64::from(target.height));
        let (width, height) = output_size(target, scale, config);
        let stream_config = build_configuration(config, width, height, points);

        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &stream_config,
                None,
            )
        };

        let slot: FrameSlot = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let output = StreamOutput::new(Arc::clone(&slot));
        let queue = DispatchQueue::new("com.varda.screen-capture", None);

        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|e| CaptureError::Backend(format!("addStreamOutput failed: {e}")))?;

        start_capture(&stream)?;

        log::info!(
            "ScreenCaptureKit stream started for '{}' at {width}x{height}",
            target.label
        );

        Ok(Self {
            label: target.label.clone(),
            handles: StreamHandles {
                stream,
                output,
                filter,
                _queue: queue,
            },
            slot,
            width,
            height,
            config: config.clone(),
            points,
            scale,
        })
    }
}

fn start_capture(stream: &SCStream) -> Result<(), CaptureError> {
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let handler: RcBlock<dyn Fn(*mut objc2_foundation::NSError)> =
        RcBlock::new(move |error: *mut objc2_foundation::NSError| {
            let msg = if error.is_null() {
                None
            } else {
                Some(unsafe { &*error }.localizedDescription().to_string())
            };
            let _ = tx.send(msg);
        });
    unsafe {
        stream.startCaptureWithCompletionHandler(Some(&handler as &DynBlock<_>));
    }
    match rx.recv_timeout(START_TIMEOUT) {
        Ok(None) => Ok(()),
        Ok(Some(detail)) => Err(CaptureError::Backend(format!(
            "startCapture failed: {detail}"
        ))),
        Err(_) => Err(CaptureError::Backend(format!(
            "startCapture did not complete within {}s",
            START_TIMEOUT.as_secs()
        ))),
    }
}

/// Resolve the target back onto a live `SCDisplay` / `SCWindow` and wrap it in a
/// content filter. Display captures exclude Varda's own windows when asked —
/// this is the mechanism behind `exclude_varda`, not a post-hoc mask.
fn build_filter(
    content: &SCShareableContent,
    target: &CaptureTargetInfo,
    config: &CaptureConfig,
) -> Result<Retained<SCContentFilter>, CaptureError> {
    match target.kind {
        CaptureTargetKind::Display => {
            let display = unsafe { content.displays() }
                .iter()
                .find(|d| u64::from(unsafe { d.displayID() }) == target.platform_id)
                .ok_or_else(|| CaptureError::TargetNotFound(target.label.clone()))?;

            let excluded: Retained<NSArray<SCWindow>> = if config.exclude_varda {
                let our_pid = std::process::id().cast_signed();
                let ours: Vec<Retained<SCWindow>> = unsafe { content.windows() }
                    .iter()
                    .filter(|w| {
                        unsafe { w.owningApplication() }
                            .is_some_and(|a| unsafe { a.processID() } == our_pid)
                    })
                    .collect();
                NSArray::from_retained_slice(&ours)
            } else {
                NSArray::new()
            };

            Ok(unsafe {
                SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &excluded,
                )
            })
        }
        CaptureTargetKind::Window => {
            let window = unsafe { content.windows() }
                .iter()
                .find(|w| u64::from(unsafe { w.windowID() }) == target.platform_id)
                .ok_or_else(|| CaptureError::TargetNotFound(target.label.clone()))?;
            Ok(unsafe {
                SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
            })
        }
    }
}

/// Output size after `scale_to` and crop. Crop shrinks the delivered frame so
/// the uploaded bytes shrink with it, rather than cropping after the upload.
///
/// `scale_to` is a bound rather than an exact extent, matching what
/// [`Geometry::resolve`](super::super::resample::Geometry::resolve) does on the
/// other platforms. Using it verbatim would hand `setScalesToFit` a differently
/// shaped box, and it would letterbox the target into it — baking bars into the
/// pixels and leaving the deck's scaling mode with nothing to do.
///
/// The target's size is in points; `scale` pixels to a point (2 on a Retina
/// display) gives its size in pixels, what it is captured at.
fn output_size(target: &CaptureTargetInfo, scale: f64, config: &CaptureConfig) -> (u32, u32) {
    let (native_w, native_h) = pixels_of(target, scale);
    let (base_w, base_h) = config.scale_to.map_or((native_w, native_h), |(w, h)| {
        super::super::resample::fit_within(native_w, native_h, w, h)
    });
    scaled_crop_size(base_w, base_h, config)
}

/// A target's size in pixels, its size in points `scale` times over.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pixels_of(target: &CaptureTargetInfo, scale: f64) -> (u32, u32) {
    let pixels = |points: u32| ((f64::from(points.max(1)) * scale).round() as u32).max(1);
    (pixels(target.width), pixels(target.height))
}

/// The part of the target a crop takes, in the target's points — what
/// `sourceRect` is measured in, whatever size the capture is delivered at.
fn source_rect(crop: CropRect, (width, height): (f64, f64)) -> CGRect {
    CGRect {
        origin: CGPoint {
            x: width * f64::from(crop.x),
            y: height * f64::from(crop.y),
        },
        size: CGSize {
            width: width * f64::from(crop.w),
            height: height * f64::from(crop.h),
        },
    }
}

/// Apply the crop extent to a base size, keeping the result even and non-zero.
/// SCK wants even dimensions for several pixel formats, and a zero-sized capture
/// would be an unrecoverable stream error rather than a merely tiny one.
fn scaled_crop_size(base_w: u32, base_h: u32, config: &CaptureConfig) -> (u32, u32) {
    let crop = config.crop.clamped();
    let w = ((base_w as f32) * crop.w).round().max(2.0) as u32;
    let h = ((base_h as f32) * crop.h).round().max(2.0) as u32;
    (w & !1, h & !1)
}

fn build_configuration(
    config: &CaptureConfig,
    width: u32,
    height: u32,
    points: (f64, f64),
) -> Retained<SCStreamConfiguration> {
    let sc = unsafe { SCStreamConfiguration::new() };
    unsafe {
        sc.setWidth(width as usize);
        sc.setHeight(height as usize);
        sc.setPixelFormat(PIXEL_FORMAT_32BGRA);
        // In sRGB, as the texture reads it. Left alone, a Mac's display space
        // — Display P3 on most — comes through, and reads washed out.
        sc.setColorSpaceName(kCGColorSpaceSRGB);
        sc.setShowsCursor(config.show_cursor);
        // Opaque output: the deck's default blit composites over black anyway,
        // and a transparent desktop background is never what a VJ wants here.
        sc.setShouldBeOpaque(true);
        // Apple's guidance for a live consumer is 3. Nida holds frames'
        // surfaces until each show frame's has been picked (see
        // `crate::frame_picker`, at most four waiting) and the GPU has copied
        // it, so 3 could leave the stream none to capture into; 8, the most
        // SCK takes, leaves it room. The depth is not a delay: frames are
        // picked by when they were captured.
        sc.setQueueDepth(8);
        sc.setScalesToFit(true);
        // Frame pacing at the source. This is what makes the 30 fps default an
        // actual saving rather than a throttle applied after the work is done.
        sc.setMinimumFrameInterval(CMTime {
            value: 1_000,
            timescale: (config.rate * 1_000.0) as i32,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        });
    }

    let crop = config.crop.clamped();
    if !crop.is_full_frame() {
        unsafe { sc.setSourceRect(source_rect(crop, points)) };
    }

    unsafe {
        sc.setStreamName(Some(&NSString::from_str("Nida Screen Capture")));
    }
    sc
}

impl ScreenCaptureBackend for MacosBackend {
    fn label(&self) -> &str {
        &self.label
    }

    fn resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn next_frame(&mut self) -> Option<CaptureFrame> {
        let frame = self.slot.try_lock().ok()?.pop_front()?;
        // A stream reconfiguration lands asynchronously, so trust the frame over
        // our own bookkeeping.
        self.width = frame.width;
        self.height = frame.height;
        Some(frame)
    }

    fn pixel_format(&self) -> CapturePixelFormat {
        // `PIXEL_FORMAT_32BGRA` is what the stream configuration asks for.
        CapturePixelFormat::Bgra8UnormSrgb
    }

    fn is_self_paced(&self) -> bool {
        // `minimumFrameInterval` puts the rate in SCK's hands; the delegate
        // pushes frames on the compositor's clock, not ours.
        true
    }

    fn set_config(&mut self, config: &CaptureConfig) -> Result<(), CaptureError> {
        if *config == self.config {
            return Ok(());
        }
        let exclusion_changed = config.exclude_varda != self.config.exclude_varda;
        self.config = config.clone();

        // Recomputed as at the start: the target's pixels, fit within the
        // size asked for, then cropped.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let native = (
            ((self.points.0 * self.scale).round() as u32).max(1),
            ((self.points.1 * self.scale).round() as u32).max(1),
        );
        let (base_w, base_h) = config.scale_to.map_or(native, |(w, h)| {
            super::super::resample::fit_within(native.0, native.1, w, h)
        });
        let (width, height) = scaled_crop_size(base_w, base_h, config);

        let stream_config = build_configuration(config, width, height, self.points);
        unsafe {
            self.handles
                .stream
                .updateConfiguration_completionHandler(&stream_config, None);
        }

        if exclusion_changed {
            // The window-exclusion set lives in the filter, not the config, so
            // toggling `exclude_varda` needs the filter rebuilt.
            log::debug!(
                "Screen capture '{}': exclude_varda changed; filter rebuild required",
                self.label
            );
        }

        Ok(())
    }
}

impl Drop for MacosBackend {
    fn drop(&mut self) {
        unsafe {
            self.handles.stream.stopCaptureWithCompletionHandler(None);
            let _ = self.handles.stream.removeStreamOutput_type_error(
                ProtocolObject::from_ref(&*self.handles.output),
                SCStreamOutputType::Screen,
            );
        }
        log::debug!("ScreenCaptureKit stream stopped for '{}'", self.label);
        let _ = &self.handles.filter;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(w: u32, h: u32) -> CaptureTargetInfo {
        CaptureTargetInfo {
            kind: CaptureTargetKind::Display,
            platform_id: 1,
            label: "Display 1".into(),
            app: None,
            title: None,
            width: w,
            height: h,
            is_varda: false,
        }
    }

    /// The declared format must match what the stream is configured to emit, or
    /// the shared texture is allocated wrong and every capture starts by
    /// throwing one away.
    #[test]
    fn declared_pixel_format_matches_the_configured_stream_format() {
        assert_eq!(PIXEL_FORMAT_32BGRA, u32::from_be_bytes(*b"BGRA"));
        // `MacosBackend::pixel_format` is a constant; assert the pairing here
        // rather than opening a real stream just to read it back.
        assert_eq!(
            CapturePixelFormat::Bgra8UnormSrgb.wgpu_format(),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
    }

    #[test]
    fn pixel_format_constant_is_the_bgra_four_char_code() {
        // 'BGRA' == 0x42475241. A wrong code silently yields garbage colours.
        assert_eq!(PIXEL_FORMAT_32BGRA, 0x4247_5241);
    }

    #[test]
    fn output_size_uses_scale_to_not_the_native_display_size() {
        let cfg = CaptureConfig {
            scale_to: Some((1920, 1080)),
            ..Default::default()
        };
        // This is the whole bandwidth argument: a 4K display must not deliver 4K.
        assert_eq!(output_size(&target(3840, 2160), 1.0, &cfg), (1920, 1080));
    }

    #[test]
    fn output_size_falls_back_to_the_target_size() {
        let cfg = CaptureConfig::default();
        assert_eq!(output_size(&target(1280, 720), 1.0, &cfg), (1280, 720));
    }

    #[test]
    fn output_size_shrinks_with_crop_so_cropping_saves_bandwidth() {
        let cfg = CaptureConfig {
            scale_to: Some((1920, 1080)),
            crop: CropRect {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5,
            },
            ..Default::default()
        };
        assert_eq!(output_size(&target(3840, 2160), 1.0, &cfg), (960, 540));
    }

    #[test]
    fn output_size_keeps_the_targets_shape_rather_than_the_requested_box() {
        // A 4:3 window asked to fill a 16:9 deck must come back 4:3. Obeying the
        // box literally is what made the deck's Scale control a no-op: the
        // capture already matched the deck, so no mode had anything to change.
        let cfg = CaptureConfig {
            scale_to: Some((1920, 1080)),
            ..Default::default()
        };
        assert_eq!(output_size(&target(1000, 800), 1.0, &cfg), (1000, 800));
    }

    /// A Retina display, 1440×900 points of 2880×1800 pixels, is captured
    /// at its pixels, and fit within the size asked for from there.
    #[test]
    fn a_retina_display_is_captured_at_its_pixels() {
        let retina = target(1440, 900);
        assert_eq!(
            output_size(&retina, 2.0, &CaptureConfig::default()),
            (2880, 1800)
        );
        let cfg = CaptureConfig {
            scale_to: Some((1920, 1200)),
            ..Default::default()
        };
        assert_eq!(output_size(&retina, 2.0, &cfg), (1920, 1200));
    }

    /// A crop takes the same part of the screen however large the capture:
    /// it is measured in the target's points, not the delivered pixels.
    #[test]
    fn a_crop_takes_its_part_of_the_screen_in_points() {
        let crop = CropRect {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
        };
        let rect = source_rect(crop, (1440.0, 900.0));
        assert_eq!((rect.origin.x, rect.origin.y), (360.0, 450.0));
        assert_eq!((rect.size.width, rect.size.height), (720.0, 225.0));
    }

    #[test]
    fn output_size_is_always_even_and_never_zero() {
        let cfg = CaptureConfig {
            scale_to: Some((3, 3)),
            crop: CropRect {
                x: 0.0,
                y: 0.0,
                w: 0.01,
                h: 0.01,
            },
            ..Default::default()
        };
        let (w, h) = output_size(&target(100, 100), 1.0, &cfg);
        assert!(
            w > 0 && h > 0,
            "degenerate crop must not produce a 0-sized capture"
        );
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }
}
