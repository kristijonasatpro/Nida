//! A lower third: the cued person's name and title on a plate, brought on
//! air and taken off with a motion. Drawn as two pictures — the plate, and
//! the text over it — on a thread of its own whenever what they show
//! changes; each frame only places them, so it costs the GPU next to
//! nothing however it moves.

use std::fmt::Write as _;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use crate::engine::value::lower_third::{
    colours, LowerThirdBar, LowerThirdLook, LowerThirdMotion, LowerThirdPlace, LowerThirdRow,
    LowerThirdSettings, LowerThirdSnapshot, LowerThirdStyle,
};
use crate::renderer::GpuContext;
use wgpu::util::DeviceExt as _;

thread_local! {
    /// Set while an Undo — not a Redo — restores the show (see
    /// [`LowerThird::restore_settings`]).
    static UNDOING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `restore` as an Undo restoring the show: a followed list is not
/// taken away by it (a Redo puts back what it undid, Stop following too).
pub fn as_undo<R>(restore: impl FnOnce() -> R) -> R {
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            UNDOING.set(false);
        }
    }
    UNDOING.set(true);
    let _done = Done;
    restore()
}

/// What a drawing is of: whatever changes it.
#[derive(Debug, Clone, PartialEq)]
struct Spec {
    row: LowerThirdRow,
    look: LowerThirdLook,
    frame: (u32, u32),
    /// How many times its picture file (Picture as plate, Full-frame picture)
    /// has been seen saved again since it was chosen: saved again, it is
    /// drawn again.
    picture_saves: u32,
}

/// A drawing: the plate and the text over it, the same size, its top-left
/// on the frame, in the frame's pixels.
struct Drawn {
    plate: image::RgbaImage,
    text: image::RgbaImage,
    at: (f32, f32),
}

/// A full-frame picture's part as shown: opacity, how far away (its
/// direction × a share of its travel), the share revealed.
type Part = (f32, (f32, f32), f32);
const HIDDEN: Part = (0.0, (0.0, 0.0), 0.0);

/// How long a part's motion takes: a cut none.
fn part_secs(motion: LowerThirdMotion, secs: f32) -> f32 {
    if motion == LowerThirdMotion::Cut {
        0.0
    } else {
        secs
    }
}

/// Drawings made together: one for each frame size it is shown at.
type Painted = Vec<(Spec, Option<Drawn>)>;

/// The thread the drawings are made on.
struct Painter {
    jobs: Option<mpsc::Sender<Vec<Spec>>>,
    done: mpsc::Receiver<Painted>,
    done_here: Option<Painted>,
}

impl Painter {
    fn start() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel::<Vec<Spec>>();
        let (done_tx, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("lower-third-paint".into())
            .spawn(move || {
                let fonts = super::svg::system_fonts();
                while let Ok(mut batch) = jobs_rx.recv() {
                    // Only the latest is worth drawing: every size it wants.
                    while let Ok(newer) = jobs_rx.try_recv() {
                        batch = newer;
                    }
                    let painted: Painted = batch
                        .into_iter()
                        .map(|spec| {
                            let drawn = draw(&spec, &fonts);
                            (spec, drawn)
                        })
                        .collect();
                    if done_tx.send(painted).is_err() {
                        break;
                    }
                }
            });
        let jobs = match spawned {
            Ok(_) => Some(jobs_tx),
            Err(e) => {
                log::warn!("Lower third drawing thread not started, drawing in the frame: {e}");
                None
            }
        };
        Self {
            jobs,
            done,
            done_here: None,
        }
    }

    fn ask(&mut self, batch: Vec<Spec>) {
        let unsent = match &self.jobs {
            Some(jobs) => jobs.send(batch).err().map(|mpsc::SendError(batch)| batch),
            None => Some(batch),
        };
        if let Some(batch) = unsent {
            let fonts = super::svg::system_fonts();
            self.done_here = Some(
                batch
                    .into_iter()
                    .map(|spec| {
                        let drawn = draw(&spec, &fonts);
                        (spec, drawn)
                    })
                    .collect(),
            );
        }
    }

    fn take(&mut self) -> Option<Painted> {
        let mut latest = self.done_here.take();
        while let Ok(painted) = self.done.try_recv() {
            latest = Some(painted);
        }
        latest
    }
}

/// A drawing on the GPU for one frame size — the master's, or a bus's
/// names area (see [`LowerThird::set_target_frames`]): what it is of, and
/// its picture (none when there was nothing to draw).
struct Variant {
    spec: Spec,
    picture: Option<Picture>,
}

/// A drawing's plate and text on the GPU, bound together with placement
/// settings of its own — each variant drawn in one frame needs its own.
struct Picture {
    size: (u32, u32),
    /// Its top-left on its frame, in the frame's pixels.
    at: (f32, f32),
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
}

/// Where and how far a drawing shows this frame (WGSL `Params`). A text
/// input's drawing is placed with it too, as text with no plate.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Params {
    pub(super) uv_scale: [f32; 2],
    pub(super) uv_offset: [f32; 2],
    pub(super) plate_alpha: f32,
    pub(super) text_alpha: f32,
    /// The share of the plate shown, from its leading edge.
    pub(super) reveal: f32,
    /// 1: revealed from its right edge (it sits on the right).
    pub(super) from_right: u32,
    /// The drawing's width, in pixels, for the reveal's soft pixel.
    pub(super) width: f32,
    /// The share of the text shown, from its leading edge: a full-frame
    /// picture's text wipes in on its own; 1 for the others.
    pub(super) text_reveal: f32,
    /// Where the text is, as `uv_offset` for the plate: a full-frame
    /// picture's text moves on its own; the plate's for the others.
    pub(super) text_uv_offset: [f32; 2],
}

const SHADER: &str = r"
struct Params {
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    plate_alpha: f32,
    text_alpha: f32,
    reveal: f32,
    from_right: u32,
    width: f32,
    text_reveal: f32,
    text_uv_offset: vec2<f32>,
};
@group(0) @binding(0) var plate_texture: texture_2d<f32>;
@group(0) @binding(1) var text_texture: texture_2d<f32>;
@group(0) @binding(2) var picture_sampler: sampler;
@group(0) @binding(3) var<uniform> params: Params;

fn inside(at: vec2<f32>) -> f32 {
    return select(0.0, 1.0, at.x >= 0.0 && at.x <= 1.0 && at.y >= 0.0 && at.y <= 1.0);
}

// How far open at `at` for a reveal of `reveal`, from its leading edge,
// softened over a pixel.
fn opened(at: vec2<f32>, reveal: f32) -> f32 {
    let from_edge = select(at.x, 1.0 - at.x, params.from_right == 1u) * params.width;
    return clamp(reveal * params.width - from_edge + 0.5, 0.0, 1.0);
}

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    // The plate and the text each where its motion has it.
    let at = uv * params.uv_scale + params.uv_offset;
    let at_text = uv * params.uv_scale + params.text_uv_offset;
    let plate = textureSampleLevel(plate_texture, picture_sampler, at, 0.0) * inside(at);
    let text = textureSampleLevel(text_texture, picture_sampler, at_text, 0.0) * inside(at_text);
    let pa = plate.a * params.plate_alpha * opened(at, params.reveal);
    let ta = text.a * params.text_alpha * opened(at_text, params.text_reveal);
    // The text over the plate, straight colour out, as decks hold it.
    let a = ta + pa * (1.0 - ta);
    if (a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>((text.rgb * ta + plate.rgb * pa * (1.0 - ta)) / a, a);
}
";

/// The pipeline that places a drawing: the text over the plate, each as far
/// shown as its motion has it, in one draw.
pub(super) struct Placer {
    /// Drawn alone, into its own clear target (the deck).
    pipeline: wgpu::RenderPipeline,
    /// Drawn over a picture (a bus's), blended over it.
    pub(super) over: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Placer {
    pub(super) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Lower Third Bind Group Layout"),
            entries: &[
                texture(0),
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: std::num::NonZeroU64::new(
                            std::mem::size_of::<Params>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Lower Third Pipeline Layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Lower Third Vertex Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../renderer/shaders/fullscreen.wgsl").into(),
            ),
        });
        let fragment = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Lower Third Fragment Shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_with = |label, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &vertex,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &fragment,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = pipeline_with("Lower Third Pipeline", wgpu::BlendState::REPLACE);
        // Straight colour over a premultiplied (or opaque) picture: the
        // result premultiplied, as a bus's picture is.
        let over = pipeline_with(
            "Lower Third Over Pipeline",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            },
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Lower Third Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            over,
            layout,
            sampler,
        }
    }

    /// Placement settings of a drawing's own.
    pub(super) fn params(device: &wgpu::Device) -> wgpu::Buffer {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Lower Third Params"),
            contents: bytemuck::bytes_of(&<Params as bytemuck::Zeroable>::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        })
    }

    pub(super) fn bind(
        &self,
        device: &wgpu::Device,
        plate: &wgpu::TextureView,
        text: &wgpu::TextureView,
        params: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Lower Third Bind Group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(plate),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(text),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params.as_entire_binding(),
                },
            ],
        })
    }
}

/// Where a lower third is in its coming and going. What it shows is held
/// by its words (`LowerThird::showing`), never by a place in the list: the
/// list can change under it — the organiser's file read again, a row
/// removed — without another name going on air.
#[derive(Debug, Clone, PartialEq)]
enum Air {
    Off,
    /// Coming in: from when its drawing was ready (none yet: waiting for it).
    In {
        since: Option<Instant>,
    },
    On {
        since: Instant,
    },
    /// Going out — then another in, by its words, when one was shown
    /// meanwhile.
    Out {
        since: Instant,
        then: Option<(LowerThirdRow, usize)>,
    },
}

pub struct LowerThird {
    context: GpuContext,
    settings: LowerThirdSettings,
    frame: (u32, u32),
    air: Air,
    /// The words on air — coming in, on, or going out — as drawn, and which
    /// of the rows with those words it was shown from (0 the first): where
    /// it is marked in the list. A look changed draws them again; words
    /// edited show the next time.
    showing: Option<(LowerThirdRow, usize)>,
    /// The words the row on air had before it was fixed live (see
    /// [`Self::edit_settings`]), since it was shown, oldest first: a drawing
    /// of them back after the next fix is still newer than the one on air,
    /// and put — typing faster than drawing keeps up, not held until it
    /// stops.
    fixed_from: Vec<LowerThirdRow>,
    placer: Placer,
    /// Its drawings on the GPU, one a frame size it is shown at.
    variants: Vec<Variant>,
    /// The frame of the drawing it shows itself (its own deck's): the
    /// master's, or — just after a resize, until the new one is in — the
    /// master's before, scaled.
    main_frame: Option<(u32, u32)>,
    /// The other frames it is drawn for: the names areas of the buses it is
    /// sent to.
    target_frames: Vec<(u32, u32)>,
    asked: Option<Vec<Spec>>,
    painter: Painter,
    /// Why its list could not be read, when it could not.
    problem: Option<String>,
    /// The list file its rows were read from, and that file's time then:
    /// none when the rows came from anywhere else since (typed, an undo),
    /// so the file is read again.
    list_read: Option<(String, std::time::SystemTime)>,
    /// A list just chosen: its rows, when read, come in cued from the first.
    fresh_list: bool,
    /// When its picture file was last saved, as last seen, and how many
    /// times it has been seen saved again since (see `Spec`); the look at
    /// it under way (on a thread of its own), and when the last was started.
    picture_time: Option<std::time::SystemTime>,
    /// Whether it has been looked at since it was chosen: the first look
    /// only notes its time (or that it is not there).
    picture_seen: bool,
    picture_saves: u32,
    picture_check: Option<mpsc::Receiver<Option<std::time::SystemTime>>>,
    picture_checked: Option<Instant>,
}

impl LowerThird {
    /// A lower third, off air.
    ///
    /// # Errors
    ///
    /// None as yet: kept as the other sources are made.
    pub fn new(
        context: &GpuContext,
        settings: LowerThirdSettings,
        frame: (u32, u32),
    ) -> anyhow::Result<Self> {
        Ok(Self {
            context: context.clone(),
            settings: settings.normalized(),
            frame: (frame.0.max(1), frame.1.max(1)),
            air: Air::Off,
            showing: None,
            fixed_from: Vec::new(),
            placer: Placer::new(&context.device, context.compositing_format),
            variants: Vec::new(),
            main_frame: None,
            target_frames: Vec::new(),
            asked: None,
            painter: Painter::start(),
            problem: None,
            list_read: None,
            fresh_list: false,
            picture_time: None,
            picture_seen: false,
            picture_saves: 0,
            picture_check: None,
            picture_checked: None,
        })
    }

    pub fn settings(&self) -> &LowerThirdSettings {
        &self.settings
    }

    /// New rows, look or cue, the cue as given. What is on air stays as it
    /// is — its words, in the new look.
    pub fn set_settings(&mut self, settings: LowerThirdSettings) {
        let old = std::mem::replace(&mut self.settings, settings.normalized());
        if old.look.image != self.settings.look.image {
            // Another picture: looked at at once; one under way was of the
            // one before.
            self.picture_time = None;
            self.picture_seen = false;
            self.picture_check = None;
            self.picture_checked = None;
        }
        if old.source != self.settings.source {
            // What was said of the list before is not of this one.
            self.problem = None;
            self.fresh_list = false;
            self.list_read = None;
        }
        if old.rows != self.settings.rows {
            // Not the file's rows now: read again, if it is followed.
            self.list_read = None;
        }
    }

    /// Settings edited directly — typed, or sent over the API. As
    /// [`Self::set_settings`]; and the row on air itself fixed while it
    /// shows (a typo in its name or title) goes on air at once, in place:
    /// its new drawing takes the old one's place when it is ready, with no
    /// going out and coming in. Rows added, removed or moved, the list read
    /// again (see [`Self::set_rows`]), or an undo leave the words on air
    /// alone.
    pub fn edit_settings(&mut self, settings: LowerThirdSettings) {
        let on_air = self.row_on_air();
        let before = self.settings.rows.clone();
        self.set_settings(settings);
        let Some(at) = on_air else {
            return;
        };
        let rows = &self.settings.rows;
        // That row alone changed, where it was.
        let only_it = rows.len() == before.len()
            && rows
                .iter()
                .zip(&before)
                .enumerate()
                .all(|(i, (now, was))| i == at || now == was);
        if !only_it || rows[at] == before[at] {
            return;
        }
        if matches!(self.air, Air::In { .. } | Air::On { .. }) {
            let nth = rows[..at].iter().filter(|r| **r == rows[at]).count();
            let fixed = Some((rows[at].clone(), nth));
            if let Some((was, _)) = std::mem::replace(&mut self.showing, fixed) {
                self.fixed_from.push(was);
                // Let go as drawings come back (see `came_back`); this only
                // bounds a painter that never answers.
                if self.fixed_from.len() > 4096 {
                    self.fixed_from.remove(0);
                }
            }
        }
    }

    /// Settings as an undo restores them: the cue stays where Show and Next
    /// left it — live, as they are — followed to the restored list. An undo
    /// can go back from one list file to another (one chosen by mistake),
    /// but never from a followed list to none: an Undo meant for a colour,
    /// or pressed while another sent the list over the API, must not strip
    /// a show's names. Stop following, undone, follows again.
    pub fn restore_settings(&mut self, mut settings: LowerThirdSettings) {
        if UNDOING.get() && self.settings.source.is_some() && settings.source.is_none() {
            settings.source.clone_from(&self.settings.source);
            settings.rows.clone_from(&self.settings.rows);
        }
        settings.cued = followed(&self.settings.rows, &settings.rows, self.settings.cued);
        self.set_settings(settings);
    }

    /// Rows read again from its list, the cue followed to the same person.
    pub fn set_rows(&mut self, rows: Vec<LowerThirdRow>) {
        let mut settings = self.settings.clone();
        let old = std::mem::replace(&mut settings.rows, rows);
        settings.cued = followed(&old, &settings.rows, settings.cued);
        self.set_settings(settings);
    }

    /// Its list file chosen (see [`Self::take_list`]): rows from it come in
    /// cued from the first.
    pub fn choose_list(&mut self, path: String) {
        let mut settings = self.settings.clone();
        settings.source = Some(path);
        self.set_settings(settings);
        self.list_read = None;
        self.fresh_list = true;
    }

    /// The list file its rows were read from, and that file's time then.
    pub fn list_read(&self) -> Option<&(String, std::time::SystemTime)> {
        self.list_read.as_ref()
    }

    /// Its list file read: the rows taken in (cued from the first, when the
    /// list was just chosen) and noted as from it; or, not read, why — the
    /// rows before kept, and the file read again next time.
    pub fn take_list(
        &mut self,
        path: &str,
        modified: std::time::SystemTime,
        rows: Result<Vec<LowerThirdRow>, String>,
    ) {
        if self.settings.source.as_deref() != Some(path) {
            return;
        }
        match rows {
            Ok(rows) => {
                if std::mem::take(&mut self.fresh_list) {
                    let mut settings = self.settings.clone();
                    settings.rows = rows;
                    settings.cued = 0;
                    self.set_settings(settings);
                } else {
                    self.set_rows(rows);
                }
                self.list_read = Some((path.to_string(), modified));
                self.problem = None;
            }
            Err(e) => {
                self.list_read = None;
                self.problem = Some(e);
            }
        }
    }

    /// Why its list could not be read, said in its panel; none clears it.
    pub fn set_problem(&mut self, problem: Option<String>) {
        self.problem = problem;
    }

    /// Whether a name is on air: coming in, on, or going out.
    pub fn is_live(&self) -> bool {
        self.air != Air::Off
    }

    /// Where the words on air are in the list, if they are: the same of the
    /// rows with those words as it was shown from (else the last of them).
    pub fn row_on_air(&self) -> Option<usize> {
        let (row, nth) = self.showing.as_ref().filter(|_| self.is_live())?;
        let alike: Vec<usize> = self
            .settings
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| *r == row)
            .map(|(i, _)| i)
            .collect();
        alike.get(*nth).or(alike.last()).copied()
    }

    /// Bring row `row` on air — the cued one when none — taking another
    /// out first; the one after it cued.
    pub fn show(&mut self, row: Option<usize>) {
        let at = row.unwrap_or(self.settings.cued);
        // A row without a name draws nothing: not put on air.
        let Some(words) = self
            .settings
            .rows
            .get(at)
            .filter(|r| !r.name.trim().is_empty())
            .cloned()
        else {
            return;
        };
        self.settings.cued = (at + 1).min(self.settings.rows.len() - 1);
        let nth = self.settings.rows[..at]
            .iter()
            .filter(|r| **r == words)
            .count();
        let now = Instant::now();
        let on_air_already = self.showing.as_ref().is_some_and(|(w, _)| *w == words);
        self.air = match self.air.clone() {
            Air::Off => {
                self.showing = Some((words, nth));
                Air::In { since: None }
            }
            air @ (Air::In { .. } | Air::On { .. }) if on_air_already => {
                // The same words: on air already, marked where shown now.
                self.showing = Some((words, nth));
                air
            }
            Air::In { since } => Air::Out {
                since: self.out_from_in(since, now),
                then: Some((words, nth)),
            },
            Air::On { .. } => Air::Out {
                since: now,
                then: Some((words, nth)),
            },
            Air::Out { since, .. } => Air::Out {
                since,
                then: Some((words, nth)),
            },
        };
    }

    /// Take it off air, as its motion goes out.
    pub fn hide(&mut self) {
        let now = Instant::now();
        self.air = match self.air.clone() {
            Air::Off | Air::In { since: None } => Air::Off,
            Air::In { since } => Air::Out {
                since: self.out_from_in(since, now),
                then: None,
            },
            Air::On { .. } => Air::Out {
                since: now,
                then: None,
            },
            Air::Out { since, .. } => Air::Out { since, then: None },
        };
        if self.air == Air::Off {
            self.showing = None;
            self.fixed_from.clear();
        }
    }

    /// Cue the row `by` from the cued one.
    pub fn step(&mut self, by: i64) {
        self.settings.cued = self.settings.stepped(by);
    }

    /// Going out from part way in: from as far in as it got, not from the
    /// top — no jump.
    fn out_from_in(&self, since: Option<Instant>, now: Instant) -> Instant {
        let secs = self.motion_secs();
        let came = since
            .map_or(0.0, |s| (now - s).as_secs_f32() / secs)
            .min(1.0);
        now.checked_sub(std::time::Duration::from_secs_f32((1.0 - came) * secs))
            .unwrap_or(now)
    }

    fn motion_secs(&self) -> f32 {
        let look = &self.settings.look;
        if look.style == LowerThirdStyle::FullFrame {
            // The later of the picture's and the text's end.
            let ends = |motion, secs: f32, delay: f32| delay + part_secs(motion, secs);
            return ends(look.picture_motion, look.picture_secs, look.picture_delay)
                .max(ends(look.text_motion, look.text_secs, look.text_delay))
                .max(1e-3);
        }
        if look.motion == LowerThirdMotion::Cut {
            0.0
        } else {
            look.motion_secs
        }
        .max(1e-3)
    }

    pub fn snapshot(&self) -> LowerThirdSnapshot {
        LowerThirdSnapshot {
            settings: self.settings.clone(),
            on_air: self.row_on_air(),
            live: self
                .showing
                .as_ref()
                .filter(|_| self.is_live())
                .map(|(words, _)| words.clone()),
            problem: self.problem.clone(),
        }
    }

    /// The words wanted drawn now: those on air, else the cued row (drawn
    /// ahead, ready for Show).
    fn wanted_row(&self) -> Option<&LowerThirdRow> {
        match self.air {
            Air::Off => self.settings.rows.get(self.settings.cued),
            _ => self.showing.as_ref().map(|(row, _)| row),
        }
    }

    /// Every frame it is drawn for: its own (the master's), and the names
    /// areas of the buses it is sent to.
    fn frames(&self) -> Vec<(u32, u32)> {
        let mut frames = vec![self.frame];
        for frame in &self.target_frames {
            if !frames.contains(frame) {
                frames.push(*frame);
            }
        }
        frames
    }

    /// The drawings wanted now: the wanted words, at every frame.
    fn wanted(&self) -> Vec<Spec> {
        let Some(row) = self.wanted_row() else {
            return Vec::new();
        };
        self.frames()
            .into_iter()
            .map(|frame| self.spec_at(row, frame))
            .collect()
    }

    fn spec_at(&self, row: &LowerThirdRow, frame: (u32, u32)) -> Spec {
        Spec {
            row: row.clone(),
            look: self.settings.look.clone(),
            frame,
            picture_saves: self.picture_saves,
        }
    }

    /// Its picture file (Picture as plate, Full-frame picture) looked at about
    /// once a second, on a thread of its own — a picture on a drive slow to
    /// answer holds nothing up: saved again (from Photoshop, say), it is
    /// drawn again.
    fn watch_picture(&mut self, now: Instant) {
        let look = &self.settings.look;
        let uses = matches!(
            look.style,
            LowerThirdStyle::Image | LowerThirdStyle::FullFrame
        ) && !look.image.trim().is_empty();
        if let Some(check) = &self.picture_check {
            match check.try_recv() {
                Ok(time) => {
                    self.picture_check = None;
                    // The first look only notes the time (or that it is not
                    // there): the drawing under way reads the file as it
                    // is. A change after — saved again, gone, back — is
                    // drawn again.
                    if uses {
                        if self.picture_seen && time != self.picture_time {
                            self.picture_saves = self.picture_saves.wrapping_add(1);
                        }
                        self.picture_time = time;
                        self.picture_seen = true;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => self.picture_check = None,
            }
        }
        if !uses
            || self
                .picture_checked
                .is_some_and(|at| now - at < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.picture_checked = Some(now);
        let path = look.image.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("lower-third-picture".into())
            .spawn(move || {
                let _ = tx.send(std::fs::metadata(&path).and_then(|m| m.modified()).ok());
            });
        if spawned.is_ok() {
            self.picture_check = Some(rx);
        }
    }

    /// The names areas of the buses it is sent to, in master pixels: drawn
    /// for each too, laid out for its shape (see [`Self::render_over`]).
    pub fn set_target_frames(&mut self, frames: &[(u32, u32)]) {
        self.target_frames = frames
            .iter()
            .map(|(w, h)| ((*w).max(1), (*h).max(1)))
            .collect();
    }

    /// Whether it is drawn for buses too (see [`Self::set_target_frames`]).
    pub fn has_target_frames(&self) -> bool {
        !self.target_frames.is_empty()
    }

    /// Each frame: drawings asked for and taken, and its coming and going
    /// moved on.
    pub fn tick(&mut self) {
        let now = Instant::now();
        self.watch_picture(now);
        let wanted = self.wanted();
        if !wanted.is_empty() && self.asked.as_ref() != Some(&wanted) {
            self.painter.ask(wanted.clone());
            self.asked = Some(wanted);
        }
        if let Some(painted) = self.painter.take() {
            for (spec, drawn) in painted {
                self.came_back(spec, drawn);
            }
        }
        let air = self.moved_on(now);
        match (&self.air, &air) {
            // Out, and the one shown meanwhile coming in: its words.
            (Air::Out { then, .. }, Air::In { .. }) => {
                self.showing.clone_from(then);
                self.fixed_from.clear();
            }
            (_, Air::Off) => {
                self.showing = None;
                self.fixed_from.clear();
            }
            _ => {}
        }
        self.air = air;
    }

    /// Where it is in its coming and going, `now`.
    fn moved_on(&self, now: Instant) -> Air {
        let secs = self.motion_secs();
        // Its words drawn at every frame it is shown at: it can come in.
        // (Or words it had before a live fix, newer than none.)
        let ready = || {
            self.showing.as_ref().is_some_and(|(row, _)| {
                self.frames().into_iter().all(|frame| {
                    let spec = self.spec_at(row, frame);
                    self.variants.iter().any(|v| {
                        v.spec == spec || v.spec.frame == frame && self.fixed_earlier(&v.spec)
                    })
                })
            })
        };
        match &self.air {
            Air::In { since: None } if ready() => Air::In { since: Some(now) },
            Air::In { since: Some(since) } if (now - *since).as_secs_f32() >= secs => {
                Air::On { since: now }
            }
            Air::On { since } => {
                let out_after = self.settings.look.auto_out_secs;
                if out_after > 0.0 && (now - *since).as_secs_f32() >= out_after {
                    Air::Out {
                        since: now,
                        then: None,
                    }
                } else {
                    self.air.clone()
                }
            }
            Air::Out { since, then } if (now - *since).as_secs_f32() >= secs => match then {
                Some(_) => Air::In { since: None },
                None => Air::Off,
            },
            air => air.clone(),
        }
    }

    /// A drawing back from the painter: put on the GPU if it is one wanted
    /// now — one asked for before (the cued name, while another was shown
    /// again) coming back late is not shown in its place.
    fn came_back(&mut self, spec: Spec, drawn: Option<Drawn>) {
        let earlier = self.fixed_earlier(&spec);
        if earlier || self.wanted().contains(&spec) {
            // Drawings come back in the order asked: words older than these
            // cannot come back now.
            if earlier {
                if let Some(at) = self.fixed_from.iter().position(|r| *r == spec.row) {
                    self.fixed_from.drain(..at);
                }
            } else if self
                .showing
                .as_ref()
                .is_some_and(|(row, _)| *row == spec.row)
            {
                self.fixed_from.clear();
            }
            self.put(spec, drawn);
        } else if self
            .asked
            .as_ref()
            .is_some_and(|asked| asked.contains(&spec))
        {
            // Not kept: asked for again should it be wanted again (the row
            // removed, then put back by an undo).
            self.asked = None;
        }
    }

    /// Whether `spec` is a drawing of the words on air before a live fix,
    /// in a look and at a frame it is drawn in now.
    fn fixed_earlier(&self, spec: &Spec) -> bool {
        self.fixed_from.contains(&spec.row)
            && spec.look == self.settings.look
            && self.frames().contains(&spec.frame)
    }

    /// A drawing on the GPU, for its frame; drawings for frames no longer
    /// shown let go.
    fn put(&mut self, spec: Spec, drawn: Option<Drawn>) {
        let frame = spec.frame;
        let picture = drawn.map(|drawn| {
            let (_, plate) = super::source::upload_image_texture(&self.context, &drawn.plate);
            let (_, text) = super::source::upload_image_texture(&self.context, &drawn.text);
            let params = Placer::params(&self.context.device);
            Picture {
                size: drawn.plate.dimensions(),
                at: drawn.at,
                bind_group: self
                    .placer
                    .bind(&self.context.device, &plate, &text, &params),
                params,
            }
        });
        match self.variants.iter_mut().find(|v| v.spec.frame == frame) {
            Some(variant) => {
                variant.spec = spec;
                variant.picture = picture;
            }
            None => self.variants.push(Variant { spec, picture }),
        }
        if frame == self.frame {
            self.main_frame = Some(frame);
        }
        let frames = self.frames();
        let main = self.main_frame;
        self.variants
            .retain(|v| frames.contains(&v.spec.frame) || Some(v.spec.frame) == main);
    }

    /// How far the plate and the text are shown now: (plate opacity, text
    /// opacity, how far away it still is — its direction × a share of its
    /// travel — and the share of the plate revealed).
    fn motion(&self, now: Instant) -> (f32, f32, (f32, f32), f32) {
        let secs = self.motion_secs();
        let look = &self.settings.look;
        let ease_out = |p: f32| 1.0 - (1.0 - p.clamp(0.0, 1.0)).powi(3);
        let ease_in = |p: f32| p.clamp(0.0, 1.0).powi(3);
        let part = |p: f32, from: f32, to: f32| ((p - from) / (to - from)).clamp(0.0, 1.0);
        let direction = match look.place {
            LowerThirdPlace::LowerRight => (1.0, 0.0),
            LowerThirdPlace::LowerCentre => (0.0, 0.5),
            LowerThirdPlace::LowerLeft | LowerThirdPlace::UpperLeft => (-1.0, 0.0),
        };
        // How far the plate has come (`plate`) and the text (`text`): the
        // same but for a wipe, or a look whose text comes after its plate.
        let shown = |plate: f32, text: f32| match look.motion {
            LowerThirdMotion::Cut => (1.0, 1.0, (0.0, 0.0), 1.0),
            LowerThirdMotion::Fade => (plate, text, (0.0, 0.0), 1.0),
            LowerThirdMotion::Slide => {
                let away = 1.0 - plate;
                (plate, text, (direction.0 * away, direction.1 * away), 1.0)
            }
            LowerThirdMotion::Wipe => (1.0, text, (0.0, 0.0), plate),
        };
        // The plate over the first two thirds, the text from just under half
        // way: the plate in, then the text.
        let staged = look.motion == LowerThirdMotion::Wipe || look.text_after;
        let at = |p: f32| {
            if staged {
                shown(ease_out(part(p, 0.0, 0.65)), ease_out(part(p, 0.45, 1.0)))
            } else {
                let e = ease_out(p);
                shown(e, e)
            }
        };
        match self.air {
            Air::Off | Air::In { since: None, .. } => (0.0, 0.0, (0.0, 0.0), 0.0),
            Air::On { .. } => (1.0, 1.0, (0.0, 0.0), 1.0),
            Air::In {
                since: Some(since), ..
            } => at((now - since).as_secs_f32() / secs),
            Air::Out { since, .. } => {
                // Its coming in backwards (1 − ease_in(p) is ease_out(1 − p)):
                // the text goes, then the plate — and going out from part way
                // in (see `out_from_in`) carries on from where it was.
                let p = (now - since).as_secs_f32() / secs;
                if staged {
                    at(1.0 - p)
                } else {
                    let e = 1.0 - ease_in(p);
                    shown(e, e)
                }
            }
        }
    }

    /// Whether anything of it shows now: coming in, on or going out, its
    /// drawing ready.
    pub fn shows(&self) -> bool {
        if self.settings.look.style == LowerThirdStyle::FullFrame {
            return self
                .full_frame_parts(Instant::now())
                .iter()
                .any(|(alpha, _, revealed)| *alpha > 0.0 && *revealed > 0.0);
        }
        let (plate, text, _, revealed) = self.motion(Instant::now());
        plate > 0.0 && revealed > 0.0 || text > 0.0
    }

    /// A full-frame picture's two parts as shown `now` — the picture's and
    /// the text's: opacity, how far away (its direction × a share of its
    /// travel) and the share revealed — each on its own time. Going out
    /// plays coming in backwards: the text goes, then the picture.
    fn full_frame_parts(&self, now: Instant) -> [Part; 2] {
        let look = &self.settings.look;
        let total = self.motion_secs();
        let at = match self.air {
            Air::Off | Air::In { since: None } => return [HIDDEN; 2],
            Air::On { .. } => total,
            Air::In { since: Some(since) } => (now - since).as_secs_f32(),
            Air::Out { since, .. } => total - (now - since).as_secs_f32(),
        };
        let part = |motion, secs: f32, delay: f32| -> Part {
            let t = at - delay;
            let secs = part_secs(motion, secs);
            let p = if secs <= 0.0 {
                if t >= 0.0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                (t / secs).clamp(0.0, 1.0)
            };
            if p <= 0.0 {
                return HIDDEN;
            }
            let e = 1.0 - (1.0 - p).powi(3);
            match motion {
                LowerThirdMotion::Cut => (1.0, (0.0, 0.0), 1.0),
                LowerThirdMotion::Fade => (e, (0.0, 0.0), 1.0),
                // From the left, as a lower third on the left slides.
                LowerThirdMotion::Slide => (e, (-(1.0 - e), 0.0), 1.0),
                LowerThirdMotion::Wipe => (1.0, (0.0, 0.0), e),
            }
        };
        [
            part(look.picture_motion, look.picture_secs, look.picture_delay),
            part(look.text_motion, look.text_secs, look.text_delay),
        ]
    }

    /// The placement settings for `variant` as its motion has it `now`;
    /// none when it shows nothing.
    #[allow(clippy::cast_precision_loss)]
    fn params_of(&self, picture: &Picture, frame: (u32, u32), now: Instant) -> Option<Params> {
        if self.settings.look.style == LowerThirdStyle::FullFrame {
            let [plate, text] = self.full_frame_parts(now);
            let seen = |(alpha, _, revealed): Part| alpha > 0.0 && revealed > 0.0;
            if !seen(plate) && !seen(text) {
                return None;
            }
            let travel = 0.12 * frame.0 as f32;
            let placed = |away: (f32, f32)| {
                let at = (
                    (picture.at.0 + away.0 * travel).round(),
                    (picture.at.1 + away.1 * travel).round(),
                );
                placement(frame, picture.size, at)
            };
            let (uv_scale, uv_offset) = placed(plate.1);
            let (_, text_uv_offset) = placed(text.1);
            return Some(Params {
                uv_scale,
                uv_offset,
                plate_alpha: plate.0,
                text_alpha: text.0,
                reveal: plate.2,
                from_right: 0,
                width: picture.size.0 as f32,
                text_reveal: text.2,
                text_uv_offset,
            });
        }
        let (plate_alpha, text_alpha, away, revealed) = self.motion(now);
        if !(plate_alpha > 0.0 && revealed > 0.0 || text_alpha > 0.0) {
            return None;
        }
        // Its travel a share of its own frame's width; on whole pixels: a
        // straight-colour drawing sampled between them would take a dark
        // fringe from its clear edge.
        let travel = 0.12 * frame.0 as f32;
        let at = (
            (picture.at.0 + away.0 * travel).round(),
            (picture.at.1 + away.1 * travel).round(),
        );
        let (uv_scale, uv_offset) = placement(frame, picture.size, at);
        Some(Params {
            uv_scale,
            uv_offset,
            plate_alpha,
            text_alpha,
            reveal: revealed,
            from_right: u32::from(self.settings.look.place == LowerThirdPlace::LowerRight),
            width: picture.size.0 as f32,
            text_reveal: 1.0,
            text_uv_offset: uv_offset,
        })
    }

    /// Draw the frame: clear, and the lower third as far as it is shown —
    /// its own drawing (the master's frame; just after a resize the one
    /// before, scaled, until the new one is in).
    pub fn render(&self, cmd_buffers: &mut Vec<wgpu::CommandBuffer>, output: &wgpu::TextureView) {
        let mut encoder =
            self.context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Lower Third Encoder"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Lower Third Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let main = self
                .main_frame
                .and_then(|frame| self.variants.iter().find(|v| v.spec.frame == frame));
            if let Some(Variant {
                spec,
                picture: Some(picture),
            }) = main
            {
                if let Some(params) = self.params_of(picture, spec.frame, Instant::now()) {
                    self.context.queue.write_buffer(
                        &picture.params,
                        0,
                        bytemuck::bytes_of(&params),
                    );
                    pass.set_pipeline(&self.placer.pipeline);
                    pass.set_bind_group(0, &picture.bind_group, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }
        cmd_buffers.push(encoder.finish());
    }

    /// Draw it in `pass` — over a bus's picture, in the same pass as that
    /// picture's copy — in the names area `area` (x, y, width, height in the
    /// target's pixels), laid out for that area's frame (see
    /// [`Self::set_target_frames`]); blended over what is there. True when
    /// anything of it was drawn.
    pub fn render_over(&self, pass: &mut wgpu::RenderPass<'_>, area: (u32, u32, u32, u32)) -> bool {
        let frame = (area.2.max(1), area.3.max(1));
        let Some(Variant {
            spec,
            picture: Some(picture),
        }) = self.variants.iter().find(|v| v.spec.frame == frame)
        else {
            return false;
        };
        let Some(params) = self.params_of(picture, spec.frame, Instant::now()) else {
            return false;
        };
        self.context
            .queue
            .write_buffer(&picture.params, 0, bytemuck::bytes_of(&params));
        #[allow(clippy::cast_precision_loss)]
        pass.set_viewport(
            area.0 as f32,
            area.1 as f32,
            frame.0 as f32,
            frame.1 as f32,
            0.0,
            1.0,
        );
        pass.set_pipeline(&self.placer.over);
        pass.set_bind_group(0, &picture.bind_group, &[]);
        pass.draw(0..3, 0..1);
        true
    }

    /// The master's size changed: drawn again to suit.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.frame = (width.max(1), height.max(1));
    }

    /// The drawing it shows itself, for tests: what it is of.
    #[cfg(test)]
    fn main_drawn(&self) -> Option<&Spec> {
        let frame = self.main_frame?;
        self.variants
            .iter()
            .find(|v| v.spec.frame == frame)
            .map(|v| &v.spec)
    }

    /// Wait (a while) for the drawings wanted now, for tests: true once
    /// they are on the GPU.
    pub fn wait_drawn(&mut self, timeout: std::time::Duration) -> bool {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            self.tick();
            let wanted = self.wanted();
            if !wanted.is_empty()
                && wanted
                    .iter()
                    .all(|spec| self.variants.iter().any(|v| v.spec == *spec))
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }
}

/// The UV transform that puts a `size` drawing with its top-left at `at`
/// on a `frame`-sized frame, at its own size.
#[allow(clippy::cast_precision_loss)]
fn placement(frame: (u32, u32), size: (u32, u32), at: (f32, f32)) -> ([f32; 2], [f32; 2]) {
    let frame = (frame.0.max(1) as f32, frame.1.max(1) as f32);
    let size = (size.0.max(1) as f32, size.1.max(1) as f32);
    (
        [frame.0 / size.0, frame.1 / size.1],
        [-at.0 / size.0, -at.1 / size.1],
    )
}

/// Where the cued row at `at` in `old` is in `new`: the same occurrence of
/// its words (the second of two alike stays the second), else the nearest
/// of them, else the same place — kept in the list.
fn followed(old: &[LowerThirdRow], new: &[LowerThirdRow], at: usize) -> usize {
    let last = new.len().saturating_sub(1);
    let Some(row) = old.get(at) else {
        return at.min(last);
    };
    let nth = old[..at].iter().filter(|r| *r == row).count();
    let alike: Vec<usize> = new
        .iter()
        .enumerate()
        .filter(|(_, r)| *r == row)
        .map(|(i, _)| i)
        .collect();
    alike
        .get(nth)
        .or_else(|| alike.iter().min_by_key(|i| i.abs_diff(at)))
        .copied()
        .unwrap_or(at.min(last))
}

// ── Drawing ─────────────────────────────────────────────────────────

/// The title's size, and the second line's, as shares of the name's.
const TITLE_SIZE: f32 = 0.62;
const TITLE2_SIZE: f32 = 0.55;
/// It is never shrunk below this share of its size to fit.
const SMALLEST: f32 = 0.6;

/// Lay out and draw `spec`: none when it cannot be drawn at all.
#[allow(
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]
fn draw(spec: &Spec, fonts: &Arc<usvg::fontdb::Database>) -> Option<Drawn> {
    if spec.look.style == LowerThirdStyle::FullFrame {
        return draw_full_frame(spec, fonts);
    }
    let look = &spec.look;
    let (fw, fh) = (spec.frame.0 as f32, spec.frame.1 as f32);
    let name = spec.row.name.trim();
    let title = spec.row.title.trim();
    let title2 = spec.row.title2.trim();
    if name.is_empty() {
        return None;
    }
    let style = look.style;
    let base = (look.size * fh).max(6.0);
    let lines: Vec<(&str, f32, u16)> = [
        (name, 1.0, 600),
        (title, TITLE_SIZE, 400),
        (title2, TITLE2_SIZE, 400),
    ]
    .into_iter()
    .filter(|(text, _, _)| !text.is_empty())
    .collect();
    let widths: Vec<f32> = lines
        .iter()
        .map(|(text, share, weight)| {
            let px = base * share;
            measure(text, px, *weight, &look.font, fonts)
                .unwrap_or(px * 0.55 * text.chars().count() as f32)
        })
        .collect();
    let widest = widths.iter().copied().fold(0.0, f32::max);
    let margin = if look.safe_area { 0.05 } else { 0.02 };
    let room = match look.place {
        LowerThirdPlace::LowerCentre => fw * (1.0 - 2.0 * margin),
        _ => fw * 0.6,
    };
    // Shrunk to fit its room, as far as it may be.
    let pad_x0 = 0.55 * base;
    let fit = ((room - 2.0 * pad_x0) / widest.max(1.0)).clamp(SMALLEST, 1.0);
    let name_px = base * fit;
    // The room round the text, and what each shape adds at its ends: a
    // pill's round ends, a ribbon's slant.
    let pad_x = 0.55 * name_px;
    let pad_y = 0.38 * name_px;
    let bar = match (style, look.bar) {
        (LowerThirdStyle::Plate | LowerThirdStyle::Ribbon, LowerThirdBar::Edge) => 0.12 * name_px,
        _ => 0.0,
    };
    let top_bar = match (style, look.bar) {
        (LowerThirdStyle::Plate | LowerThirdStyle::Ribbon, LowerThirdBar::Top) => 0.09 * name_px,
        _ => 0.0,
    };
    // Baselines, from the plate's top: each line as tall as its size.
    let mut baselines = Vec::new();
    let mut y = pad_y + top_bar;
    for (i, (_, share, _)) in lines.iter().enumerate() {
        let px = name_px * share;
        if i > 0 {
            y += 0.3 * name_px;
        }
        y += 0.78 * px;
        baselines.push(y);
        y += 0.22 * px;
    }
    let plate_h = y + pad_y;
    let ends = match style {
        LowerThirdStyle::Pill => 0.35 * plate_h,
        LowerThirdStyle::Ribbon => 0.4 * plate_h,
        _ => 0.0,
    };
    let text_w = widest * fit + 2.0 * pad_x + bar + 2.0 * ends;
    // A picture of one's own sets the plate's shape: as wide as it is for
    // the plate's height, wider only for a name that needs it.
    let picture = (style == LowerThirdStyle::Image)
        .then(|| plate_picture(&look.image))
        .flatten();
    // ...but never wider than the frame for a picture's sake.
    let plate_w = match &picture {
        Some((_, aspect)) => text_w.max((plate_h * aspect).min(fw)),
        None => text_w,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (wide, high) = (plate_w.ceil() as u32, plate_h.ceil() as u32);
    if wide == 0 || high == 0 || wide > 8192 || high > 8192 {
        return None;
    }
    let x = match look.place {
        LowerThirdPlace::LowerLeft | LowerThirdPlace::UpperLeft => margin * fw,
        LowerThirdPlace::LowerRight => fw * (1.0 - margin) - plate_w,
        LowerThirdPlace::LowerCentre => (fw - plate_w) / 2.0,
    };
    let y = match look.place {
        LowerThirdPlace::UpperLeft => margin * fh + 0.03 * fh,
        _ => fh * (1.0 - margin) - plate_h - 0.05 * fh,
    };
    let [plate_c, bar_c, name_c, title_c] = colours(look);
    let right = look.place == LowerThirdPlace::LowerRight;
    let text_left = if right {
        plate_w - ends - pad_x - widest * fit
    } else {
        ends + pad_x + bar
    };
    let opacity = look.plate_opacity;

    let mut plate_svg = svg_head(wide, high);
    match style {
        LowerThirdStyle::Plate | LowerThirdStyle::Pill => {
            if opacity > 0.0 {
                let r = if style == LowerThirdStyle::Pill {
                    plate_h / 2.0
                } else {
                    0.1 * name_px
                };
                let _ = write!(
                    plate_svg,
                    r#"<rect width="{wide}" height="{high}" rx="{r}" fill="{}" fill-opacity="{opacity}"/>"#,
                    hex(plate_c),
                );
            }
        }
        LowerThirdStyle::Ribbon => {
            if opacity > 0.0 {
                // Slanted ends, leaning the way it reads.
                let (s, w, h) = (ends, plate_w, plate_h);
                let _ = write!(
                    plate_svg,
                    r#"<polygon points="{s},0 {w},0 {},{h} 0,{h}" fill="{}" fill-opacity="{opacity}"/>"#,
                    w - s,
                    hex(plate_c),
                );
            }
        }
        LowerThirdStyle::TwoTone => {
            // The name on a band of the bar's colour, the lines under it on
            // the plate's.
            let split = if baselines.len() > 1 {
                (baselines[0] + 0.22 * name_px + baselines[1] - 0.78 * name_px * TITLE_SIZE) / 2.0
            } else {
                plate_h
            };
            let _ = write!(
                plate_svg,
                r#"<rect width="{wide}" height="{split}" fill="{}"/>"#,
                hex(bar_c),
            );
            if split < plate_h && opacity > 0.0 {
                let _ = write!(
                    plate_svg,
                    r#"<rect y="{split}" width="{wide}" height="{}" fill="{}" fill-opacity="{opacity}"/>"#,
                    plate_h - split,
                    hex(plate_c),
                );
            }
        }
        LowerThirdStyle::TextOnly | LowerThirdStyle::FullFrame => {}
        LowerThirdStyle::Image => match &picture {
            Some((uri, _)) => {
                let _ = write!(
                    plate_svg,
                    r#"<image width="{wide}" height="{high}" preserveAspectRatio="none" opacity="{opacity}" href="{uri}"/>"#,
                );
            }
            // Not there (moved, not chosen yet): a plain plate meanwhile.
            None => {
                let _ = write!(
                    plate_svg,
                    r#"<rect width="{wide}" height="{high}" rx="{}" fill="{}" fill-opacity="{opacity}"/>"#,
                    0.1 * name_px,
                    hex(plate_c),
                );
            }
        },
    }
    match (style, look.bar) {
        (LowerThirdStyle::Plate, LowerThirdBar::Edge) => {
            let bx = if right { plate_w - bar } else { 0.0 };
            let _ = write!(
                plate_svg,
                r#"<rect x="{bx}" width="{bar}" height="{high}" fill="{}"/>"#,
                hex(bar_c)
            );
        }
        (LowerThirdStyle::Ribbon, LowerThirdBar::Edge) => {
            // A slanted stripe along its leading end.
            let (s, h) = (ends, plate_h);
            let points = if right {
                let w = plate_w;
                format!("{},0 {w},0 {},{h} {},{h}", w - bar, w - s, w - s - bar)
            } else {
                format!("{s},0 {},0 {bar},{h} 0,{h}", s + bar)
            };
            let _ = write!(
                plate_svg,
                r#"<polygon points="{points}" fill="{}"/>"#,
                hex(bar_c)
            );
        }
        (LowerThirdStyle::Plate, LowerThirdBar::Top) => {
            let _ = write!(
                plate_svg,
                r#"<rect width="{wide}" height="{top_bar}" fill="{}"/>"#,
                hex(bar_c)
            );
        }
        (LowerThirdStyle::Ribbon, LowerThirdBar::Top) => {
            // The ribbon's own top, slant and all.
            let (s, w, h, t) = (ends, plate_w, plate_h, top_bar);
            let lean = s * t / h;
            let _ = write!(
                plate_svg,
                r#"<polygon points="{s},0 {w},0 {},{t} {},{t}" fill="{}"/>"#,
                w - lean,
                s - lean,
                hex(bar_c)
            );
        }
        (style, LowerThirdBar::Under) if style != LowerThirdStyle::TwoTone => {
            let under = baselines[0] + 0.14 * name_px;
            let _ = write!(
                plate_svg,
                r#"<rect x="{text_left}" y="{under}" width="{}" height="{}" fill="{}"/>"#,
                widths[0] * fit,
                0.06 * name_px,
                hex(bar_c)
            );
        }
        _ => {}
    }
    plate_svg.push_str("</svg>");

    let mut texts = String::new();
    for (i, (text, share, weight)) in lines.iter().enumerate() {
        let colour = if i == 0 { name_c } else { title_c };
        let px = name_px * share;
        let _ = write!(
            texts,
            r#"<text x="{text_left}" y="{}" {}>{}</text>"#,
            baselines[i],
            attributes(&look.font, px, *weight, colour),
            escape(text)
        );
    }
    let mut text_svg = svg_head(wide, high);
    if style == LowerThirdStyle::TextOnly {
        // With no plate under it, shadows keep it readable on any picture:
        // a soft one, dropped, and a tight dark halo hugging the letters, so
        // light lines read on a bright picture too. Each blurred over the
        // picture alone (its group moved by its drop): nothing shows past
        // its edge.
        let black = texts
            .replace(&format!(r#"fill="{}""#, hex(name_c)), r##"fill="#000000""##)
            .replace(
                &format!(r#"fill="{}""#, hex(title_c)),
                r##"fill="#000000""##,
            );
        let shadow = |svg: &mut String, id: &str, blur: f32, drop: f32, opacity: f32, halo: f32| {
            let _ = write!(
                svg,
                r##"<filter id="{id}" filterUnits="userSpaceOnUse" x="{x}" y="{x}" width="{wide}" height="{high}"><feGaussianBlur stdDeviation="{blur}"/></filter><g transform="translate({drop} {drop})" filter="url(#{id})" opacity="{opacity}" stroke="#000000" stroke-width="{halo}" stroke-linejoin="round">{black}</g>"##,
                x = -drop,
            );
        };
        shadow(
            &mut text_svg,
            "soft",
            0.08 * name_px,
            0.06 * name_px,
            0.6,
            0.0,
        );
        shadow(
            &mut text_svg,
            "tight",
            0.025 * name_px,
            0.02 * name_px,
            0.95,
            0.08 * name_px,
        );
    }
    text_svg.push_str(&texts);
    text_svg.push_str("</svg>");
    Some(Drawn {
        plate: rasterize(&plate_svg, wide, high, fonts)?,
        text: rasterize(&text_svg, wide, high, fonts)?,
        at: (x, y),
    })
}

/// A full-frame picture: the picture over the whole frame as drawn — scaled
/// only to the frame, kept in shape (a 16:9 picture on a 16:9 frame pixel
/// for pixel where it was put), then by its own Scale and offset — and the
/// lines in the text box over it, made smaller to fit its width (to six
/// tenths), then wrapped, never moving the picture. On a frame of another
/// shape (a bus's LED band) the design is fitted inside it, at its foot.
/// Only what shows is kept: the picture's visible part and the text box.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn draw_full_frame(spec: &Spec, fonts: &Arc<usvg::fontdb::Database>) -> Option<Drawn> {
    let look = &spec.look;
    if spec.row.name.trim().is_empty() {
        return None;
    }
    let (fw, fh) = (spec.frame.0 as f32, spec.frame.1 as f32);
    let picture = full_picture(&look.image);
    // The design's frame: the picture's shape (16:9 without one), fitted in
    // the frame, at its foot.
    let aspect = picture.as_ref().map_or(16.0 / 9.0, |read| {
        read.0.width() as f32 / read.0.height().max(1) as f32
    });
    let cw = fw.min(fh * aspect);
    let ch = cw / aspect;
    let (cx, cy) = ((fw - cw) / 2.0, fh - ch);
    let [bx, by, bw, bh] = look.text_box;
    let text_box = (cx + bx * cw, cy + by * ch, bw * cw, bh * ch);
    // The picture: scaled about the design's middle, moved.
    let (pw, ph) = (cw * look.picture_scale, ch * look.picture_scale);
    let px0 = cx + (cw - pw) / 2.0 + look.picture_offset[0] * cw;
    let py0 = cy + (ch - ph) / 2.0 + look.picture_offset[1] * ch;
    let scale = picture
        .as_ref()
        .map_or(1.0, |p| pw / p.0.width().max(1) as f32);
    // Its visible part, on the frame.
    let visible = picture.as_ref().and_then(|read| {
        let (x, y, w, h) = read.1?;
        Some((
            px0 + x as f32 * scale,
            py0 + y as f32 * scale,
            w as f32 * scale,
            h as f32 * scale,
        ))
    });
    let (ux0, uy0, ux1, uy1) = match visible {
        Some((x, y, w, h)) => (
            x.min(text_box.0),
            y.min(text_box.1),
            (x + w).max(text_box.0 + text_box.2),
            (y + h).max(text_box.1 + text_box.3),
        ),
        None => (
            text_box.0,
            text_box.1,
            text_box.0 + text_box.2,
            text_box.1 + text_box.3,
        ),
    };
    let x0 = ux0.max(0.0).floor();
    let y0 = uy0.max(0.0).floor();
    let x1 = ux1.min(fw).ceil();
    let y1 = uy1.min(fh).ceil();
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (wide, high) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut plate = image::RgbaImage::new(wide, high);
    if let (Some(read), Some((vx, vy, vw, vh))) = (&picture, picture.as_ref().and_then(|p| p.1)) {
        // The visible part that lands on the frame — only that read and
        // scaled, so a big picture made bigger and mostly off the frame
        // costs no more than the frame — at its size on the frame (as is
        // when that is its own), where it lands.
        let src = |frame: f32, start: f32| (frame - start) / scale;
        let x = (vx as f32).max(src(0.0, px0).floor()) as u32;
        let y = (vy as f32).max(src(0.0, py0).floor()) as u32;
        let right = ((vx + vw) as f32).min(src(fw, px0).ceil()).max(0.0) as u32;
        let bottom = ((vy + vh) as f32).min(src(fh, py0).ceil()).max(0.0) as u32;
        let (w, h) = (right.saturating_sub(x), bottom.saturating_sub(y));
        if w == 0 || h == 0 {
            return full_frame_finish(spec, plate, text_box, (x0, y0), (wide, high), fonts);
        }
        let part = image::imageops::crop_imm(&read.0, x, y, w, h).to_image();
        let (sw, sh) = (
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        );
        let part = if (sw, sh) == (w, h) {
            part
        } else {
            image::imageops::resize(&part, sw, sh, image::imageops::FilterType::CatmullRom)
        };
        image::imageops::replace(
            &mut plate,
            &part,
            i64::from((px0 + x as f32 * scale - x0).round() as i32),
            i64::from((py0 + y as f32 * scale - y0).round() as i32),
        );
    }
    full_frame_finish(spec, plate, text_box, (x0, y0), (wide, high), fonts)
}

/// A full-frame picture's drawing finished: its text over `plate`, the
/// region it covers at `at` on the frame.
fn full_frame_finish(
    spec: &Spec,
    plate: image::RgbaImage,
    text_box: (f32, f32, f32, f32),
    (x0, y0): (f32, f32),
    size: (u32, u32),
    fonts: &Arc<usvg::fontdb::Database>,
) -> Option<Drawn> {
    let text = full_frame_text(
        spec,
        (text_box.0 - x0, text_box.1 - y0, text_box.2, text_box.3),
        size,
        fonts,
    )?;
    Some(Drawn {
        plate,
        text,
        at: (x0, y0),
    })
}

/// A full-frame picture's lines in their box (x, y, width, height on a
/// `wide` × `high` picture): as tall as fill the box; a line too long for
/// its width made smaller (to three quarters), then wrapped at words, the
/// other lines kept as they are; all smaller only if they must be to fit
/// its height; centred in its height, lined up as asked, a little in from
/// the box's edges.
fn full_frame_text(
    spec: &Spec,
    text_box: (f32, f32, f32, f32),
    (wide, high): (u32, u32),
    fonts: &Arc<usvg::fontdb::Database>,
) -> Option<image::RgbaImage> {
    let mut texts = String::new();
    for (text, px, weight, colour, x, baseline) in full_frame_lines(spec, text_box, fonts) {
        let _ = write!(
            texts,
            r#"<text x="{x}" y="{baseline}" {}>{}</text>"#,
            attributes(&spec.look.font, px, weight, colour),
            escape(&text)
        );
    }
    let svg = format!("{}{texts}</svg>", svg_head(wide, high));
    rasterize(&svg, wide, high, fonts)
}

/// A line laid out in a full-frame picture's text box: its text, size
/// (pixels), weight, colour, left and baseline.
type BoxLine = (String, f32, u16, [u8; 3], f32, f32);

/// How far a line's letters reach above its baseline (an accented capital,
/// Ž) and below it (ą, j), as shares of its size, when the font's own
/// reach could not be measured (see [`ink_reach`]).
const BOX_ASCENT: f32 = 0.98;
const BOX_DESCENT: f32 = 0.26;

/// `words` (each `widths` wide, `space` between) broken into lines no wider
/// than `room`, as many as that takes and as even as they can be — the last
/// not one word left on its own — a word too wide alone on a line.
fn wrap_balanced(words: &[&str], widths: &[f32], space: f32, room: f32) -> Vec<String> {
    let greedy = |most: f32| -> Vec<std::ops::Range<usize>> {
        let mut lines = Vec::new();
        let (mut start, mut wide) = (0, 0.0_f32);
        for (i, w) in widths.iter().enumerate() {
            let tried = if i == start { *w } else { wide + space + w };
            if i > start && tried > most {
                lines.push(start..i);
                start = i;
                wide = *w;
            } else {
                wide = tried;
            }
        }
        if start < widths.len() {
            lines.push(start..widths.len());
        }
        lines
    };
    let lines = greedy(room);
    // The narrowest width that still takes no more lines.
    let (mut low, mut high) = (room / lines.len().max(1) as f32, room);
    for _ in 0..16 {
        let mid = f32::midpoint(low, high);
        if greedy(mid).len() <= lines.len() {
            high = mid;
        } else {
            low = mid;
        }
    }
    greedy(high)
        .into_iter()
        .map(|range| words[range].join(" "))
        .collect()
}

/// How small a full-frame picture's line is made to fit its box before it
/// is wrapped instead: below this it would be hard to read.
const WRAP_BELOW: f32 = 0.75;

/// A field's line being laid out: its text, size (a share of the base),
/// weight, colour, and whether it carries on the line before it (wrapped).
type LaidLine = (String, f32, u16, [u8; 3], bool);

/// [`full_frame_text`]'s lines, laid out.
#[allow(clippy::cast_precision_loss)]
fn full_frame_lines(
    spec: &Spec,
    (bx, by, bw, bh): (f32, f32, f32, f32),
    fonts: &Arc<usvg::fontdb::Database>,
) -> Vec<BoxLine> {
    use crate::engine::value::text::TextAlign;
    // Kept a little in from each side — a fiftieth of the box and a pixel
    // for its smoothing — so a line run to its edge does not meet a plate's
    // rounded end (see the lining up, below).
    let margin = 0.02 * bw + 1.0;
    let room = (bw - 2.0 * margin).max(1.0);
    let look = &spec.look;
    let [_, _, name_c, title_c] = colours(look);
    let lines: Vec<(String, f32, u16, [u8; 3])> = [
        (spec.row.name.trim(), 1.0, 600, name_c),
        (spec.row.title.trim(), TITLE_SIZE, 400, title_c),
        (spec.row.title2.trim(), TITLE2_SIZE, 400, title_c),
    ]
    .into_iter()
    .filter(|(text, ..)| !text.is_empty())
    .map(|(text, share, weight, colour)| (text.to_string(), share, weight, colour))
    .collect();
    // Each line as tall as its letters reach in this font — an accented
    // capital (Ž) above, a tail (ą, j) below — so none leaves the box; a
    // gap of 0.3 of the name's size between fields, of 0.2 of its own
    // between a field's wrapped lines.
    let words: Vec<(&str, u16)> = lines.iter().map(|l| (l.0.as_str(), l.2)).collect();
    let (ascent, descent) =
        ink_reach(&look.font, &words, fonts).unwrap_or((BOX_ASCENT, BOX_DESCENT));
    let height_of = |lines: &[LaidLine]| {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let gap = match (i, l.4) {
                    (0, _) => 0.0,
                    (_, true) => 0.2 * l.1,
                    (_, false) => 0.3,
                };
                l.1 * (ascent + descent) + gap
            })
            .sum::<f32>()
    };
    let fields: Vec<LaidLine> = lines
        .into_iter()
        .map(|(text, share, weight, colour)| (text, share, weight, colour, false))
        .collect();
    // As tall as fill the box.
    let base = (bh / height_of(&fields).max(0.1)).max(4.0);
    // A line's ink, from its origin: left and right. Lined up and fitted by
    // its ink, not its advance — a letter reaching left of its origin
    // (Georgia's bold A) is not cut at the box's edge.
    let ink = |text: &str, px: f32, weight: u16| {
        ink_across(text, px, weight, &look.font, fonts)
            .unwrap_or((0.0, px * 0.55 * text.chars().count() as f32))
    };
    let width = |text: &str, px: f32, weight: u16| {
        let (left, right) = ink(text, px, weight);
        right - left
    };
    // Each field on its own: as it is if it fits the width, else smaller
    // (to three quarters), else wrapped at words — at three quarters of its
    // size times `shrink`. `wrap` off: each line as it is, or smaller to fit
    // (any size). Each line's field's own size comes back with them.
    let lay = |shrink: f32, wrap: bool| -> (Vec<LaidLine>, Vec<f32>) {
        let mut laid = Vec::new();
        let mut own = Vec::new();
        for (text, share, weight, colour, _) in &fields {
            let fit = (room / width(text, base * share, *weight).max(1.0)).min(1.0);
            if !wrap || fit >= WRAP_BELOW {
                laid.push((text.clone(), share * fit, *weight, *colour, false));
                own.push(*share);
                continue;
            }
            let field = *share;
            let share = share * WRAP_BELOW * shrink;
            let px = base * share;
            // Each word measured once; a line as wide as its words and the
            // spaces between.
            let words: Vec<&str> = text.split_whitespace().collect();
            let widths: Vec<f32> = words.iter().map(|w| width(w, px, *weight)).collect();
            let space = (width("a a", px, *weight) - width("aa", px, *weight)).max(0.0);
            for (i, line) in wrap_balanced(&words, &widths, space, room)
                .into_iter()
                .enumerate()
            {
                laid.push((line, share, *weight, *colour, i > 0));
                own.push(field);
            }
        }
        (laid, own)
    };
    // A word wider than the box still: that line as small as fits it. Then
    // the lines together no taller than the box: all smaller if they must.
    // Back: the lines, how much smaller all are, and the smallest line's
    // share of its own size, all told.
    let finish = |(mut laid, own): (Vec<LaidLine>, Vec<f32>)| {
        for (text, share, weight, _, _) in &mut laid {
            let w = width(text, base * *share, *weight);
            if w > room {
                *share *= room / w;
            }
        }
        let all = (bh / (height_of(&laid) * base).max(1.0)).min(1.0);
        let least = laid
            .iter()
            .zip(&own)
            .map(|(line, field)| line.1 / field.max(1e-3))
            .fold(1.0_f32, f32::min);
        (laid, all, least * all)
    };
    // The ways to lay them: each line on one line (smaller to fit), or the
    // long ones wrapped — at three quarters, or smaller, wrapped again —
    // the lines then all smaller together as far as they must be to fit the
    // box's height. Whichever leaves its smallest line the largest: a title
    // a little long stays on its line, the name as set; a long one goes
    // onto even lines, the name a little smaller if it must be, rather than
    // the title tiny.
    let mut best = finish(lay(1.0, false));
    let mut shrink = 1.0_f32;
    while shrink > 0.4 {
        let wrapped = finish(lay(shrink, true));
        if wrapped.2 > best.2 + 1e-4 {
            best = wrapped;
        }
        shrink *= 0.9;
    }
    let (laid, all, _) = best;
    let unit = base * all;
    let total = height_of(&laid) * unit;
    let mut y = by + (bh - total) / 2.0;
    let mut out = Vec::new();
    for (i, (text, share, weight, colour, continues)) in laid.into_iter().enumerate() {
        let px = unit * share;
        if i > 0 {
            y += if continues { 0.2 * px } else { 0.3 * unit };
        }
        let baseline = y + ascent * px;
        y += (ascent + descent) * px;
        let (left, right) = ink(&text, px, weight);
        let w = right - left;
        // In from the edge it is lined up to by the margin.
        let x = match look.text_align {
            TextAlign::Left => bx + margin,
            TextAlign::Centre => bx + (bw - w) / 2.0,
            TextAlign::Right => bx + bw - margin - w,
        } - left;
        out.push((text, px, weight, colour, x, baseline));
    }
    out
}

/// A full-frame picture as read: its pixels (no more than 8K across), and
/// the box of those that show (x, y, width, height), if any do.
type FullPicture = Option<Arc<(image::RgbaImage, Option<(u32, u32, u32, u32)>)>>;

thread_local! {
    /// The full-frame picture last read on this thread (the painter's), by
    /// its file and that file's time: a name changed or the look tweaked
    /// does not read it again.
    #[allow(clippy::type_complexity)]
    static FULL_PICTURE: std::cell::RefCell<Option<((String, std::time::SystemTime), FullPicture)>> =
        const { std::cell::RefCell::new(None) };
}

fn full_picture(path: &str) -> FullPicture {
    if path.trim().is_empty() {
        return None;
    }
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let key = (path.to_string(), modified);
    if let Some(made) = FULL_PICTURE.with(|p| {
        p.borrow()
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, made)| made.clone())
    }) {
        return made;
    }
    let made = read_full_picture(path);
    FULL_PICTURE.with(|p| *p.borrow_mut() = Some((key, made.clone())));
    made
}

fn read_full_picture(path: &str) -> FullPicture {
    const MOST_SIDE: u32 = 7680;
    let (w, h) = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MOST_PICTURE_PIXELS {
        log::warn!("Full-frame picture {path} not used: {w}×{h}");
        return None;
    }
    let picture = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let picture = if w.max(h) > MOST_SIDE {
        picture.resize(MOST_SIDE, MOST_SIDE, image::imageops::FilterType::Triangle)
    } else {
        picture
    };
    let picture = picture.into_rgba8();
    let shows = visible_box(&picture);
    Some(Arc::new((picture, shows)))
}

/// The box of a picture's pixels that show (x, y, width, height); none
/// when it is clear all over.
fn visible_box(picture: &image::RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = picture.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, px) in picture.enumerate_pixels() {
        if px[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
}

/// The largest a plate picture is kept, on its long side: plenty for a plate
/// at 4K, small to draw again.
const PICTURE_SIDE: u32 = 2048;
/// Pictures past this many pixels are not read: a mistaken huge file would
/// take gigabytes to decode.
const MOST_PICTURE_PIXELS: u64 = 100_000_000;

thread_local! {
    /// The plate picture last read on this thread (the painter's): its file
    /// and that file's time, and what was made of it — so a look tweaked or
    /// a name changed does not read and decode it again.
    #[allow(clippy::type_complexity)]
    static PICTURE: std::cell::RefCell<Option<((String, std::time::SystemTime), Option<(String, f32)>)>> =
        const { std::cell::RefCell::new(None) };
}

/// The picture at `path` for a plate: as a data URI for the SVG (made no
/// bigger than a plate needs), and its width over its height. None when it
/// cannot be read, or is too big to.
fn plate_picture(path: &str) -> Option<(String, f32)> {
    if path.trim().is_empty() {
        return None;
    }
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let key = (path.to_string(), modified);
    if let Some(made) = PICTURE.with(|p| {
        p.borrow()
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, made)| made.clone())
    }) {
        return made;
    }
    let made = read_plate_picture(path);
    PICTURE.with(|p| *p.borrow_mut() = Some((key, made.clone())));
    made
}

fn read_plate_picture(path: &str) -> Option<(String, f32)> {
    use base64::Engine as _;
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let (w, h) = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MOST_PICTURE_PIXELS {
        log::warn!("Plate picture {path} not used: {w}×{h}");
        return None;
    }
    let picture = reader.decode().ok()?;
    let picture = if w.max(h) > PICTURE_SIDE {
        picture.thumbnail(PICTURE_SIDE, PICTURE_SIDE)
    } else {
        picture
    };
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    #[allow(clippy::cast_precision_loss)]
    Some((format!("data:image/png;base64,{data}"), w as f32 / h as f32))
}

fn svg_head(wide: u32, high: u32) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wide}" height="{high}" viewBox="0 0 {wide} {high}">"#
    )
}

fn rasterize(
    svg: &str,
    wide: u32,
    high: u32,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Option<image::RgbaImage> {
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let tree = match usvg::Tree::from_str(svg, &options) {
        Ok(tree) => tree,
        Err(e) => {
            log::warn!("Lower third not drawn: {e}");
            return None;
        }
    };
    let mut pixmap = resvg::tiny_skia::Pixmap::new(wide, high)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut rgba = image::RgbaImage::new(wide, high);
    for (dst, src) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(rgba)
}

/// How wide `text` sets at `size`: none if no glyph of it could be drawn.
fn measure(
    text: &str,
    size: f32,
    weight: u16,
    font: &str,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Option<f32> {
    #[allow(clippy::cast_precision_loss)]
    let wide = size * (text.chars().count() as f32 + 2.0) * 1.5;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wide}" height="{h}"><text x="0" y="{base}" {}>{}</text></svg>"#,
        attributes(font, size, weight, [0, 0, 0]),
        escape(text),
        h = size * 3.0,
        base = size * 2.0,
    );
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&svg, &options).ok()?;
    let text_node = tree.root().children().iter().find_map(|node| match node {
        usvg::Node::Text(text) => Some(text),
        _ => None,
    })?;
    let glyphs = text_node.flattened();
    if !glyphs.has_children() {
        return None;
    }
    let bounds = glyphs.abs_bounding_box();
    (bounds.width() > 0.0).then(|| bounds.right())
}

/// How far `font`'s letters reach above and below the baseline, as shares
/// of the size: those of `lines` (the words drawn, each in its weight) and
/// of accented capitals and tails (so the lines sit alike from person to
/// person), a little room added. The words themselves are measured too: a
/// font lacking one of the probe's letters may have it measured in another
/// font, its own tails reaching further (Chalkduster's p). None if nothing
/// could be measured.
fn ink_reach(
    font: &str,
    lines: &[(&str, u16)],
    fonts: &Arc<usvg::fontdb::Database>,
) -> Option<(f32, f32)> {
    const SIZE: f32 = 100.0;
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let mut reach: Option<(f32, f32)> = None;
    let probes = [(INK_PROBE, 600), (INK_PROBE, 400)];
    for (text, weight) in probes.iter().chain(lines) {
        #[allow(clippy::cast_precision_loss)]
        let wide = SIZE * (text.chars().count() as f32 + 2.0) * 1.5;
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wide}" height="{h}"><text x="0" y="{base}" {}>{}</text></svg>"#,
            attributes(font, SIZE, *weight, [0, 0, 0]),
            escape(text),
            h = SIZE * 3.0,
            base = SIZE * 2.0,
        );
        let Ok(tree) = usvg::Tree::from_str(&svg, &options) else {
            continue;
        };
        let Some(text_node) = tree.root().children().iter().find_map(|node| match node {
            usvg::Node::Text(text) => Some(text),
            _ => None,
        }) else {
            continue;
        };
        let glyphs = text_node.flattened();
        if !glyphs.has_children() {
            continue;
        }
        let bounds = glyphs.abs_bounding_box();
        let up = (2.0 * SIZE - bounds.top()) / SIZE;
        let down = (bounds.bottom() - 2.0 * SIZE) / SIZE;
        reach = Some(match reach {
            Some((u, d)) => (u.max(up), d.max(down)),
            None => (up, down),
        });
    }
    reach.map(|(up, down)| (up + 0.02, down.max(0.0) + 0.02))
}

/// The letters whose reach is measured: accented capitals and tails.
const INK_PROBE: &str = "ŽĄÁÉÛjgyąęų|";

/// A line's ink across, from its origin: its left and right edges, at
/// `size`; none if no glyph of it could be drawn.
fn ink_across(
    text: &str,
    size: f32,
    weight: u16,
    font: &str,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Option<(f32, f32)> {
    #[allow(clippy::cast_precision_loss)]
    let wide = size * (text.chars().count() as f32 + 2.0) * 1.5;
    // Its origin a size in, so ink left of it is measured too.
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wide}" height="{h}"><text x="{size}" y="{base}" {}>{}</text></svg>"#,
        attributes(font, size, weight, [0, 0, 0]),
        escape(text),
        h = size * 3.0,
        base = size * 2.0,
    );
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&svg, &options).ok()?;
    let text_node = tree.root().children().iter().find_map(|node| match node {
        usvg::Node::Text(text) => Some(text),
        _ => None,
    })?;
    let glyphs = text_node.flattened();
    if !glyphs.has_children() {
        return None;
    }
    let bounds = glyphs.abs_bounding_box();
    (bounds.width() > 0.0).then(|| (bounds.left() - size, bounds.right() - size))
}

/// Whether the font named — empty, the system's — has every letter of
/// `letters`: one without them shows boxes on air.
pub fn font_has(font: &str, letters: &str) -> bool {
    let fonts = super::svg::system_fonts();
    let family = if font.trim().is_empty() {
        usvg::fontdb::Family::SansSerif
    } else {
        usvg::fontdb::Family::Name(font.trim())
    };
    let query = usvg::fontdb::Query {
        families: &[family],
        ..Default::default()
    };
    let Some(id) = fonts.query(&query) else {
        return false;
    };
    fonts
        .with_face_data(id, |data, index| {
            ttf_parser::Face::parse(data, index)
                .is_ok_and(|face| letters.chars().all(|c| face.glyph_index(c).is_some()))
        })
        .unwrap_or(false)
}

/// A `<text>`'s font and fill.
fn attributes(font: &str, size: f32, weight: u16, colour: [u8; 3]) -> String {
    format!(
        r#"font-family="'{}', sans-serif" font-size="{size}" font-weight="{weight}" fill="{}""#,
        escape(&font.replace('\'', "")),
        hex(colour),
    )
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `text` safe in SVG: its markup escaped, and control characters — which
/// XML cannot hold, and would leave the whole drawing undrawn — left out (a
/// tab a space).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push(' '),
            c if c.is_control() => {}
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, title: &str) -> Spec {
        Spec {
            row: LowerThirdRow {
                name: name.into(),
                title: title.into(),
                ..LowerThirdRow::default()
            },
            look: LowerThirdLook::default(),
            frame: (1920, 1080),
            picture_saves: 0,
        }
    }

    /// A name and title draw as a plate in the lower left, inside the safe
    /// area, its text over it; a Lithuanian name draws too.
    #[test]
    fn a_name_and_title_draw_on_a_plate_in_the_lower_left() {
        let fonts = super::super::svg::system_fonts();
        let drawn = draw(
            &spec("Justina Prunskienė", "Lietuvos Respublikos Seimo narė"),
            &fonts,
        )
        .expect("drawn");
        let (w, h) = drawn.plate.dimensions();
        assert_eq!(drawn.text.dimensions(), (w, h));
        assert!(
            drawn.at.0 >= 0.05 * 1920.0 - 0.5,
            "inside the safe area: {:?}",
            drawn.at
        );
        assert!(
            drawn.at.1 > 1080.0 / 2.0,
            "in the lower half: {:?}",
            drawn.at
        );
        assert!(
            drawn.at.1 + h as f32 <= 1080.0 * 0.95 + 0.5,
            "above the safe edge"
        );
        // The plate is mostly solid, the text has ink.
        let solid = drawn.plate.pixels().filter(|p| p[3] > 128).count();
        assert!(solid > (w * h / 2) as usize);
        assert!(drawn.text.pixels().any(|p| p[3] > 200));
    }

    /// A name far too long for the room is shrunk to fit, so far and no
    /// further.
    #[test]
    fn a_long_name_is_shrunk_to_fit_its_room() {
        let fonts = super::super::svg::system_fonts();
        let short = draw(&spec("Ana", ""), &fonts).expect("drawn");
        let long = draw(&spec(&"Ilgavardenė ".repeat(12), ""), &fonts).expect("drawn");
        assert!(long.plate.height() < short.plate.height(), "shrunk");
        assert!(
            long.plate.height() as f32 >= short.plate.height() as f32 * (SMALLEST - 0.1),
            "not past its smallest"
        );
    }

    fn styled(style: LowerThirdStyle) -> Spec {
        let mut s = spec("Justina Prunskienė", "Seimo narė");
        s.look.style = style;
        s.look.palette = 0;
        s.look.plate = [0.1, 0.1, 0.6];
        s.look.bar_colour = [0.9, 0.2, 0.1];
        s.look.plate_opacity = 1.0;
        s.look.bar = LowerThirdBar::None;
        s
    }

    /// Each style draws its own shape: Two-tone a band of the bar's colour
    /// over one of the plate's; Pill round ends and Ribbon slanted ones,
    /// their corners clear; Text only no plate, a soft shadow under the
    /// letters.
    #[test]
    fn each_style_draws_its_shape() {
        let fonts = super::super::svg::system_fonts();
        let px = |img: &image::RgbaImage, fx: f32, fy: f32| {
            let (w, h) = img.dimensions();
            *img.get_pixel(((w - 1) as f32 * fx) as u32, ((h - 1) as f32 * fy) as u32)
        };
        let two = draw(&styled(LowerThirdStyle::TwoTone), &fonts).expect("drawn");
        let top = px(&two.plate, 0.98, 0.05);
        let bottom = px(&two.plate, 0.98, 0.95);
        assert!(
            top[0] > 150 && top[2] < 100,
            "the name's band in the bar's colour: {top:?}"
        );
        assert!(
            bottom[2] > 100 && bottom[0] < 80,
            "the lines' band in the plate's: {bottom:?}"
        );
        for style in [LowerThirdStyle::Pill, LowerThirdStyle::Ribbon] {
            let d = draw(&styled(style), &fonts).expect("drawn");
            assert_eq!(px(&d.plate, 0.0, 0.0)[3], 0, "{style:?}: a corner clear");
            assert!(
                px(&d.plate, 0.5, 0.5)[3] > 200,
                "{style:?}: solid in the middle"
            );
        }
        let text_only = draw(&styled(LowerThirdStyle::TextOnly), &fonts).expect("drawn");
        assert!(text_only.plate.pixels().all(|p| p[3] == 0), "no plate");
        assert!(
            text_only.text.pixels().any(|p| p[3] > 40 && p[0] < 60),
            "a dark shadow under the letters"
        );
    }

    /// Text only over a white picture: the ring just round the title's
    /// light letters is dark — they read on a bright picture, not only on a
    /// dark one.
    #[test]
    fn text_only_reads_over_a_white_picture() {
        let fonts = super::super::svg::system_fonts();
        let mut s = styled(LowerThirdStyle::TextOnly);
        s.frame = (1920, 1080);
        let drawn = draw(&s, &fonts).expect("drawn");
        let (wide, high) = drawn.text.dimensions();
        // Over white, as the eye sees it (0 black, 1 white).
        let over: Vec<f32> = drawn
            .text
            .pixels()
            .map(|p| {
                let alpha = f32::from(p[3]) / 255.0;
                let luma = (0.2126 * f32::from(p[0])
                    + 0.7152 * f32::from(p[1])
                    + 0.0722 * f32::from(p[2]))
                    / 255.0;
                luma * alpha + (1.0 - alpha)
            })
            .collect();
        // The title's letters: light blue ink, its lower half of the picture.
        let ink = |x: u32, y: u32| {
            let p = drawn.text.get_pixel(x, y);
            p[3] > 200 && p[2] > 200 && p[0] < 160
        };
        let mut ring = Vec::new();
        for y in high / 2..high {
            for x in 0..wide {
                if ink(x, y) {
                    continue;
                }
                let near = (y.saturating_sub(3)..(y + 4).min(high))
                    .any(|yy| (x.saturating_sub(3)..(x + 4).min(wide)).any(|xx| ink(xx, yy)));
                if near {
                    ring.push(over[(y * wide + x) as usize]);
                }
            }
        }
        assert!(
            ring.len() > 100,
            "the title's letters found: {}",
            ring.len()
        );
        let mean = ring.iter().sum::<f32>() / ring.len() as f32;
        println!(
            "ring round the title over white: {mean:.2} ({} pixels)",
            ring.len()
        );
        assert!(
            mean < 0.45,
            "the title's edge too light over white: {mean:.2}"
        );
    }

    /// A full-frame design for the tests: a 1920×1080 PNG, clear but for a
    /// red 200×100 plate at (100, 800); the text box over the plate.
    fn full_frame(dir: &std::path::Path, title: &str) -> Spec {
        let path = dir.join("design.png");
        let mut design = image::RgbaImage::new(1920, 1080);
        for y in 800..900 {
            for x in 100..300 {
                design.put_pixel(x, y, image::Rgba([230, 20, 20, 255]));
            }
        }
        design.save(&path).expect("saved");
        let mut s = spec("Ūla Žemaitė", title);
        s.look.style = LowerThirdStyle::FullFrame;
        s.look.image = path.to_string_lossy().into_owned();
        s.look.text_box = [
            110.0 / 1920.0,
            805.0 / 1080.0,
            180.0 / 1920.0,
            90.0 / 1080.0,
        ];
        s
    }

    /// Where the plate's red is on the frame: left, top, right, bottom.
    fn red_on_frame(drawn: &Drawn) -> (u32, u32, u32, u32) {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for (x, y, px) in drawn.plate.enumerate_pixels() {
            if px[3] > 128 && px[0] > 150 && px[1] < 80 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
        let (ax, ay) = (drawn.at.0 as u32, drawn.at.1 as u32);
        (ax + x0, ay + y0, ax + x1, ay + y1)
    }

    /// A full-frame picture lands where it was drawn, scaled to the frame
    /// (1920 → 2560 wide: by 4/3), and only what shows is kept — not a
    /// frame-sized picture.
    #[test]
    fn a_full_frame_picture_lands_where_it_was_drawn() {
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        let mut s = full_frame(dir.path(), "Pranešėja");
        s.frame = (2560, 1440);
        let drawn = draw(&s, &fonts).expect("drawn");
        let (x0, y0, x1, y1) = red_on_frame(&drawn);
        let near = |got: u32, want: u32| got.abs_diff(want) <= 2;
        assert!(
            near(x0, 133) && near(y0, 1067) && near(x1, 400) && near(y1, 1200),
            "the plate at ({x0}, {y0})–({x1}, {y1})"
        );
        let (w, h) = drawn.plate.dimensions();
        assert!(w < 400 && h < 200, "only what shows kept: {w}×{h}");
        assert_eq!(drawn.plate.dimensions(), drawn.text.dimensions());
    }

    /// A long title is made smaller, then wrapped, inside the text box: the
    /// picture does not move or grow, and the text stays in its box.
    #[test]
    fn a_long_title_fits_the_text_box_and_leaves_the_picture_alone() {
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        let mut short = full_frame(dir.path(), "Pranešėja");
        short.frame = (1920, 1080);
        let mut long = short.clone();
        long.row.title = "Lietuvos Respublikos švietimo, mokslo ir sporto ministerijos \
                          Strateginių kompetencijų centro vyriausioji patarėja"
            .into();
        let a = draw(&short, &fonts).expect("drawn");
        let b = draw(&long, &fonts).expect("drawn");
        assert_eq!(
            red_on_frame(&a),
            red_on_frame(&b),
            "the picture where it was"
        );
        assert_eq!(a.plate.dimensions(), b.plate.dimensions(), "the same size");
        // Every bit of text inside the box (110, 805)–(290, 895), give or
        // take a pixel of smoothing.
        for (x, y, px) in b.text.enumerate_pixels() {
            if px[3] > 64 {
                let (fx, fy) = (b.at.0 as u32 + x, b.at.1 as u32 + y);
                assert!(
                    (108..=292).contains(&fx) && (803..=897).contains(&fy),
                    "text at ({fx}, {fy}), outside its box"
                );
            }
        }
        assert!(b.text.pixels().any(|p| p[3] > 200), "the text drawn");
    }

    /// A title far too long wraps onto more lines; the name stays near its
    /// size — not the whole text made tiny.
    #[test]
    fn a_long_title_wraps_and_the_name_keeps_its_size() {
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        let short = full_frame(dir.path(), "Pranešėja");
        let mut long = short.clone();
        long.row.title = "Lietuvos Respublikos švietimo, mokslo ir sporto ministerijos \
             Strateginių kompetencijų centro vyriausioji patarėja ir Valstybės \
             duomenų agentūros tarybos pirmininkė"
            .into();
        let text_box = (120.0, 865.0, 960.0, 160.0);
        let a = full_frame_lines(&short, text_box, &fonts);
        let b = full_frame_lines(&long, text_box, &fonts);
        assert!(b.len() > a.len(), "the title wrapped: {b:?}");
        assert!(
            b[0].1 > a[0].1 * 0.75,
            "the name kept: {} vs {}",
            b[0].1,
            a[0].1
        );
        assert!(
            b[1].1 > a[1].1 * 0.3,
            "the title readable: {} vs {}",
            b[1].1,
            a[1].1
        );
    }

    /// A picture made four times bigger, mostly off the frame: the part on
    /// the frame is its middle, where it belongs — the left of the middle
    /// red, the right blue — and no bigger than the frame.
    #[test]
    fn a_picture_made_bigger_shows_its_middle() {
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("halves.png");
        image::RgbaImage::from_fn(1920, 1080, |x, _| {
            if x < 960 {
                image::Rgba([230, 20, 20, 255])
            } else {
                image::Rgba([20, 20, 230, 255])
            }
        })
        .save(&path)
        .expect("saved");
        let mut s = spec("Ūla Žemaitė", "");
        s.look.style = LowerThirdStyle::FullFrame;
        s.look.image = path.to_string_lossy().into_owned();
        s.look.picture_scale = 4.0;
        s.frame = (1920, 1080);
        let drawn = draw(&s, &fonts).expect("drawn");
        assert_eq!(drawn.at, (0.0, 0.0));
        assert_eq!(
            drawn.plate.dimensions(),
            (1920, 1080),
            "no bigger than the frame"
        );
        let left = drawn.plate.get_pixel(100, 540);
        let right = drawn.plate.get_pixel(1800, 540);
        assert!(
            left[0] > 200 && left[2] < 60,
            "the middle's left red: {left:?}"
        );
        assert!(
            right[2] > 200 && right[0] < 60,
            "the middle's right blue: {right:?}"
        );
        // The halves meet in the frame's middle.
        assert!(drawn.plate.get_pixel(950, 540)[0] > 200);
        assert!(drawn.plate.get_pixel(970, 540)[2] > 200);
    }

    /// A name with an accented capital and tails, alone in its box: its
    /// letters stay inside the box, not cut at its top or foot.
    #[test]
    fn accents_and_tails_stay_inside_the_text_box() {
        let fonts = super::super::svg::system_fonts();
        // The system's font, one whose accents reach higher, and one whose
        // tails do (and lacks a probe letter).
        for (font, name) in [
            ("", "Živilė Ąjūnė"),
            ("Verdana", "Živilė Ąjūnė"),
            ("Chalkduster", "Poppy Quigley"),
        ] {
            let mut s = spec(name, "");
            s.look.style = LowerThirdStyle::FullFrame;
            s.look.font = font.into();
            s.look.text_box = [0.1, 0.8, 0.8, 150.0 / 1080.0];
            s.frame = (1920, 1080);
            let drawn = draw(&s, &fonts).expect("drawn");
            // No picture: what is drawn is the box itself.
            let (w, h) = drawn.text.dimensions();
            assert!(
                drawn.text.pixels().any(|p| p[3] > 200),
                "{font}: the name drawn"
            );
            let inked = |y: u32| (0..w).any(|x| drawn.text.get_pixel(x, y)[3] > 16);
            assert!(!inked(0), "{font}: Ž's accent cut at the box's top");
            assert!(!inked(h - 1), "{font}: the tails cut at the box's foot");
        }
    }

    /// Its picture saved again (from Photoshop, say) while it is shown: it
    /// is drawn again, from the new file.
    #[test]
    fn a_picture_saved_again_is_drawn_again() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let dir = tempfile::tempdir().expect("dir");
        let s = full_frame(dir.path(), "Pranešėja");
        let mut settings = LowerThirdSettings {
            rows: vec![s.row.clone()],
            ..LowerThirdSettings::default()
        };
        settings.look = s.look.clone();
        let mut lower = LowerThird::new(&gpu, settings, (1920, 1080)).expect("made");
        lower.show(Some(0));
        let saves = |lower: &LowerThird| lower.main_drawn().map(|s| s.picture_saves);
        let until = Instant::now() + std::time::Duration::from_secs(10);
        while (saves(&lower).is_none() || lower.picture_time.is_none()) && Instant::now() < until {
            lower.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(saves(&lower), Some(0), "drawn once: the first look no save");
        let first = lower.picture_time.expect("its picture seen");
        // Saved again, later.
        let path = std::path::Path::new(&s.look.image);
        image::RgbaImage::from_pixel(1920, 1080, image::Rgba([20, 20, 230, 255]))
            .save(path)
            .expect("saved again");
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(first + std::time::Duration::from_secs(5)))
            .expect("its time set");
        let until = Instant::now() + std::time::Duration::from_secs(10);
        while saves(&lower) == Some(0) && Instant::now() < until {
            lower.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(saves(&lower), Some(1), "drawn again");
    }

    /// Lined up left or right, a letter reaching past its origin (Georgia's
    /// bold A) is not cut at the box's edge.
    #[test]
    fn text_lined_up_to_an_edge_is_not_cut_there() {
        use crate::engine::value::text::TextAlign;
        let fonts = super::super::svg::system_fonts();
        for font in ["", "Georgia"] {
            for align in [TextAlign::Left, TextAlign::Right] {
                let mut s = spec("Alice Avery", "Ąžuolų AV");
                s.look.style = LowerThirdStyle::FullFrame;
                s.look.font = font.into();
                s.look.text_align = align;
                s.look.text_box = [0.1, 0.8, 0.8, 150.0 / 1080.0];
                s.frame = (1920, 1080);
                let drawn = draw(&s, &fonts).expect("drawn");
                // No picture: what is drawn is the box itself.
                let (w, h) = drawn.text.dimensions();
                let inked = |x: u32| (0..h).any(|y| drawn.text.get_pixel(x, y)[3] > 16);
                let edge = if align == TextAlign::Left { 0 } else { w - 1 };
                assert!(!inked(edge), "{font} {align:?}: cut at the box's edge");
                assert!(drawn.text.pixels().any(|p| p[3] > 200), "{font}: drawn");
            }
        }
    }

    /// Its picture not there when shown, there later (a drive plugged in):
    /// drawn again with it.
    #[test]
    fn a_picture_that_comes_later_is_drawn_then() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let dir = tempfile::tempdir().expect("dir");
        let s = full_frame(dir.path(), "Pranešėja");
        let path = std::path::PathBuf::from(&s.look.image);
        let kept = dir.path().join("kept.png");
        std::fs::rename(&path, &kept).expect("moved away");
        let mut settings = LowerThirdSettings {
            rows: vec![s.row.clone()],
            ..LowerThirdSettings::default()
        };
        settings.look = s.look.clone();
        let mut lower = LowerThird::new(&gpu, settings, (1920, 1080)).expect("made");
        lower.show(Some(0));
        let saves = |lower: &LowerThird| lower.main_drawn().map(|s| s.picture_saves);
        let until = Instant::now() + std::time::Duration::from_secs(10);
        while (saves(&lower).is_none() || !lower.picture_seen) && Instant::now() < until {
            lower.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(saves(&lower), Some(0), "drawn without it");
        std::fs::rename(&kept, &path).expect("back");
        let until = Instant::now() + std::time::Duration::from_secs(10);
        while saves(&lower) == Some(0) && Instant::now() < until {
            lower.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(saves(&lower), Some(1), "drawn again, with it");
    }

    /// A long title in Max5's box (his plate's) goes onto even lines at a
    /// readable size, not one tiny line, its ink clear of the box's right
    /// edge (a plate's rounded end); one a little too long stays on its
    /// line, the name exactly as set.
    #[test]
    fn a_long_title_wraps_before_it_gets_small_and_keeps_off_the_edge() {
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        // Max5's box, his title: on even lines (no word left on its own),
        // at a readable size, the name still the largest; clear of the
        // box's right edge.
        let mut s = full_frame(dir.path(), "Pranešėja");
        let low = (0.07 * 1920.0, 0.765 * 1080.0, 0.5 * 1920.0, 0.15 * 1080.0);
        let short = full_frame_lines(&s, low, &fonts);
        s.row.title = "Projektų vadovas, atsakingas už tvarių pakuočių sprendimus \
                       Baltijos šalyse, Skandinavijoje ir Vidurio Europoje"
            .into();
        let lines = full_frame_lines(&s, low, &fonts);
        assert!(lines.len() > short.len(), "wrapped: {lines:?}");
        let title = &lines[1..];
        assert!(
            title.iter().all(|l| l.1 >= short[1].1 * 0.55),
            "readable: {lines:?} vs {}",
            short[1].1
        );
        assert!(lines[0].1 > title[0].1, "the name the largest");
        let lengths: Vec<usize> = title.iter().map(|l| l.0.chars().count()).collect();
        let (shortest, longest) = (lengths.iter().min().unwrap(), lengths.iter().max().unwrap());
        assert!(shortest * 2 >= *longest, "even lines: {lines:?}");
        s.look.text_box = [0.07, 0.765, 0.5, 0.15];
        s.frame = (1920, 1080);
        let drawn = draw(&s, &fonts).expect("drawn");
        let right = low.0 + low.2 - 0.02 * low.2;
        for (x, _, px) in drawn.text.enumerate_pixels() {
            if px[3] > 32 {
                let at = drawn.at.0 + x as f32;
                assert!(at <= right, "ink at {at} past {right}");
            }
        }
        // A title a little too long: on its line, a little smaller, the name
        // exactly as set.
        let mut s = full_frame(dir.path(), "Pranešėja");
        s.row.title = "Pranešėja ir renginio vedėja".into();
        let narrow = (100.0, 800.0, 520.0, 160.0);
        let a = full_frame_lines(&full_frame(dir.path(), "Pranešėja"), narrow, &fonts);
        let b = full_frame_lines(&s, narrow, &fonts);
        assert_eq!(b.len(), a.len(), "one line: {b:?}");
        assert!(
            b[1].1 < a[1].1 && b[1].1 >= a[1].1 * WRAP_BELOW,
            "a little smaller: {} vs {}",
            b[1].1,
            a[1].1
        );
        assert!((b[0].1 - a[0].1).abs() < 0.01, "the name as set");
        // Shrunk to its full width: still clear of the box's right edge.
        s.look.text_box = [
            narrow.0 / 1920.0,
            narrow.1 / 1080.0,
            narrow.2 / 1920.0,
            narrow.3 / 1080.0,
        ];
        s.frame = (1920, 1080);
        let drawn = draw(&s, &fonts).expect("drawn");
        let right = narrow.0 + narrow.2 - 0.02 * narrow.2;
        for (x, _, px) in drawn.text.enumerate_pixels() {
            if px[3] > 32 {
                let at = drawn.at.0 + x as f32;
                assert!(at <= right + 0.5, "ink at {at} past {right}");
            }
        }
    }

    /// A name too long for the box on its own, with a long title: the name
    /// kept as large as its width lets it be — the layout not chosen by a
    /// size it does not keep.
    #[test]
    fn a_long_name_and_title_keep_the_name_as_large_as_it_fits() {
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        let low = (0.07 * 1920.0, 0.765 * 1080.0, 0.5 * 1920.0, 0.15 * 1080.0);
        let mut s = full_frame(dir.path(), "x");
        s.row.name = "Aleksandras-Petrauskas-Kazlauskas-Žemaitienė-Jonaitytė".into();
        let alone = full_frame_lines(&s, low, &fonts);
        s.row.title = "Projektų vadovas, atsakingas už tvarių pakuočių sprendimus \
                       Baltijos šalyse, Skandinavijoje ir Vidurio Europoje"
            .into();
        let both = full_frame_lines(&s, low, &fonts);
        assert!(
            both[0].1 >= alone[0].1 * 0.9,
            "the name: {} with the title, {} without",
            both[0].1,
            alone[0].1
        );
    }

    /// Words broken into as few lines as fit, as even as they can be.
    #[test]
    fn words_wrap_onto_even_lines() {
        let words = ["aaaa", "bbbb", "cccc", "dddd", "ee"];
        let widths = [40.0, 40.0, 40.0, 40.0, 20.0];
        // Room for three words a line: two lines, not four words and one.
        let lines = wrap_balanced(&words, &widths, 10.0, 145.0);
        assert_eq!(lines, vec!["aaaa bbbb", "cccc dddd ee"]);
        // A word wider than the room alone on its line.
        let lines = wrap_balanced(&["x", "wide", "y"], &[5.0, 300.0, 5.0], 2.0, 100.0);
        assert_eq!(lines, vec!["x", "wide", "y"]);
    }

    /// On a bus's LED band (450 × 688) the whole design is fitted inside
    /// it, at its foot.
    #[test]
    fn a_full_frame_design_fits_inside_an_led_band() {
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        let mut s = full_frame(dir.path(), "Pranešėja");
        s.frame = (450, 688);
        let drawn = draw(&s, &fonts).expect("drawn");
        let (x0, y0, x1, y1) = red_on_frame(&drawn);
        // The design 450 × 253 at the band's foot (from 435): its plate at
        // (100, 800)·450/1920 + (0, 435).
        let near = |got: u32, want: u32| got.abs_diff(want) <= 2;
        assert!(
            near(x0, 23) && near(y0, 622) && near(x1, 70) && near(y1, 646),
            "the plate at ({x0}, {y0})–({x1}, {y1})"
        );
    }

    /// The picture comes in first, the text after it; going out, the text
    /// goes first.
    #[test]
    fn a_full_frame_picture_comes_in_before_its_text_and_goes_after_it() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let dir = tempfile::tempdir().expect("dir");
        let s = full_frame(dir.path(), "Pranešėja");
        let mut settings = LowerThirdSettings {
            rows: vec![s.row.clone()],
            ..LowerThirdSettings::default()
        };
        settings.look = s.look.clone();
        settings.look.picture_motion = LowerThirdMotion::Wipe;
        settings.look.picture_secs = 0.5;
        settings.look.picture_delay = 0.0;
        settings.look.text_motion = LowerThirdMotion::Fade;
        settings.look.text_secs = 0.4;
        settings.look.text_delay = 0.3;
        let mut lower = LowerThird::new(&gpu, settings, (1920, 1080)).expect("made");
        let now = Instant::now();
        let ago = |secs: f32| {
            now.checked_sub(std::time::Duration::from_secs_f32(secs))
                .expect("a moment ago")
        };
        let seen = |(alpha, _, revealed): Part| alpha > 0.0 && revealed > 0.0;
        lower.air = Air::In {
            since: Some(ago(0.2)),
        };
        let [plate, text] = lower.full_frame_parts(now);
        assert!(seen(plate) && plate.2 < 1.0, "the picture wiping in");
        assert!(!seen(text), "the text not yet");
        lower.air = Air::In {
            since: Some(ago(0.6)),
        };
        let [plate, text] = lower.full_frame_parts(now);
        assert!(seen(plate) && seen(text), "both by then");
        assert!((lower.motion_secs() - 0.7).abs() < 1e-4, "in for 0.7 s");
        lower.air = Air::Out {
            since: ago(0.5),
            then: None,
        };
        let [plate, text] = lower.full_frame_parts(now);
        assert!(seen(plate) && !seen(text), "the text gone first");
    }

    /// A picture of one's own is the plate: its colours, and its shape —
    /// as wide as it is for the plate's height.
    #[test]
    fn a_picture_of_ones_own_is_the_plate() {
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("plate.png");
        image::RgbaImage::from_pixel(800, 100, image::Rgba([200, 30, 40, 255]))
            .save(&path)
            .expect("saved");
        let mut s = styled(LowerThirdStyle::Image);
        s.row.name = "Ona".into();
        s.row.title = String::new();
        s.look.image = path.to_string_lossy().into_owned();
        let d = draw(&s, &fonts).expect("drawn");
        let (w, h) = d.plate.dimensions();
        let aspect = w as f32 / h as f32;
        assert!((aspect - 8.0).abs() < 0.3, "its own shape: {aspect:.2}");
        let middle = d.plate.get_pixel(w / 2, h / 2);
        assert!(middle[0] > 180 && middle[1] < 60, "its colours: {middle:?}");
        // Not there: a plain plate meanwhile, not nothing.
        s.look.image = dir.path().join("gone.png").to_string_lossy().into_owned();
        assert!(draw(&s, &fonts).is_some());
    }

    /// A very wide picture keeps the plate within the frame, and draws; a
    /// big one is kept no bigger than a plate needs; at 0% Plate the picture
    /// is not shown.
    #[test]
    fn a_wide_or_big_picture_is_kept_in_bounds() {
        use base64::Engine as _;
        let fonts = super::super::svg::system_fonts();
        let dir = tempfile::tempdir().expect("dir");
        let wide = dir.path().join("wide.png");
        image::RgbaImage::from_pixel(10_000, 100, image::Rgba([200, 30, 40, 255]))
            .save(&wide)
            .expect("saved");
        let mut s = styled(LowerThirdStyle::Image);
        s.row.title = String::new();
        s.look.image = wide.to_string_lossy().into_owned();
        let d = draw(&s, &fonts).expect("drawn");
        assert!(
            d.plate.width() <= 1920,
            "within the frame: {}",
            d.plate.width()
        );
        let big = dir.path().join("big.png");
        image::RgbaImage::from_pixel(4000, 1000, image::Rgba([10, 200, 40, 255]))
            .save(&big)
            .expect("saved");
        let (uri, aspect) = plate_picture(&big.to_string_lossy()).expect("read");
        assert!((aspect - 4.0).abs() < 0.01);
        let png = base64::engine::general_purpose::STANDARD
            .decode(uri.trim_start_matches("data:image/png;base64,"))
            .expect("base64");
        let kept = image::load_from_memory(&png).expect("a picture");
        assert!(kept.width() <= PICTURE_SIDE && kept.height() <= PICTURE_SIDE);
        s.look.plate_opacity = 0.0;
        let clear = draw(&s, &fonts).expect("drawn");
        assert!(
            clear.plate.pixels().all(|p| p[3] == 0),
            "0% Plate: the picture not shown"
        );
    }

    /// A ribbon's top bar follows its slant: its corner stays clear.
    #[test]
    fn a_ribbons_top_bar_follows_its_slant() {
        let fonts = super::super::svg::system_fonts();
        let mut s = styled(LowerThirdStyle::Ribbon);
        s.look.bar = LowerThirdBar::Top;
        let d = draw(&s, &fonts).expect("drawn");
        assert_eq!(d.plate.get_pixel(1, 1)[3], 0, "the top-left corner clear");
    }

    /// No name, nothing drawn.
    #[test]
    fn a_row_without_a_name_draws_nothing() {
        let fonts = super::super::svg::system_fonts();
        assert!(draw(&spec("  ", "title"), &fonts).is_none());
    }

    /// A name shown again while the cued one was being drawn: the cued
    /// one's drawing, back late, is not put in its place.
    #[test]
    fn a_late_drawing_of_another_name_is_not_put() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let fonts = super::super::svg::system_fonts();
        let a = spec("Ana Ąžuolaitė", "Pirmoji");
        let b = spec("Bronius Čepas", "Antrasis");
        let settings = LowerThirdSettings {
            rows: vec![a.row.clone(), b.row.clone()],
            ..LowerThirdSettings::default()
        };
        let mut lower = LowerThird::new(&gpu, settings, (1920, 1080)).expect("made");
        lower.show(Some(0));
        let a = lower.wanted().first().cloned().expect("a wanted");
        lower.came_back(a.clone(), draw(&a, &fonts));
        assert_eq!(lower.main_drawn(), Some(&a));
        // Off, and on again before the cued name's drawing is back.
        lower.air = Air::Off;
        lower.showing = None;
        let b = lower
            .wanted()
            .first()
            .cloned()
            .expect("the cued one wanted");
        assert_eq!(b.row.name, "Bronius Čepas");
        lower.show(Some(0));
        lower.came_back(b.clone(), draw(&b, &fonts));
        assert_eq!(lower.main_drawn(), Some(&a), "the late one is not put");
    }

    /// The name on air fixed twice before the first fix's drawing is back:
    /// that drawing, back late, is put — newer than the one on air — and
    /// once another name is shown, it no longer is.
    #[test]
    fn typing_on_the_name_on_air_shows_each_drawing_as_it_comes() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let fonts = super::super::svg::system_fonts();
        let a = spec("Ana", "Pirmoji");
        let b = spec("Bronius Čepas", "Antrasis");
        let settings = LowerThirdSettings {
            rows: vec![a.row.clone(), b.row.clone()],
            ..LowerThirdSettings::default()
        };
        let mut lower = LowerThird::new(&gpu, settings.clone(), (1920, 1080)).expect("made");
        lower.show(Some(0));
        let on = lower.wanted().first().cloned().expect("wanted");
        lower.came_back(on.clone(), draw(&on, &fonts));
        lower.air = Air::On {
            since: Instant::now(),
        };
        let typed = |lower: &mut LowerThird, name: &str| {
            let mut s = lower.settings().clone();
            s.rows[0].name = name.into();
            lower.edit_settings(s);
            lower.wanted().first().cloned().expect("wanted")
        };
        let anas = typed(&mut lower, "Ana S");
        let _ = typed(&mut lower, "Ana Sa");
        lower.came_back(anas.clone(), draw(&anas, &fonts));
        assert_eq!(
            lower.main_drawn(),
            Some(&anas),
            "the first fix, back late, put"
        );
        // However many fixes come while one is drawn, it is put when back.
        let slow = typed(&mut lower, "Ana Sa.");
        let mut later = None;
        for n in 0..50 {
            let spec = typed(&mut lower, &format!("Ana Sa {n}"));
            if n == 10 {
                later = Some(spec);
            }
        }
        let later = later.expect("a later fix");
        lower.came_back(slow.clone(), draw(&slow, &fonts));
        assert_eq!(lower.main_drawn(), Some(&slow), "put after many fixes");
        // Words older than one put cannot come back: let go.
        assert!(!lower.fixed_from.contains(&anas.row));
        // Fixed again, taken off and another name shown: a late drawing of
        // the fixes before is not put.
        let _ = typed(&mut lower, "Ana Sab");
        lower.hide();
        lower.air = Air::Off;
        lower.tick();
        lower.show(Some(1));
        assert!(lower.fixed_from.is_empty(), "forgotten once off");
        lower.came_back(later.clone(), draw(&later, &fonts));
        assert_eq!(
            lower.main_drawn(),
            Some(&slow),
            "not after another is shown"
        );
    }

    /// Shown on a bus too, its name fixed before either drawing is in: the
    /// drawing for its own frame alone does not bring it in — the bus's
    /// frame has none yet.
    #[test]
    fn a_fix_drawn_for_one_frame_does_not_bring_it_in_for_all() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let fonts = super::super::svg::system_fonts();
        let a = spec("Ana", "Pirmoji");
        let settings = LowerThirdSettings {
            rows: vec![a.row.clone()],
            ..LowerThirdSettings::default()
        };
        let mut lower = LowerThird::new(&gpu, settings, (1920, 1080)).expect("made");
        lower.set_target_frames(&[(450, 688)]);
        lower.show(Some(0));
        let own = lower
            .wanted()
            .into_iter()
            .find(|s| s.frame == (1920, 1080))
            .expect("its own frame wanted");
        let mut fixed = lower.settings().clone();
        fixed.rows[0].name = "Ana S".into();
        lower.edit_settings(fixed);
        lower.came_back(own.clone(), draw(&own, &fonts));
        assert_eq!(
            lower.moved_on(Instant::now()),
            Air::In { since: None },
            "waits for the bus's frame"
        );
    }

    /// A drawing thrown away (its row removed before it came back) is asked
    /// for again should it be wanted again — or Show would wait for ever.
    #[test]
    fn a_drawing_thrown_away_is_asked_for_again() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let fonts = super::super::svg::system_fonts();
        let a = spec("Ana Ąžuolaitė", "Pirmoji");
        let settings = LowerThirdSettings {
            rows: vec![a.row.clone()],
            ..LowerThirdSettings::default()
        };
        let mut lower = LowerThird::new(&gpu, settings.clone(), (1920, 1080)).expect("made");
        lower.tick();
        let asked = lower
            .asked
            .clone()
            .and_then(|batch| batch.first().cloned())
            .expect("the cued row asked for");
        lower.set_settings(LowerThirdSettings::default());
        lower.came_back(asked.clone(), draw(&asked, &fonts));
        assert!(lower.main_drawn().is_none(), "not wanted now: not put");
        assert!(lower.asked.is_none(), "to be asked for again");
    }

    /// The cue follows the same person when the list is read again: the
    /// second of two alike stays the second; gone, the same place.
    #[test]
    fn the_cue_follows_the_same_occurrence() {
        let row = |n: &str| LowerThirdRow {
            name: n.into(),
            ..LowerThirdRow::default()
        };
        let (a, b, x) = (row("A"), row("B"), row("X"));
        let old = [a.clone(), a.clone(), b.clone()];
        let new = [x.clone(), a.clone(), a.clone(), b.clone()];
        assert_eq!(followed(&old, &new, 1), 2, "the second A");
        assert_eq!(followed(&old, &new, 0), 1, "the first A");
        assert_eq!(followed(&old, &new, 2), 3, "B");
        assert_eq!(
            followed(&[a.clone(), b.clone()], &[b, x], 0),
            0,
            "A gone: the same place"
        );
        assert_eq!(followed(&[a], &[], 0), 0);
    }

    /// The system's font has the Lithuanian letters.
    #[test]
    fn the_system_font_has_the_lithuanian_letters() {
        assert!(font_has(
            "",
            crate::engine::value::lower_third::LITHUANIAN_LETTERS
        ));
    }
}
