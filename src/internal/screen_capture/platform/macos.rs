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
    CaptureTargetKind, PermissionState, ScreenCaptureBackend,
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

/// A display's UUID, which macOS keeps across restarts and whatever else is
/// plugged in (as `crate::app::screens` keeps a screen's given name by).
fn display_uuid(display: u32) -> Option<String> {
    use objc2_core_foundation::{CFRetained, CFUUID};

    #[link(name = "ColorSync", kind = "framework")]
    extern "C" {
        fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> Option<std::ptr::NonNull<CFUUID>>;
    }
    // SAFETY: any display number; a new reference, or null.
    let uuid = unsafe { CGDisplayCreateUUIDFromDisplayID(display) }?;
    // SAFETY: created above (the Create rule), released once.
    let uuid = unsafe { CFRetained::from_raw(uuid) };
    CFUUID::new_string(None, Some(&uuid)).map(|s| s.to_string())
}

/// Enumerate displays and on-screen windows.
///
/// # Errors
///
/// Returns [`CaptureError::PermissionDenied`] if TCC has not granted Screen
/// Recording, or [`CaptureError::Backend`] if `SCShareableContent` fails or does
/// not answer within [`ENUMERATE_TIMEOUT`].
pub fn enumerate() -> Result<Vec<CaptureTargetInfo>, CaptureError> {
    let content = shareable_content(true)?;
    let our_pid = std::process::id().cast_signed();
    let mut targets = Vec::new();

    // Displays first — they are the stable entries and belong at the top of the
    // library panel.
    for (i, display) in unsafe { content.displays() }.iter().enumerate() {
        let (w, h) = unsafe { (display.width(), display.height()) };
        let id = unsafe { display.displayID() };
        targets.push(CaptureTargetInfo {
            kind: CaptureTargetKind::Display,
            platform_id: u64::from(id),
            // Its place in macOS's list as it is now: another screen plugged
            // in, and the same one is listed under another number.
            label: format!("Display {}", i + 1),
            app: None,
            title: None,
            width: u32::try_from(w).unwrap_or(0),
            height: u32::try_from(h).unwrap_or(0),
            is_varda: false,
            uuid: display_uuid(id),
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
            uuid: None,
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

/// What macOS lets Nida capture: the displays, and the windows on a screen
/// — or every window, `on_screen_only` false, to find one by its number
/// that is on no screen (a web talk's, kept off every screen).
fn shareable_content(on_screen_only: bool) -> Result<Retained<SCShareableContent>, CaptureError> {
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
            on_screen_only,
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
    /// When the target's size was last looked at (see `follow_target`).
    followed: std::time::Instant,
    /// What it captures, to look its size up as it is now: a display or a
    /// window, and its id.
    kind: CaptureTargetKind,
    platform_id: u64,
    /// Its pixels to a point: 2 on a Retina display.
    scale: f64,
}

impl MacosBackend {
    fn new(target: &CaptureTargetInfo, config: &CaptureConfig) -> Result<Self, CaptureError> {
        // A window is found by its number, on a screen or not.
        let content = shareable_content(target.kind == CaptureTargetKind::Display)?;
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
        let stream_config =
            build_configuration(config, width, height, pixels_of(target, scale), scale);

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
            followed: std::time::Instant::now(),
            kind: target.kind,
            platform_id: target.platform_id,
        })
    }

    /// The target's pixels as they are now.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn native(&self) -> (u32, u32) {
        (
            ((self.points.0 * self.scale).round() as u32).max(1),
            ((self.points.1 * self.scale).round() as u32).max(1),
        )
    }

    /// The stream set to the target's pixels as they are now: the part its
    /// crop keeps, fitted within the size asked for (see [`stream_size`]).
    fn reconfigure(&self) {
        let native = self.native();
        let (width, height) = stream_size(native, &self.config);
        let stream_config =
            build_configuration(&self.config, width, height, native, self.scale);
        unsafe {
            self.handles
                .stream
                .updateConfiguration_completionHandler(&stream_config, None);
        }
    }
}

/// How often a capture looks at its target's size.
const FOLLOW_EVERY: Duration = Duration::from_millis(500);

/// A display's size in points and its pixels to a point, as they are now:
/// a "looks like" setting changed shows here at once.
fn display_geometry(display: u32) -> Option<((f64, f64), f64)> {
    use objc2_core_graphics::{CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode};
    let bounds = CGDisplayBounds(display);
    let mode = CGDisplayCopyDisplayMode(display)?;
    let points = CGDisplayMode::width(Some(&mode));
    let pixels = CGDisplayMode::pixel_width(Some(&mode));
    if points == 0 || pixels == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let scale = pixels as f64 / points as f64;
    Some(((bounds.size.width, bounds.size.height), scale))
}

/// A window's size in points, and the pixels to a point of the screen its
/// middle is on — none off every screen, where it is still captured — as the
/// window server has them now. `ScreenCaptureKit`'s own account of a
/// capture's filter keeps the size it was made with.
fn window_geometry(window: u32) -> Option<((f64, f64), Option<f64>)> {
    use objc2_core_foundation::{CFArray, CFDictionary, CFString, CFType};
    use objc2_core_graphics::{
        kCGWindowBounds, CGGetDisplaysWithPoint, CGRectMakeWithDictionaryRepresentation,
        CGWindowListCopyWindowInfo, CGWindowListOption,
    };
    let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionIncludingWindow, window)?;
    // SAFETY: the window server lists each window as a dictionary.
    let list: &CFArray<CFDictionary<CFString, CFType>> = unsafe { list.cast_unchecked() };
    let info = list.get(0)?;
    // SAFETY: CoreGraphics' own key.
    let key: &CFString = unsafe { kCGWindowBounds };
    let bounds = info.get(key)?;
    let bounds = bounds.downcast_ref::<CFDictionary>()?;
    let mut rect = CGRect::default();
    // SAFETY: a rectangle's dictionary, and a place for the rectangle.
    if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &raw mut rect) } {
        return None;
    }
    let middle = CGPoint {
        x: rect.origin.x + rect.size.width / 2.0,
        y: rect.origin.y + rect.size.height / 2.0,
    };
    let mut display = 0u32;
    let mut found = 0u32;
    // SAFETY: room for one display id and its count.
    let err = unsafe { CGGetDisplaysWithPoint(middle, 1, &raw mut display, &raw mut found) };
    let scale = if err.0 == 0 && found > 0 {
        display_geometry(display).map(|(_, scale)| scale)
    } else {
        None
    };
    Some(((rect.size.width, rect.size.height), scale))
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
    stream_size(pixels_of(target, scale), config)
}

/// The size a stream of a target `native` pixels big is set to: the part
/// its crop keeps, as big as it is drawn in `scale_to` and never enlarged — as
/// [`Geometry::resolve`](super::super::resample::Geometry::resolve) has it
/// on every platform — and not zero: a zero-sized capture would be an
/// unrecoverable stream error rather than a merely tiny one.
///
/// To the pixel, odd or even: the stream is BGRA, which has no pairs of
/// pixels to keep whole. Rounded down to even numbers — how this was — a
/// window 1499 × 827 came in a stream 1498 × 826, and macOS, fitting it in,
/// rescaled all of it by a thousandth: every pixel resampled between two,
/// text a third as crisp for as long as the window kept that size (measured
/// on an M5 Max: every window size with an odd side).
///
/// The crop comes first: bounding the whole target and taking the crop's
/// share of that — how this was — gave half a 4K screen in a 1080p show
/// 960 × 540 pixels, which the show then enlarged twice over, though the
/// screen had all 1920 × 1080 of them.
fn stream_size(native: (u32, u32), config: &CaptureConfig) -> (u32, u32) {
    let fitted = super::super::resample::Geometry::resolve(native.0, native.1, config);
    (fitted.out_w.max(1), fitted.out_h.max(1))
}

/// Whether a stream of a target `native` pixels big, set as `runs`, is
/// already as `asked` would set it: only the size asked for moved — the
/// bound, or how big the picture is drawn in it — and to one that gives the
/// same size: a picture drawn bigger still than the target has pixels.
/// Nothing to tell the stream then.
fn same_stream(native: (u32, u32), runs: &CaptureConfig, asked: &CaptureConfig) -> bool {
    let but_for_its_size = CaptureConfig {
        scale_to: runs.scale_to,
        drawn: runs.drawn,
        ..asked.clone()
    };
    but_for_its_size == *runs && stream_size(native, asked) == stream_size(native, runs)
}

/// A target's size in pixels, its size in points `scale` times over.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pixels_of(target: &CaptureTargetInfo, scale: f64) -> (u32, u32) {
    let pixels = |points: u32| ((f64::from(points.max(1)) * scale).round() as u32).max(1);
    (pixels(target.width), pixels(target.height))
}

/// The part of the target a crop takes, in the target's points — what
/// `sourceRect` is measured in, whatever size the capture is delivered at —
/// on whole pixels of the target, `scale` of them to a point: the ones
/// [`stream_size`] counts. Taken as the crop's share of the points — how
/// this was — a third of a window began between two of its pixels and was
/// a fraction of a pixel wider than the stream it came in, and macOS
/// resampled all of it.
fn source_rect(native: (u32, u32), scale: f64, config: &CaptureConfig) -> CGRect {
    let part = super::super::resample::Geometry::resolve(native.0, native.1, config);
    let points = |pixels: u32| f64::from(pixels) / scale;
    CGRect {
        origin: CGPoint {
            x: points(part.src_x),
            y: points(part.src_y),
        },
        size: CGSize {
            width: points(part.src_w),
            height: points(part.src_h),
        },
    }
}

fn build_configuration(
    config: &CaptureConfig,
    width: u32,
    height: u32,
    native: (u32, u32),
    scale: f64,
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
        unsafe { sc.setSourceRect(source_rect(native, scale, config)) };
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

    /// A window resized, or moved to a screen of other pixels, or a screen
    /// given another resolution ("looks like" changed): the stream kept the
    /// size it started at, squeezing a window made bigger into it — then
    /// enlarged again on the wall (a window taken from 1622×930 to 3400×1950
    /// points stayed at 1622×930, measured on an M5 Max) until the capture
    /// was added again. Looked at twice a second, on the capture's thread,
    /// as the window server and the display have it now.
    fn follow_target(&mut self) {
        if self.followed.elapsed() < FOLLOW_EVERY {
            return;
        }
        self.followed = std::time::Instant::now();
        let Ok(id) = u32::try_from(self.platform_id) else {
            return;
        };
        // A window off every screen keeps the pixels to a point it had.
        let now = match self.kind {
            CaptureTargetKind::Display => display_geometry(id),
            CaptureTargetKind::Window => window_geometry(id)
                .map(|(points, scale)| (points, scale.unwrap_or(self.scale))),
        };
        let Some((points, scale)) = now else {
            return;
        };
        let sound = points.0.is_finite()
            && points.1.is_finite()
            && points.0 >= 1.0
            && points.1 >= 1.0
            && scale.is_finite()
            && scale >= 1.0;
        let same = (points.0 - self.points.0).abs() < 0.5
            && (points.1 - self.points.1).abs() < 0.5
            && (scale - self.scale).abs() < 0.01;
        if !sound || same {
            return;
        }
        log::info!(
            "Screen capture '{}': now {:.0}x{:.0} points at {scale}x (was {:.0}x{:.0} at {}x)",
            self.label,
            points.0,
            points.1,
            self.points.0,
            self.points.1,
            self.scale
        );
        self.points = points;
        self.scale = scale;
        self.reconfigure();
    }

    fn set_config(&mut self, config: &CaptureConfig) -> Result<(), CaptureError> {
        if *config == self.config {
            return Ok(());
        }
        if same_stream(self.native(), &self.config, config) {
            self.config = config.clone();
            return Ok(());
        }
        let exclusion_changed = config.exclude_varda != self.config.exclude_varda;
        self.config = config.clone();
        // Recomputed as at the start.
        self.reconfigure();

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
    use crate::screen_capture::backend::CropRect;

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
            uuid: None,
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

    /// A Retina or scaled 4K screen, "looks like 1920×1080": its 1920×1080
    /// points are 3840×2160 pixels. Capped at a 4K show it comes at its
    /// pixels; capped at its points fitted into the show — how a capture was
    /// opened before — it came at half of them and was blurred on the wall.
    #[test]
    fn a_retina_screen_in_a_4k_show_comes_at_its_pixels() {
        let show = |w, h| CaptureConfig {
            scale_to: Some((w, h)),
            ..Default::default()
        };
        assert_eq!(
            output_size(&target(1920, 1080), 2.0, &show(3840, 2160)),
            (3840, 2160)
        );
        // In a 1080p show, still no more than the show takes.
        assert_eq!(
            output_size(&target(1920, 1080), 2.0, &show(1920, 1080)),
            (1920, 1080)
        );
        // A window keeps its own shape within the show's size.
        assert_eq!(
            output_size(&target(1200, 800), 2.0, &show(3840, 2160)),
            (2400, 1600)
        );
    }

    #[test]
    fn output_size_falls_back_to_the_target_size() {
        let cfg = CaptureConfig::default();
        assert_eq!(output_size(&target(1280, 720), 1.0, &cfg), (1280, 720));
    }

    /// A crop zooms into the screen: the part it keeps comes at the
    /// screen's own pixels, as far as the show takes them. Bounded whole and
    /// then cropped — how it was — half a 4K screen came 960 × 540 in a
    /// 1080p show and was enlarged twice over.
    #[test]
    fn a_crop_comes_at_the_screens_pixels_within_the_bound() {
        let half = |scale_to| CaptureConfig {
            scale_to,
            crop: CropRect {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5,
            },
            ..Default::default()
        };
        let show = Some((1920, 1080));
        assert_eq!(output_size(&target(3840, 2160), 1.0, &half(show)), (1920, 1080));
        // No more than the show takes, a bigger screen's half.
        assert_eq!(output_size(&target(5120, 2880), 1.0, &half(show)), (1920, 1080));
        // No more than the screen has: a 1080p screen's half.
        assert_eq!(output_size(&target(1920, 1080), 1.0, &half(show)), (960, 540));
        assert_eq!(output_size(&target(3840, 2160), 1.0, &half(None)), (1920, 1080));
    }

    /// A bound moved past what the target has changes nothing of the
    /// stream; one that changes its size, or any other setting, does.
    #[test]
    fn a_bound_past_the_targets_pixels_leaves_the_stream_as_it_is() {
        let at = |w, h| CaptureConfig {
            scale_to: Some((w, h)),
            ..Default::default()
        };
        let native = (3840, 2160);
        assert!(same_stream(native, &at(3840, 2160), &at(4800, 2700)));
        assert!(same_stream(native, &at(4800, 2700), &at(7680, 4320)));
        assert!(!same_stream(native, &at(1920, 1080), &at(2400, 1350)), "bigger");
        assert!(!same_stream(native, &at(4800, 2700), &at(1920, 1080)), "smaller");
        let cursor = CaptureConfig {
            show_cursor: true,
            ..at(4800, 2700)
        };
        assert!(!same_stream(native, &at(3840, 2160), &cursor), "its cursor shown");
        let cropped = CaptureConfig {
            crop: CropRect {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
            ..at(4800, 2700)
        };
        assert!(!same_stream(native, &at(3840, 2160), &cropped), "cropped");
    }

    /// Drawn bigger than fitted, the stream is asked for those pixels, as
    /// far as the screen has them; and a size drawn that changes nothing of
    /// what comes leaves the stream as it is.
    #[test]
    fn a_stream_is_sized_as_its_picture_is_drawn() {
        use crate::screen_capture::backend::Drawn;
        // Fitted so many times, or filling the frame so many times over.
        let drawn = |w, h, fitted, fills| CaptureConfig {
            scale_to: Some((w, h)),
            drawn: Drawn {
                fitted,
                across: fills,
                down: fills,
            },
            ..Default::default()
        };
        let four_k = target(3840, 2160);
        assert_eq!(output_size(&four_k, 1.0, &drawn(1920, 1080, 1.0, 0.0)), (1920, 1080));
        assert_eq!(output_size(&four_k, 1.0, &drawn(1920, 1080, 1.5, 0.0)), (2880, 1620));
        assert_eq!(output_size(&four_k, 1.0, &drawn(1920, 1080, 2.0, 0.0)), (3840, 2160));
        assert_eq!(output_size(&four_k, 1.0, &drawn(1920, 1080, 8.0, 0.0)), (3840, 2160));
        // Filling an upright frame: 1920 high, not the 608 that fit.
        assert_eq!(output_size(&four_k, 1.0, &drawn(1080, 1920, 1.0, 0.0)), (1080, 608));
        assert_eq!(output_size(&four_k, 1.0, &drawn(1080, 1920, 1.0, 1.0)), (3413, 1920));
        // Stretched to it, twice as wide and half as high: the pixels its
        // width shows, not all the screen has.
        let stretched = CaptureConfig {
            drawn: Drawn {
                fitted: 1.0,
                across: 2.0,
                down: 0.5,
            },
            ..drawn(1080, 1920, 1.0, 0.0)
        };
        assert_eq!(output_size(&four_k, 1.0, &stretched), (2160, 1215));
        // As far as a Transform scales, in a frame however small.
        assert_eq!(output_size(&four_k, 1.0, &drawn(128, 72, 25.0, 0.0)), (3200, 1800));

        let native = (3840, 2160);
        assert!(
            !same_stream(native, &drawn(1920, 1080, 1.0, 0.0), &drawn(1920, 1080, 2.0, 0.0)),
            "zoomed into: bigger"
        );
        assert!(
            !same_stream(native, &drawn(1920, 1080, 2.0, 0.0), &drawn(1920, 1080, 1.0, 0.0)),
            "and smaller again"
        );
        assert!(
            same_stream(native, &drawn(1920, 1080, 2.0, 0.0), &drawn(1920, 1080, 5.0, 0.0)),
            "past what the screen has"
        );
        assert!(
            same_stream(native, &drawn(1920, 1080, 1.0, 0.0), &drawn(1920, 1080, 1.0, 1.0)),
            "a frame of its shape, filled as it is fitted"
        );
        assert!(
            same_stream((1920, 1080), &drawn(1920, 1080, 1.0, 0.0), &drawn(1920, 1080, 3.0, 3.0)),
            "a screen with no more pixels"
        );
        let cursor = CaptureConfig {
            show_cursor: true,
            ..drawn(1920, 1080, 5.0, 0.0)
        };
        assert!(
            !same_stream(native, &drawn(1920, 1080, 2.0, 0.0), &cursor),
            "its cursor shown"
        );
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
        let cropped = CaptureConfig {
            scale_to: Some((1920, 1080)),
            crop: CropRect {
                x: 0.25,
                y: 0.5,
                w: 0.5,
                h: 0.25,
            },
            ..Default::default()
        };
        // 1440×900 points of 2880×1800 pixels.
        let rect = source_rect((2880, 1800), 2.0, &cropped);
        assert_eq!((rect.origin.x, rect.origin.y), (360.0, 450.0));
        assert_eq!((rect.size.width, rect.size.height), (720.0, 225.0));
    }

    /// A window comes at its own pixels, odd or even. Rounded down to even
    /// numbers, a window with an odd side came a pixel short along it and
    /// macOS rescaled all of it to fit: soft for as long as it kept that
    /// size (the sizes are those measured on an M5 Max).
    #[test]
    fn a_stream_has_its_windows_pixels_odd_or_even() {
        let room = CaptureConfig {
            scale_to: Some((3840, 2160)),
            ..Default::default()
        };
        for (w, h) in [(1500, 828), (1499, 828), (1500, 827), (1499, 827), (1301, 729)] {
            assert_eq!(output_size(&target(w, h), 1.0, &room), (w, h));
            assert_eq!(
                output_size(&target(w, h), 1.0, &CaptureConfig::default()),
                (w, h)
            );
        }
        // Shrunk by macOS, to the nearest pixel either way.
        let fitted = CaptureConfig {
            scale_to: Some((1250, 704)),
            ..Default::default()
        };
        assert_eq!(output_size(&target(1499, 827), 1.0, &fitted), (1250, 690));
        // A third of a Retina window, 501 pixels of its 1503.
        let third = CaptureConfig {
            crop: CropRect {
                x: 0.0,
                y: 0.0,
                w: 1.0 / 3.0,
                h: 1.0,
            },
            ..room.clone()
        };
        assert_eq!(output_size(&target(1503, 827), 1.0, &third), (501, 827));
    }

    /// A crop is taken on whole pixels of the target, the ones its stream
    /// is as big as: it begins on a pixel and is as wide and high as the
    /// stream, so macOS copies them and resamples nothing.
    #[test]
    fn a_crop_is_taken_on_whole_pixels_the_stream_has_one_for_each() {
        let crop = |x, y, w, h| CaptureConfig {
            scale_to: Some((3840, 2160)),
            crop: CropRect { x, y, w, h },
            ..Default::default()
        };
        // Of a window 1499×827 (one pixel to a point): a third of it from a
        // third in is 499.67 points from 499.67 — pixels 500 to 1000.
        let third = crop(1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0);
        let rect = source_rect((1499, 827), 1.0, &third);
        assert_eq!((rect.origin.x, rect.origin.y), (500.0, 0.0));
        assert_eq!((rect.size.width, rect.size.height), (500.0, 827.0));
        assert_eq!(stream_size((1499, 827), &third), (500, 827));
        // On a Retina screen a pixel is half a point.
        let rect = source_rect((2998, 1654), 2.0, &third);
        assert_eq!((rect.origin.x, rect.origin.y), (499.5, 0.0));
        assert_eq!((rect.size.width, rect.size.height), (499.5, 827.0));
        assert_eq!(stream_size((2998, 1654), &third), (999, 1654));
        // Whatever the crop: a pixel's place, and the stream's size.
        for native in [(1499_u32, 827_u32), (1920, 1080), (3024, 1964), (1301, 729)] {
            for scale in [1.0, 2.0] {
                for (x, y, w, h) in [
                    (0.1, 0.2, 0.37, 0.41),
                    (0.333, 0.0, 0.667, 0.5),
                    (0.0, 0.123, 0.999, 0.877),
                    (0.61, 0.59, 0.39, 0.41),
                    // From the far corner: the last pixel, not one past it.
                    (4095.0 / 4096.0, 4095.0 / 4096.0, 1.0 / 4096.0, 1.0 / 4096.0),
                ] {
                    let cropped = crop(x, y, w, h);
                    let rect = source_rect(native, scale, &cropped);
                    let pixels = |points: f64| points * scale;
                    for at in [rect.origin.x, rect.origin.y] {
                        assert_eq!(pixels(at), pixels(at).round(), "{native:?} {scale} {x}");
                    }
                    let (width, height) = stream_size(native, &cropped);
                    assert_eq!(pixels(rect.size.width), f64::from(width));
                    assert_eq!(pixels(rect.size.height), f64::from(height));
                    assert!(pixels(rect.origin.x + rect.size.width) <= f64::from(native.0));
                    assert!(pixels(rect.origin.y + rect.size.height) <= f64::from(native.1));
                }
            }
        }
    }

    #[test]
    fn output_size_is_never_zero() {
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
    }
}
