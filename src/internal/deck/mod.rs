mod effect;
pub mod lower_third;
pub mod media;
pub mod playlist;
pub mod presenter;
mod render;
pub mod slides;
mod source;
pub mod svg;
pub mod text;
pub mod timer;

pub use render::get_current_date;
pub(crate) use render::Framing;

use crate::isf::{ISFPass, ISFShader};
use crate::params::ShaderParams;
use crate::renderer::{BlitPipeline, ComputePipeline, HapConvertPipeline, UnifiedPipeline};
use crate::video::{HapTextureFormat, PlaybackSnapshot, VideoCommand, VideoDecodeHandle};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Generate a short 8-character hex UUID for entity identity.
pub fn generate_short_uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// How large a deck renders relative to the master resolution.
///
/// A generator computes every pixel every frame, so at 4K it is the costliest
/// thing in a show; soft procedural content drawn at half size and scaled up
/// on the way into the mix looks almost the same for a quarter of the work.
/// The deck's texture is simply smaller: everything downstream samples it
/// across the full frame, so it is scaled up where it is used.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
pub enum DeckRenderScale {
    /// The master resolution.
    #[default]
    Full,
    /// Half the width and height — a quarter of the pixels.
    Half,
    /// A quarter of the width and height — a sixteenth of the pixels.
    Quarter,
}

impl DeckRenderScale {
    pub const ALL: [Self; 3] = [Self::Full, Self::Half, Self::Quarter];

    /// The deck size for a master of `width × height`, at least 1×1.
    pub fn apply(self, width: u32, height: u32) -> (u32, u32) {
        let divisor = match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        };
        (
            width.div_ceil(divisor).max(1),
            height.div_ceil(divisor).max(1),
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Half => "Half",
            Self::Quarter => "Quarter",
        }
    }
}

/// Scaling mode for non-shader sources (images, video)
#[derive(
    Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema, Default,
)]
pub enum ScalingMode {
    /// Scale to fill the entire target, cropping edges if aspect ratio differs
    #[default]
    Fill,
    /// Scale to fit within the target, letterboxing if aspect ratio differs
    Fit,
    /// Stretch to exactly match target dimensions (may distort)
    Stretch,
    /// No scaling, center at native resolution
    Center,
}

impl ScalingMode {
    /// Compute UV scale and offset for blitting source into target
    /// Returns (`uv_scale`, `uv_offset`) to transform target UVs to source UVs
    pub fn compute_uv_transform(
        &self,
        source_w: u32,
        source_h: u32,
        target_w: u32,
        target_h: u32,
    ) -> ([f32; 2], [f32; 2]) {
        let src_aspect = source_w as f32 / source_h as f32;
        let tgt_aspect = target_w as f32 / target_h as f32;

        match self {
            ScalingMode::Stretch => ([1.0, 1.0], [0.0, 0.0]),
            // Filling or fitted, a picture as wide as the target — or as
            // high — is drawn a pixel for a pixel: its other side is
            // centred on whole pixels then, as one centred is.
            ScalingMode::Fill => {
                if src_aspect > tgt_aspect {
                    let scale_x = tgt_aspect / src_aspect;
                    let offset_x = if source_h == target_h {
                        centred_on_whole_pixels(source_w, target_w)
                    } else {
                        (1.0 - scale_x) * 0.5
                    };
                    ([scale_x, 1.0], [offset_x, 0.0])
                } else {
                    let scale_y = src_aspect / tgt_aspect;
                    let offset_y = if source_w == target_w {
                        centred_on_whole_pixels(source_h, target_h)
                    } else {
                        (1.0 - scale_y) * 0.5
                    };
                    ([1.0, scale_y], [0.0, offset_y])
                }
            }
            ScalingMode::Fit => {
                if src_aspect > tgt_aspect {
                    let scale_y = src_aspect / tgt_aspect;
                    let offset_y = if source_w == target_w {
                        centred_on_whole_pixels(source_h, target_h)
                    } else {
                        (1.0 - scale_y) * 0.5
                    };
                    ([1.0, scale_y], [0.0, offset_y])
                } else {
                    let scale_x = tgt_aspect / src_aspect;
                    let offset_x = if source_h == target_h {
                        centred_on_whole_pixels(source_w, target_w)
                    } else {
                        (1.0 - scale_x) * 0.5
                    };
                    ([scale_x, 1.0], [offset_x, 0.0])
                }
            }
            ScalingMode::Center => {
                let scale_x = target_w as f32 / source_w as f32;
                let scale_y = target_h as f32 / source_h as f32;
                (
                    [scale_x, scale_y],
                    [
                        centred_on_whole_pixels(source_w, target_w),
                        centred_on_whole_pixels(source_h, target_h),
                    ],
                )
            }
        }
    }
}

/// Where a picture `source` pixels across, drawn a pixel for a pixel in a
/// target `target` across, begins — in its own UV, as the offset of
/// [`ScalingMode::compute_uv_transform`]: centred, on whole pixels.
///
/// Centred exactly, a picture an odd number of pixels wider or narrower
/// than its target sits half a pixel off: each pixel drawn is then a blend
/// of two of its own, and text goes soft. So it sits the half pixel to the
/// right — or lower.
fn centred_on_whole_pixels(source: u32, target: u32) -> f32 {
    let before = (i64::from(source) - i64::from(target)).div_euclid(2);
    before as f32 / source as f32
}

/// Whether a picture of `source` pixels, scaled into `target` by `mode`,
/// reaches every edge — leaves no transparent bar.
pub(crate) fn fills_the_frame(mode: ScalingMode, source: (u32, u32), target: (u32, u32)) -> bool {
    if source.0 == 0 || source.1 == 0 || target.0 == 0 || target.1 == 0 {
        return false;
    }
    let (scale, offset) = mode.compute_uv_transform(source.0, source.1, target.0, target.1);
    // Every target point lands inside the source: offset ≥ 0 and
    // offset + scale ≤ 1 on both axes (a hair of slack for rounding: never
    // a quarter of one of the target's pixels — a picture a pixel short of
    // a very wide frame leaves a column of it bare).
    let across = [target.0 as f32, target.1 as f32];
    (0..2).all(|i| {
        let slack = (0.25 * scale[i] / across[i]).min(1e-4);
        offset[i] >= -slack && offset[i] + scale[i] <= 1.0 + slack
    })
}

/// How big a screen or window capture is drawn in the show's frame by an
/// input that shows it by `mode`, zoomed into `zoom` times across and down
/// (its Transform's scale, where the pass drawing the capture does that
/// too: see `Deck::capture_drawn`). What its capture is sized by (see
/// `ScreenCaptureManager::size_as_drawn`).
///
/// Fitted or filling the frame it keeps its shape, and comes as the side
/// zoomed into the more wants; stretched to the frame each side is the
/// frame's, times its own zoom. Never less than fitted: shrunk into a
/// corner it comes as it did. Centred it is placed as it comes fitted (see
/// [`placed_size`]), and wants what one fitted does.
pub(crate) fn capture_drawn_as(
    mode: ScalingMode,
    zoom: [f32; 2],
) -> crate::screen_capture::backend::Drawn {
    use crate::screen_capture::backend::Drawn;
    let [across, down] = zoom.map(|z| if z.is_finite() { z } else { 1.0 });
    let most = across.max(down);
    match mode {
        ScalingMode::Fit | ScalingMode::Center => Drawn {
            fitted: most.max(1.0),
            ..Drawn::default()
        },
        ScalingMode::Fill => Drawn {
            fitted: 1.0,
            across: most,
            down: most,
        },
        ScalingMode::Stretch => Drawn {
            fitted: 1.0,
            across,
            down,
        },
    }
}

/// The size a live source's picture is placed by in its deck's frame: its
/// own — `source`, as it comes — but for a screen or window capture shown
/// centred, which is placed as it comes fitted within the show's size.
///
/// Centred means at its own pixels, and a capture's are as many as it is
/// asked for: fitted within the show's size, until an input of its screen
/// — this one, or another — fills its frame or is zoomed into and the
/// capture comes bigger for that (see `ScreenCaptureManager::size_as_drawn`).
/// Placed by the pixels it came with then, the centred input would show the
/// middle of its picture, enlarged, for as long as that lasted. The other
/// modes go by the picture's shape, which stays. A PowerPoint show's
/// capture is never sized so.
pub(crate) fn placed_size(
    mode: ScalingMode,
    source: (u32, u32),
    capture: Option<&ScreenCaptureState>,
) -> (u32, u32) {
    let bound = capture
        .filter(|state| mode == ScalingMode::Center && state.presentation.is_none())
        .and_then(|state| state.config.scale_to);
    match bound {
        Some((width, height)) if source.0 > 0 && source.1 > 0 => {
            crate::screen_capture::resample::fit_within(source.0, source.1, width, height)
        }
        _ => source,
    }
}

/// The view a deck draws its live source from, and whether that view reads
/// it as it was coded: the `coded` one where the deck has both (see
/// `Deck::external_source_coded_view`), else its `view`, read as light.
pub(crate) fn source_read<'a>(
    view: Option<&'a wgpu::TextureView>,
    coded: Option<&'a wgpu::TextureView>,
) -> Option<(&'a wgpu::TextureView, bool)> {
    match (view, coded) {
        (Some(_), Some(coded)) => Some((coded, true)),
        (view, _) => view.map(|view| (view, false)),
    }
}

/// A colour layer's name when nobody gave it one: its colour as hex.
pub(crate) fn colour_name(color: [f32; 4]) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (color[0] * 255.0) as u8,
        (color[1] * 255.0) as u8,
        (color[2] * 255.0) as u8,
    )
}

/// Double-buffered staging buffers for non-blocking GPU texture uploads.
///
/// Uses a ping-pong pattern: CPU writes to buffer\[current\], GPU copies from
/// buffer\[1-current\]. By the time we circle back two frames later, the GPU
/// is done with the buffer and it can be re-mapped without stalling.
///
/// This eliminates the per-frame staging buffer allocation that
/// `queue.write_texture()` performs internally, which can block for 2-9ms
/// under GPU saturation.
pub struct VideoStagingBuffers {
    buffers: [wgpu::Buffer; 2],
    current: usize,
    mapped: [Arc<AtomicBool>; 2],
    /// Bytes per row padded to `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT` (256).
    padded_bpr: u32,
    /// Unpadded bytes per row (actual source data stride).
    unpadded_bpr: u32,
    /// Number of rows (height for RGBA, `blocks_y` for compressed).
    rows: u32,
    /// Tracks which buffers need `map_async` after the next `queue.submit()`.
    needs_remap: [bool; 2],
}

impl VideoStagingBuffers {
    /// Create a new double-buffered staging pair.
    /// Buffers start unmapped — call `request_remap()` after the first
    /// `queue.submit()` to begin the mapping lifecycle.
    pub fn new(device: &wgpu::Device, unpadded_bpr: u32, rows: u32, label: &str) -> Self {
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bpr = (unpadded_bpr + align - 1) & !(align - 1);
        let buffer_size = u64::from(padded_bpr) * u64::from(rows);

        let make_buf = |idx: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label} Staging {idx}")),
                size: buffer_size,
                usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };

        let mapped_0 = Arc::new(AtomicBool::new(false));
        let mapped_1 = Arc::new(AtomicBool::new(false));

        Self {
            buffers: [make_buf(0), make_buf(1)],
            current: 0,
            mapped: [mapped_0, mapped_1],
            padded_bpr,
            unpadded_bpr,
            rows,
            needs_remap: [true, true],
        }
    }

    /// Whether [`Self::upload`] can take a frame now: its buffer is mapped
    /// again after the last one went to the GPU.
    pub fn ready(&self) -> bool {
        self.mapped[self.current].load(Ordering::Acquire)
    }

    /// Write frame data into the current staging buffer and encode a copy
    /// to the destination texture. Returns true if the upload was performed.
    ///
    /// # Panics
    ///
    /// Panics if a staging slot marked as mapped no longer exposes its mapped range.
    pub fn upload(
        &mut self,
        data: &[u8],
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        encoder: &mut wgpu::CommandEncoder,
    ) -> bool {
        let idx = self.current;
        if !self.mapped[idx].load(Ordering::Acquire) {
            // Buffer not yet mapped — skip this upload.
            // The stale texture from last frame will remain on screen.
            return false;
        }
        // A frame short of its size (a corrupt HAP frame) is skipped too:
        // copying it would panic the render loop.
        if data.len() < self.unpadded_bpr as usize * self.rows as usize {
            return false;
        }

        {
            let buf = &self.buffers[idx];
            let mut view = buf
                .slice(..)
                .get_mapped_range_mut()
                .expect("upload staging buffer must remain mapped");
            if self.padded_bpr == self.unpadded_bpr {
                // Row stride matches — single memcpy
                let copy_len = (self.unpadded_bpr as usize) * (self.rows as usize);
                view.slice(..copy_len).copy_from_slice(&data[..copy_len]);
            } else {
                // Need to copy row-by-row with padding
                for row in 0..self.rows as usize {
                    let src_start = row * self.unpadded_bpr as usize;
                    let dst_start = row * self.padded_bpr as usize;
                    view.slice(dst_start..dst_start + self.unpadded_bpr as usize)
                        .copy_from_slice(&data[src_start..src_start + self.unpadded_bpr as usize]);
                }
            }
        }

        self.buffers[idx].unmap();
        self.mapped[idx].store(false, Ordering::Release);

        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffers[idx],
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bpr),
                    rows_per_image: Some(self.rows),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        // Mark for re-mapping after submit
        self.needs_remap[idx] = true;

        // Advance to next buffer
        self.current = 1 - self.current;
        true
    }

    /// Request re-mapping of any buffers that were used since the last call.
    /// **Must be called AFTER `queue.submit()`** — calling `map_async` before
    /// submit can complete synchronously on UMA/Metal, leaving the buffer
    /// mapped during submit (which is a validation error).
    pub fn request_remap(&mut self) {
        for i in 0..2 {
            if self.needs_remap[i] {
                self.needs_remap[i] = false;
                let flag = self.mapped[i].clone();
                self.buffers[i]
                    .slice(..)
                    .map_async(wgpu::MapMode::Write, move |result| {
                        if result.is_ok() {
                            flag.store(true, Ordering::Release);
                        }
                    });
            }
        }
    }
}

/// Source type for a deck - what generates the base image
pub enum DeckSource {
    /// ISF shader generator
    Shader {
        shader: ISFShader,
        pipeline: UnifiedPipeline,
        pass_buffers: HashMap<String, PassBuffer>,
        passes: Vec<ISFPass>,
        /// GPU textures loaded from ISF IMPORTED images (sorted by name for deterministic binding)
        imported_textures: Vec<(String, wgpu::Texture, wgpu::TextureView)>,
        /// Preprocessor texture slots for analyzer-driven textures
        preprocessor_textures: Vec<PreprocessorSlot>,
    },
    /// Video file playback (ffmpeg CPU decode → RGBA, background decode thread)
    Video {
        handle: VideoDecodeHandle,
        /// The file's soundtrack, when it has one.
        audio: Option<crate::audio_mix::ClipAudio>,
        /// A sound file: the picture is a clear stand-in.
        sound_only: bool,
        texture: wgpu::Texture,
        texture_view: wgpu::TextureView,
        blit_pipeline: BlitPipeline,
        source_width: u32,
        source_height: u32,
        scaling_mode: ScalingMode,
        staging: VideoStagingBuffers,
    },
    /// HAP video playback (GPU-native `BCn`, background decode thread)
    HapVideo {
        handle: VideoDecodeHandle,
        /// The file's soundtrack, when it has one.
        audio: Option<crate::audio_mix::ClipAudio>,
        texture: wgpu::Texture,
        texture_view: wgpu::TextureView,
        alpha_texture: Option<wgpu::Texture>,
        alpha_texture_view: Option<wgpu::TextureView>,
        dummy_alpha_view: wgpu::TextureView,
        convert_pipeline: HapConvertPipeline,
        blit_pipeline: BlitPipeline,
        hap_format: HapTextureFormat,
        source_width: u32,
        source_height: u32,
        scaling_mode: ScalingMode,
        staging: VideoStagingBuffers,
        alpha_staging: Option<VideoStagingBuffers>,
    },
    /// Static image
    Image {
        texture: wgpu::Texture,
        texture_view: wgpu::TextureView,
        blit_pipeline: BlitPipeline,
        source_width: u32,
        source_height: u32,
        scaling_mode: ScalingMode,
        /// Vector artwork this texture was rendered from, kept so a resolution
        /// change can re-render it at the new size rather than magnifying
        /// pixels. `None` for raster images, which have nothing to re-render.
        /// Boxed to keep the enum's other variants from paying for the tree.
        svg: Option<Box<usvg::Tree>>,
        /// Every pixel of the decoded image is fully opaque, so the deck hides
        /// whatever is composited below it (see [`Deck::covers_frame`]).
        opaque: bool,
    },
    /// Solid color fill
    SolidColor { color: [f64; 4] },
    /// External live source (camera, NDI, Syphon, SRT, HLS, DASH, RTMP)
    ExternalSource {
        kind: ExternalSourceKind,
        /// REPLACE blit — writes the source's straight RGBA verbatim. Used when
        /// the deck is flagged `transparent` (preserves source alpha).
        blit_pipeline: BlitPipeline,
        /// `ALPHA_BLENDING` blit over an opaque black clear — flattens the source
        /// to opaque. Used by default (unflagged), so an HTML source with alpha<1
        /// composites over black instead of punching transparent holes.
        blit_pipeline_over_black: BlitPipeline,
        source_width: u32,
        source_height: u32,
        scaling_mode: ScalingMode,
    },
    /// GLSL compute shader generator
    ComputeShader {
        shader: ISFShader,
        pipeline: ComputePipeline,
    },
    /// Clips and pictures one after another (see [`playlist`]).
    Playlist(Box<playlist::Playlist>),
    /// A conference's presentations in running order, one on at a time
    /// (see [`slides`]).
    Slides(Box<slides::Slides>),
    /// A count down, count up or clock drawn as text (see [`timer`]).
    Timer(Box<timer::Timer>),
    /// A lower third: names and titles brought on air one at a time (see
    /// [`lower_third`]).
    LowerThird(Box<lower_third::LowerThird>),
    /// Lines of text typed in (see [`text`]).
    Text(Box<text::Text>),
    /// A PowerPoint input's show for the speaker's screen (see
    /// [`presenter`]).
    Presenter(Box<presenter::Presenter>),
    /// An input that could not be opened as its show loaded — a file moved
    /// or renamed, a drive not there, a camera unplugged: drawn clear, kept
    /// as saved (see [`OfflineSource`]).
    Offline(Box<OfflineSource>),
}

/// What a layer whose input could not be opened keeps: the input as saved,
/// written back as it was at every save — left out, the show lost it at the
/// next one — and why it is offline. Opening the show again tries it anew.
#[derive(Debug, Clone)]
pub struct OfflineSource {
    pub saved: crate::scene::SourceConfig,
    pub reason: String,
}

/// Discriminant for external source types sharing the same `DeckSource` layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSourceKind {
    Camera(crate::camera::CameraId),
    Ndi(usize),
    Syphon(usize),
    Srt(usize),
    Hls(usize),
    Dash(usize),
    Rtmp(usize),
    Html(usize),
    /// Depth sensor (Kinect/LIDAR). Unlike other external sources, this is
    /// reprojected as a point cloud into the deck texture rather than blitted.
    /// See spec/depth-sensors.md.
    DepthSensor(crate::depth::DepthSensorId),
    /// OS display or application window. See spec/screen-capture.md.
    ScreenCapture(crate::screen_capture::CaptureId),
    /// Varda's own master program or a channel composite, from the previous
    /// frame. The source it points at lives in `Deck::tap`, because a channel
    /// UUID is not `Copy`. See spec/program-tap.md.
    Tap,
}

impl ExternalSourceKind {
    /// Get the source type string for serialization.
    pub fn source_type(&self) -> &str {
        match self {
            Self::Camera(_) => "camera",
            Self::Ndi(_) => "ndi",
            Self::Syphon(_) => "syphon",
            Self::Srt(_) => "srt",
            Self::Hls(_) => "hls",
            Self::Dash(_) => "dash",
            Self::Rtmp(_) => "rtmp",
            Self::Html(_) => "html",
            Self::DepthSensor(_) => "depth_sensor",
            Self::ScreenCapture(_) => "screen_capture",
            Self::Tap => "tap",
        }
    }

    /// Render label for logging/debug.
    pub fn label(&self) -> &str {
        match self {
            Self::Camera(_) => "Camera",
            Self::Ndi(_) => "NDI",
            Self::Syphon(_) => "Syphon",
            Self::Srt(_) | Self::Hls(_) | Self::Dash(_) | Self::Rtmp(_) => "Stream",
            Self::Html(_) => "HTML",
            Self::DepthSensor(_) => "Depth Sensor",
            Self::ScreenCapture(_) => "Screen Capture",
            Self::Tap => "Tap",
        }
    }

    /// Depth-sensor id if this is a depth source.
    pub fn depth_sensor_id(&self) -> Option<crate::depth::DepthSensorId> {
        match self {
            Self::DepthSensor(id) => Some(*id),
            _ => None,
        }
    }

    /// Screen-capture id if this is a capture source.
    pub fn screen_capture_id(&self) -> Option<crate::screen_capture::CaptureId> {
        match self {
            Self::ScreenCapture(id) => Some(*id),
            _ => None,
        }
    }
}

/// The order capture settings were given in, across the decks given them (see
/// [`ScreenCaptureState::given`]).
static CAPTURE_GIVEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Live binding for a screen-capture deck.
///
/// The `CaptureId` is a runtime handle and is never persisted; `identity` is the
/// handle-free name a scene stores and rebinds by. See spec/screen-capture.md.
#[derive(Debug, Clone)]
pub struct ScreenCaptureState {
    pub capture_id: crate::screen_capture::CaptureId,
    pub identity: crate::screen_capture::backend::TargetIdentity,
    pub config: crate::screen_capture::backend::CaptureConfig,
    /// What the router or UI gave `config` since the render loop last sent
    /// settings down, in the order given. The render loop gives them to the
    /// capture — one for all the inputs of its screen or window, in the
    /// order given across those inputs — and empties this, and `config` is
    /// then as the capture runs. The deck layer never reaches up into a
    /// device, so the change travels down rather than sideways.
    pub given: Vec<crate::screen_capture::backend::CaptureGiven>,
    /// What the input was last named after: its screen or window as it was
    /// listed then ("Keynote — Opening"), its name then "🖥 " and that. A
    /// screen or window picked for it, it is named after that one while its
    /// name is still this (see `set_screen_capture_target`); any other name
    /// is one given it — a preset's — and stays. Kept with the show: a
    /// window is saved by its app's bundle id, which is not what the app is
    /// called, so an input whose app is closed has nothing else to hold its
    /// name against. `None`: never named after one, or named in a show that
    /// kept no record of it.
    pub listed_as: Option<String>,
    /// A PowerPoint input: the deck PowerPoint plays in the captured window.
    /// Its window is found by the presentation worker, never by name.
    pub presentation: Option<crate::presentation::PresentationConfig>,
}

/// What a tap deck reads. See spec/program-tap.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapSource {
    /// The master program, tapped before tonemap and LUT so the whole feedback
    /// path stays in linear light and tonemaps exactly once.
    MasterProgram,
    /// A channel composite, addressed by the channel's stable UUID so a tap
    /// survives reordering. See spec/entity-identity.md.
    Channel(String),
}

impl TapSource {
    /// Display name. Channels are resolved through the `(uuid, name)` list the
    /// caller holds; an unresolvable UUID falls back to the UUID itself, and
    /// whether it is genuinely missing is reported separately so a stale label
    /// never masquerades as an error.
    pub fn label(&self, channels: &[(String, String)]) -> String {
        match self {
            Self::MasterProgram => "Master Program".to_string(),
            Self::Channel(uuid) => channels
                .iter()
                .find(|(u, _)| u == uuid)
                .map_or_else(|| format!("Channel {uuid}"), |(_, n)| n.clone()),
        }
    }
}

/// Live binding for a tap deck. There is no runtime handle to hold: the tap
/// resolves to a mixer-owned texture view every frame, so an unresolvable
/// source simply renders black until the channel comes back.
#[derive(Debug, Clone)]
pub struct TapState {
    pub source: TapSource,
}

/// Live state for a deck's `depth_sensor` shader preprocessor.
///
/// The sensor reference is ref-counted on `DepthSensorManager`; it is acquired
/// before the deck is constructed and released when the deck is removed, so any
/// number of preprocessor decks and point-cloud decks can share one device.
pub struct DepthPreprocessState {
    /// The acquired sensor. Held so deck teardown can release the reference.
    pub sensor_id: crate::depth::DepthSensorId,
    /// Device name, captured at acquisition. Persistence matches sensors by name
    /// (ids are not stable across replugs), and snapshotting a scene must not
    /// need the device manager.
    pub sensor_name: String,
    /// Router-exposed params (`deck/<uuid>/depth_prepro/*`).
    pub params: crate::depth::preprocess::DepthPreprocessParams,
    /// The conversion pipeline and its owned output textures.
    pub pipeline: crate::depth::preprocess::DepthPreprocessPipeline,
    /// Whether any consuming shader declared the `rgb` output. When false the
    /// colour pass is skipped entirely.
    pub wants_rgb: bool,
    /// Last sensor frame generation processed, so a 30 Hz sensor does not drive
    /// 60 Hz of redundant passes.
    pub last_generation: Option<u64>,
    /// Set while the sensor reports disconnected, so the warning fires once.
    pub warned_disconnected: bool,
    /// This frame's sensor inputs, pushed by the app render loop (which owns the
    /// manager). `None` before the first tick or while the sensor is gone.
    pub input: Option<DepthPreprocessInput>,
}

/// Per-frame sensor inputs handed to a deck's depth preprocessor by the app layer.
pub struct DepthPreprocessInput {
    /// Shared `R16Uint` depth texture owned by `DepthSensorManager`.
    pub depth_view: wgpu::TextureView,
    /// Shared colour texture owned by `DepthSensorManager`.
    pub rgb_view: wgpu::TextureView,
    /// Manager upload counter, used to skip redundant passes.
    pub generation: u64,
    /// Measured seconds between the last two sensor frames.
    pub frame_dt: f32,
    /// Whether the sensor is currently producing frames.
    pub connected: bool,
}

/// A preprocessor texture slot — holds a GPU texture that gets updated with analyzer output.
pub struct PreprocessorSlot {
    /// Name prefix for shader uniforms (e.g. "depth" → `depth_depth_map`)
    pub name: String,
    /// Analyzer type this preprocessor needs (e.g. "`depth_estimate`")
    pub analyzer_type: String,
    /// Options to pass when starting the analyzer
    pub options: serde_json::Value,
    /// GPU texture (initially 1×1 black, updated at runtime)
    pub texture: wgpu::Texture,
    /// Texture view for shader binding
    pub view: wgpu::TextureView,
}

/// An effect in the deck's effect chain (ISF filter)
pub struct Effect {
    /// Stable UUID for this effect (8-char hex).
    ///
    /// Private, with [`Effect::set_uuid`] as the only way to change it, because
    /// `param_prefix` is derived from it and the two must never disagree. Scene
    /// restore used to assign the field directly, leaving the prefix pointing at
    /// the throwaway UUID minted by `Effect::new`; modulation assignments are
    /// keyed on the prefix, so every effect modulation silently stopped applying
    /// after a reload while the UI still showed it attached.
    uuid: String,
    /// Cached "fx_{uuid}" prefix for modulation key lookups (avoids per-frame format!)
    param_prefix: String,
    pub shader: ISFShader,
    pub pipeline: UnifiedPipeline,
    pub enabled: bool,
    pub params: ShaderParams,
    pub pass_buffers: HashMap<String, PassBuffer>,
    pub passes: Vec<ISFPass>,
    pub target_format: wgpu::TextureFormat,
    /// GPU textures loaded from ISF IMPORTED images (sorted by name for deterministic binding)
    pub imported_textures: Vec<(String, wgpu::Texture, wgpu::TextureView)>,
    /// Preprocessor textures from PREPROCESSORS declarations (placeholder until analyzer provides data)
    pub preprocessor_textures: Vec<PreprocessorSlot>,
    /// Phase accumulators for smooth speed transitions
    pub phase_accumulators: [f32; 4],
    /// Phase input config from shader metadata
    pub phase_inputs_config: Option<Vec<crate::isf::PhaseInput>>,
    /// The uniforms of each pass, by its index, for a multi-pass filter
    /// none of whose targets is kept from frame to frame: its passes are
    /// drawn in their turn with the rest of the frame (see
    /// `Effect::apply_passes`), so each needs a buffer of its own. Empty for
    /// any other effect.
    pass_uniform_buffers: Vec<wgpu::Buffer>,
    /// Frames this effect has actually been run on a deck — neutral framing
    /// is skipped (see `Deck::render`).
    runs: u64,
    /// Each pixel it puts out reads the input at that pixel only (see
    /// [`ISFShader::is_pointwise`]), so it can be run on part of the frame.
    pointwise: bool,
    /// Where the effect shows: only inside (or outside) this shape. `None`
    /// is everywhere.
    pub mask: Option<crate::engine::value::mask::EffectMask>,
    /// The pass that mixes the effect in through its mask, made when there
    /// first is one.
    mask_pass: Option<crate::renderer::mask::MaskPass>,
    /// The shader's path as the show named it, when that file was gone and
    /// this copy's shader of the same name stood in (see
    /// [`Effect::saved_path`]).
    show_path: Option<String>,
    /// Why its shader could not be loaded, when it could not: the effect is
    /// then kept as saved, drawing nothing (see `persistence::restore_effect_kept`).
    offline: Option<String>,
}

// Effect impl is in effect.rs

/// Multi-pass buffer for ISF PASSES array
/// Uses ping-pong buffers for persistent passes to allow read/write in same frame
pub struct PassBuffer {
    /// Buffer name (from ISF PASSES TARGET field)
    pub name: String,
    /// Primary texture (read source for persistent buffers)
    pub texture_a: wgpu::Texture,
    /// Primary texture view
    pub view_a: wgpu::TextureView,
    /// Secondary texture (write target for persistent buffers) - only for persistent
    pub texture_b: Option<wgpu::Texture>,
    /// Secondary texture view
    pub view_b: Option<wgpu::TextureView>,
    /// Whether this buffer persists across frames
    pub persistent: bool,
    /// Current read index (0 = read from A, 1 = read from B)
    pub read_idx: usize,
}

/// A Deck is an independent render unit that outputs a texture
// Each bool is an independent property of the deck, not a state enum.
#[allow(clippy::struct_excessive_bools)]
pub struct Deck {
    /// Stable UUID for this deck (8-char hex, persists across moves/saves)
    uuid: String,

    /// Cached "deck_{uuid}" prefix for modulation key lookups (avoids per-frame format!)
    param_prefix: String,

    /// Name of this deck's source
    source_name: String,

    /// Original file path used to create this deck (for persistence).
    /// Shader path, video path, or image path. None for solid color / camera.
    source_path: Option<String>,

    /// Source type and pipeline (shader, video, or image)
    source: DeckSource,

    /// Generator shader parameters (if source is a shader)
    pub generator_params: ShaderParams,

    /// Render target texture (primary)
    pub texture: wgpu::Texture,

    /// Texture view
    pub texture_view: wgpu::TextureView,

    /// Secondary texture for ping-pong rendering in effect chain
    texture_b: wgpu::Texture,
    texture_b_view: wgpu::TextureView,

    /// Effect chain (ISF filters applied to generator output)
    pub effects: Vec<Effect>,

    /// Deck opacity (0.0 - 1.0)
    pub opacity: f32,

    /// When true, the deck's base texture preserves source alpha (transparent
    /// letterbox + transparent HTML regions). When false (default), the source is
    /// composited over opaque black, reproducing the historical opaque behavior.
    /// See /spec/html-source.md §2 (Option A).
    transparent: bool,

    /// What a clip does by itself going on and off air. Only clips (video
    /// and sound files) use it.
    pub auto_playback: crate::engine::value::video::AutoPlayback,

    /// The size and rate chosen for a camera or capture card; `None` takes
    /// its best. Only cameras use it.
    pub camera_mode: Option<crate::engine::value::camera::CameraMode>,

    /// The unique ID of a camera deck's device, for the show to find that
    /// one of two identical cameras again; `None` for a show saved before
    /// they were kept. Only cameras use it.
    pub camera_device: Option<String>,

    /// How a camera deck's picture is read into colours: as the camera says,
    /// or range and matrix set by hand. Every deck on one camera shares its
    /// capture, so they share this too. Only cameras use it.
    pub camera_colour: crate::engine::value::camera::CameraColour,

    /// Show the camera's newest picture each frame, a frame or so sooner but
    /// with a judder whenever one comes late, rather than each in turn at
    /// the delay that keeps them even (see `FramePicker::set_newest`).
    /// Shared, like the colour, by every deck on one camera.
    pub camera_lowest_latency: bool,

    /// What a camera deck's picture is coded as — Rec.709, or a log or HLG
    /// picture converted for the show (see `renderer::camera_profile`).
    /// Shared, like the colour, by every deck on one camera. Only cameras
    /// use it.
    pub camera_profile: crate::engine::value::camera::CameraProfile,

    /// Accumulated render time for TIME uniform (advances by fixed dt each render).
    /// Decoupled from wall clock so skipped frames don't cause animation jumps.
    render_time: f32,

    /// Fixed time step per render (`1/target_fps`). Updated by the channel when
    /// the deck is rendered, so skipped frames simply don't advance `render_time`.
    render_dt: f32,

    /// Frame counter
    frame_count: u32,

    /// Last wall-clock render instant (for FPS measurement only, not for TIME uniform)
    last_frame_time: Instant,

    /// External source texture view (set each frame for `ExternalSource` decks).
    /// For `DepthSensor` decks this holds the `R16Uint` depth view.
    pub external_source_view: Option<wgpu::TextureView>,

    /// The same picture read as it was coded — sRGB code values, not light
    /// — where its source has such a view: a screen or window captured (set
    /// each frame beside `external_source_view`; see
    /// `ScreenCaptureManager::coded_view`). The deck draws from this one
    /// then, filtering the code values and making them light after (see
    /// [`source_read`]): text drawn smaller keeps its weight, as it does
    /// shrunk by anything else. `None`: drawn from `external_source_view`,
    /// filtered as light — every other source, and whatever reads that
    /// field without knowing of this one.
    pub external_source_coded_view: Option<wgpu::TextureView>,
    /// Which picture `external_source_view` holds, where its source says —
    /// a screen or window captured: another number each time a frame is put
    /// in its texture or the texture made anew, no two the same anywhere
    /// (set each frame beside the view; see
    /// `ScreenCaptureManager::picture`). The same from one frame to the
    /// next, the picture is the same: its channel, standing still, keeps
    /// what it has put together (see `Channel::keep_still_inputs`). `None`:
    /// not known — any other source — and nothing is kept on its account.
    pub external_source_picture: Option<u64>,
    /// A tap of an input drawn before it this frame: that input's picture
    /// number (`Channel::picture`), which is another the moment the input
    /// is put together anew — read when this deck's own input is, later in
    /// the same frame, so what the input shows is never a frame behind. Set
    /// each frame beside the view (`Mixer::prepare_taps`). `None`: not a
    /// tap, or one of Main or of an input drawn after it (a frame late,
    /// round a loop), whose picture is not told so.
    pub tapped_picture: Option<Arc<std::sync::atomic::AtomicU64>>,

    /// Depth-sensor RGB view (set each frame for `DepthSensor` decks).
    pub depth_rgb_view: Option<wgpu::TextureView>,

    /// Depth-sensor intrinsics (set each frame for `DepthSensor` decks).
    pub depth_intrinsics: Option<crate::depth::backend::DepthIntrinsics>,

    /// Native depth-sensor resolution `(w, h)` (set each frame).
    pub depth_source_size: Option<(u32, u32)>,

    /// Point-cloud reprojection params for `DepthSensor` decks (router-driven).
    pub point_cloud_params: crate::depth::point_cloud::PointCloudParams,

    /// Lazily-built point-cloud pipeline for `DepthSensor` decks.
    point_cloud_pipeline: Option<crate::depth::point_cloud::PointCloudPipeline>,

    /// Depth-sensor shader preprocessor, present when this deck's shader (or one
    /// of its effects) declared a `depth_sensor` PREPROCESSOR and the device was
    /// successfully acquired. See spec/depth-sensor-preprocessor.md.
    pub depth_prepro: Option<DepthPreprocessState>,

    /// Screen-capture binding, present only on `ScreenCapture` decks. Held on
    /// the deck rather than looked up in the manager so `snapshot_scene` can
    /// serialize the target and settings from the mixer alone.
    /// See spec/screen-capture.md § Configuration and Persistence.
    pub screen_capture: Option<ScreenCaptureState>,

    /// Tap binding, present only on `Tap` decks. See spec/program-tap.md.
    pub tap: Option<TapState>,

    /// Smoothed FPS derived from actual render pipeline timing (EMA of `1/time_delta`)
    fps_smoothed: f32,

    /// Phase accumulators for smooth speed transitions (generator shader)
    phase_accumulators: [f32; 4],

    /// Phase input config from generator shader metadata
    generator_phase_inputs: Option<Vec<crate::isf::PhaseInput>>,

    /// Per-deck analyzer instances (brightness, beat detection, etc.)
    pub(crate) analyzers: crate::analyzer::DeckAnalyzers,

    /// Set when this deck raised a GPU error, which quarantines it: it stops
    /// rendering and holds its last good frame instead of aborting the process.
    /// Cleared by [`Deck::clear_gpu_error`] when the shader is reloaded.
    /// See spec/error-handling.md § Shader Errors.
    gpu_error: Option<String>,

    /// Resolution this deck renders at, relative to the master.
    render_scale: DeckRenderScale,
    /// The master size this deck was last sized for, so a new render scale
    /// can be applied without being told it again.
    master_size: (u32, u32),

    /// For a still image — or a lower third or a text standing still: what
    /// its last render depended on. While that holds the texture already
    /// has this frame's picture, so rendering is skipped (see
    /// [`Deck::still_frame`]).
    rendered_still: Option<StillFrame>,
    /// Keep a lower third or a text standing still as it is drawn, as a
    /// still image is kept (see [`Deck::still_frame`]); off draws it every
    /// frame, as before. Set by its channel each frame.
    keep_still_text: bool,
    /// Keep a clip's frame while the clip gives no new one — paused, held
    /// on its last, or between two of its frames in a show running faster
    /// (see [`Deck::still_frame`]); off draws it every frame, as before.
    /// Set by its channel each frame: off in a deck no channel holds — an
    /// item of a playlist, a talk's clip.
    keep_still_clips: bool,
    /// Keep the frame drawn of a live source whose picture is told (see
    /// [`Deck::source_picture`]) while that picture is the same — another
    /// input standing still, a window captured where nothing moves — as a
    /// clip's is kept between two of its frames (see
    /// [`Deck::still_frame`]); off draws it every frame, as before. Set by
    /// its channel each frame (`Channel::keep_still_inputs`): off in a deck
    /// no channel holds.
    keep_still_sources: bool,
    /// A number no other deck has, whatever its UUID: a deck made anew in
    /// another's place — a show opened again, an undo — has another.
    instance: u64,
    /// Which textures the deck has: one more each time they are made anew,
    /// empty (see [`Deck::texture_generation`]).
    texture_generation: u32,

    /// The parts of the frame that show of this deck once the opaque decks
    /// over it are drawn, set by its channel each frame; `None` for all of
    /// it. Its passes draw only there (see [`crate::renderer::region`]).
    visible: Option<Vec<crate::renderer::region::Area>>,
    /// `visible` as it was when the deck last rendered: the rest of its
    /// texture is stale, so a frame that shows more must render.
    drawn_visible: Option<Vec<crate::renderer::region::Area>>,
    /// The share of the frame its source drew in its last render.
    drawn_share: f32,
    /// Draws the picture cropped and placed in one pass, made when first
    /// needed (see `Deck::fold`).
    placed: Option<crate::renderer::placed_picture::PlacedPicturePipeline>,
    /// A clip's frames put in its texture: none yet, and it shows nothing
    /// (see [`Deck::has_video_frame`]).
    frames_uploaded: u64,
}

/// Everything a deck's picture depends on while it stands still — a still
/// image, a lower third on air or off, a text shown or hidden: its size,
/// what it is of, and each framing effect's settings. Equal between frames
/// means the texture is already right.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StillFrame {
    size: (u32, u32),
    source: StillSource,
    effects: Vec<(String, bool, Vec<u32>)>,
}

impl StillFrame {
    /// Drawn only where it shows, as a deck not standing still is — a
    /// clip's frame, or a live source's picture: kept for as long as the
    /// clip or the source gives no new one, not for good, it is not worth
    /// drawing whole.
    fn in_parts(&self) -> bool {
        matches!(
            self.source,
            StillSource::Clip { .. } | StillSource::Live { .. }
        )
    }
}

/// What a deck standing still is of (see `StillFrame`).
#[derive(Debug, Clone, PartialEq)]
enum StillSource {
    /// An image, as it is fitted.
    Image(ScalingMode),
    /// A lower third's or a text's drawing.
    Drawing(Stand),
    /// A clip's frame: which one, as it is fitted, and the parts of it
    /// drawn (a clip is drawn only where it shows).
    Clip {
        frame: u64,
        scaling_mode: ScalingMode,
        visible: Option<Vec<crate::renderer::region::Area>>,
    },
    /// A live source's picture — another input's, a capture's: which one
    /// (`Deck::source_picture`), the size it is placed by (`placed_size`:
    /// its own, or a centred capture's as fitted), as it is fitted,
    /// flattened over black or not, read as it was coded or as light, and
    /// the parts of it drawn (drawn only where it shows, as a clip is).
    /// The show's size is not here: another, the frame is forgotten
    /// (`Deck::resize`).
    Live {
        picture: u64,
        placed: (u32, u32),
        scaling_mode: ScalingMode,
        transparent: bool,
        coded: bool,
        visible: Option<Vec<crate::renderer::region::Area>>,
    },
}

/// A lower third or a text standing still — neither coming nor going: what
/// its frame shows (see `LowerThird::still`, `Text::still`).
#[derive(Debug, Clone, PartialEq)]
enum Stand {
    /// Nothing: off, hidden, or with no drawing.
    Clear,
    /// A drawing — which (`next_drawing`: another each time one is put on
    /// the GPU), and where and how far shown, as the numbers its shader is
    /// given.
    Drawing { serial: u64, placed: Vec<u8> },
}

/// Drawings put on the GPU, counted across every lower third and text.
static DRAWINGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A number no other drawing has (see `Stand::Drawing`).
fn next_drawing() -> u64 {
    DRAWINGS.fetch_add(1, Ordering::Relaxed)
}

/// Pictures put where a deck can be drawn straight from — a capture's
/// texture, an input's own picture — counted across them all.
static PICTURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A number no other picture has (see `Deck::source_picture`).
pub(crate) fn next_picture() -> u64 {
    PICTURES.fetch_add(1, Ordering::Relaxed)
}

impl Deck {
    /// Which picture its live source holds, where that is told: a capture's
    /// (`external_source_picture`) or, a tap, the input's it shows
    /// (`tapped_picture`). The same from one frame to the next, the source
    /// is as it was.
    pub fn source_picture(&self) -> Option<u64> {
        match &self.tapped_picture {
            Some(picture) => Some(picture.load(Ordering::Relaxed)),
            None => self.external_source_picture,
        }
    }
}

/// Decks made, counted.
static DECKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A number no other deck has (see `Deck::instance`).
fn next_instance() -> u64 {
    DECKS.fetch_add(1, Ordering::Relaxed)
}

/// A parameter value as comparable bits, for [`StillFrame`].
fn param_bits(value: Option<&crate::params::ParamValue>) -> Vec<u32> {
    use crate::params::ParamValue;
    match value {
        None => vec![u32::MAX],
        Some(ParamValue::Float(v)) => vec![v.to_bits()],
        Some(ParamValue::Bool(v)) => vec![u32::from(*v)],
        Some(ParamValue::Long(v)) => vec![v.cast_unsigned()],
        Some(ParamValue::Color(c)) => c.iter().map(|v| v.to_bits()).collect(),
        Some(ParamValue::Point2D(p)) => p.iter().map(|v| v.to_bits()).collect(),
    }
}

/// Accessors for Deck properties.
/// Constructors are in source.rs, rendering in render.rs.
impl Deck {
    /// Get the stable UUID for this deck
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Get the cached param prefix ("deck_{uuid}")
    pub fn param_prefix(&self) -> &str {
        &self.param_prefix
    }

    /// Set the UUID (used during scene restore to preserve identity)
    pub fn set_uuid(&mut self, uuid: String) {
        self.param_prefix = format!("deck_{uuid}");
        self.uuid = uuid;
    }

    /// Get the source name (shader name, video filename, etc.)
    /// Which textures the deck has: one more each time they are made anew,
    /// empty, as a resize makes them — what was drawn into the old ones is
    /// gone.
    #[must_use]
    pub fn texture_generation(&self) -> u32 {
        self.texture_generation
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Override the display name (e.g. when loading a preset with a custom name).
    pub fn set_source_name(&mut self, name: String) {
        self.source_name = name;
    }

    /// Keep `path`, the show's, as the source path a save writes — this
    /// copy's shader stood in for a file gone (see
    /// `persistence::saved_shader`).
    pub(crate) fn keep_source_path(&mut self, path: String) {
        self.source_path = Some(path);
    }

    /// Get the source file path (for persistence). None for solid color / camera.
    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }

    /// What the audio mixer reads of this deck's soundtrack: `None` unless
    /// it is a video with sound.
    pub fn clip_audio(&self) -> Option<std::sync::Arc<crate::audio_mix::ClipAudioShared>> {
        match &self.source {
            DeckSource::Video { audio, .. } | DeckSource::HapVideo { audio, .. } => {
                audio.as_ref().map(crate::audio_mix::ClipAudio::shared)
            }
            DeckSource::Playlist(list) => list.clip_audio(),
            DeckSource::Slides(slides) => slides.clip_audio(),
            _ => None,
        }
    }

    /// Which decode thread plays this clip, to tell a clip rebuilt under the
    /// same UUID (a structural undo) from the one it replaced. `None` for
    /// anything but a clip.
    pub fn playback_identity(&self) -> Option<usize> {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                Some(std::sync::Arc::as_ptr(&handle.snapshot_cell()) as usize)
            }
            _ => None,
        }
    }

    /// Whether a clip has put a frame in its texture: until then its
    /// picture is empty — a Slides talk waits for it. True for anything but
    /// a clip.
    pub fn has_video_frame(&self) -> bool {
        match &self.source {
            DeckSource::Video { .. } | DeckSource::HapVideo { .. } => self.frames_uploaded > 0,
            _ => true,
        }
    }

    /// Whether this deck is a sound file, whose picture is a clear stand-in.
    pub fn is_sound_only(&self) -> bool {
        matches!(
            self.source,
            DeckSource::Video {
                sound_only: true,
                ..
            }
        )
    }

    /// Get the source type as a string for serialization
    pub fn source_type(&self) -> &str {
        match &self.source {
            DeckSource::Shader { .. } => "shader",
            DeckSource::Video { .. } | DeckSource::HapVideo { .. } => "video",
            DeckSource::Image { .. } => "image",
            DeckSource::SolidColor { .. } => "solid_color",
            DeckSource::ExternalSource { kind, .. } => kind.source_type(),
            DeckSource::ComputeShader { .. } => "compute_shader",
            DeckSource::Playlist(_) => "playlist",
            DeckSource::Slides(_) => "slides",
            DeckSource::Timer(_) => "timer",
            DeckSource::LowerThird(_) => "lower_third",
            DeckSource::Text(_) => "text",
            DeckSource::Presenter(_) => "presenter",
            DeckSource::Offline(_) => "offline",
        }
    }

    /// Get a read-only snapshot of the video playback state.
    /// Whether its own picture changes by itself, for how often the control
    /// window is drawn (see `app::state::ui_motion`): a generator reading the
    /// clock, a clip or playlist playing, a live source, a talk that moves
    /// (a clip playing, PowerPoint or a web page live) or another coming in. A timer's or a presenter's clock moves
    /// by the second, which the window's idle rate keeps up with. Its effects
    /// are asked about apart.
    pub fn moves_by_itself(&self) -> bool {
        match &self.source {
            DeckSource::Shader { shader, .. } | DeckSource::ComputeShader { shader, .. } => {
                shader.animates()
            }
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                handle.playback_snapshot().playing
            }
            // A camera, NDI, a capture, a stream, a web page: live — once it
            // has a picture. A camera not plugged in, a screen not there, is
            // a black layer that stands still. Another input shown as a layer
            // (a tap) moves with what it shows, which is asked about itself.
            DeckSource::ExternalSource { kind, .. } => {
                !matches!(kind, ExternalSourceKind::Tap) && self.external_source_view.is_some()
            }
            DeckSource::Playlist(list) => list.is_playing(),
            DeckSource::Slides(slides) => slides.moves_by_itself(),
            DeckSource::Image { .. }
            | DeckSource::SolidColor { .. }
            | DeckSource::Timer(_)
            | DeckSource::LowerThird(_)
            | DeckSource::Text(_)
            | DeckSource::Presenter(_)
            | DeckSource::Offline(_) => false,
        }
    }

    pub fn playback_snapshot(&self) -> Option<PlaybackSnapshot> {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                Some(handle.playback_snapshot())
            }
            _ => None,
        }
    }

    /// Bound this deck's decode rate to what the renderer can present
    /// (0 = uncapped). No-op for non-video decks.
    pub fn set_video_output_fps(&self, fps: u32) {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                handle.set_output_fps(fps);
            }
            DeckSource::Playlist(list) => {
                list.set_output_fps(fps);
                list.for_each_deck(|d| d.set_video_output_fps(fps));
            }
            DeckSource::Slides(slides) => {
                slides.set_output_fps(fps);
                slides.for_each_deck(|d| d.set_video_output_fps(fps));
            }
            _ => {}
        }
    }

    /// Stop or resume this deck's decoding, leaving its play/pause state alone.
    /// No-op for non-video decks. See /spec/deck-residency.md.
    pub fn set_video_suspended(&self, suspended: bool) {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                handle.set_suspended(suspended);
            }
            DeckSource::Playlist(list) => list.for_each_deck(|d| d.set_video_suspended(suspended)),
            DeckSource::Slides(slides) => {
                slides.for_each_deck(|d| d.set_video_suspended(suspended));
            }
            _ => {}
        }
    }

    /// Whether this deck's decoding is currently suspended. False for anything
    /// that is not a video deck, which never suspends.
    pub fn video_is_suspended(&self) -> bool {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                handle.is_suspended()
            }
            _ => false,
        }
    }

    /// Send a command to the video decode thread (no-op for non-video decks).
    fn video_send(&self, cmd: VideoCommand) -> bool {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                handle.send(cmd);
                true
            }
            _ => false,
        }
    }

    /// Toggle play/pause on the video decode thread.
    pub fn video_toggle_play(&self) -> bool {
        if let Some(snap) = self.playback_snapshot() {
            if snap.playing {
                self.video_send(VideoCommand::Pause)
            } else {
                self.video_send(VideoCommand::Play)
            }
        } else {
            false
        }
    }

    /// Set playing state on the video decode thread.
    pub fn video_set_playing(&self, playing: bool) -> bool {
        if playing {
            self.video_send(VideoCommand::Play)
        } else {
            self.video_send(VideoCommand::Pause)
        }
    }

    /// Set playback speed on the video decode thread.
    pub fn video_set_speed(&self, speed: f64) -> bool {
        self.video_send(VideoCommand::SetSpeed(speed))
    }

    /// Set loop mode on the video decode thread.
    pub fn video_set_loop_mode(&self, mode: crate::video::LoopMode) -> bool {
        self.video_send(VideoCommand::SetLoopMode(mode))
    }

    /// Map this clip onto the show transport. No-op for non-video decks.
    pub fn video_set_transport_sync(&self, sync: crate::video::DeckTransportSync) -> bool {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                handle.set_transport_sync(sync);
                true
            }
            _ => false,
        }
    }

    pub fn video_transport_sync(&self) -> Option<crate::video::DeckTransportSync> {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                Some(handle.transport_sync())
            }
            _ => None,
        }
    }

    /// Publish this frame's transport so a chasing clip can servo.
    pub fn publish_video_chase(
        &self,
        sample: crate::video::VideoChaseBroadcast,
        discontinuity: bool,
    ) {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                handle.publish_chase(sample, discontinuity);
            }
            _ => {}
        }
    }

    /// Publish this frame's resolved playback modulation so the decode thread
    /// can act on it. No-op for non-video decks.
    /// See /spec/video-playback-modulation.md.
    pub fn publish_video_modulation(&self, value: crate::video::PlaybackModulation) {
        match &self.source {
            DeckSource::Video { handle, .. } | DeckSource::HapVideo { handle, .. } => {
                handle.publish_modulation(value);
            }
            _ => {}
        }
    }

    /// Set in-point on the video decode thread.
    pub fn video_set_in_point(&self, secs: f64) -> bool {
        self.video_send(VideoCommand::SetInPoint(secs))
    }

    /// Set out-point on the video decode thread.
    pub fn video_set_out_point(&self, secs: f64) -> bool {
        self.video_send(VideoCommand::SetOutPoint(secs))
    }

    /// Clear in/out points on the video decode thread.
    pub fn video_clear_in_out_points(&self) -> bool {
        self.video_send(VideoCommand::ClearInOutPoints)
    }

    /// Seek the video to a specific position in seconds.
    pub fn video_seek(&self, time_secs: f64) -> bool {
        self.video_send(VideoCommand::Seek(time_secs))
    }

    /// Get the solid color value (if source is a solid color)
    pub fn solid_color(&self) -> Option<[f32; 4]> {
        match &self.source {
            DeckSource::SolidColor { color } => Some([
                color[0] as f32,
                color[1] as f32,
                color[2] as f32,
                color[3] as f32,
            ]),
            _ => None,
        }
    }

    /// Set the solid color value (only applies to `SolidColor` sources)
    pub fn set_solid_color(&mut self, new_color: [f32; 4]) {
        if let DeckSource::SolidColor { color } = &mut self.source {
            // Still called after its colour: the name follows it.
            let old = colour_name(color.map(|c| c as f32));
            if self.source_name == old {
                self.source_name = colour_name(new_color);
            }
            *color = [
                f64::from(new_color[0]),
                f64::from(new_color[1]),
                f64::from(new_color[2]),
                f64::from(new_color[3]),
            ];
        }
    }

    /// Get the scaling mode (if applicable for this source type)
    pub fn scaling_mode(&self) -> Option<ScalingMode> {
        match &self.source {
            DeckSource::Image { scaling_mode, .. }
            | DeckSource::Video { scaling_mode, .. }
            | DeckSource::HapVideo { scaling_mode, .. }
            | DeckSource::ExternalSource { scaling_mode, .. } => Some(*scaling_mode),
            DeckSource::Playlist(list) => Some(list.scaling_mode()),
            DeckSource::Slides(slides) => Some(slides.scaling_mode()),
            _ => None,
        }
    }

    /// The size of the picture a scaling mode fits into the frame, once known
    /// (a live source reports it with its first frame; a capture centred, as
    /// it is placed: see [`placed_size`]).
    pub fn source_size(&self) -> Option<[u32; 2]> {
        let (w, h) = match &self.source {
            DeckSource::Image {
                source_width,
                source_height,
                ..
            }
            | DeckSource::Video {
                source_width,
                source_height,
                ..
            }
            | DeckSource::HapVideo {
                source_width,
                source_height,
                ..
            } => (*source_width, *source_height),
            DeckSource::ExternalSource {
                source_width,
                source_height,
                scaling_mode,
                ..
            } => placed_size(
                *scaling_mode,
                (*source_width, *source_height),
                self.screen_capture.as_ref(),
            ),
            _ => return None,
        };
        (w > 0 && h > 0).then_some([w, h])
    }

    /// Set the scaling mode (applies to Image, Video, `HapVideo`, and `ExternalSource` sources)
    pub fn set_scaling_mode(&mut self, mode: ScalingMode) {
        match &mut self.source {
            DeckSource::Image { scaling_mode, .. }
            | DeckSource::Video { scaling_mode, .. }
            | DeckSource::HapVideo { scaling_mode, .. }
            | DeckSource::ExternalSource { scaling_mode, .. } => *scaling_mode = mode,
            DeckSource::Playlist(list) => list.set_scaling_mode(mode),
            DeckSource::Slides(slides) => slides.set_scaling_mode(mode),
            _ => {}
        }
    }

    /// A layer showing another input (a tap) whose input is not there: it
    /// has no picture this frame.
    pub fn shows_a_lost_input(&self) -> bool {
        self.tap.is_some() && self.external_source_view.is_none()
    }

    /// The playlist this deck plays, if it is one.
    pub fn playlist(&self) -> Option<&playlist::Playlist> {
        match &self.source {
            DeckSource::Playlist(list) => Some(list),
            _ => None,
        }
    }

    pub fn playlist_mut(&mut self) -> Option<&mut playlist::Playlist> {
        match &mut self.source {
            DeckSource::Playlist(list) => Some(list),
            _ => None,
        }
    }

    /// The Slides layer this deck is, if it is one.
    pub fn slides(&self) -> Option<&slides::Slides> {
        match &self.source {
            DeckSource::Slides(slides) => Some(slides),
            _ => None,
        }
    }

    pub fn slides_mut(&mut self) -> Option<&mut slides::Slides> {
        match &mut self.source {
            DeckSource::Slides(slides) => Some(slides),
            _ => None,
        }
    }

    /// When the refresh that called for this frame came (see
    /// [`crate::mixer::FrameTiming::refresh_at`]): a playlist moves its
    /// transition on by it.
    pub fn set_frame_refresh_at(&mut self, at: Option<std::time::Instant>) {
        if let DeckSource::Playlist(list) = &mut self.source {
            list.frame_refresh_at(at);
        }
    }

    /// Each frame is held on screen a frame's time (see
    /// [`crate::mixer::FrameTiming::held`]): a playlist steps a frame per
    /// frame.
    pub fn set_frame_held(&mut self, held: bool) {
        if let DeckSource::Playlist(list) = &mut self.source {
            list.frame_held(held);
        }
    }

    /// The timer this deck shows, if it is one.
    pub fn timer(&self) -> Option<&timer::Timer> {
        match &self.source {
            DeckSource::Timer(timer) => Some(timer),
            _ => None,
        }
    }

    pub fn timer_mut(&mut self) -> Option<&mut timer::Timer> {
        match &mut self.source {
            DeckSource::Timer(timer) => Some(timer),
            _ => None,
        }
    }

    /// The lower third this deck shows, if it is one.
    pub fn lower_third(&self) -> Option<&lower_third::LowerThird> {
        match &self.source {
            DeckSource::LowerThird(l) => Some(l),
            _ => None,
        }
    }

    /// The text this deck shows, if it is a text input.
    pub fn text(&self) -> Option<&text::Text> {
        match &self.source {
            DeckSource::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn text_mut(&mut self) -> Option<&mut text::Text> {
        match &mut self.source {
            DeckSource::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn lower_third_mut(&mut self) -> Option<&mut lower_third::LowerThird> {
        match &mut self.source {
            DeckSource::LowerThird(l) => Some(l),
            _ => None,
        }
    }

    /// What an offline layer keeps of its input (see [`OfflineSource`]), if
    /// it is one.
    pub fn offline(&self) -> Option<&OfflineSource> {
        match &self.source {
            DeckSource::Offline(offline) => Some(offline),
            _ => None,
        }
    }

    /// The presenter view this deck shows, if it is one.
    pub fn presenter(&self) -> Option<&presenter::Presenter> {
        match &self.source {
            DeckSource::Presenter(presenter) => Some(presenter),
            _ => None,
        }
    }

    pub fn presenter_mut(&mut self) -> Option<&mut presenter::Presenter> {
        match &mut self.source {
            DeckSource::Presenter(presenter) => Some(presenter),
            _ => None,
        }
    }

    /// Set a screen-capture parameter from a normalized value (0.0–1.0), and
    /// keep it to be given the deck's capture (see
    /// [`ScreenCaptureState::given`]). Returns `false` if this deck is not a
    /// screen-capture source or `name` is not a capture parameter. See
    /// spec/screen-capture.md § Parameters.
    pub fn set_capture_param(&mut self, name: &str, value: f32) -> bool {
        use crate::screen_capture::backend::{CaptureGiven, CaptureSetting};
        let Some(state) = &mut self.screen_capture else {
            return false;
        };
        let Some(setting) = CaptureSetting::named(name) else {
            return false;
        };
        state.config.give(setting, value);
        state.given.push(CaptureGiven {
            at: CAPTURE_GIVEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            setting,
            value,
        });
        true
    }

    /// Set a depth point-cloud parameter from a normalized value (0.0–1.0).
    /// Returns `false` if this deck is not a depth-sensor source. Continuous
    /// params map linearly to their range; `color_mode` buckets into 3 modes.
    /// See spec/depth-sensors.md.
    pub fn set_depth_param(&mut self, name: &str, value: f32) -> bool {
        if !matches!(
            self.external_source_kind(),
            Some(ExternalSourceKind::DepthSensor(_))
        ) {
            return false;
        }
        self.point_cloud_params.set_normalized_param(name, value)
    }

    /// Normalized (`0..1`) value of a point-cloud parameter, for snapshots.
    /// `None` when this deck is not a depth-sensor source, so a consumer cannot
    /// render faders for a deck that has no point cloud.
    pub fn depth_param(&self, name: &str) -> Option<f32> {
        if !matches!(
            self.external_source_kind(),
            Some(ExternalSourceKind::DepthSensor(_))
        ) {
            return None;
        }
        self.point_cloud_params.normalized_param(name)
    }

    /// Set a depth-preprocessor parameter from a normalized value (0.0–1.0).
    /// Returns `false` if this deck has no `depth_sensor` preprocessor.
    /// See spec/depth-sensor-preprocessor.md.
    pub fn set_depth_prepro_param(&mut self, name: &str, value: f32) -> bool {
        self.depth_prepro
            .as_mut()
            .is_some_and(|s| s.params.set_normalized_param(name, value))
    }

    /// Normalized (`0..1`) value of a depth-preprocessor parameter, for snapshots.
    pub fn depth_prepro_param(&self, name: &str) -> Option<f32> {
        self.depth_prepro
            .as_ref()
            .and_then(|s| s.params.normalized_param(name))
    }

    /// Attach an acquired depth sensor's preprocessor to this deck.
    ///
    /// Rebinds every `depth_sensor` preprocessor slot — on the source shader and
    /// on every effect — to the pipeline's owned output textures. The clones are
    /// `Arc`-backed handles to the same GPU resources, so this is a rebind, not a
    /// copy. Called by the app layer after `open_depth_sensor` succeeds, because
    /// device managers live above `internal::deck`.
    pub fn attach_depth_preprocessor(
        &mut self,
        sensor_id: crate::depth::DepthSensorId,
        sensor_name: String,
        pipeline: crate::depth::preprocess::DepthPreprocessPipeline,
        params: crate::depth::preprocess::DepthPreprocessParams,
    ) {
        self.depth_prepro = Some(DepthPreprocessState {
            sensor_id,
            sensor_name,
            params,
            pipeline,
            wants_rgb: false,
            last_generation: None,
            warned_disconnected: false,
            input: None,
        });
        self.rebind_depth_preprocessor_slots();
    }

    /// Point every `depth_sensor` preprocessor slot at the attached pipeline's
    /// outputs, and recompute whether the colour pass is needed.
    ///
    /// Idempotent, and must be re-run after adding an effect that declares the
    /// preprocessor to a deck that already has one attached.
    pub fn rebind_depth_preprocessor_slots(&mut self) {
        use crate::depth::preprocess::{Output, PREPROCESSOR_TYPE};

        // Take the state so the pipeline can be read while `self.source` and
        // `self.effects` are mutably borrowed.
        let Some(mut state) = self.depth_prepro.take() else {
            return;
        };
        let mut wants_rgb = false;
        let mut rebind = |slots: &mut Vec<PreprocessorSlot>| {
            for slot in slots {
                if slot.analyzer_type != PREPROCESSOR_TYPE {
                    continue;
                }
                let Some(output) = Output::from_name(&slot.name) else {
                    log::warn!(
                        "Shader declared unknown depth_sensor output '{}'; leaving it blank",
                        slot.name
                    );
                    continue;
                };
                if output == Output::Rgb {
                    wants_rgb = true;
                }
                if let Some((texture, view)) = state.pipeline.output(output) {
                    slot.texture = texture;
                    slot.view = view;
                }
            }
        };

        if let DeckSource::Shader {
            preprocessor_textures,
            ..
        } = &mut self.source
        {
            rebind(preprocessor_textures);
        }
        for effect in &mut self.effects {
            rebind(&mut effect.preprocessor_textures);
        }

        state.wants_rgb = wants_rgb;
        self.depth_prepro = Some(state);
    }

    /// Whether any slot on this deck still consumes the `depth_sensor`
    /// preprocessor. Used after removing an effect to decide whether the sensor
    /// reference is still needed.
    fn wants_depth_preprocessor(&self) -> bool {
        let ty = crate::depth::preprocess::PREPROCESSOR_TYPE;
        let source_wants = matches!(
            &self.source,
            DeckSource::Shader { preprocessor_textures, .. }
                if preprocessor_textures.iter().any(|s| s.analyzer_type == ty)
        );
        source_wants
            || self.effects.iter().any(|e| {
                e.preprocessor_textures
                    .iter()
                    .any(|s| s.analyzer_type == ty)
            })
    }

    /// Drop the depth preprocessor if nothing on this deck consumes it any more,
    /// returning the sensor ID the caller must release on the manager.
    ///
    /// Called after removing an effect: if that effect was the only consumer,
    /// holding the device open would keep a capture thread and three GPU passes
    /// alive for nothing.
    pub fn detach_depth_preprocessor_if_unused(&mut self) -> Option<crate::depth::DepthSensorId> {
        if self.depth_prepro.is_none() || self.wants_depth_preprocessor() {
            return None;
        }
        self.depth_prepro.take().map(|s| s.sensor_id)
    }

    /// Sensor IDs this deck holds ref-counted references to, for release on
    /// removal. Covers both point-cloud depth sources and shader preprocessors.
    pub fn held_depth_sensors(&self) -> Vec<crate::depth::DepthSensorId> {
        let mut ids = Vec::new();
        if let Some(id) = self
            .external_source_kind()
            .and_then(|k| k.depth_sensor_id())
        {
            ids.push(id);
        }
        if let Some(state) = &self.depth_prepro {
            ids.push(state.sensor_id);
        }
        ids
    }

    /// The GPU error that quarantined this deck, if any.
    pub fn gpu_error(&self) -> Option<&str> {
        self.gpu_error.as_deref()
    }

    /// Lift the quarantine and let the deck render again.
    ///
    /// Called on shader hot-reload: the author has just changed the source, so
    /// the thing that failed may no longer exist. Without this a single bad save
    /// would black out the deck until the app restarts.
    pub fn clear_gpu_error(&mut self) {
        if self.gpu_error.take().is_some() {
            log::info!("Deck '{}': GPU quarantine lifted", self.uuid);
        }
    }

    /// When a picture of it was last counted as shown (see
    /// `count_frame_shown`), for tests.
    #[cfg(test)]
    pub(crate) fn last_shown(&self) -> std::time::Instant {
        self.last_frame_time
    }

    /// As a GPU fault leaves it (see `gpu_error`), for tests.
    #[cfg(test)]
    pub(crate) fn fault(&mut self, message: &str) {
        self.gpu_error = Some(message.to_string());
    }

    /// Resolution this deck renders at, relative to the master.
    pub fn render_scale(&self) -> DeckRenderScale {
        self.render_scale
    }

    /// Render at `scale` of the master resolution from now on.
    pub fn set_render_scale(
        &mut self,
        context: &crate::renderer::GpuContext,
        scale: DeckRenderScale,
    ) {
        if scale == self.render_scale {
            return;
        }
        self.render_scale = scale;
        let (width, height) = self.master_size;
        self.resize(context, width, height);
    }

    /// Frames this deck has actually rendered — a still image, and a lower
    /// third or a text standing still, skip the ones where nothing they
    /// depend on changed (see [`Deck::still_frame`]).
    pub fn rendered_frames(&self) -> u32 {
        self.frame_count
    }

    /// Whether a lower third or a text standing still keeps the frame it
    /// has (see [`Deck::still_frame`]); its channel says each frame.
    pub(crate) fn set_keep_still_text(&mut self, keep: bool) {
        self.keep_still_text = keep;
    }

    /// Whether a clip that gives no new frame keeps the one it has (see
    /// [`Deck::still_frame`]); its channel says each frame.
    pub(crate) fn set_keep_still_clips(&mut self, keep: bool) {
        self.keep_still_clips = keep;
    }

    /// Whether the frame drawn of a live source keeps while the source's
    /// picture is the same (see [`Deck::still_frame`]); its channel says
    /// each frame.
    pub(crate) fn set_keep_still_sources(&mut self, keep: bool) {
        self.keep_still_sources = keep;
    }

    /// Standing still for as long as it is left alone — a still image, a
    /// lower third or a text not moving — not a clip between two of its
    /// frames, kept only till the next (see [`Deck::still_frame`]): for the
    /// UI, which gives such a deck no rate.
    pub(crate) fn is_still(&self, modulation: &crate::modulation::ModulationEngine) -> bool {
        self.still_frame(modulation)
            .is_some_and(|still| !still.in_parts())
    }

    /// A number no other deck has, whatever its UUID (see `instance`).
    pub(crate) fn instance(&self) -> u64 {
        self.instance
    }

    /// Frames of its clip put in its texture so far (tests).
    #[cfg(test)]
    pub(crate) fn clip_frames(&self) -> u64 {
        self.frames_uploaded
    }

    /// Whether this deck preserves source alpha (transparent compositing).
    pub fn transparent(&self) -> bool {
        self.transparent
    }

    /// Whether this deck draws its live source onto its frame one to one:
    /// the source's frame exactly its own, with no bars and nothing cut off —
    /// as a tap does of a picture the master's size, whatever its fit.
    pub(crate) fn maps_source_one_to_one(&self) -> bool {
        let DeckSource::ExternalSource {
            source_width,
            source_height,
            scaling_mode,
            ..
        } = &self.source
        else {
            return false;
        };
        let (scale, offset) = scaling_mode.compute_uv_transform(
            *source_width,
            *source_height,
            self.master_size.0,
            self.master_size.1,
        );
        scale.iter().all(|s| (s - 1.0).abs() < 1e-6) && offset.iter().all(|o| o.abs() < 1e-6)
    }

    /// Whether this deck's picture is opaque over the whole frame, so that at
    /// full opacity in Normal blend nothing composited below it can show.
    ///
    /// The channel skips those hidden decks — at 4K each cost a full-frame
    /// copy and blend (~330 MB of GPU traffic) for pixels nobody sees. A wrong
    /// "yes" would make layers vanish, so only what is known is trusted:
    /// raster images with no transparent pixel, opaque solid colours, and
    /// live sources flattened to opaque; carrying no effects other than crop
    /// and transform at their neutral settings, with nothing modulating them.
    /// Letterbox bars from Fit and Center are transparent, so an image scaled
    /// that way covers only when its picture reaches every edge.
    /// What this deck's picture depends on, if it is a still image that only
    /// changes when that does; `None` for anything that may change by itself.
    ///
    /// A slide stack re-blitted every image at the full render size each
    /// frame — at 4K about 100 MB of GPU traffic per slide — though a PNG never
    /// changes. Only raster/vector images qualify, carrying no effects but
    /// crop and transform with nothing modulating them (both are pure
    /// functions of their settings), and no analyzer attached.
    ///
    /// A lower third and a text qualify the same way while they stand still
    /// — on air or off, shown or hidden; not coming in, going out or fading
    /// — and `keep_still_text` is on: a name on air was drawn again every
    /// frame, a frame cleared and its plate placed, though nothing of it
    /// moved. What they show then is their drawing as placed (see
    /// `LowerThird::still`, `Text::still`): another drawing, another look
    /// or place, and the frame is drawn again.
    ///
    /// A clip qualifies from one of its frames to the next — paused, held
    /// on its last, or 25 frames a second in a show of 60 — while
    /// `keep_still_clips` is on: its frame was drawn again every show
    /// frame, though the clip had given no new one. What it shows then is
    /// the frame it has, as fitted, in the parts drawn (see
    /// `StillFrame::in_parts`): a new frame — played, stepped, sought — or
    /// more of it showing, and it is drawn again. Not a HAP clip, drawn
    /// another way.
    ///
    /// A live source whose picture is told — another input shown through a
    /// tap, a screen or window captured (see [`Deck::source_picture`]) —
    /// qualifies from one of its pictures to the next, while
    /// `keep_still_sources` is on: a deck that draws its own frame of one
    /// (its thumbnail on screen, a blend or a size the composite does not
    /// draw straight from the source) drew it again every show frame,
    /// though the source stood still — and its input, seeing a new frame of
    /// it each time, was put together again with it. What it shows then is
    /// that picture, as it is placed, fitted and read, in the parts drawn:
    /// another picture, another size or fit, or more of it showing, and it
    /// is drawn again — the very frame the source has the new picture,
    /// never one behind. One whose picture is not told — a camera, a
    /// stream, Main — or that has nothing to draw from yet is drawn every
    /// frame, as before; and a depth sensor, whose picture is made here.
    pub(crate) fn still_frame(
        &self,
        modulation: &crate::modulation::ModulationEngine,
    ) -> Option<StillFrame> {
        let source = match &self.source {
            DeckSource::Image { scaling_mode, .. } => StillSource::Image(*scaling_mode),
            DeckSource::LowerThird(lower) if self.keep_still_text => {
                StillSource::Drawing(lower.still()?)
            }
            DeckSource::Text(text) if self.keep_still_text => StillSource::Drawing(text.still()?),
            DeckSource::Video { scaling_mode, .. } if self.keep_still_clips => StillSource::Clip {
                frame: self.frames_uploaded,
                scaling_mode: *scaling_mode,
                visible: self.visible.clone(),
            },
            // Not a depth sensor: its picture is made here, of more than
            // the one view, and is not the same for a number's sake.
            DeckSource::ExternalSource {
                kind,
                source_width,
                source_height,
                scaling_mode,
                ..
            } if self.keep_still_sources
                && !matches!(kind, ExternalSourceKind::DepthSensor(_)) =>
            {
                // Nothing to draw from yet: the frame drawn now is not the
                // picture's, whatever its number.
                self.external_source_view.as_ref()?;
                StillSource::Live {
                    picture: self.source_picture()?,
                    placed: placed_size(
                        *scaling_mode,
                        (*source_width, *source_height),
                        self.screen_capture.as_ref(),
                    ),
                    scaling_mode: *scaling_mode,
                    transparent: self.transparent,
                    coded: self.external_source_coded_view.is_some(),
                    visible: self.visible.clone(),
                }
            }
            _ => return None,
        };
        if self.analyzers.has_active_instances() {
            return None;
        }
        let mut effects = Vec::with_capacity(self.effects.len());
        for effect in &self.effects {
            // An offline effect is kept, not run: it changes nothing.
            if effect.enabled && effect.offline().is_none() && !effect.is_static_framing(modulation)
            {
                return None;
            }
            let values = effect
                .params
                .param_order
                .iter()
                .flat_map(|name| param_bits(effect.params.values.get(name)))
                .collect();
            effects.push((effect.uuid().to_string(), effect.enabled, values));
        }
        Some(StillFrame {
            size: (self.texture.width(), self.texture.height()),
            source,
            effects,
        })
    }

    /// Nothing of this deck shows, and nothing will until it is told to
    /// show: a lower third that is off or a text hidden and faded out,
    /// with no effect that could draw on the clear frame (an analyzer
    /// feeding one counts). Its frame, drawn now, is clear — and so is its
    /// channel's picture when every deck there is such (see
    /// `Channel::blank`).
    pub(crate) fn shows_nothing(&self, modulation: &crate::modulation::ModulationEngine) -> bool {
        let off = match &self.source {
            DeckSource::LowerThird(lower) => lower.is_off(),
            DeckSource::Text(text) => text.is_off(),
            _ => false,
        };
        off && self.gpu_error.is_none()
            && !self.analyzers.has_active_instances()
            && self.effects.iter().all(|effect| {
                !effect.enabled || effect.offline().is_some() || effect.keeps_clear(modulation)
            })
    }

    pub fn covers_frame(&self, modulation: &crate::modulation::ModulationEngine) -> bool {
        let source_opaque = match &self.source {
            DeckSource::Image {
                opaque,
                source_width,
                source_height,
                scaling_mode,
                ..
            } => {
                *opaque
                    && fills_the_frame(
                        *scaling_mode,
                        (*source_width, *source_height),
                        self.master_size,
                    )
            }
            DeckSource::SolidColor { color } => color[3] >= 1.0,
            DeckSource::ExternalSource { .. } => !self.transparent,
            _ => false,
        };
        // An offline effect is kept, not run: it changes nothing.
        source_opaque
            && self.effects.iter().all(|effect| {
                !effect.enabled
                    || effect.offline().is_some()
                    || effect.is_neutral_framing(modulation)
            })
    }

    /// Where this deck's picture is certain to be opaque, in UV of the frame:
    /// an opaque image where it is fitted, an opaque colour or camera
    /// everywhere, then cut by its crop and moved by its transform, less a
    /// margin for the soft edges those leave. `None` when that cannot be
    /// told — a clip, a generator, anything but framing among its effects,
    /// framing that is modulated, rotated or wrapped. A deck over another
    /// hides that much of it.
    pub fn opaque_area(
        &self,
        modulation: &crate::modulation::ModulationEngine,
    ) -> Option<crate::renderer::region::Area> {
        use crate::renderer::region::Area;
        // Opaque past the frame's edges, where nothing can blend in.
        const EVERYWHERE: Area = Area {
            x0: -1.0,
            y0: -1.0,
            x1: 2.0,
            y1: 2.0,
        };
        let (width, height) = self.master_size;
        let mut area = match &self.source {
            DeckSource::Image {
                opaque: true,
                source_width,
                source_height,
                scaling_mode,
                ..
            } if *source_width > 0 && *source_height > 0 && width > 0 && height > 0 => {
                // The blit reads source UV `uv · scale + offset`; the picture
                // is where that falls inside the image, cut sharp.
                let (scale, offset) =
                    scaling_mode.compute_uv_transform(*source_width, *source_height, width, height);
                let lo = |v: f32| if v <= 0.0 { -1.0 } else { v };
                let hi = |v: f32| if v >= 1.0 { 2.0 } else { v };
                Area {
                    x0: lo(-offset[0] / scale[0]),
                    y0: lo(-offset[1] / scale[1]),
                    x1: hi((1.0 - offset[0]) / scale[0]),
                    y1: hi((1.0 - offset[1]) / scale[1]),
                }
            }
            DeckSource::SolidColor { color } if color[3] >= 1.0 => EVERYWHERE,
            // Drawn over black unless flagged transparent: opaque everywhere.
            DeckSource::ExternalSource { kind, .. }
                if !self.transparent && !matches!(kind, ExternalSourceKind::DepthSensor(_)) =>
            {
                EVERYWHERE
            }
            _ => return None,
        };
        let aspect = width as f32 / height.max(1) as f32;
        // Texels an edge inside the frame may be soft by; a pixel more for
        // the composite's own sampling of the texture.
        let mut fringe = 0.0;
        for effect in &self.effects {
            // An offline effect is kept, not run: it changes nothing.
            if effect.enabled
                && effect.offline().is_none()
                && !effect.is_neutral_framing(modulation)
            {
                area = effect.opaque_after(area, aspect, modulation, &mut fringe)?;
            }
        }
        let margin = fringe + 1.0;
        let area = area
            .shrunk(
                margin / self.texture.width().max(1) as f32,
                margin / self.texture.height().max(1) as f32,
            )
            .intersect(&Area::FRAME);
        (!area.is_empty()).then_some(area)
    }

    /// Limit this deck to the parts of the frame that show of it, or `None`
    /// for all of it. Its channel sets this each frame before rendering.
    pub(crate) fn set_visible(&mut self, visible: Option<Vec<crate::renderer::region::Area>>) {
        self.visible = visible;
    }

    /// The share of the frame this deck's source drew in its last render: 1
    /// for all of it, less when parts covered on air were left out.
    pub fn drawn_share(&self) -> f32 {
        self.drawn_share
    }

    /// Whether this frame shows a part of the deck its last render did not
    /// draw — its texture is stale there, so the frame must not be skipped.
    pub(crate) fn shows_undrawn_part(&self) -> bool {
        match (&self.drawn_visible, &self.visible) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(drawn), Some(now)) => drawn != now,
        }
    }

    /// Set whether this deck preserves source alpha (transparent compositing).
    pub fn set_transparent(&mut self, transparent: bool) {
        self.transparent = transparent;
    }

    /// Get the external source kind (if source is external)
    pub fn external_source_kind(&self) -> Option<ExternalSourceKind> {
        match &self.source {
            DeckSource::ExternalSource { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// Get the NDI receiver index (if source is NDI)
    pub fn ndi_receiver_idx(&self) -> Option<usize> {
        match &self.source {
            DeckSource::ExternalSource {
                kind: ExternalSourceKind::Ndi(idx),
                ..
            } => Some(*idx),
            _ => None,
        }
    }

    /// Get the Syphon client index (if source is Syphon)
    pub fn syphon_client_idx(&self) -> Option<usize> {
        match &self.source {
            DeckSource::ExternalSource {
                kind: ExternalSourceKind::Syphon(idx),
                ..
            } => Some(*idx),
            _ => None,
        }
    }

    /// Get the SRT/HLS/DASH/RTMP receiver index (if source is a stream)
    pub fn srt_receiver_idx(&self) -> Option<usize> {
        match &self.source {
            DeckSource::ExternalSource {
                kind: ExternalSourceKind::Srt(idx),
                ..
            }
            | DeckSource::ExternalSource {
                kind: ExternalSourceKind::Hls(idx),
                ..
            }
            | DeckSource::ExternalSource {
                kind: ExternalSourceKind::Dash(idx),
                ..
            }
            | DeckSource::ExternalSource {
                kind: ExternalSourceKind::Rtmp(idx),
                ..
            } => Some(*idx),
            _ => None,
        }
    }

    /// Get the camera ID (if source is a camera)
    /// The camera a deck waiting for its device should open, by name: `None`
    /// unless the deck's camera is [`crate::camera::UNBOUND_CAMERA`].
    pub fn camera_waiting_for(&self) -> Option<&str> {
        (self.camera_id() == Some(crate::camera::UNBOUND_CAMERA))
            .then(|| self.source_name.trim_start_matches("📹 "))
    }

    /// Point a camera deck at an opened camera, `size` its picture.
    pub fn bind_camera(&mut self, id: crate::camera::CameraId, size: (u32, u32)) {
        if let DeckSource::ExternalSource {
            kind: kind @ ExternalSourceKind::Camera(_),
            source_width,
            source_height,
            ..
        } = &mut self.source
        {
            *kind = ExternalSourceKind::Camera(id);
            (*source_width, *source_height) = size;
        }
    }

    pub fn camera_id(&self) -> Option<crate::camera::CameraId> {
        match &self.source {
            DeckSource::ExternalSource {
                kind: ExternalSourceKind::Camera(id),
                ..
            } => Some(*id),
            _ => None,
        }
    }

    /// Whether this input's capture is drawn from its code values (see
    /// `external_source_coded_view`): a screen's or a window's is. A
    /// PowerPoint show's is left as it was, drawn from its light — it is
    /// its presentation's, as its size is — and so is what is no capture.
    pub fn draws_its_capture_coded(&self) -> bool {
        self.screen_capture
            .as_ref()
            .is_some_and(|state| state.presentation.is_none())
    }

    /// Get the screen-capture ID (if source is a screen capture)
    pub fn screen_capture_id(&self) -> Option<crate::screen_capture::CaptureId> {
        match &self.source {
            DeckSource::ExternalSource {
                kind: ExternalSourceKind::ScreenCapture(id),
                ..
            } => Some(*id),
            _ => None,
        }
    }

    /// A screen capture capped at the show's new size `width × height`, as it
    /// is when added; one added to a 1080p show stayed at 1080p after the
    /// show went to 4K. Its capture, which inputs on the same screen share,
    /// is the caller's to cap (`ScreenCaptureManager::cap_to`), once and with
    /// nothing else changed: sending this input's whole settings would put
    /// back another's crop. Gives that capture, if any. A PowerPoint show's
    /// capture is left to the presentation, whose cap is the picture its
    /// slide is shown at.
    pub fn cap_capture_to_show(
        &mut self,
        width: u32,
        height: u32,
    ) -> Option<crate::screen_capture::CaptureId> {
        let state = self.screen_capture.as_mut()?;
        if state.presentation.is_some() {
            return None;
        }
        state.config.scale_to = Some((width, height));
        self.screen_capture_id()
            .filter(|id| *id != crate::screen_capture::UNBOUND_CAPTURE_ID)
    }

    /// Point a screen-capture deck at another capture session — its window
    /// has come (a PowerPoint show started) or is a new one — and give back
    /// the session it held.
    pub fn rebind_screen_capture(
        &mut self,
        capture_id: crate::screen_capture::CaptureId,
        identity: crate::screen_capture::backend::TargetIdentity,
        config: crate::screen_capture::backend::CaptureConfig,
        width: u32,
        height: u32,
    ) -> Option<crate::screen_capture::CaptureId> {
        let state = self.screen_capture.as_mut()?;
        let old = state.capture_id;
        state.capture_id = capture_id;
        state.identity = identity;
        state.config = config;
        // What it was given was for the one it held; this one runs as it
        // is set.
        state.given.clear();
        if let DeckSource::ExternalSource {
            kind: ExternalSourceKind::ScreenCapture(id),
            source_width,
            source_height,
            ..
        } = &mut self.source
        {
            *id = capture_id;
            *source_width = width;
            *source_height = height;
        }
        Some(old)
    }

    /// Push the live source dimensions of an external source down from its
    /// manager. Sources whose resolution can change mid-session (a screen
    /// capture being cropped, a stream reconnecting at a new size) need this or
    /// the blit keeps letterboxing to a stale aspect ratio.
    pub fn set_external_source_size(&mut self, width: u32, height: u32) {
        if let DeckSource::ExternalSource {
            source_width,
            source_height,
            ..
        } = &mut self.source
        {
            *source_width = width;
            *source_height = height;
        }
    }

    /// Get the shader (if source is a shader or compute shader)
    pub fn shader(&self) -> Option<&ISFShader> {
        match &self.source {
            DeckSource::Shader { shader, .. } | DeckSource::ComputeShader { shader, .. } => {
                Some(shader)
            }
            _ => None,
        }
    }

    /// Set the fixed time step used for the TIME uniform.
    /// Called by the channel to keep `render_dt` in sync with the target FPS.
    pub fn set_render_dt(&mut self, dt: f32) {
        self.render_dt = dt;
    }

    /// Get the smoothed FPS derived from actual render pipeline timing
    pub fn fps(&self) -> f32 {
        self.fps_smoothed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a deck keeps of its screen capture `id`, for a show of `cap`.
    fn capture_state(
        id: crate::screen_capture::CaptureId,
        cap: (u32, u32),
        presentation: bool,
    ) -> ScreenCaptureState {
        ScreenCaptureState {
            capture_id: id,
            identity: crate::screen_capture::backend::TargetIdentity::Display {
                label: "Display 1".into(),
                uuid: None,
            },
            config: crate::screen_capture::backend::CaptureConfig {
                scale_to: Some(cap),
                ..Default::default()
            },
            given: Vec::new(),
            listed_as: None,
            presentation: presentation.then(|| crate::presentation::PresentationConfig {
                path: "/talks/a.pptx".into(),
                slide: None,
                plays_on: crate::engine::value::presentation::PlaysOn::default(),
            }),
        }
    }

    /// A screen capture `id` for a show of `cap`.
    fn capture_deck(
        gpu: &crate::renderer::GpuContext,
        id: crate::screen_capture::CaptureId,
        cap: (u32, u32),
        presentation: bool,
    ) -> Deck {
        Deck::new_from_screen_capture(
            gpu,
            capture_state(id, cap, presentation),
            "Display 1",
            cap.0,
            cap.1,
            cap.0,
            cap.1,
        )
        .expect("a capture deck")
    }

    /// An input that fills the frame wants its capture filling it, one
    /// zoomed into wants it that many times bigger — by the side zoomed
    /// into the more, where the picture keeps its shape; stretched, each
    /// side by its own. Never less than fitted; centred, as one fitted.
    #[test]
    fn a_capture_is_drawn_as_its_input_fits_and_zooms_it() {
        use crate::screen_capture::backend::Drawn;
        use ScalingMode::{Center, Fill, Fit, Stretch};
        let drawn = |fitted, across, down| Drawn {
            fitted,
            across,
            down,
        };
        assert_eq!(capture_drawn_as(Fit, [1.0; 2]), drawn(1.0, 0.0, 0.0));
        assert_eq!(capture_drawn_as(Fit, [1.0; 2]), Drawn::default());
        assert_eq!(capture_drawn_as(Fit, [2.5; 2]), drawn(2.5, 0.0, 0.0));
        assert_eq!(capture_drawn_as(Fill, [1.0; 2]), drawn(1.0, 1.0, 1.0));
        assert_eq!(capture_drawn_as(Fill, [3.0; 2]), drawn(1.0, 3.0, 3.0));
        assert_eq!(capture_drawn_as(Stretch, [2.0; 2]), drawn(1.0, 2.0, 2.0));
        assert_eq!(capture_drawn_as(Center, [1.0; 2]), Drawn::default());
        assert_eq!(capture_drawn_as(Center, [4.0; 2]), drawn(4.0, 0.0, 0.0));
        // Scaled apart: a picture keeping its shape, by the bigger; one
        // stretched, each side by its own.
        assert_eq!(capture_drawn_as(Fit, [1.5, 0.5]), drawn(1.5, 0.0, 0.0));
        assert_eq!(capture_drawn_as(Fit, [0.5, 3.0]), drawn(3.0, 0.0, 0.0));
        assert_eq!(capture_drawn_as(Fill, [2.0, 0.5]), drawn(1.0, 2.0, 2.0));
        assert_eq!(capture_drawn_as(Fill, [0.5, 2.0]), drawn(1.0, 2.0, 2.0));
        assert_eq!(capture_drawn_as(Stretch, [2.0, 0.5]), drawn(1.0, 2.0, 0.5));
        assert_eq!(capture_drawn_as(Stretch, [0.5, 2.0]), drawn(1.0, 0.5, 2.0));
        // Shrunk: no less than fitted, which its `fitted` of 1 keeps.
        assert_eq!(capture_drawn_as(Fit, [0.3; 2]), drawn(1.0, 0.0, 0.0));
        assert_eq!(capture_drawn_as(Center, [0.3; 2]), drawn(1.0, 0.0, 0.0));
        assert_eq!(capture_drawn_as(Fill, [0.3; 2]), drawn(1.0, 0.3, 0.3));
        assert_eq!(capture_drawn_as(Stretch, [0.3; 2]), drawn(1.0, 0.3, 0.3));
        // No size at all, a side or both: that side as not zoomed.
        assert_eq!(capture_drawn_as(Fit, [f32::NAN; 2]), drawn(1.0, 0.0, 0.0));
        assert_eq!(
            capture_drawn_as(Fit, [f32::INFINITY, 2.0]),
            drawn(2.0, 0.0, 0.0)
        );
        assert_eq!(capture_drawn_as(Fill, [f32::NAN, 2.0]), drawn(1.0, 2.0, 2.0));
        assert_eq!(
            capture_drawn_as(Stretch, [f32::NAN, f32::INFINITY]),
            drawn(1.0, 1.0, 1.0)
        );
        assert_eq!(
            capture_drawn_as(Stretch, [2.0, f32::NAN]),
            drawn(1.0, 2.0, 1.0)
        );
    }

    /// A deck's capture is drawn as the deck shows it: by its scaling mode,
    /// and its Transform's scale where the pass drawing the capture does it
    /// — first of its effects (after a crop), not turned, the deck's frames
    /// not read by an analyzer.
    #[test]
    fn a_decks_capture_is_drawn_by_its_mode_and_its_folded_zoom() {
        use crate::params::ParamValue::{Bool, Float};
        use crate::screen_capture::backend::Drawn;
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let modulation = crate::modulation::ModulationEngine::new();
        let apart = |fitted, across, down| {
            Some((
                7,
                Drawn {
                    fitted,
                    across,
                    down,
                },
            ))
        };
        let drawn = |fitted, fills| apart(fitted, fills, fills);
        let effect = |name: &str| {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("shaders")
                .join(format!("{name}.fs"));
            let shader = crate::isf::ISFShader::from_file(path).expect("the shader");
            Effect::new(&gpu, shader).expect("an effect")
        };
        let mut deck = capture_deck(&gpu, 7, (1920, 1080), false);
        assert_eq!(deck.scaling_mode(), Some(ScalingMode::Fill), "as added");
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 1.0));
        deck.set_scaling_mode(ScalingMode::Fit);
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 0.0));

        // Its crop and Transform, as every input has them: neutral, nothing.
        deck.add_effect(effect("crop"));
        deck.add_effect(effect("transform"));
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 0.0));
        deck.effects[1].params.set("scale", Float(2.0));
        assert_eq!(deck.capture_drawn(&modulation), drawn(2.0, 0.0), "zoomed");
        deck.effects[0].params.set("crop_left", Float(0.25));
        assert_eq!(deck.capture_drawn(&modulation), drawn(2.0, 0.0), "cropped too");
        deck.set_scaling_mode(ScalingMode::Fill);
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 2.0));
        deck.set_scaling_mode(ScalingMode::Center);
        assert_eq!(deck.capture_drawn(&modulation), drawn(2.0, 0.0), "as one fitted");
        deck.set_scaling_mode(ScalingMode::Fit);

        // Scaled apart: the wider of the two, the picture keeping its shape;
        // each side by its own, stretched.
        deck.effects[1].params.set("scale_mode", Bool(true));
        deck.effects[1].params.set("scale_x", Float(1.5));
        deck.effects[1].params.set("scale_y", Float(0.5));
        assert_eq!(deck.capture_drawn(&modulation), drawn(3.0, 0.0));
        deck.set_scaling_mode(ScalingMode::Fill);
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 3.0));
        deck.set_scaling_mode(ScalingMode::Stretch);
        assert_eq!(deck.capture_drawn(&modulation), apart(1.0, 3.0, 1.0));
        deck.set_scaling_mode(ScalingMode::Fit);
        deck.effects[1].params.set("scale_mode", Bool(false));

        // Shrunk into a corner: as fitted.
        deck.effects[1].params.set("scale", Float(0.4));
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 0.0), "shrunk");
        deck.set_scaling_mode(ScalingMode::Fill);
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 0.4), "shrunk");
        deck.set_scaling_mode(ScalingMode::Fit);
        deck.effects[1].params.set("scale", Float(2.0));

        // Its frames read by an analyzer, the deck draws every pass apart
        // (nothing is folded): the capture is drawn into its frame fitted.
        let reads = deck.analyzers.request(
            "brightness",
            &crate::analyzer::default_registry(),
            &serde_json::Value::Null,
        );
        assert!(reads.is_some(), "an analyzer running");
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 0.0), "read");
        deck.analyzers.release("brightness");
        assert_eq!(deck.capture_drawn(&modulation), drawn(2.0, 0.0), "no more");

        // Turned, the Transform is a pass of its own on the deck's frame:
        // the capture is drawn into that fitted.
        deck.effects[1].params.set("rotation", Float(0.5));
        assert_eq!(deck.capture_drawn(&modulation), drawn(1.0, 0.0), "turned");
        deck.effects[1].params.set("rotation", Float(0.0));
        assert_eq!(deck.capture_drawn(&modulation), drawn(2.0, 0.0));

        // A second Transform after it is such a pass too: only the first
        // is the capture's.
        deck.add_effect(effect("transform"));
        deck.effects[2].params.set("scale", Float(4.0));
        assert_eq!(deck.capture_drawn(&modulation), drawn(2.0, 0.0), "the first");
        // And with the first off, the second comes first.
        deck.effects[1].enabled = false;
        deck.effects[0].enabled = false;
        assert_eq!(deck.capture_drawn(&modulation), drawn(4.0, 0.0));
    }

    /// A deck draws its live source from the view that reads it as coded
    /// where it has one beside its view — and says so — else from its view,
    /// as light; with no view it draws nothing, whatever else it has.
    #[test]
    fn a_source_is_read_as_coded_where_its_deck_has_that_view_too() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        if !gpu.reads_a_texture_two_ways() {
            eprintln!("Skipping: this GPU reads a texture one way");
            return;
        }
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("a source"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });
        let light = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let coded = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        assert_ne!(light, coded);
        assert_eq!(source_read(Some(&light), Some(&coded)), Some((&coded, true)));
        assert_eq!(source_read(Some(&light), None), Some((&light, false)));
        assert_eq!(source_read(None, Some(&coded)), None);
        assert_eq!(source_read(None, None), None);

        // And so a deck is framed for what draws it from its source in its
        // stead — a Merge, its channel drawing it straight onto its
        // picture: the view and how it is read, the two together.
        let modulation = crate::modulation::ModulationEngine::new();
        let mut deck = capture_deck(&gpu, 7, (1920, 1080), false);
        deck.external_source_view = Some(light.clone());
        let framing = deck.framing(&modulation).expect("a framing");
        assert_eq!(framing.source, Some(&light));
        assert!(!framing.placed.coded, "read as light");
        deck.external_source_coded_view = Some(coded.clone());
        let framing = deck.framing(&modulation).expect("a framing");
        assert_eq!(framing.source, Some(&coded));
        assert!(framing.placed.coded, "read as coded");
        deck.external_source_view = None;
        let framing = deck.framing(&modulation).expect("a framing");
        assert_eq!(framing.source, None, "nothing to draw yet");
        assert!(!framing.placed.coded);
    }

    /// A PowerPoint show's capture is sized by its presentation, and one
    /// not there by nothing: neither is an input's to size, nor anything
    /// that is no capture.
    #[test]
    fn a_shows_capture_and_one_not_there_are_no_inputs_to_size() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let modulation = crate::modulation::ModulationEngine::new();
        let live = capture_deck(&gpu, 7, (1920, 1080), false);
        assert!(live.capture_drawn(&modulation).is_some(), "a capture's is");
        let show = capture_deck(&gpu, 8, (1920, 1080), true);
        assert_eq!(show.capture_drawn(&modulation), None, "its presentation's");
        let gone = capture_deck(
            &gpu,
            crate::screen_capture::UNBOUND_CAPTURE_ID,
            (1920, 1080),
            false,
        );
        assert_eq!(gone.capture_drawn(&modulation), None, "no capture");
        let colour = Deck::new_solid_color(&gpu, [1.0; 4], 64, 36).expect("a colour");
        assert_eq!(colour.capture_drawn(&modulation), None, "not a capture");
    }

    /// A screen's or a window's capture is drawn from its code values; a
    /// PowerPoint show's is left as it was, and so is what is no capture.
    #[test]
    fn a_shows_capture_is_drawn_from_its_light() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let live = capture_deck(&gpu, 7, (1920, 1080), false);
        assert!(live.draws_its_capture_coded(), "a capture's is");
        let show = capture_deck(&gpu, 8, (1920, 1080), true);
        assert!(!show.draws_its_capture_coded(), "its presentation's");
        let colour = Deck::new_solid_color(&gpu, [1.0; 4], 64, 36).expect("a colour");
        assert!(!colour.draws_its_capture_coded(), "not a capture");
    }

    /// A capture shown centred is placed as it comes fitted within the
    /// show's size, however big it comes for another input of its screen
    /// zoomed into: the same picture, where the deck draws it, where a Merge
    /// reads its framing, where it is told opaque and where the panel draws
    /// its outline. Any other mode goes by the picture as it comes, and so
    /// does a capture with no size to come within, or a PowerPoint show's.
    #[test]
    fn a_centred_capture_is_placed_as_it_comes_fitted() {
        use ScalingMode::{Center, Fill, Fit, Stretch};
        let show = (1920, 1080);
        let state = capture_state(7, show, false);
        let placed = |mode, source| placed_size(mode, source, Some(&state));
        assert_eq!(placed(Center, (1920, 1080)), (1920, 1080), "as it comes");
        assert_eq!(placed(Center, (3840, 2160)), (1920, 1080), "come bigger");
        assert_eq!(placed(Center, (2880, 1620)), (1920, 1080));
        assert_eq!(placed(Center, (3456, 2160)), (1728, 1080), "another shape");
        assert_eq!(placed(Center, (2160, 3840)), (608, 1080), "upright");
        assert_eq!(placed(Center, (800, 600)), (800, 600), "smaller: its own");
        assert_eq!(placed(Center, (0, 0)), (0, 0), "no frame yet");
        for mode in [Fit, Fill, Stretch] {
            assert_eq!(placed(mode, (3840, 2160)), (3840, 2160), "{mode:?}");
        }
        let mut unsized_one = capture_state(7, show, false);
        unsized_one.config.scale_to = None;
        assert_eq!(
            placed_size(Center, (3840, 2160), Some(&unsized_one)),
            (3840, 2160),
            "nothing to come within"
        );
        let slides = capture_state(8, show, true);
        assert_eq!(
            placed_size(Center, (3840, 2160), Some(&slides)),
            (3840, 2160),
            "a PowerPoint show's"
        );
        assert_eq!(
            placed_size(Center, (3840, 2160), None),
            (3840, 2160),
            "a camera"
        );

        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let modulation = crate::modulation::ModulationEngine::new();
        let mut deck = capture_deck(&gpu, 7, show, false);
        deck.set_scaling_mode(Center);
        deck.set_external_source_size(1920, 1080);
        let fitted = deck.framing(&modulation).expect("a framing").placed;
        assert_eq!(fitted.uv_scale, [1.0, 1.0], "the whole picture");
        assert_eq!(fitted.uv_offset, [0.0, 0.0]);
        assert_eq!(fitted.over_black, 0.0, "filling the frame");
        assert_eq!(deck.source_size(), Some([1920, 1080]));

        // Another input of the screen zoomed into: it comes twice the size.
        deck.set_external_source_size(3840, 2160);
        let framing = deck.framing(&modulation).expect("a framing");
        assert_eq!(framing.placed, fitted, "placed as it was");
        assert_eq!(framing.source_size, (3840, 2160), "the pixels it has");
        assert_eq!(deck.source_size(), Some([1920, 1080]), "its outline too");

        // A screen of another shape leaves bars at the sides, and is drawn
        // over black — still, when it comes bigger than the frame.
        deck.set_external_source_size(1728, 1080);
        let barred = deck.framing(&modulation).expect("a framing").placed;
        assert_eq!(barred.over_black, 1.0, "bars at its sides");
        deck.set_external_source_size(3456, 2160);
        let framing = deck.framing(&modulation).expect("a framing");
        assert_eq!(framing.placed, barred, "placed as it was, bars and all");
        assert_eq!(deck.source_size(), Some([1728, 1080]));

        // Fitted, it goes by the picture as it comes.
        deck.set_scaling_mode(Fit);
        assert_eq!(deck.source_size(), Some([3456, 2160]));
    }

    /// A capture setting given a deck is set on it and kept to be given its
    /// capture, in the order given; a name that is no setting's is refused
    /// and nothing kept. Pointed at another capture it has nothing left to
    /// give: that one runs as it is set.
    #[test]
    fn a_capture_setting_given_is_kept_to_be_sent_down() {
        use crate::screen_capture::backend::CaptureSetting;
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let mut deck = capture_deck(&gpu, 7, (1920, 1080), false);
        assert!(deck.set_capture_param("crop_w", 0.5));
        assert!(deck.set_capture_param("cursor", 1.0));
        assert!(!deck.set_capture_param("scale_to", 0.5), "no setting");
        let state = deck.screen_capture.as_ref().expect("a capture");
        assert!((state.config.crop.w - 0.5).abs() < 1e-6, "set on it");
        assert!(state.config.show_cursor, "set on it");
        let given: Vec<_> = state.given.iter().map(|g| (g.setting, g.value)).collect();
        assert_eq!(
            given,
            [(CaptureSetting::CropW, 0.5), (CaptureSetting::Cursor, 1.0)]
        );
        assert!(state.given[0].at < state.given[1].at, "in the order given");

        let (identity, config) = (state.identity.clone(), state.config.clone());
        assert_eq!(
            deck.rebind_screen_capture(8, identity, config, 1920, 1080),
            Some(7)
        );
        let state = deck.screen_capture.as_ref().expect("a capture");
        assert!(state.given.is_empty(), "nothing of the one it left");
    }

    /// A capture added to a 1080p show is capped at 4K when the show goes to
    /// 4K, and names its capture for the caller to cap — once, nothing else
    /// sent; an unbound one names none; a PowerPoint show's is left to its
    /// presentation.
    #[test]
    fn a_capture_follows_the_shows_size() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let mut deck = capture_deck(&gpu, 7, (1920, 1080), false);
        assert_eq!(deck.cap_capture_to_show(3840, 2160), Some(7));
        let state = deck.screen_capture.as_ref().expect("a capture");
        assert_eq!(state.config.scale_to, Some((3840, 2160)));
        assert!(state.given.is_empty(), "nothing of its settings to send");

        let unbound = crate::screen_capture::UNBOUND_CAPTURE_ID;
        let mut waiting = capture_deck(&gpu, unbound, (1920, 1080), false);
        assert_eq!(waiting.cap_capture_to_show(3840, 2160), None);
        let state = waiting.screen_capture.as_ref().expect("a capture");
        assert_eq!(state.config.scale_to, Some((3840, 2160)), "opened so when it comes");

        let mut show = capture_deck(&gpu, 8, (1920, 1080), true);
        assert_eq!(show.cap_capture_to_show(3840, 2160), None);
        let state = show.screen_capture.as_ref().expect("a capture");
        assert_eq!(state.config.scale_to, Some((1920, 1080)));
    }

    #[test]
    fn generate_short_uuid_format() {
        let id = generate_short_uuid();
        assert_eq!(id.len(), 8, "UUID should be 8 chars");
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "UUID should be hex: {id}"
        );
    }

    #[test]
    fn generate_short_uuid_unique() {
        let ids: Vec<String> = (0..100).map(|_| generate_short_uuid()).collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), 100, "100 UUIDs should all be unique");
    }

    #[test]
    fn scaling_mode_default_is_fill() {
        assert_eq!(ScalingMode::default(), ScalingMode::Fill);
    }

    #[test]
    fn stretch_returns_identity() {
        let (scale, offset) = ScalingMode::Stretch.compute_uv_transform(800, 600, 1920, 1080);
        assert_eq!(scale, [1.0, 1.0]);
        assert_eq!(offset, [0.0, 0.0]);
    }

    #[test]
    fn stretch_same_aspect() {
        let (scale, offset) = ScalingMode::Stretch.compute_uv_transform(1920, 1080, 1920, 1080);
        assert_eq!(scale, [1.0, 1.0]);
        assert_eq!(offset, [0.0, 0.0]);
    }

    #[test]
    fn fill_same_aspect_is_identity() {
        let (scale, offset) = ScalingMode::Fill.compute_uv_transform(1920, 1080, 960, 540);
        assert!((scale[0] - 1.0).abs() < 1e-5);
        assert!((scale[1] - 1.0).abs() < 1e-5);
        assert!((offset[0]).abs() < 1e-5);
        assert!((offset[1]).abs() < 1e-5);
    }

    #[test]
    fn fill_wide_source_crops_horizontal() {
        // Source 2:1, target 1:1 → crop left/right
        let (scale, offset) = ScalingMode::Fill.compute_uv_transform(200, 100, 100, 100);
        assert!(
            (scale[0] - 0.5).abs() < 1e-5,
            "scale_x should be 0.5, got {}",
            scale[0]
        );
        assert!((scale[1] - 1.0).abs() < 1e-5);
        assert!(
            (offset[0] - 0.25).abs() < 1e-5,
            "offset_x should center crop"
        );
        assert!((offset[1]).abs() < 1e-5);
    }

    #[test]
    fn fill_tall_source_crops_vertical() {
        // Source 1:2, target 1:1 → crop top/bottom
        let (scale, offset) = ScalingMode::Fill.compute_uv_transform(100, 200, 100, 100);
        assert!((scale[0] - 1.0).abs() < 1e-5);
        assert!(
            (scale[1] - 0.5).abs() < 1e-5,
            "scale_y should be 0.5, got {}",
            scale[1]
        );
        assert!((offset[0]).abs() < 1e-5);
        assert!(
            (offset[1] - 0.25).abs() < 1e-5,
            "offset_y should center crop"
        );
    }

    #[test]
    fn fit_same_aspect_is_identity() {
        let (scale, offset) = ScalingMode::Fit.compute_uv_transform(1920, 1080, 960, 540);
        assert!((scale[0] - 1.0).abs() < 1e-5);
        assert!((scale[1] - 1.0).abs() < 1e-5);
        assert!((offset[0]).abs() < 1e-5);
        assert!((offset[1]).abs() < 1e-5);
    }

    #[test]
    fn fit_wide_source_letterboxes() {
        // Source 2:1, target 1:1 → letterbox top/bottom
        let (scale, _offset) = ScalingMode::Fit.compute_uv_transform(200, 100, 100, 100);
        assert!((scale[0] - 1.0).abs() < 1e-5);
        assert!((scale[1] - 2.0).abs() < 1e-5, "scale_y={}", scale[1]);
    }

    #[test]
    fn fit_tall_source_pillarboxes() {
        // Source 1:2, target 1:1 → pillarbox left/right
        let (scale, _offset) = ScalingMode::Fit.compute_uv_transform(100, 200, 100, 100);
        assert!((scale[0] - 2.0).abs() < 1e-5, "scale_x={}", scale[0]);
        assert!((scale[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn center_smaller_source() {
        // Source 100x100 in target 200x200 → scale 2.0, offset -0.5
        let (scale, offset) = ScalingMode::Center.compute_uv_transform(100, 100, 200, 200);
        assert!((scale[0] - 2.0).abs() < 1e-5);
        assert!((scale[1] - 2.0).abs() < 1e-5);
        assert!((offset[0] - -0.5).abs() < 1e-5);
        assert!((offset[1] - -0.5).abs() < 1e-5);
    }

    #[test]
    fn center_larger_source() {
        // Source 400x400 in target 200x200 → scale 0.5, offset 0.25
        let (scale, offset) = ScalingMode::Center.compute_uv_transform(400, 400, 200, 200);
        assert!((scale[0] - 0.5).abs() < 1e-5);
        assert!((scale[1] - 0.5).abs() < 1e-5);
        assert!((offset[0] - 0.25).abs() < 1e-5);
        assert!((offset[1] - 0.25).abs() < 1e-5);
    }

    /// The source pixel a target's first one shows, across and down, and
    /// that the picture is drawn a pixel for a pixel: as many of its pixels
    /// across the target as the target has.
    fn first_pixel_shown(mode: ScalingMode, source: (u32, u32), target: (u32, u32)) -> (f32, f32) {
        let (scale, offset) = mode.compute_uv_transform(source.0, source.1, target.0, target.1);
        let (across, down) = (scale[0] * source.0 as f32, scale[1] * source.1 as f32);
        assert!(
            (across - target.0 as f32).abs() < 1e-2 && (down - target.1 as f32).abs() < 1e-2,
            "{mode:?} {source:?} in {target:?}: {across} × {down} of its pixels shown"
        );
        (offset[0] * source.0 as f32, offset[1] * source.1 as f32)
    }

    /// `at` is the pixel `whole`, not between two.
    fn on(at: (f32, f32), whole: (f32, f32)) -> bool {
        (at.0 - whole.0).abs() < 1e-3 && (at.1 - whole.1).abs() < 1e-3
    }

    /// A centred picture begins a whole number of its pixels from the
    /// target's edge, an odd number of pixels wider or narrower too: half
    /// a pixel off, every pixel drawn would be a blend of two. The half
    /// pixel it is moved by is to the right, and down.
    #[test]
    fn a_centred_picture_sits_on_whole_pixels() {
        let cases = [
            ((1499, 827), (3840, 2160), (-1171.0, -667.0)),
            ((1500, 828), (3840, 2160), (-1170.0, -666.0)),
            ((1500, 828), (3841, 2161), (-1171.0, -667.0)),
            ((3841, 2161), (1920, 1080), (960.0, 540.0)),
            ((63, 35), (64, 36), (-1.0, -1.0)),
            ((65, 37), (64, 36), (0.0, 0.0)),
        ];
        for (source, target, first) in cases {
            let at = first_pixel_shown(ScalingMode::Center, source, target);
            assert!(on(at, first), "{source:?} in {target:?}: begins at {at:?}, not {first:?}");
        }
    }

    /// A fitted picture as wide as the target, or as high, is drawn a pixel
    /// for a pixel: its bars are whole pixels, an odd number of pixels
    /// between them too.
    #[test]
    fn a_fitted_picture_as_wide_as_the_target_sits_on_whole_pixels() {
        let cases = [
            ((1920, 1055), (1920, 1080), (0.0, -13.0)),
            ((1920, 1054), (1920, 1080), (0.0, -13.0)),
            ((1001, 1080), (1920, 1080), (-460.0, 0.0)),
            ((64, 35), (64, 36), (0.0, -1.0)),
        ];
        for (source, target, first) in cases {
            let at = first_pixel_shown(ScalingMode::Fit, source, target);
            assert!(on(at, first), "{source:?} in {target:?}: begins at {at:?}, not {first:?}");
        }
    }

    /// A picture filling a target as wide as it is, or as high, is drawn a
    /// pixel for a pixel: what is cut from it is whole pixels.
    #[test]
    fn a_filling_picture_as_wide_as_the_target_sits_on_whole_pixels() {
        let cases = [
            ((2561, 1080), (1920, 1080), (320.0, 0.0)),
            ((1920, 1201), (1920, 1080), (0.0, 60.0)),
            ((1920, 1200), (1920, 1080), (0.0, 60.0)),
            ((64, 39), (64, 36), (0.0, 1.0)),
        ];
        for (source, target, first) in cases {
            let at = first_pixel_shown(ScalingMode::Fill, source, target);
            assert!(on(at, first), "{source:?} in {target:?}: begins at {at:?}, not {first:?}");
        }
    }

    /// A picture a pixel short of its frame does not fill it, however wide
    /// the frame: on whole pixels, the pixel it lacks is a bare column.
    #[test]
    fn a_picture_a_pixel_short_of_a_wide_frame_does_not_fill_it() {
        let frame = (16384, 1080);
        for mode in [ScalingMode::Center, ScalingMode::Fit] {
            assert!(!fills_the_frame(mode, (16383, 1080), frame), "{mode:?}, a column short");
            assert!(!fills_the_frame(mode, (16384, 1079), frame), "{mode:?}, a row short");
            assert!(!fills_the_frame(mode, (1919, 1080), (1920, 1080)), "{mode:?}, a narrow frame");
        }
        for mode in [ScalingMode::Center, ScalingMode::Fit, ScalingMode::Fill] {
            assert!(fills_the_frame(mode, frame, frame), "{mode:?}, as big as the frame");
        }
        for mode in [ScalingMode::Center, ScalingMode::Fill] {
            assert!(fills_the_frame(mode, (16385, 1081), frame), "{mode:?}, a pixel more");
        }
    }

    /// Scaled, a fitted picture is centred as it was: exactly.
    #[test]
    fn a_fitted_picture_scaled_is_centred_exactly() {
        let (_, offset) = ScalingMode::Fit.compute_uv_transform(1499, 827, 3840, 2160);
        // 3840 across; 827 × 3840 / 1499 = 2118.53 down: bars of 20.73.
        let bar = -offset[1] * 827.0 * (3840.0 / 1499.0);
        assert!(
            offset[0].abs() < 1e-6 && (bar - 20.734).abs() < 1e-2,
            "{offset:?}: bars of {bar}"
        );
    }

    /// And a filling one.
    #[test]
    fn a_filling_picture_scaled_is_centred_exactly() {
        let (_, offset) = ScalingMode::Fill.compute_uv_transform(1499, 827, 3840, 2160);
        // 2160 down; 1499 × 2160 / 827 = 3915.16 across: 37.58 cut each side.
        let cut = offset[0] * 1499.0 * (2160.0 / 827.0);
        assert!(
            offset[1].abs() < 1e-6 && (cut - 37.582).abs() < 1e-2,
            "{offset:?}: {cut} cut"
        );
    }

    #[test]
    fn center_same_size_is_identity() {
        let (scale, offset) = ScalingMode::Center.compute_uv_transform(1920, 1080, 1920, 1080);
        assert!((scale[0] - 1.0).abs() < 1e-5);
        assert!((scale[1] - 1.0).abs() < 1e-5);
        assert!((offset[0]).abs() < 1e-5);
        assert!((offset[1]).abs() < 1e-5);
    }
}
