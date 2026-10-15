//! Decoding clips on the Mac's media engine (`VideoToolbox`) instead of the CPU.
//!
//! H.264, HEVC and `ProRes` decode in hardware on every Apple-silicon Mac, and
//! AV1 on M3 and newer. At 4K it costs a fifteenth of the CPU a software
//! decode does, which is what leaves room for several clips at once beside
//! the renderer. The decoded picture is copied back to memory and goes
//! through the same colour conversion as a software-decoded one.
//!
//! Anything the media engine will not take — a codec or profile it lacks, an
//! AV1 file on an M1 or M2 — decodes in software as before, and a clip that
//! fails in hardware part-way through is reopened in software by the player.

extern crate ffmpeg_next as ffmpeg;

use anyhow::{Context as _, Result};
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::video::Video;

/// Set to decode everything in software, for troubleshooting.
pub const SOFTWARE_ENV: &str = "VARDA_SOFTWARE_DECODE";

/// Whether clips should try the media engine: on macOS, unless
/// [`SOFTWARE_ENV`] is set.
pub fn wanted() -> bool {
    cfg!(target_os = "macos") && std::env::var_os(SOFTWARE_ENV).is_none()
}

#[cfg(test)]
thread_local! {
    /// Tests: attach no media engine even when asked, as on a Mac whose
    /// engine lacks the codec.
    pub(super) static PRETEND_NO_ENGINE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Open a decoder for a stream, on the media engine when `hardware`.
///
/// AV1 needs care: ffmpeg's default AV1 decoder (dav1d) is software-only, so
/// hardware AV1 means asking for its native `av1` decoder by name — which
/// decodes nothing without a media engine behind it. The player falls back
/// to the default decoder when that happens.
pub(crate) fn open(
    parameters: ffmpeg::codec::Parameters,
    hardware: bool,
) -> Result<ffmpeg::decoder::Video> {
    open_with(parameters, hardware, false)
}

/// [`open`], for a live stream when `live`: decoding in software, it uses
/// every core either way — frame by frame for a clip, slice by slice for a
/// stream, which decoding frames in parallel would hold back several frames.
pub(crate) fn open_with(
    parameters: ffmpeg::codec::Parameters,
    hardware: bool,
    live: bool,
) -> Result<ffmpeg::decoder::Video> {
    let mut context = ffmpeg::codec::context::Context::from_parameters(parameters)?;
    // In software, one thread per core (0 lets FFmpeg count them): by
    // default a decoder uses one, and a 4K clip falls behind. The media
    // engine needs none.
    if !hardware {
        unsafe {
            let raw = context.as_mut_ptr();
            (*raw).thread_count = 0;
            (*raw).thread_type = if live {
                ffmpeg::ffi::FF_THREAD_SLICE
            } else {
                ffmpeg::ffi::FF_THREAD_FRAME | ffmpeg::ffi::FF_THREAD_SLICE
            };
        }
    }
    let id = context.id();
    let codec = if hardware && id == ffmpeg::codec::Id::AV1 {
        ffmpeg::decoder::find_by_name("av1")
    } else {
        ffmpeg::decoder::find(id)
    }
    .with_context(|| format!("No decoder for {id:?}"))?;
    if hardware {
        attach_media_engine(&mut context);
    }
    Ok(context.decoder().open_as(codec)?.video()?)
}

/// Point a decoder at the media engine before it is opened. Without one (not
/// a Mac, or `VideoToolbox` unavailable) the decoder stays in software.
fn attach_media_engine(context: &mut ffmpeg::codec::context::Context) {
    #[cfg(test)]
    if PRETEND_NO_ENGINE.with(std::cell::Cell::get) {
        return;
    }
    // Safety: `context` is a valid, not-yet-opened codec context we own. The
    // device reference is handed to it (`av_buffer_ref`) and our own copy
    // released, so the context holds the only reference and frees it.
    unsafe {
        let mut device: *mut ffmpeg::ffi::AVBufferRef = std::ptr::null_mut();
        let created = ffmpeg::ffi::av_hwdevice_ctx_create(
            &raw mut device,
            ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        );
        if created < 0 || device.is_null() {
            log::info!("No VideoToolbox device ({created}); decoding in software");
            return;
        }
        let raw = context.as_mut_ptr();
        (*raw).hw_device_ctx = ffmpeg::ffi::av_buffer_ref(device);
        (*raw).get_format = Some(prefer_media_engine);
        ffmpeg::ffi::av_buffer_unref(&raw mut device);
    }
}

/// Pick `VideoToolbox` when the decoder offers it, else what ffmpeg would.
unsafe extern "C" fn prefer_media_engine(
    context: *mut ffmpeg::ffi::AVCodecContext,
    formats: *const ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::ffi::AVPixelFormat {
    // Safety: ffmpeg passes a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        let mut format = formats;
        while *format != ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *format == ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX {
                return *format;
            }
            format = format.add(1);
        }
        ffmpeg::ffi::avcodec_default_get_format(context, formats)
    }
}

/// Whether a decoded frame lives on the media engine.
pub(crate) fn on_engine(frame: &Video) -> bool {
    frame.format() == Pixel::VIDEOTOOLBOX
}

/// Copy a media-engine frame into `into` in memory — NV12, or P010 for
/// 10-bit — with its colour tags, ready for the usual YUV → RGBA conversion.
pub(crate) fn to_memory(frame: &Video, into: &mut Video) -> Result<()> {
    // Safety: both frames are valid; ffmpeg allocates `into`'s planes to the
    // engine frame's software format and size.
    unsafe {
        ffmpeg::ffi::av_frame_unref(into.as_mut_ptr());
        let copied = ffmpeg::ffi::av_hwframe_transfer_data(into.as_mut_ptr(), frame.as_ptr(), 0);
        if copied < 0 {
            anyhow::bail!("copying the decoded frame from the media engine failed ({copied})");
        }
        // Matrix, range and primaries, which the conversion reads.
        ffmpeg::ffi::av_frame_copy_props(into.as_mut_ptr(), frame.as_ptr());
    }
    Ok(())
}
