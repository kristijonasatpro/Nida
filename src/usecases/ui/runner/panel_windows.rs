//! Panels in windows of their own (see `screen_layout::PaneWindow`): each an
//! egui immediate viewport the dock shows in the control window's pass
//! (`panels::dock`), given here a winit window, egui input and a surface of
//! its own — painted with the control window's egui renderer and textures,
//! after the control window, in the same frame.
//!
//! egui runs a viewport's pass inside the control window's, through a
//! callback it keeps for this thread ([`PanelWindows::install`]); the
//! callback reaches the windows through [`Shared`]. A viewport first shown
//! has no window yet: its pass runs unseen, and its window is made after the
//! frame ([`PanelWindows::make_wanted`]). One no longer shown is closed
//! ([`PanelWindows::close_unshown`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowButtons, WindowId};

use crate::renderer::context::{GpuContext, WindowSurface};

/// The panels' windows, shared with egui's viewport callback.
#[derive(Default)]
pub(super) struct PanelWindows {
    shared: Rc<RefCell<Shared>>,
}

#[derive(Default)]
struct Shared {
    windows: HashMap<egui::ViewportId, PanelWindow>,
    /// Viewports shown this frame with no window yet, and how each asked to
    /// look: made once the frame is drawn.
    wanted: Vec<(egui::ViewportId, egui::ViewportBuilder)>,
    /// What each window's pass drew this frame, to paint.
    drawn: Vec<Drawn>,
    /// The textures every pass changed — the windows' and the control
    /// window's — in the order egui handed them out: applied in that order
    /// before anything is painted, freed after (see
    /// [`PanelWindows::take_textures`]). Kept for the next frame when this
    /// one is not painted.
    textures: Vec<egui::TexturesDelta>,
    /// The control window's scale: a pass with no window yet is laid out
    /// at it.
    scale: f32,
}

// The flags are unrelated things heard of the window (its first reading,
// closed, shown, focused, covered), not one state.
#[allow(clippy::struct_excessive_bools)]
struct PanelWindow {
    window: Arc<Window>,
    state: egui_winit::State,
    surface: WindowSurface,
    info: egui::ViewportInfo,
    /// Its viewport info not yet read: the first reading asks more.
    fresh: bool,
    /// Closed by its own close button: told to the dock in the next pass.
    close_asked: bool,
    /// Shown by the dock this frame: one that is not is closed.
    shown: bool,
    /// Its keys pressed and not let go (see [`HeldKeys`]).
    held: HeldKeys,
    focused: bool,
    /// Covered whole, or minimized: not painted (macOS may hold back its
    /// next picture to draw in, and the control window would wait for it).
    occluded: bool,
    title: String,
    close_button: bool,
}

/// The keys pressed in a window and not yet let go there. A key held down
/// since before the window had the keyboard — pressed in another of Nida's
/// windows, or another app — repeats into it, and egui, which never saw it
/// go down, takes its first repeat for a press: one more take, one more
/// slide. Such repeats are kept from the window.
#[derive(Default)]
pub(super) struct HeldKeys(HashSet<winit::keyboard::PhysicalKey>);

impl HeldKeys {
    /// Whether `event` is the window's to have: all but a repeat of a key
    /// not pressed in it. A window that loses the keyboard holds none.
    pub fn takes(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => self.key(
                event.physical_key,
                event.state.is_pressed(),
                event.repeat,
                *is_synthetic,
            ),
            WindowEvent::Focused(false) => {
                self.0.clear();
                true
            }
            _ => true,
        }
    }

    /// A key's press (`pressed`), repeat or release: whether the window has
    /// it. A synthetic press — winit's, as the window takes the keyboard
    /// with the key held — is no press here (egui ignores it).
    fn key(
        &mut self,
        key: winit::keyboard::PhysicalKey,
        pressed: bool,
        repeat: bool,
        synthetic: bool,
    ) -> bool {
        match (pressed, repeat) {
            (true, false) => {
                if !synthetic {
                    self.0.insert(key);
                }
                true
            }
            (true, true) => self.0.contains(&key),
            (false, _) => {
                self.0.remove(&key);
                true
            }
        }
    }
}

/// What a window's pass drew this frame.
struct Drawn {
    id: egui::ViewportId,
    jobs: Vec<egui::ClippedPrimitive>,
    pixels_per_point: f32,
}

impl PanelWindows {
    /// Have the viewports `ctx` shows drawn in windows of their own, here —
    /// not inside the control window, as egui does with no backend's help.
    pub fn install(&self, ctx: &egui::Context) {
        ctx.set_embed_viewports(false);
        let shared = Rc::downgrade(&self.shared);
        egui::Context::set_immediate_viewport_renderer(move |ctx, viewport| {
            run_viewport(ctx, &shared, viewport);
        });
    }

    /// Before the control window's pass: no window shown yet, nothing drawn;
    /// `scale` the control window's.
    pub fn begin_frame(&self, scale: f32) {
        let mut s = self.shared.borrow_mut();
        s.scale = scale;
        s.drawn.clear();
        for window in s.windows.values_mut() {
            window.shown = false;
        }
    }

    /// The control window's own textures changed by its pass, after the
    /// windows' (whose passes ran inside it).
    pub fn push_textures(&self, delta: egui::TexturesDelta) {
        self.shared.borrow_mut().textures.push(delta);
    }

    /// Every pass's texture changes since they were last taken, in order.
    pub fn take_textures(&self) -> Vec<egui::TexturesDelta> {
        std::mem::take(&mut self.shared.borrow_mut().textures)
    }

    /// Each texture drawn in the windows this frame, and the size in pixels
    /// of each time it is drawn: their previews are encoded too.
    pub fn for_each_drawn(&self, mut f: impl FnMut(egui::TextureId, [u32; 2])) {
        let s = self.shared.borrow();
        for drawn in &s.drawn {
            for job in &drawn.jobs {
                if let egui::epaint::Primitive::Mesh(mesh) = &job.primitive {
                    let size = mesh.calc_bounds().size() * drawn.pixels_per_point;
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    f(mesh.texture_id, [size.x.ceil() as u32, size.y.ceil() as u32]);
                }
            }
        }
    }

    /// Paint what each window's pass drew, with the control window's egui
    /// renderer: its textures already set.
    pub fn paint(&self, gpu: &GpuContext, renderer: &mut egui_wgpu::Renderer) {
        let mut s = self.shared.borrow_mut();
        let drawn = std::mem::take(&mut s.drawn);
        for drawn in &drawn {
            let Some(window) = s.windows.get_mut(&drawn.id) else {
                continue;
            };
            if window.occluded {
                continue;
            }
            paint(
                gpu,
                renderer,
                &mut window.surface,
                &drawn.jobs,
                drawn.pixels_per_point,
            );
        }
    }

    /// Close the windows the dock did not show this frame: their panels put
    /// back, or closed.
    pub fn close_unshown(&self) {
        self.shared.borrow_mut().windows.retain(|_, window| window.shown);
    }

    /// Put the window `id`, if it is open, at `rect` — its frame's corner and
    /// the size inside it, in egui's points at the UI's `zoom` — as a layout
    /// chosen asks: only on a screen still connected, else it stays where it
    /// is (as one made new opens beside the control window). One not open
    /// yet is made there anyway.
    pub fn place(&self, id: egui::ViewportId, rect: [f32; 4], zoom: f32) {
        let shared = self.shared.borrow();
        let Some(panel) = shared.windows.get(&id) else {
            return;
        };
        let screens = screens_of(panel.window.available_monitors());
        if let Some([x, y, w, h]) = placed_at(rect, zoom, &screens) {
            panel.window.set_outer_position(winit::dpi::LogicalPosition::new(x, y));
            let _ = panel.window.request_inner_size(winit::dpi::LogicalSize::new(w, h));
        }
    }

    /// Make the windows the dock asked for this frame, each beside the
    /// control window `beside` when it has no place on a screen there now,
    /// its surface as `like` (the control window's: drawn by the same
    /// renderer). Returns those that could not be, and why.
    pub fn make_wanted(
        &self,
        ctx: &egui::Context,
        event_loop: &ActiveEventLoop,
        gpu: &GpuContext,
        like: &wgpu::SurfaceConfiguration,
        beside: Option<&Window>,
    ) -> Vec<(egui::ViewportId, String)> {
        let wanted = std::mem::take(&mut self.shared.borrow_mut().wanted);
        let mut failed = Vec::new();
        for (id, builder) in wanted {
            let made = self.shared.borrow().windows.len();
            match make(ctx, event_loop, gpu, like, beside, made, id, &builder) {
                Ok(window) => {
                    log::info!("Panel window \"{}\" opened", window.title);
                    self.shared.borrow_mut().windows.insert(id, window);
                }
                Err(e) => {
                    let title = builder.title.unwrap_or_default();
                    log::warn!("Panel window \"{title}\" could not be opened: {e:#}");
                    failed.push((id, format!("{title}: {e:#}")));
                }
            }
        }
        failed
    }

    /// An event of window `id`, taken when it is a panel's window: into its
    /// egui input — its keys too: the dock runs the show's shortcuts in its
    /// pass, on them. Returns whether it was a panel's window's.
    pub fn window_event(&self, id: WindowId, event: &WindowEvent, gpu: Option<&GpuContext>) -> bool {
        let mut s = self.shared.borrow_mut();
        let Some(window) = s.windows.values_mut().find(|w| w.window.id() == id) else {
            return false;
        };
        if !window.held.takes(event) {
            return true;
        }
        let _ = window.state.on_window_event(&window.window, event);
        match event {
            WindowEvent::CloseRequested => window.close_asked = true,
            WindowEvent::Resized(size) => {
                if let Some(gpu) = gpu {
                    window.surface.resize(gpu, *size);
                }
            }
            WindowEvent::Focused(focused) => window.focused = *focused,
            WindowEvent::Occluded(occluded) => window.occluded = *occluded,
            _ => {}
        }
        true
    }

    /// Whether a panel's window has the keyboard.
    pub fn any_focused(&self) -> bool {
        self.shared.borrow().windows.values().any(|w| w.focused)
    }
}

/// egui's callback for a viewport shown: its pass run, on its window's
/// input — unseen when it has no window yet, as egui wants it run all the
/// same — and what it drew kept to paint.
fn run_viewport(
    ctx: &egui::Context,
    shared: &Weak<RefCell<Shared>>,
    viewport: egui::ImmediateViewport<'_>,
) {
    let egui::ImmediateViewport {
        ids,
        builder,
        mut viewport_ui_cb,
    } = viewport;
    let id = ids.this;
    let shared = shared.upgrade();
    let input = shared.as_ref().and_then(|shared| {
        let mut s = shared.borrow_mut();
        if let Some(window) = s.windows.get_mut(&id) {
            window.shown = true;
            window.follow(&builder);
            return Some(window.take_input(ctx, id));
        }
        if !s.wanted.iter().any(|(wanted, _)| *wanted == id) {
            s.wanted.push((id, builder.clone()));
        }
        None
    });
    let scale = shared.as_ref().map_or(1.0, |s| s.borrow().scale);
    let input = input.unwrap_or_else(|| unseen_input(id, &builder, scale));
    let output = ctx.run_ui(input, |ui| viewport_ui_cb(ui));
    let jobs = ctx.tessellate(output.shapes, output.pixels_per_point);
    let Some(shared) = shared else {
        return;
    };
    let mut s = shared.borrow_mut();
    s.textures.push(output.textures_delta);
    let Some(window) = s.windows.get_mut(&id) else {
        return;
    };
    window
        .state
        .handle_platform_output(&window.window, output.platform_output);
    s.drawn.push(Drawn {
        id,
        jobs,
        pixels_per_point: output.pixels_per_point,
    });
}

/// The input of a viewport with no window yet: its size as asked, the
/// control window's scale, nothing done.
fn unseen_input(id: egui::ViewportId, builder: &egui::ViewportBuilder, scale: f32) -> egui::RawInput {
    let size = builder.inner_size.unwrap_or(egui::vec2(480.0, 320.0));
    let info = egui::ViewportInfo {
        native_pixels_per_point: Some(scale),
        ..Default::default()
    };
    egui::RawInput {
        viewport_id: id,
        viewports: std::iter::once((id, info)).collect(),
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        ..Default::default()
    }
}

impl PanelWindow {
    /// The window's input since its last pass, with where it is now and its
    /// close asked, when it was.
    fn take_input(&mut self, ctx: &egui::Context, id: egui::ViewportId) -> egui::RawInput {
        egui_winit::update_viewport_info(&mut self.info, ctx, &self.window, self.fresh);
        self.fresh = false;
        if std::mem::take(&mut self.close_asked) {
            self.info.events.push(egui::ViewportEvent::Close);
        }
        let mut input = self.state.take_egui_input(&self.window);
        input.viewports = std::iter::once((id, self.info.clone())).collect();
        self.info.events.clear();
        // Files dragged in from another app: the real pointer, which the OS
        // does not send while the drag runs (as the control window's).
        if !input.hovered_files.is_empty() || !input.dropped_files.is_empty() {
            if let Some((x, y)) = super::drag_position::pointer_in_window(&self.window) {
                let zoom = f64::from(ctx.zoom_factor());
                #[allow(clippy::cast_possible_truncation)]
                input.events.push(egui::Event::PointerMoved(egui::pos2(
                    (x / zoom) as f32,
                    (y / zoom) as f32,
                )));
            }
        }
        input
    }

    /// Its title, and whether it can be closed, as the dock now has them.
    fn follow(&mut self, builder: &egui::ViewportBuilder) {
        if let Some(title) = builder.title.as_ref().filter(|t| **t != self.title) {
            self.window.set_title(title);
            title.clone_into(&mut self.title);
        }
        let close_button = builder.close_button.unwrap_or(true);
        if close_button != self.close_button {
            let mut buttons = self.window.enabled_buttons();
            buttons.set(WindowButtons::CLOSE, close_button);
            self.window.set_enabled_buttons(buttons);
            self.close_button = close_button;
        }
    }
}

/// A panel's window as `builder` asks — where it was, when that is on a
/// screen still; else beside the control window, the `made`th — and its
/// surface, configured as `like`.
#[allow(clippy::too_many_arguments)]
fn make(
    ctx: &egui::Context,
    event_loop: &ActiveEventLoop,
    gpu: &GpuContext,
    like: &wgpu::SurfaceConfiguration,
    beside: Option<&Window>,
    made: usize,
    id: egui::ViewportId,
    builder: &egui::ViewportBuilder,
) -> anyhow::Result<PanelWindow> {
    use anyhow::Context as _;
    let mut attrs = egui_winit::create_winit_window_attributes(ctx, builder.clone());
    let zoom = f64::from(ctx.zoom_factor());
    let kept = builder.position.map(|p| (f64::from(p.x) * zoom, f64::from(p.y) * zoom));
    let screens = screens_of(event_loop.available_monitors());
    let kept = kept.filter(|at| on_a_screen(*at, &screens));
    if kept.is_none() {
        attrs.position = beside.and_then(|main| {
            let at = main.outer_position().ok()?.to_logical::<f64>(main.scale_factor());
            #[allow(clippy::cast_precision_loss)]
            let step = 40.0 + 28.0 * (made % 8) as f64;
            Some(winit::dpi::LogicalPosition::new(at.x + step, at.y + step).into())
        });
    }
    let attrs = crate::app::app_id::tagged(attrs, crate::app::app_id::PANEL);
    let window = Arc::new(event_loop.create_window(attrs).context("no window")?);
    // Where it was kept is its frame's corner, as egui reads it back; macOS
    // makes a window with its inside's corner there, a title bar lower.
    if let Some((x, y)) = kept {
        window.set_outer_position(winit::dpi::LogicalPosition::new(x, y));
    }
    // As the control window: macOS takes the window as live once asked to
    // draw (wgpu#5722).
    window.request_redraw();
    let size = window.inner_size();
    let surface = gpu
        .instance
        .create_surface(window.clone())
        .context("no surface for it")?;
    let formats = surface.get_capabilities(&gpu.adapter).formats;
    anyhow::ensure!(
        formats.contains(&like.format),
        "its screen does not take the control window's colours ({:?})",
        like.format
    );
    let mut config = like.clone();
    config.width = size.width.max(1);
    config.height = size.height.max(1);
    gpu.configure_surface(&surface, &config);
    let state = egui_winit::State::new(
        ctx.clone(),
        id,
        &*window,
        Some(window.scale_factor() as f32),
        None,
        Some(2 * 1024),
    );
    Ok(PanelWindow {
        title: window.title(),
        close_button: builder.close_button.unwrap_or(true),
        window,
        state,
        surface: WindowSurface {
            surface,
            default_present_mode: config.present_mode,
            surface_config: config,
            size,
        },
        info: egui::ViewportInfo::default(),
        fresh: true,
        close_asked: false,
        shown: false,
        held: HeldKeys::default(),
        focused: false,
        occluded: false,
    })
}

/// Whether a window whose frame's top-left corner is at `at` (logical
/// points) has its title bar on one of `screens` (each its left, top,
/// width and height, logical): a screen unplugged since leaves it on none.
/// The screens connected, each its corner and size in logical points.
fn screens_of(monitors: impl Iterator<Item = winit::monitor::MonitorHandle>) -> Vec<[f64; 4]> {
    monitors
        .map(|m| {
            let scale = m.scale_factor();
            let at = m.position().to_logical::<f64>(scale);
            let size = m.size().to_logical::<f64>(scale);
            [at.x, at.y, size.width, size.height]
        })
        .collect()
}

/// Where a window kept at `rect` — egui's points at the UI's `zoom` — goes,
/// in logical points: its frame's corner and its inside's size; none when
/// that corner is on no screen connected now.
fn placed_at([x, y, w, h]: [f32; 4], zoom: f32, screens: &[[f64; 4]]) -> Option<[f64; 4]> {
    let zoom = f64::from(zoom);
    let at = (f64::from(x) * zoom, f64::from(y) * zoom);
    on_a_screen(at, screens).then(|| [at.0, at.1, f64::from(w) * zoom, f64::from(h) * zoom])
}

fn on_a_screen(at: (f64, f64), screens: &[[f64; 4]]) -> bool {
    // A grip of the title bar: a little in from the corner.
    let (x, y) = (at.0 + 40.0, at.1 + 12.0);
    screens
        .iter()
        .any(|[left, top, w, h]| x >= *left && x < left + w && y >= *top && y < top + h)
}

/// Draw `jobs` into the window's surface, and show them.
fn paint(
    gpu: &GpuContext,
    renderer: &mut egui_wgpu::Renderer,
    surface: &mut WindowSurface,
    jobs: &[egui::ClippedPrimitive],
    pixels_per_point: f32,
) {
    let _ = gpu.device.poll(wgpu::PollType::Poll);
    let frame = match surface.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame)
        | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
        wgpu::CurrentSurfaceTexture::Outdated => {
            gpu.configure_surface(&surface.surface, &surface.surface_config);
            match surface.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
                other => {
                    log::debug!("Panel window surface unavailable: {other:?}");
                    return;
                }
            }
        }
        other => {
            log::debug!("Panel window surface unavailable: {other:?}");
            return;
        }
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Panel Window Encoder"),
        });
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [surface.size.width, surface.size.height],
        pixels_per_point,
    };
    renderer.update_buffers(&gpu.device, &gpu.queue, &mut encoder, jobs, &screen);
    {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Panel Window Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let mut pass = pass.forget_lifetime();
        renderer.render(&mut pass, jobs, &screen);
    }
    gpu.submit(std::iter::once(encoder.finish()));
    gpu.queue.present(frame);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A place kept is used while a screen has it; a screen unplugged since
    /// leaves the window to open beside the control window.
    #[test]
    fn a_window_opens_where_it_was_only_while_a_screen_is_there() {
        let screens = [[0.0, 0.0, 1512.0, 982.0], [1512.0, -200.0, 1920.0, 1080.0]];
        assert!(on_a_screen((100.0, 50.0), &screens), "this Mac's");
        assert!(on_a_screen((2000.0, 300.0), &screens), "the screen beside it");
        assert!(!on_a_screen((3600.0, 300.0), &screens), "past them");
        assert!(!on_a_screen((100.0, -500.0), &screens), "above them");
        assert!(!on_a_screen((100.0, 50.0), &[]), "no screens known");
    }

    /// A layout chosen puts a window open already where it keeps it — at
    /// the UI's zoom, as egui's points are — but not on a screen no longer
    /// connected: there it stays where it is.
    #[test]
    fn a_window_is_placed_at_the_uis_zoom_on_a_screen_still_there() {
        let screens = [[0.0, 0.0, 1512.0, 982.0], [1512.0, -200.0, 1920.0, 1080.0]];
        assert_eq!(
            placed_at([1100.0, 100.0, 600.0, 400.0], 2.0, &screens),
            Some([2200.0, 200.0, 1200.0, 800.0]),
            "zoomed: twice the points"
        );
        assert_eq!(
            placed_at([100.0, 50.0, 600.0, 400.0], 1.0, &screens),
            Some([100.0, 50.0, 600.0, 400.0])
        );
        assert_eq!(
            placed_at([3600.0, 300.0, 600.0, 400.0], 1.0, &screens),
            None,
            "its screen unplugged: left where it is"
        );
    }

    /// A key held since before the window had the keyboard repeats into it
    /// unheard; one pressed in it repeats as it does; a synthetic press (the
    /// window taking the keyboard, the key held) is no press here; losing
    /// the keyboard, the window holds none.
    #[test]
    fn a_key_held_since_before_a_window_had_the_keyboard_is_not_its() {
        use winit::keyboard::{KeyCode, PhysicalKey};
        let page = PhysicalKey::Code(KeyCode::PageDown);
        let mut held = HeldKeys::default();
        assert!(!held.key(page, true, true, false), "held elsewhere: unheard");
        assert!(held.key(page, false, false, false), "its release does no harm");
        assert!(held.key(page, true, false, false), "pressed here");
        assert!(held.key(page, true, true, false), "its repeats are");
        assert!(held.key(page, false, false, false));
        assert!(!held.key(page, true, true, false), "let go: no more");
        assert!(held.key(page, true, false, true), "a synthetic press passes (egui drops it)");
        assert!(!held.key(page, true, true, false), "but holds nothing");
        assert!(held.key(page, true, false, false));
        assert!(held.takes(&WindowEvent::Focused(false)));
        assert!(!held.key(page, true, true, false), "lost the keyboard: holds none");
    }

    /// A pass with no window yet is laid out at its size asked and the
    /// control window's scale, and finds its viewport's info: egui looks
    /// for it.
    #[test]
    fn an_unseen_pass_has_its_viewport() {
        let id = egui::ViewportId::from_hash_of("panel");
        let builder = egui::ViewportBuilder::default().with_inner_size([300.0, 200.0]);
        let input = unseen_input(id, &builder, 2.0);
        assert_eq!(input.viewport_id, id);
        assert_eq!(input.screen_rect.map(|r| r.size()), Some(egui::vec2(300.0, 200.0)));
        assert_eq!(input.viewports[&id].native_pixels_per_point, Some(2.0));
    }
}
