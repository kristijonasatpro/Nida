//! Hardware decoding: `VideoToolbox` on macOS and D3D11VA on Windows.
//!
//! H.264, HEVC and `ProRes` decode in hardware on every Apple-silicon Mac, and
//! AV1 on M3 and newer. At 4K it costs a fifteenth of the CPU a software
//! decode does, which is what leaves room for several clips at once beside
//! the renderer. The decoded picture is copied back to memory and goes
//! through the same colour conversion as a software-decoded one.
//!
//! Windows uses the driver's D3D11VA decoder where the codec and device offer
//! it. Frames return through the same NV12/P010 download and colour conversion
//! path; this is hardware decoding, not zero-copy rendering.
//!
//! Anything the media engine will not take — a codec or profile it lacks, an
//! AV1 file on an M1 or M2 — decodes in software as before, and a clip that
//! fails in hardware part-way through is reopened in software by the player.

extern crate ffmpeg_next as ffmpeg;

use anyhow::{Context as _, Result};
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::video::Video;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};

/// Set to decode everything in software, for troubleshooting.
pub const SOFTWARE_ENV: &str = "VARDA_SOFTWARE_DECODE";
/// Windows decoding is experimental until playback and seeking are validated
/// on more drivers. macOS retains its existing automatic selection.
pub const WINDOWS_HARDWARE_ENV: &str = "VARDA_WINDOWS_HARDWARE_DECODE";

/// Whether clips should try the media engine: automatically on macOS, or
/// opted into on Windows. [`SOFTWARE_ENV`] overrides either choice.
pub fn wanted() -> bool {
    (cfg!(target_os = "macos") || (cfg!(target_os = "windows") && windows_enabled()))
        && std::env::var_os(SOFTWARE_ENV).is_none()
}

/// Windows' choice is read once, then shared with decoder workers without
/// changing process environment variables or disturbing open decoders.
static WINDOWS_ENABLED: OnceLock<AtomicBool> = OnceLock::new();

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct WindowsPreference {
    hardware_decode: bool,
}

fn windows_preferences_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("Nida").join("performance.json"))
}

fn load_windows_enabled(path: &Path, env_enabled: bool) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<WindowsPreference>(&text) {
            Ok(preference) => preference.hardware_decode,
            Err(e) => {
                log::warn!("Could not read decoding preference {}: {e}", path.display());
                false
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => env_enabled,
        Err(e) => {
            log::warn!("Could not read decoding preference {}: {e}", path.display());
            false
        }
    }
}

fn windows_choice() -> &'static AtomicBool {
    WINDOWS_ENABLED.get_or_init(|| {
        let env_enabled = std::env::var_os(WINDOWS_HARDWARE_ENV).is_some();
        AtomicBool::new(
            windows_preferences_path()
                .map_or(env_enabled, |path| load_windows_enabled(&path, env_enabled)),
        )
    })
}

/// The operator's Windows decoding choice, for newly opened decoders.
pub fn windows_enabled() -> bool {
    windows_choice().load(Ordering::Relaxed)
}

/// Change the Windows choice immediately and return its file for the
/// background writer. Existing decoders retain their selection.
///
/// # Errors
///
/// Returns an error if no settings folder is available or serialization
/// fails. The choice still applies for the current run.
pub fn set_windows_enabled(enabled: bool) -> Result<(PathBuf, String)> {
    windows_choice().store(enabled, Ordering::Relaxed);
    let path = windows_preferences_path().context("No folder for decoding preferences")?;
    let text = serde_json::to_string_pretty(&WindowsPreference {
        hardware_decode: enabled,
    })?;
    Ok((path, text))
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
) -> Result<(ffmpeg::decoder::Video, bool)> {
    open_with(parameters, hardware, false)
}

/// [`open`], for a live stream when `live`: decoding in software, it uses
/// every core either way — frame by frame for a clip, slice by slice for a
/// stream, which decoding frames in parallel would hold back several frames.
pub(crate) fn open_with(
    parameters: ffmpeg::codec::Parameters,
    hardware: bool,
    live: bool,
) -> Result<(ffmpeg::decoder::Video, bool)> {
    let mut context = ffmpeg::codec::context::Context::from_parameters(parameters)?;
    let id = context.id();
    let requested_codec = if hardware && id == ffmpeg::codec::Id::AV1 {
        ffmpeg::decoder::find_by_name("av1")
    } else {
        ffmpeg::decoder::find(id)
    }
    .with_context(|| format!("No decoder for {id:?}"))?;
    let hardware = hardware && attach_media_engine(&mut context, &requested_codec);
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
    let codec = if hardware {
        Some(requested_codec)
    } else {
        ffmpeg::decoder::find(id)
    }
    .with_context(|| format!("No decoder for {id:?}"))?;
    Ok((context.decoder().open_as(codec)?.video()?, hardware))
}

fn engine_device() -> ffmpeg::ffi::AVHWDeviceType {
    if cfg!(target_os = "windows") {
        ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA
    } else {
        ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX
    }
}

fn engine_format() -> Pixel {
    if cfg!(target_os = "windows") {
        Pixel::D3D11
    } else {
        Pixel::VIDEOTOOLBOX
    }
}

/// Point a decoder at the media engine before it is opened. Unsupported
/// codecs or unavailable devices stay in software with normal threading.
fn attach_media_engine(
    context: &mut ffmpeg::codec::context::Context,
    codec: &ffmpeg::Codec,
) -> bool {
    #[cfg(test)]
    if PRETEND_NO_ENGINE.with(std::cell::Cell::get) {
        return false;
    }
    // Safety: `context` is a valid, not-yet-opened codec context we own. The
    // device reference is handed to it (`av_buffer_ref`) and our own copy
    // released, so the context holds the only reference and frees it.
    unsafe {
        // Skip device creation for a decoder without this backend. In
        // particular, an unsupported codec must keep software threading.
        let mut index = 0;
        loop {
            let config = ffmpeg::ffi::avcodec_get_hw_config(codec.as_ptr(), index);
            if config.is_null() {
                return false;
            }
            if (*config).device_type == engine_device()
                && (*config).methods & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32
                    != 0
            {
                break;
            }
            index += 1;
        }
        let mut device: *mut ffmpeg::ffi::AVBufferRef = std::ptr::null_mut();
        let created = ffmpeg::ffi::av_hwdevice_ctx_create(
            &raw mut device,
            engine_device(),
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        );
        if created < 0 || device.is_null() {
            log::info!(
                "No {:?} device ({created}); decoding in software",
                engine_device()
            );
            return false;
        }
        let raw = context.as_mut_ptr();
        (*raw).hw_device_ctx = ffmpeg::ffi::av_buffer_ref(device);
        (*raw).get_format = Some(prefer_media_engine);
        ffmpeg::ffi::av_buffer_unref(&raw mut device);
        !(*raw).hw_device_ctx.is_null()
    }
}

/// Pick the platform's media engine when offered, else what ffmpeg would.
unsafe extern "C" fn prefer_media_engine(
    context: *mut ffmpeg::ffi::AVCodecContext,
    formats: *const ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::ffi::AVPixelFormat {
    // Safety: ffmpeg passes a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        let desired: ffmpeg::ffi::AVPixelFormat = engine_format().into();
        let mut format = formats;
        while *format != ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *format == desired {
                return *format;
            }
            format = format.add(1);
        }
        ffmpeg::ffi::avcodec_default_get_format(context, formats)
    }
}

/// Whether a decoded frame lives on the media engine.
pub(crate) fn on_engine(frame: &Video) -> bool {
    frame.format() == engine_format()
}

pub(crate) struct Download {
    pub frame: Video,
    layout: Option<(ffmpeg::ffi::AVPixelFormat, i32, i32)>,
}

impl Default for Download {
    fn default() -> Self {
        Self {
            frame: Video::empty(),
            layout: None,
        }
    }
}

pub(crate) fn to_memory(frame: &Video, download: &mut Download) -> Result<()> {
    if !cfg!(target_os = "windows") {
        unsafe {
            ffmpeg::ffi::av_frame_unref(download.frame.as_mut_ptr());
            let copied = ffmpeg::ffi::av_hwframe_transfer_data(
                download.frame.as_mut_ptr(),
                frame.as_ptr(),
                0,
            );
            anyhow::ensure!(
                copied >= 0,
                "copying the decoded frame from the media engine failed ({copied})"
            );
        }
        return replace_properties(frame, &mut download.frame);
    }
    unsafe {
        let source = frame.as_ptr();
        anyhow::ensure!(
            !(*source).hw_frames_ctx.is_null(),
            "No hardware frames context"
        );
        #[allow(
            clippy::cast_ptr_alignment,
            reason = "av_hwframe_ctx_alloc provides aligned AVHWFramesContext storage"
        )]
        let context = &*((*(*source).hw_frames_ctx)
            .data
            .cast::<ffmpeg::ffi::AVHWFramesContext>());
        let layout = (context.sw_format, context.width, context.height);
        let into = &mut download.frame;
        let raw = into.as_mut_ptr();
        if download.layout != Some(layout) || ffmpeg::ffi::av_frame_is_writable(raw) == 0 {
            ffmpeg::ffi::av_frame_unref(raw);
            (*raw).format = context.sw_format as i32;
            (*raw).width = context.width;
            (*raw).height = context.height;
            let allocated = ffmpeg::ffi::av_frame_get_buffer(raw, 0);
            anyhow::ensure!(
                allocated >= 0,
                "allocating download frame failed ({allocated})"
            );
            download.layout = Some(layout);
        }
        (*raw).width = context.width;
        (*raw).height = context.height;
        let copied = ffmpeg::ffi::av_hwframe_transfer_data(into.as_mut_ptr(), frame.as_ptr(), 0);
        if copied < 0 {
            anyhow::bail!("copying the decoded frame from the media engine failed ({copied})");
        }
        (*raw).width = (*source).width;
        (*raw).height = (*source).height;
    }
    replace_properties(frame, &mut download.frame)
}

fn replace_properties(frame: &Video, into: &mut Video) -> Result<()> {
    unsafe {
        let raw = into.as_mut_ptr();
        ffmpeg::ffi::av_frame_side_data_free(
            &raw mut (*raw).side_data,
            &raw mut (*raw).nb_side_data,
        );
        ffmpeg::ffi::av_dict_free(&raw mut (*raw).metadata);
        let properties = ffmpeg::ffi::av_frame_copy_props(raw, frame.as_ptr());
        anyhow::ensure!(
            properties >= 0,
            "copying download properties failed ({properties})"
        );
    }
    Ok(())
}

#[cfg(test)]
mod preference_tests {
    use super::*;

    #[test]
    fn reused_download_properties_replace_hdr_side_data_and_clear_absent_tags() {
        let mut source = Video::new(Pixel::P010LE, 64, 64);
        source.set_color_space(ffmpeg::util::color::Space::BT2020NCL);
        source.set_color_range(ffmpeg::util::color::Range::JPEG);
        source.set_color_transfer_characteristic(
            ffmpeg::util::color::TransferCharacteristic::SMPTE2084,
        );
        source.set_pts(Some(42));
        unsafe {
            let side = ffmpeg::ffi::av_frame_new_side_data(
                source.as_mut_ptr(),
                ffmpeg::ffi::AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
                4,
            );
            assert!(!side.is_null());
            std::ptr::write_bytes((*side).data, 7, 4);
        }
        let mut into = Video::new(Pixel::P010LE, 64, 64);
        let storage = into.data(0).as_ptr();
        for _ in 0..100 {
            replace_properties(&source, &mut into).unwrap();
            assert_eq!(into.data(0).as_ptr(), storage);
            assert_eq!(into.pts(), Some(42));
            assert_eq!(into.color_space(), source.color_space());
            assert_eq!(into.color_range(), source.color_range());
            assert_eq!(
                into.color_transfer_characteristic(),
                source.color_transfer_characteristic()
            );
            unsafe {
                assert_eq!((*into.as_ptr()).nb_side_data, 1);
                let side = *(*into.as_ptr()).side_data;
                assert_eq!(std::slice::from_raw_parts((*side).data, 4), &[7; 4]);
            }
        }
        let sdr = Video::new(Pixel::NV12, 64, 64);
        replace_properties(&sdr, &mut into).unwrap();
        assert_eq!(into.pts(), None);
        assert_eq!(
            into.color_transfer_characteristic(),
            sdr.color_transfer_characteristic()
        );
        unsafe {
            assert_eq!((*into.as_ptr()).nb_side_data, 0);
        }
    }

    #[test]
    fn saved_decoding_choice_survives_restart_and_overrides_the_environment_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("performance.json");
        assert!(!load_windows_enabled(&path, false));
        assert!(load_windows_enabled(&path, true));
        for hardware_decode in [true, false] {
            let text =
                serde_json::to_string_pretty(&WindowsPreference { hardware_decode }).unwrap();
            std::fs::write(&path, text).unwrap();
            assert_eq!(
                load_windows_enabled(&path, !hardware_decode),
                hardware_decode
            );
        }
        std::fs::write(&path, "broken").unwrap();
        assert!(!load_windows_enabled(&path, true));
    }
}
