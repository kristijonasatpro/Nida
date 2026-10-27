//! The rundown end to end: a step changes every screen of a wall as one
//! picture, takes Main, starts what it starts — all of it or none — and GO
//! walks the show in order.

use std::time::Duration;

use varda::app::VardaApp;
use varda::engine::value::led::{LedFit, LedLayout, LedScreen};
use varda::engine::value::timer::{TimerMode, TimerSettings};
use varda::engine::value::video::AutoPlayback;
use varda::engine::{
    BusInputRef, CommandResult, EngineCommand, RundownSnapshot, RundownStepSnapshot,
};
use varda::mixer::{MainSource, MixMode};
use varda::renderer::context::{OutputSource, OutputTarget, UnifiedOutput};
use varda::rundown::{PlayAction, TimerSet};
use varda::usecases::ui::UILayoutState;

mod common;

const W: u32 = 320;
const H: u32 = 180;

fn app_in(workspace: &std::path::Path) -> Option<VardaApp> {
    use clap::Parser as _;
    let gpu = common::headless_gpu()?;
    let config = varda::app::AppConfig::parse_from([
        "varda",
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        "--no-camera",
        "--workspace",
        workspace.to_str().unwrap(),
    ]);
    Some(VardaApp::new(gpu, &config).expect("VardaApp::new"))
}

fn send(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    app.command_sender().send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

fn id(result: CommandResult) -> String {
    match result {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected an id, got {other:?}"),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn ok(result: CommandResult) {
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
}

/// Why a command was refused.
#[allow(clippy::needless_pass_by_value)]
fn refused(result: CommandResult) -> String {
    match result {
        CommandResult::Err { message, .. } => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn ch(uuid: &str) -> BusInputRef {
    BusInputRef::Channel(uuid.to_string())
}

/// A show in Main mode: channels A (red), B (green) and C (blue), and a wall
/// of three buses — Left, Middle, Right — all on A. The app's own PGM bus is
/// there too, on Master.
struct Show {
    app: VardaApp,
    a: String,
    b: String,
    c: String,
    wall: [String; 3],
    _dir: tempfile::TempDir,
}

fn show() -> Option<Show> {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path())?;
    ok(send(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: W,
            height: H,
        },
    ));
    ok(send(
        &mut app,
        EngineCommand::SetMixMode {
            mode: MixMode::Program,
        },
    ));
    let state = app.build_engine_state();
    let a = state.mixer.channels[0].uuid.clone();
    let b = state.mixer.channels[1].uuid.clone();
    let c = id(send(&mut app, EngineCommand::AddChannel));
    for (channel, color) in [
        (&a, [1.0, 0.0, 0.0, 1.0]),
        (&b, [0.0, 1.0, 0.0, 1.0]),
        (&c, [0.0, 0.0, 1.0, 1.0]),
    ] {
        id(send(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: channel.clone(),
                color,
            },
        ));
    }
    let wall = ["Left", "Middle", "Right"].map(|name| {
        let bus = id(send(
            &mut app,
            EngineCommand::AddBus {
                name: name.into(),
                width: W,
                height: H,
            },
        ));
        cut_bus(&mut app, &bus, ch(&a));
        bus
    });
    Some(Show {
        app,
        a,
        b,
        c,
        wall,
        _dir: dir,
    })
}

fn cut_bus(app: &mut VardaApp, bus: &str, input: BusInputRef) {
    ok(send(
        app,
        EngineCommand::SetBusPreview {
            uuid: bus.to_string(),
            input,
        },
    ));
    ok(send(
        app,
        EngineCommand::BusCut {
            uuid: bus.to_string(),
        },
    ));
}

/// A bus set off on a take of its own, a minute long.
fn slow_fade(app: &mut VardaApp, bus: &str, input: BusInputRef) {
    ok(send(
        app,
        EngineCommand::SetBusPreview {
            uuid: bus.to_string(),
            input,
        },
    ));
    ok(send(
        app,
        EngineCommand::BusAuto {
            uuid: bus.to_string(),
            duration_secs: Some(60.0),
        },
    ));
}

/// A new step of what the wall shows now.
fn add_step(show: &mut Show, name: &str) -> String {
    id(send(
        &mut show.app,
        EngineCommand::AddRundownStep {
            name: name.into(),
            buses: Some(show.wall.to_vec()),
            main: false,
        },
    ))
}

/// A new step with the wall on these three inputs.
fn wall_step(show: &mut Show, name: &str, inputs: [&BusInputRef; 3]) -> String {
    let step = add_step(show, name);
    for (bus, input) in show.wall.clone().iter().zip(inputs) {
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownScreen {
                uuid: step.clone(),
                bus_uuid: bus.clone(),
                input: Some(input.clone()),
            },
        ));
    }
    step
}

fn take(app: &mut VardaApp, step: &str, fade: bool) -> CommandResult {
    send(
        app,
        EngineCommand::TakeRundownStep {
            uuid: step.to_string(),
            fade,
        },
    )
}

fn go(app: &mut VardaApp, expect: Option<&str>) -> CommandResult {
    send(
        app,
        EngineCommand::RundownGo {
            fade: false,
            expect: expect.map(str::to_string),
        },
    )
}

fn rundown(app: &VardaApp) -> RundownSnapshot {
    app.build_engine_state().rundown
}

fn step_named(app: &VardaApp, name: &str) -> RundownStepSnapshot {
    rundown(app)
        .steps
        .into_iter()
        .find(|s| s.step.name == name)
        .unwrap_or_else(|| panic!("no step {name}"))
}

/// The step the show is on, and the one GO takes, by name.
fn place(app: &VardaApp) -> (Option<String>, Option<String>) {
    let rundown = rundown(app);
    let name = |uuid: &Option<String>| {
        uuid.as_ref().and_then(|uuid| {
            rundown
                .steps
                .iter()
                .find(|s| &s.step.uuid == uuid)
                .map(|s| s.step.name.clone())
        })
    };
    (name(&rundown.last), name(&rundown.next))
}

fn names(app: &VardaApp) -> Vec<String> {
    rundown(app).steps.into_iter().map(|s| s.step.name).collect()
}

/// What each bus of the wall is on, or heading to.
fn wall_shows(show: &Show) -> Vec<BusInputRef> {
    show.wall
        .iter()
        .map(|bus| {
            let bus = show.app.production_buses().get(bus).expect("the bus");
            bus.heading().0.clone()
        })
        .collect()
}

fn fading(show: &Show) -> Vec<bool> {
    show.wall
        .iter()
        .map(|bus| show.app.production_buses().get(bus).unwrap().is_transitioning())
        .collect()
}

fn on_main(app: &VardaApp) -> MainSource {
    app.build_engine_state().mixer.main.program
}

/// One engine frame: state moved on, the mixer and the outputs drawn.
fn frame(app: &mut VardaApp) {
    app.advance_frame_state();
    app.render_mixer_frame();
    app.render_outputs();
}

/// A started headless output carrying the wall: three 60×40 screens side by
/// side, each showing its bus.
fn wall_feed(show: &mut Show) -> String {
    let app = &mut show.app;
    ok(send(
        app,
        EngineCommand::CreateHeadlessOutput {
            target: OutputTarget::NdiSend {
                sender_name: "LED".into(),
            },
        },
    ));
    let output = app
        .build_engine_state()
        .outputs
        .windows
        .last()
        .unwrap()
        .uuid
        .clone();
    ok(send(
        app,
        EngineCommand::StartOutput {
            output_uuid: output.clone(),
        },
    ));
    let screens = show
        .wall
        .iter()
        .zip(["Left", "Middle", "Right"])
        .enumerate()
        .map(|(i, (bus, name))| LedScreen {
            fit: LedFit::Fill,
            ..LedScreen::new(
                name,
                [60, 40],
                [20 + 100 * i32::try_from(i).unwrap(), 30],
                OutputSource::BusProgram(bus.clone()),
            )
        })
        .collect();
    ok(send(
        app,
        EngineCommand::SetOutputLed {
            output_uuid: output.clone(),
            layout: Some(LedLayout {
                feed: [W, H],
                screens,
                test_card: false,
            }),
        },
    ));
    output
}

fn output_pixels(app: &VardaApp, uuid: &str) -> Vec<[u8; 4]> {
    let tex = app
        .outputs_ref()
        .iter()
        .find_map(|o| match o {
            UnifiedOutput::Headless(h) if h.uuid == uuid => Some(&h.texture),
            _ => None,
        })
        .expect("the output");
    let ctx = app.gpu_context();
    let (w, h) = (tex.width(), tex.height());
    let padded =
        (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rundown readback"),
        size: u64::from(padded * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    ctx.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = ctx.device.poll(wgpu::PollType::wait_indefinitely());
    let data = buffer.slice(..).get_mapped_range().unwrap();
    (0..h)
        .flat_map(|y| {
            let row = &data[(y * padded) as usize..];
            (0..w as usize)
                .map(move |x| [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]])
        })
        .collect()
}

/// The middle pixel of each of the wall's three screens.
fn wall_pixels(app: &VardaApp, output: &str) -> [[u8; 4]; 3] {
    let px = output_pixels(app, output);
    [0u32, 1, 2].map(|i| px[((30 + 20) * W + 20 + 100 * i + 30) as usize])
}

fn near(got: [u8; 4], want: [u8; 3]) -> bool {
    (0..3).all(|c| got[c].abs_diff(want[c]) <= 3)
}

// ── Every screen at once ───────────────────────────────────────────

/// What the operator asked for: the three parts of a wall fade as one
/// picture. On every frame of the fade the three screens show the same
/// blend, pixel for pixel, and they land on the same frame — though each
/// bus has a different AUTO time of its own.
#[test]
fn every_screen_of_a_wall_fades_as_one_picture() {
    let Some(mut show) = show() else {
        return;
    };
    let output = wall_feed(&mut show);
    for (bus, secs) in show.wall.clone().iter().zip([0.2, 3.0, 9.0]) {
        ok(send(
            &mut show.app,
            EngineCommand::SetBusAutoDuration {
                uuid: bus.clone(),
                duration_secs: secs,
            },
        ));
    }
    let (b, a) = (ch(&show.b), ch(&show.a));
    let talk = wall_step(&mut show, "Talk", [&b, &b, &b]);
    ok(send(&mut show.app, EngineCommand::SetRundownFade { secs: 0.5 }));
    for _ in 0..3 {
        frame(&mut show.app);
    }
    let before = wall_pixels(&show.app, &output);
    assert!(before.iter().all(|p| near(*p, [255, 0, 0])), "all on A: {before:?}");

    ok(take(&mut show.app, &talk, true));
    let mut blends = 0;
    let mut frames = 0;
    loop {
        frame(&mut show.app);
        frames += 1;
        let [left, middle, right] = wall_pixels(&show.app, &output);
        assert!(
            left == middle && middle == right,
            "frame {frames}: the screens are apart — {left:?} {middle:?} {right:?}"
        );
        let landed = fading(&show).iter().all(|f| !f);
        if !near(left, [255, 0, 0]) && !near(left, [0, 255, 0]) {
            blends += 1;
            assert!(!landed, "a blend on a wall that has landed");
        }
        if landed {
            break;
        }
        assert!(frames < 2000, "the fade never ended");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(blends >= 5, "the wall was seen mid-fade on {blends} frames only");
    frame(&mut show.app);
    let after = wall_pixels(&show.app, &output);
    assert!(after.iter().all(|p| near(*p, [0, 255, 0])), "all on B: {after:?}");
    assert_eq!(wall_shows(&show), [b.clone(), b.clone(), b]);

    // And a cut: every screen on the very next frame.
    let back = wall_step(&mut show, "Back", [&a, &a, &a]);
    ok(take(&mut show.app, &back, false));
    frame(&mut show.app);
    let cut = wall_pixels(&show.app, &output);
    assert!(cut.iter().all(|p| near(*p, [255, 0, 0])), "all on A: {cut:?}");
}

/// A step sets only what it lists: a bus it leaves alone, Main and the
/// stream's own bus are untouched by a change of the wall.
#[test]
fn a_step_leaves_alone_what_it_does_not_set() {
    let Some(mut show) = show() else {
        return;
    };
    let (a, b, c) = (ch(&show.a), ch(&show.b), ch(&show.c));
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.c.clone()),
            fade: false,
        },
    ));
    let step = wall_step(&mut show, "Sides", [&b, &a, &b]);
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownScreen {
            uuid: step.clone(),
            bus_uuid: show.wall[1].clone(),
            input: None,
        },
    ));
    let middle = show.wall[1].clone();
    cut_bus(&mut show.app, &middle, c.clone());
    let pgm = show.app.build_engine_state().buses[0].clone();

    ok(take(&mut show.app, &step, false));
    assert_eq!(wall_shows(&show), [b.clone(), c, b]);
    assert_eq!(on_main(&show.app), MainSource::Channel(show.c.clone()));
    let pgm_now = show.app.build_engine_state().buses[0].clone();
    assert_eq!((pgm_now.program, pgm_now.preview), (pgm.program, pgm.preview));
    assert!(step_named(&show.app, "Sides").shown);
}

// ── All of it or none ──────────────────────────────────────────────

/// A fade one of its buses cannot join changes nothing — not the other
/// buses, not Main, not the timer it starts, not where the show is — and
/// says which bus. A cut takes all of it.
#[test]
fn a_step_is_taken_whole_or_not_at_all() {
    let Some(mut show) = show() else {
        return;
    };
    let (b, c) = (ch(&show.b), ch(&show.c));
    let timer = id(send(
        &mut show.app,
        EngineCommand::AddTimerDeck {
            channel_uuid: show.c.clone(),
            settings: Some(TimerSettings {
                mode: TimerMode::Countdown,
                duration_secs: 600,
                ..TimerSettings::default()
            }),
        },
    ));
    let first = add_step(&mut show, "A");
    let step = wall_step(&mut show, "B", [&b, &b, &b]);
    for cmd in [
        EngineCommand::SetRundownMain {
            uuid: step.clone(),
            main: Some(MainSource::Channel(show.b.clone())),
        },
        EngineCommand::SetRundownPlay {
            uuid: step.clone(),
            deck_uuid: timer.clone(),
            action: Some(PlayAction::Start),
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    ok(take(&mut show.app, &first, false));
    let right = show.wall[2].clone();
    slow_fade(&mut show.app, &right, c.clone());
    let timer_running = |app: &VardaApp| {
        let state = app.build_engine_state();
        let decks = state.mixer.channels.iter().flat_map(|ch| &ch.decks);
        let deck = decks.into_iter().find(|d| d.uuid == timer);
        deck.and_then(|d| d.timer.clone()).expect("a timer").running
    };
    let before = (wall_shows(&show), fading(&show), on_main(&show.app));
    assert_eq!(step_named(&show.app, "B").fading, ["Right"]);

    let why = refused(take(&mut show.app, &step, true));
    assert_eq!(why, "Step 'B' not taken — still fading: Right");
    assert_eq!((wall_shows(&show), fading(&show), on_main(&show.app)), before);
    assert!(!timer_running(&show.app));
    assert_eq!(place(&show.app), (Some("A".into()), Some("B".into())));

    ok(take(&mut show.app, &step, false));
    assert_eq!(wall_shows(&show), [b.clone(), b.clone(), b]);
    assert_eq!(fading(&show), [false; 3]);
    assert_eq!(on_main(&show.app), MainSource::Channel(show.b.clone()));
    assert!(timer_running(&show.app));
    assert_eq!(place(&show.app), (Some("B".into()), None));
}

/// A step made for things that have since changed under it is refused whole
/// and says what: Main's input turned into an overlay, an overlay turned
/// back into an input, a timer set to run by the clock, Main mode switched
/// off. Put right, it is taken.
#[test]
fn a_step_that_names_something_gone_is_refused_and_says_what() {
    let Some(mut show) = show() else {
        return;
    };
    let b = ch(&show.b);
    let timer = id(send(
        &mut show.app,
        EngineCommand::AddTimerDeck {
            channel_uuid: show.a.clone(),
            settings: None,
        },
    ));
    let overlay = id(send(&mut show.app, EngineCommand::AddChannel));
    let set_overlay = |app: &mut VardaApp, channel: &str, overlay: bool| {
        ok(send(
            app,
            EngineCommand::SetChannelOverlay {
                channel_uuid: channel.to_string(),
                overlay,
            },
        ));
    };
    set_overlay(&mut show.app, &overlay, true);
    // Something on Main: switched to with nothing on it, Main mode takes a
    // channel of its own accord.
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
    ));
    let step = wall_step(&mut show, "B", [&b, &b, &b]);
    for cmd in [
        EngineCommand::SetRundownMain {
            uuid: step.clone(),
            main: Some(MainSource::Channel(show.c.clone())),
        },
        EngineCommand::SetRundownOverlay {
            uuid: step.clone(),
            channel_uuid: overlay.clone(),
            on: Some(true),
        },
        EngineCommand::SetRundownPlay {
            uuid: step.clone(),
            deck_uuid: timer.clone(),
            action: Some(PlayAction::Start),
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    assert!(step_named(&show.app, "B").missing.is_empty());
    let before = (wall_shows(&show), on_main(&show.app));

    // Each on its own, then put right.
    set_overlay(&mut show.app, &show.c.clone(), true);
    let why = refused(take(&mut show.app, &step, false));
    assert!(why.contains("for Main (it is an overlay now)"), "{why}");
    set_overlay(&mut show.app, &show.c.clone(), false);

    set_overlay(&mut show.app, &overlay, false);
    let why = refused(take(&mut show.app, &step, false));
    assert!(why.contains("as an overlay (it is not one now)"), "{why}");
    set_overlay(&mut show.app, &overlay, true);

    let clock = TimerSettings {
        mode: TimerMode::Clock,
        ..TimerSettings::default()
    };
    ok(send(
        &mut show.app,
        EngineCommand::SetTimerSettings {
            deck_uuid: timer.clone(),
            settings: clock,
        },
    ));
    let why = refused(take(&mut show.app, &step, false));
    assert!(why.contains("runs by the clock"), "{why}");
    assert_eq!(step_named(&show.app, "B").missing.len(), 1);
    ok(send(
        &mut show.app,
        EngineCommand::SetTimerSettings {
            deck_uuid: timer.clone(),
            settings: TimerSettings::default(),
        },
    ));

    ok(send(
        &mut show.app,
        EngineCommand::SetMixMode {
            mode: MixMode::Layered,
        },
    ));
    let why = refused(take(&mut show.app, &step, false));
    assert!(why.contains("the layered mix is on"), "{why}");
    ok(send(
        &mut show.app,
        EngineCommand::SetMixMode {
            mode: MixMode::Program,
        },
    ));

    // Refused every time, nothing moved.
    assert_eq!((wall_shows(&show), on_main(&show.app)), before);
    assert_eq!(place(&show.app), (None, Some("B".into())));

    ok(take(&mut show.app, &step, false));
    assert_eq!(on_main(&show.app), MainSource::Channel(show.c.clone()));
    assert_eq!(show.app.build_engine_state().mixer.main.overlays_on, [overlay]);
}

/// A step cannot be made against what is not there: a bus, an input, an
/// overlay that is not one, something that cannot be started.
#[test]
fn a_step_is_not_made_against_what_is_not_there() {
    let Some(mut show) = show() else {
        return;
    };
    let step = add_step(&mut show, "A");
    let solid = show.app.build_engine_state().mixer.channels[0].decks[0]
        .uuid
        .clone();
    let cases = [
        EngineCommand::SetRundownScreen {
            uuid: step.clone(),
            bus_uuid: "nosuchbus".into(),
            input: Some(BusInputRef::Black),
        },
        EngineCommand::SetRundownScreen {
            uuid: step.clone(),
            bus_uuid: show.wall[0].clone(),
            input: Some(ch("nosuchid")),
        },
        EngineCommand::SetRundownMain {
            uuid: step.clone(),
            main: Some(MainSource::Channel("nosuchid".into())),
        },
        EngineCommand::SetRundownOverlay {
            uuid: step.clone(),
            channel_uuid: show.a.clone(),
            on: Some(true),
        },
        EngineCommand::SetRundownPlay {
            uuid: step.clone(),
            deck_uuid: solid,
            action: Some(PlayAction::Start),
        },
        EngineCommand::SetRundownThen {
            uuid: step.clone(),
            secs: Some(0.0),
        },
        EngineCommand::RenameRundownStep {
            uuid: step.clone(),
            name: "  ".into(),
        },
        EngineCommand::AddRundownStep {
            name: String::new(),
            buses: None,
            main: false,
        },
        EngineCommand::AddRundownStep {
            name: "B".into(),
            buses: Some(vec!["nosuchbus".into()]),
            main: false,
        },
        EngineCommand::SetRundownFade { secs: 0.0 },
        EngineCommand::TakeRundownStep {
            uuid: "nosuchid".into(),
            fade: false,
        },
        EngineCommand::SetRundownNext {
            uuid: "nosuchid".into(),
        },
    ];
    let before = step_named(&show.app, "A").step;
    for cmd in cases {
        let text = format!("{cmd:?}");
        let result = send(&mut show.app, cmd);
        assert!(matches!(result, CommandResult::Err { .. }), "{text}: {result:?}");
    }
    assert_eq!(step_named(&show.app, "A").step, before);
    assert_eq!(names(&show.app), ["A"]);
}

// ── GO, and going on by itself ─────────────────────────────────────

/// GO takes the steps in order and stops at the end. A press that names the
/// step it was meant for is refused when the rundown is already past it —
/// so a press landing just after a step went on by itself skips nothing.
#[test]
fn go_walks_the_rundown_and_a_late_press_skips_nothing() {
    let Some(mut show) = show() else {
        return;
    };
    let (a, b, c) = (ch(&show.a), ch(&show.b), ch(&show.c));
    let steps = [
        wall_step(&mut show, "A", [&b, &b, &b]),
        wall_step(&mut show, "B", [&c, &c, &c]),
        wall_step(&mut show, "C", [&a, &b, &c]),
    ];
    assert_eq!(place(&show.app), (None, Some("A".into())));
    assert_eq!(refused(go(&mut show.app, Some(&steps[1]))), "The rundown has moved on — 'A' is next now");
    ok(go(&mut show.app, Some(&steps[0])));
    assert_eq!(wall_shows(&show), [b.clone(), b.clone(), b.clone()]);
    assert_eq!(place(&show.app), (Some("A".into()), Some("B".into())));

    ok(go(&mut show.app, Some(&steps[1])));
    // The same press again, late: B is not taken twice, and C is not taken.
    let why = refused(go(&mut show.app, Some(&steps[1])));
    assert_eq!(why, "The rundown has moved on — 'C' is next now");
    assert_eq!(wall_shows(&show), [c.clone(), c.clone(), c.clone()]);
    assert_eq!(place(&show.app), (Some("B".into()), Some("C".into())));

    ok(go(&mut show.app, None));
    assert_eq!(wall_shows(&show), [a, b, c]);
    assert_eq!(refused(go(&mut show.app, None)), "The rundown is at its end");
    assert_eq!(place(&show.app), (Some("C".into()), None));

    // Back to the top without taking anything.
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownNext {
            uuid: steps[0].clone(),
        },
    ));
    assert_eq!(place(&show.app), (Some("C".into()), Some("A".into())));
    for step in &steps {
        ok(send(
            &mut show.app,
            EngineCommand::RemoveRundownStep { uuid: step.clone() },
        ));
    }
    assert_eq!(refused(go(&mut show.app, None)), "The rundown has no steps");
}

/// A step with a wait goes on to the next by itself, cut or faded as it was
/// taken; Hold stops that, and so does the operator taking a step.
#[test]
fn a_step_goes_on_by_itself_and_hold_stops_it() {
    let Some(mut show) = show() else {
        return;
    };
    let (b, c) = (ch(&show.b), ch(&show.c));
    let steps = [
        wall_step(&mut show, "A", [&b, &b, &b]),
        wall_step(&mut show, "B", [&c, &c, &c]),
        add_step(&mut show, "C"),
    ];
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownThen {
            uuid: steps[0].clone(),
            secs: Some(0.15),
        },
    ));
    let wait = |app: &mut VardaApp, secs: f64| {
        std::thread::sleep(Duration::from_secs_f64(secs));
        app.advance_frame_state();
    };

    ok(take(&mut show.app, &steps[0], false));
    let counting = rundown(&show.app).wait.expect("counting");
    assert_eq!((counting.from, counting.to, counting.fade), (steps[0].clone(), steps[1].clone(), false));
    assert!(counting.secs_left > 0.0 && counting.secs_left <= 0.15);
    wait(&mut show.app, 0.02);
    assert_eq!(place(&show.app).0, Some("A".into()), "not before its time");
    wait(&mut show.app, 0.2);
    assert_eq!(place(&show.app), (Some("B".into()), Some("C".into())));
    assert_eq!(wall_shows(&show), [c.clone(), c.clone(), c.clone()]);
    assert!(rundown(&show.app).wait.is_none(), "B has no wait of its own");

    // Held: the show stays on A until the operator says.
    ok(take(&mut show.app, &steps[0], false));
    ok(send(&mut show.app, EngineCommand::RundownHold));
    wait(&mut show.app, 0.25);
    assert_eq!(place(&show.app), (Some("A".into()), Some("B".into())));

    // The operator takes another step during the wait: nothing follows it.
    ok(take(&mut show.app, &steps[0], false));
    ok(take(&mut show.app, &steps[2], false));
    wait(&mut show.app, 0.25);
    assert_eq!(place(&show.app), (Some("C".into()), None));

    // Even a press that is refused is the operator stepping in: the wait is
    // over, and the show stays on A rather than going on behind their back.
    ok(take(&mut show.app, &steps[0], false));
    let right = show.wall[2].clone();
    slow_fade(&mut show.app, &right, ch(&show.a));
    refused(take(&mut show.app, &steps[1], true));
    cut_bus(&mut show.app, &right, b.clone());
    assert!(rundown(&show.app).wait.is_none());
    wait(&mut show.app, 0.25);
    assert_eq!(place(&show.app), (Some("A".into()), Some("B".into())));

    // Faded to, the step it leads to is faded to.
    ok(send(&mut show.app, EngineCommand::SetRundownFade { secs: 0.05 }));
    ok(take(&mut show.app, &steps[0], true));
    wait(&mut show.app, 0.25);
    assert_eq!(place(&show.app).0, Some("B".into()));
    assert_eq!(fading(&show), [true; 3]);
    wait(&mut show.app, 0.1);
    assert_eq!(fading(&show), [false; 3]);

    // A fade longer than the wait: it goes on when its own fade is done —
    // not in the middle of it, where the next fade would be refused.
    ok(send(&mut show.app, EngineCommand::SetRundownFade { secs: 0.5 }));
    ok(take(&mut show.app, &steps[0], true));
    let counting = rundown(&show.app).wait.expect("counting");
    assert!(counting.secs_left > 0.4, "the fade's half second, not 0.15: {}", counting.secs_left);
    wait(&mut show.app, 0.3);
    assert_eq!(place(&show.app).0, Some("A".into()), "still fading to A");
    wait(&mut show.app, 0.3);
    assert_eq!(place(&show.app).0, Some("B".into()));
    assert_eq!(fading(&show), [true; 3], "and B fades in turn");
}

/// A step that cannot be taken when its turn comes by itself says so, and
/// is not tried again: the show stays where it is.
#[test]
fn a_wait_that_cannot_go_on_says_so_and_does_not_try_again() {
    let Some(mut show) = show() else {
        return;
    };
    let (b, c) = (ch(&show.b), ch(&show.c));
    let steps = [
        add_step(&mut show, "A"),
        wall_step(&mut show, "B", [&b, &b, &b]),
    ];
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownThen {
            uuid: steps[0].clone(),
            secs: Some(0.05),
        },
    ));
    ok(take(&mut show.app, &steps[0], true));
    let right = show.wall[2].clone();
    slow_fade(&mut show.app, &right, c);
    // Its turn come, a fade still landing is waited for — a second, no more.
    std::thread::sleep(Duration::from_millis(80));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app), (Some("A".into()), Some("B".into())));
    let waiting = rundown(&show.app).wait.expect("still its turn");
    assert!(waiting.secs_left.abs() < 1e-6, "{}", waiting.secs_left);
    std::thread::sleep(Duration::from_millis(1050));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app), (Some("A".into()), Some("B".into())));
    assert!(rundown(&show.app).wait.is_none(), "given up, and said so");
    // The bus's take cut short: B would go now, but nobody asks.
    cut_bus(&mut show.app, &right, ch(&show.a));
    std::thread::sleep(Duration::from_millis(30));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app).0, Some("A".into()));

    // Hold while it waits for a fade: it does not go on when the fade ends.
    ok(take(&mut show.app, &steps[0], true));
    slow_fade(&mut show.app, &right, ch(&show.c));
    std::thread::sleep(Duration::from_millis(80));
    show.app.advance_frame_state();
    assert!(rundown(&show.app).wait.is_some(), "its turn, and waiting");
    ok(send(&mut show.app, EngineCommand::RundownHold));
    cut_bus(&mut show.app, &right, ch(&show.a));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app).0, Some("A".into()));

    // Something missing as well as something fading: there is nothing to
    // wait for, and it says so at once.
    let overlay = id(send(&mut show.app, EngineCommand::AddChannel));
    for cmd in [
        EngineCommand::SetChannelOverlay {
            channel_uuid: overlay.clone(),
            overlay: true,
        },
        EngineCommand::SetRundownOverlay {
            uuid: steps[1].clone(),
            channel_uuid: overlay.clone(),
            on: Some(true),
        },
        EngineCommand::SetChannelOverlay {
            channel_uuid: overlay.clone(),
            overlay: false,
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    ok(take(&mut show.app, &steps[0], true));
    slow_fade(&mut show.app, &right, ch(&show.c));
    std::thread::sleep(Duration::from_millis(80));
    show.app.advance_frame_state();
    assert!(rundown(&show.app).wait.is_none(), "refused at once");
    assert_eq!(place(&show.app).0, Some("A".into()));
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownOverlay {
            uuid: steps[1].clone(),
            channel_uuid: overlay,
            on: None,
        },
    ));
    cut_bus(&mut show.app, &right, ch(&show.a));

    // A cut is not held up by a fade: it goes on at once, and lands it.
    ok(take(&mut show.app, &steps[0], false));
    slow_fade(&mut show.app, &right, ch(&show.c));
    std::thread::sleep(Duration::from_millis(80));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app).0, Some("B".into()));
    assert_eq!(wall_shows(&show), [b.clone(), b.clone(), b]);
    assert_eq!(fading(&show), [false; 3]);
}

/// A step that fades Main and goes on by itself is followed when Main has
/// landed: its wait, shorter than Main's fade, is made as long — and the
/// step it leads to, which fades Main again, is taken, not refused for the
/// frame or two by which Main's fade outlasts the wait.
#[test]
fn a_step_that_fades_main_goes_on_when_main_has_landed() {
    let Some(mut show) = show() else {
        return;
    };
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
    ));
    ok(send(&mut show.app, EngineCommand::SetMainFadeTime { secs: 0.3 }));
    let mut main_step = |name: &str, channel: &str| {
        let step = id(send(
            &mut show.app,
            EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![]),
                main: false,
            },
        ));
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownMain {
                uuid: step.clone(),
                main: Some(MainSource::Channel(channel.to_string())),
            },
        ));
        step
    };
    let (b, c) = (show.b.clone(), show.c.clone());
    let steps = [main_step("A", &b), main_step("B", &c)];
    // A also says the logo is off — as it is: nothing of it fades, and it
    // adds nothing to the wait.
    let logo = id(send(&mut show.app, EngineCommand::AddChannel));
    for cmd in [
        EngineCommand::SetChannelOverlay {
            channel_uuid: logo.clone(),
            overlay: true,
        },
        EngineCommand::SetRundownOverlay {
            uuid: steps[0].clone(),
            channel_uuid: logo,
            on: Some(false),
        },
        EngineCommand::SetRundownThen {
            uuid: steps[0].clone(),
            secs: Some(0.05),
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    ok(take(&mut show.app, &steps[0], true));
    let counting = rundown(&show.app).wait.expect("counting");
    assert!(counting.secs_left > 0.25, "Main's fade, not 0.05 s: {}", counting.secs_left);
    // Taken again with Main's fade time since made long: what is left of
    // the fade that is running is what counts, not the new time.
    ok(send(&mut show.app, EngineCommand::SetMainFadeTime { secs: 30.0 }));
    ok(take(&mut show.app, &steps[0], true));
    let counting = rundown(&show.app).wait.expect("counting");
    assert!(counting.secs_left < 0.35, "the running fade's 0.3 s: {}", counting.secs_left);
    ok(send(&mut show.app, EngineCommand::SetMainFadeTime { secs: 0.3 }));
    let until = std::time::Instant::now() + Duration::from_secs(3);
    let mut seen_b_fading = false;
    while place(&show.app).0 != Some("B".into()) {
        assert!(std::time::Instant::now() < until, "B was never taken");
        frame(&mut show.app);
        let main = show.app.build_engine_state().mixer.main;
        seen_b_fading |= main.program == MainSource::Channel(b.clone()) && main.fade.is_some();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(seen_b_fading, "A's fade to Main was seen running");
    let main = show.app.build_engine_state().mixer.main;
    assert_eq!(main.program, MainSource::Channel(c));
    assert_eq!(main.fade.map(|f| f.from), Some(MainSource::Channel(b)), "from A's picture, landed");
}

/// Taken again while its own fade runs, a step has nothing to start — and
/// its wait is still as long as what is left of that fade.
#[test]
fn a_wait_outlasts_a_fade_the_step_did_not_start() {
    let Some(mut show) = show() else {
        return;
    };
    let (b, c) = (ch(&show.b), ch(&show.c));
    let steps = [
        wall_step(&mut show, "A", [&b, &b, &b]),
        wall_step(&mut show, "B", [&c, &c, &c]),
    ];
    for cmd in [
        EngineCommand::SetRundownThen {
            uuid: steps[0].clone(),
            secs: Some(0.05),
        },
        EngineCommand::SetRundownFade { secs: 0.6 },
    ] {
        ok(send(&mut show.app, cmd));
    }
    ok(take(&mut show.app, &steps[0], true));
    std::thread::sleep(Duration::from_millis(150));
    show.app.advance_frame_state();
    ok(take(&mut show.app, &steps[0], true));
    let left = rundown(&show.app).wait.expect("counting").secs_left;
    assert!((0.3..0.5).contains(&left), "what is left of the fade: {left}");
    std::thread::sleep(Duration::from_millis(200));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app).0, Some("A".into()), "the fade is still running");
    std::thread::sleep(Duration::from_millis(350));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app).0, Some("B".into()));
    assert_eq!(fading(&show), [true; 3], "B fading in turn");
}

/// A press for a step that has just been taken by itself is refused — and
/// is not the operator stepping in: that step's own wait runs on.
#[test]
fn a_late_press_does_not_stop_the_step_it_was_late_for() {
    let Some(mut show) = show() else {
        return;
    };
    let steps = [
        add_step(&mut show, "A"),
        add_step(&mut show, "B"),
        add_step(&mut show, "C"),
    ];
    for step in &steps[..2] {
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownThen {
                uuid: step.clone(),
                secs: Some(0.2),
            },
        ));
    }
    ok(take(&mut show.app, &steps[0], false));
    std::thread::sleep(Duration::from_millis(250));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app), (Some("B".into()), Some("C".into())));
    // The press meant for B lands now.
    refused(go(&mut show.app, Some(&steps[1])));
    assert!(rundown(&show.app).wait.is_some(), "B's wait runs on");
    std::thread::sleep(Duration::from_millis(250));
    show.app.advance_frame_state();
    assert_eq!(place(&show.app), (Some("C".into()), None));
}

// ── Main and overlays ──────────────────────────────────────────────

/// A step takes Main as a hand does — cut, or by Main's own fade — and
/// switches overlays on and off. A fade to another input while Main is
/// mid-fade is refused; one to where Main is going already is not.
#[test]
fn a_step_takes_main_and_switches_overlays() {
    let Some(mut show) = show() else {
        return;
    };
    let logo = id(send(&mut show.app, EngineCommand::AddChannel));
    ok(send(
        &mut show.app,
        EngineCommand::SetChannelOverlay {
            channel_uuid: logo.clone(),
            overlay: true,
        },
    ));
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
    ));
    let main_step = |show: &mut Show, name: &str, channel: &str, logo_on: bool| {
        let step = id(send(
            &mut show.app,
            EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![]),
                main: false,
            },
        ));
        for cmd in [
            EngineCommand::SetRundownMain {
                uuid: step.clone(),
                main: Some(MainSource::Channel(channel.to_string())),
            },
            EngineCommand::SetRundownOverlay {
                uuid: step.clone(),
                channel_uuid: logo.clone(),
                on: Some(logo_on),
            },
        ] {
            ok(send(&mut show.app, cmd));
        }
        step
    };
    let (b, c) = (show.b.clone(), show.c.clone());
    let to_b = main_step(&mut show, "B", &b, true);
    let to_c = main_step(&mut show, "C", &c, false);
    ok(send(&mut show.app, EngineCommand::SetMainFadeTime { secs: 5.0 }));

    ok(take(&mut show.app, &to_b, true));
    let main = show.app.build_engine_state().mixer.main;
    assert_eq!(main.program, MainSource::Channel(b.clone()));
    assert_eq!(main.fade.map(|f| f.from), Some(MainSource::Channel(show.a.clone())));
    assert_eq!(main.overlays_on, std::slice::from_ref(&logo));
    assert!(step_named(&show.app, "B").shown && !step_named(&show.app, "C").shown);

    // Mid-fade: another input is not faded to; the same step again is fine.
    // (And the logo, fading on, is not turned round before it is seen.)
    let holding = step_named(&show.app, "C").fading;
    assert_eq!((holding.len(), holding[0].as_str()), (2, "Main"), "{holding:?}");
    let why = refused(take(&mut show.app, &to_c, true));
    assert!(why.starts_with("Step 'C' not taken — still fading: Main, '"), "{why}");
    assert_eq!(on_main(&show.app), MainSource::Channel(b.clone()));
    assert_eq!(
        show.app.build_engine_state().mixer.main.overlays_on,
        std::slice::from_ref(&logo)
    );
    ok(take(&mut show.app, &to_b, true));

    // A cut ends the fade, and takes the logo off.
    ok(take(&mut show.app, &to_c, false));
    let main = show.app.build_engine_state().mixer.main;
    assert_eq!((main.program, main.fade.is_none()), (MainSource::Channel(c), true));
    assert!(main.overlays_on.is_empty());
    // An overlay fading on is part of the step's own fade: its wait is not
    // shorter than what its fade takes — Main's fade time.
    let logo_only = id(send(
        &mut show.app,
        EngineCommand::AddRundownStep {
            name: "Logo".into(),
            buses: Some(vec![]),
            main: false,
        },
    ));
    for cmd in [
        EngineCommand::SetRundownOverlay {
            uuid: logo_only.clone(),
            channel_uuid: logo.clone(),
            on: Some(true),
        },
        EngineCommand::SetRundownThen {
            uuid: logo_only.clone(),
            secs: Some(0.1),
        },
        EngineCommand::MoveRundownStep {
            uuid: logo_only.clone(),
            later: false,
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    ok(take(&mut show.app, &logo_only, true));
    let counting = rundown(&show.app).wait.expect("counting");
    assert!(counting.secs_left > 4.5, "Main's five seconds: {}", counting.secs_left);
    // Still fading on, it is not turned round by a fade; a cut takes it off.
    assert_eq!(step_named(&show.app, "C").fading.len(), 1);
    refused(take(&mut show.app, &to_c, true));
    ok(take(&mut show.app, &to_c, false));
    assert!(show.app.build_engine_state().mixer.main.overlays_on.is_empty());
}

// ── Making steps ───────────────────────────────────────────────────

/// A new step keeps what is on now: where each bus is, or is heading — but
/// not the bus the stream takes, unless it is asked for; and Main with its
/// overlays, on and off, only when asked for.
#[test]
fn a_new_step_keeps_what_is_on_now_but_not_the_streams_bus() {
    let Some(mut show) = show() else {
        return;
    };
    let (a, b, c) = (ch(&show.a), ch(&show.b), ch(&show.c));
    let pgm = show.app.build_engine_state().buses[0].uuid.clone();
    let mut options = show.app.stream_settings().options.clone();
    options.source = OutputSource::BusProgram(pgm.clone());
    ok(send(&mut show.app, EngineCommand::SetStreamOptions { options }));
    let (left, right) = (show.wall[0].clone(), show.wall[2].clone());
    cut_bus(&mut show.app, &left, b.clone());
    slow_fade(&mut show.app, &right, c.clone());
    let logo = id(send(&mut show.app, EngineCommand::AddChannel));
    ok(send(
        &mut show.app,
        EngineCommand::SetChannelOverlay {
            channel_uuid: logo.clone(),
            overlay: true,
        },
    ));
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.c.clone()),
            fade: false,
        },
    ));

    let add = |app: &mut VardaApp, name: &str, buses: Option<Vec<String>>, main: bool| {
        id(send(
            app,
            EngineCommand::AddRundownStep {
                name: name.into(),
                buses,
                main,
            },
        ))
    };
    add(&mut show.app, "Wall", None, false);
    let step = step_named(&show.app, "Wall").step;
    let sets: Vec<_> = step.screens.iter().map(|s| (s.bus.clone(), s.input.clone())).collect();
    let wall = show.wall.clone();
    assert_eq!(
        sets,
        [(wall[0].clone(), b.clone()), (wall[1].clone(), a), (wall[2].clone(), c)],
        "the wall, the right bus where it is heading, and not the stream's bus"
    );
    assert_eq!((step.main, step.overlays.len(), step.plays.len()), (None, 0, 0));

    add(&mut show.app, "All", Some(vec![pgm.clone(), wall[0].clone()]), true);
    let step = step_named(&show.app, "All").step;
    let sets: Vec<_> = step.screens.iter().map(|s| (s.bus.clone(), s.input.clone())).collect();
    assert_eq!(sets, [(pgm, BusInputRef::Master), (wall[0].clone(), b)]);
    assert_eq!(step.main, Some(MainSource::Channel(show.c.clone())));
    let overlays: Vec<_> = step.overlays.iter().map(|o| (o.channel.clone(), o.on)).collect();
    assert_eq!(overlays, [(logo, false)], "the logo, off, so the step takes it off");
    assert_eq!(names(&show.app), ["Wall", "All"]);
}

/// "Update from what is on now" re-reads the parts a step sets and no
/// others; a copy is a step of its own, right after.
#[test]
fn a_step_is_updated_from_what_is_on_and_copied() {
    let Some(mut show) = show() else {
        return;
    };
    let (a, b, c) = (ch(&show.a), ch(&show.b), ch(&show.c));
    let step = wall_step(&mut show, "A", [&a, &a, &a]);
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownScreen {
            uuid: step.clone(),
            bus_uuid: show.wall[1].clone(),
            input: None,
        },
    ));
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownThen {
            uuid: step.clone(),
            secs: Some(7.0),
        },
    ));
    let wall = show.wall.clone();
    cut_bus(&mut show.app, &wall[0], b.clone());
    cut_bus(&mut show.app, &wall[1], c.clone());
    cut_bus(&mut show.app, &wall[2], c.clone());
    ok(send(
        &mut show.app,
        EngineCommand::CaptureRundownStep { uuid: step.clone() },
    ));
    let now = step_named(&show.app, "A").step;
    let sets: Vec<_> = now.screens.iter().map(|s| (s.bus.clone(), s.input.clone())).collect();
    assert_eq!(sets, [(wall[0].clone(), b), (wall[2].clone(), c)], "the two it sets");
    assert_eq!((now.main.clone(), now.then_secs), (None, Some(7.0)));
    assert!(now.overlays.is_empty(), "it sets no overlay, and still sets none");

    // With an overlay it switches — and only that — Main is still not its.
    let logo = id(send(&mut show.app, EngineCommand::AddChannel));
    let other = id(send(&mut show.app, EngineCommand::AddChannel));
    for channel in [&logo, &other] {
        ok(send(
            &mut show.app,
            EngineCommand::SetChannelOverlay {
                channel_uuid: channel.clone(),
                overlay: true,
            },
        ));
    }
    for cmd in [
        EngineCommand::SetRundownOverlay {
            uuid: step.clone(),
            channel_uuid: logo.clone(),
            on: Some(false),
        },
        EngineCommand::SetOverlayOn {
            channel_uuid: logo.clone(),
            on: true,
            fade: false,
        },
        EngineCommand::CaptureRundownStep { uuid: step.clone() },
    ] {
        ok(send(&mut show.app, cmd));
    }
    let now = step_named(&show.app, "A").step;
    let overlays: Vec<_> = now.overlays.iter().map(|o| (o.channel.clone(), o.on)).collect();
    assert_eq!(overlays, [(logo, true)], "the one it switches, as it is now");
    assert_eq!(now.main, None);
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownOverlay {
            uuid: step.clone(),
            channel_uuid: now.overlays[0].channel.clone(),
            on: None,
        },
    ));
    let now = step_named(&show.app, "A").step;

    let copy = id(send(
        &mut show.app,
        EngineCommand::DuplicateRundownStep { uuid: step.clone() },
    ));
    assert_eq!(names(&show.app), ["A", "A copy"]);
    let copied = step_named(&show.app, "A copy").step;
    assert_eq!((copied.screens, copied.then_secs), (now.screens, Some(7.0)));
    assert_ne!(copy, step);
    ok(send(
        &mut show.app,
        EngineCommand::MoveRundownStep {
            uuid: copy,
            later: false,
        },
    ));
    assert_eq!(names(&show.app), ["A copy", "A"]);
}

// ── What is deleted, undone, and saved ─────────────────────────────

/// A deleted input leaves the steps that used it — the rest of each step
/// stays — and undo puts both back. A removed bus leaves them too.
#[test]
fn what_is_deleted_leaves_the_steps_and_undo_puts_it_back() {
    let Some(mut show) = show() else {
        return;
    };
    let (b, c) = (ch(&show.b), ch(&show.c));
    let step = wall_step(&mut show, "A", [&b, &c, &b]);
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownMain {
            uuid: step.clone(),
            main: Some(MainSource::Channel(show.c.clone())),
        },
    ));
    let whole = step_named(&show.app, "A").step;

    ok(send(
        &mut show.app,
        EngineCommand::RemoveChannel {
            channel_uuid: show.c.clone(),
        },
    ));
    let without = step_named(&show.app, "A");
    assert_eq!(without.step.screens.len(), 2);
    assert_eq!(without.step.main, None);
    assert!(without.missing.is_empty(), "it can still be taken: {:?}", without.missing);
    ok(send(&mut show.app, EngineCommand::Undo));
    assert_eq!(step_named(&show.app, "A").step, whole);
    assert!(step_named(&show.app, "A").missing.is_empty());

    ok(send(
        &mut show.app,
        EngineCommand::RemoveBus {
            uuid: show.wall[0].clone(),
        },
    ));
    let without = step_named(&show.app, "A");
    assert_eq!(without.step.screens.len(), 2);
    assert!(without.missing.is_empty());
    ok(send(&mut show.app, EngineCommand::Undo));
    assert_eq!(step_named(&show.app, "A").step, whole);
}

/// Undo puts back a step that was removed or changed — and never what is
/// on: the wall, Main and where the show is stay as the operator left them.
#[test]
fn undo_puts_a_step_back_without_changing_what_is_on() {
    let Some(mut show) = show() else {
        return;
    };
    let (b, c) = (ch(&show.b), ch(&show.c));
    let steps = [
        wall_step(&mut show, "A", [&b, &b, &b]),
        wall_step(&mut show, "B", [&c, &c, &c]),
        add_step(&mut show, "C"),
    ];
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownFade { secs: 2.5 },
    ));
    ok(take(&mut show.app, &steps[0], false));
    ok(take(&mut show.app, &steps[1], false));
    let on = (wall_shows(&show), on_main(&show.app), place(&show.app));
    assert_eq!(on.2, (Some("B".into()), Some("C".into())));

    ok(send(
        &mut show.app,
        EngineCommand::RenameRundownStep {
            uuid: steps[0].clone(),
            name: "Opening".into(),
        },
    ));
    ok(send(
        &mut show.app,
        EngineCommand::RemoveRundownStep {
            uuid: steps[2].clone(),
        },
    ));
    assert_eq!(names(&show.app), ["Opening", "B"]);
    assert_eq!(place(&show.app), (Some("B".into()), None));

    ok(send(&mut show.app, EngineCommand::Undo));
    assert_eq!(names(&show.app), ["Opening", "B", "C"]);
    ok(send(&mut show.app, EngineCommand::Undo));
    assert_eq!(names(&show.app), ["A", "B", "C"]);
    assert_eq!((wall_shows(&show), on_main(&show.app), place(&show.app)), on);
    assert!((rundown(&show.app).fade_secs - 2.5).abs() < 1e-6);

    // The step the show is on, removed: GO still takes what followed. Put
    // back, the show is on it again — GO does not take it a second time.
    ok(send(
        &mut show.app,
        EngineCommand::RemoveRundownStep {
            uuid: steps[1].clone(),
        },
    ));
    assert_eq!(place(&show.app), (None, Some("C".into())));
    ok(send(&mut show.app, EngineCommand::Undo));
    assert_eq!((wall_shows(&show), on_main(&show.app), place(&show.app)), on);
    // Taking a step is not something undo knows of.
    let undone = send(&mut show.app, EngineCommand::Undo);
    assert_eq!(wall_shows(&show), on.0, "{undone:?}");
}

/// Saved and opened again, the rundown is there: its steps, the fade time,
/// where the show is. A show that never had one writes nothing of it.
#[test]
fn the_rundown_is_there_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path()) else {
        return;
    };
    let stage_file = dir.path().join(".varda").join("stage.json");
    app.save_workspace(&UILayoutState::default()).expect("saved");
    let text = std::fs::read_to_string(&stage_file).unwrap();
    assert!(!text.contains("rundown"), "no rundown, nothing written of one");

    let state = app.build_engine_state();
    let (a, b) = (
        state.mixer.channels[0].uuid.clone(),
        state.mixer.channels[1].uuid.clone(),
    );
    let bus = state.buses[0].uuid.clone();
    let mut steps = Vec::new();
    for name in ["A", "B", "C"] {
        steps.push(id(send(
            &mut app,
            EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![bus.clone()]),
                main: false,
            },
        )));
    }
    for cmd in [
        EngineCommand::SetRundownScreen {
            uuid: steps[1].clone(),
            bus_uuid: bus.clone(),
            input: Some(ch(&b)),
        },
        EngineCommand::SetRundownMain {
            uuid: steps[1].clone(),
            main: Some(MainSource::Channel(a.clone())),
        },
        EngineCommand::SetRundownThen {
            uuid: steps[1].clone(),
            secs: Some(30.0),
        },
        EngineCommand::SetRundownFade { secs: 1.5 },
        EngineCommand::SetMixMode {
            mode: MixMode::Program,
        },
        EngineCommand::TakeRundownStep {
            uuid: steps[1].clone(),
            fade: false,
        },
    ] {
        ok(send(&mut app, cmd));
    }
    assert!(rundown(&app).wait.is_some());
    let saved = rundown(&app);
    app.save_workspace(&UILayoutState::default()).expect("saved");
    drop(app);

    let Some(mut app) = app_in(dir.path()) else {
        return;
    };
    let _ = app.load_workspace();
    let opened = rundown(&app);
    let steps_of = |r: &RundownSnapshot| r.steps.iter().map(|s| s.step.clone()).collect::<Vec<_>>();
    assert_eq!(steps_of(&opened), steps_of(&saved));
    assert!((opened.fade_secs - 1.5).abs() < 1e-6);
    assert_eq!(place(&app), (Some("B".into()), Some("C".into())));
    assert!(opened.wait.is_none(), "a wait is not carried over a restart");
    assert!(opened.steps.iter().all(|s| s.missing.is_empty()));
    // Nothing goes on by itself after the restart.
    std::thread::sleep(Duration::from_millis(30));
    app.advance_frame_state();
    assert_eq!(place(&app).0, Some("B".into()));
}

/// A step waiting to go on is dropped when another show is opened: it is
/// not taken into the show that opens.
#[test]
fn a_wait_does_not_outlive_its_show() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path()) else {
        return;
    };
    let mut steps = Vec::new();
    for name in ["A", "B"] {
        steps.push(id(send(
            &mut app,
            EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![]),
                main: false,
            },
        )));
    }
    app.save_workspace(&UILayoutState::default()).expect("saved");
    ok(send(
        &mut app,
        EngineCommand::SetRundownThen {
            uuid: steps[0].clone(),
            secs: Some(0.05),
        },
    ));
    ok(take(&mut app, &steps[0], false));
    assert!(rundown(&app).wait.is_some());
    let _ = app.load_workspace();
    assert!(rundown(&app).wait.is_none());
    std::thread::sleep(Duration::from_millis(80));
    app.advance_frame_state();
    assert_eq!(place(&app), (None, Some("A".into())), "as the show was saved");

    // (And a folder with no show in it at all: nothing is loaded, and the
    // wait is dropped all the same.)
    let nowhere = tempfile::tempdir().unwrap();
    if let Some(mut other) = app_in(nowhere.path()) {
        let mut steps = Vec::new();
        for name in ["A", "B"] {
            steps.push(id(send(
                &mut other,
                EngineCommand::AddRundownStep {
                    name: name.into(),
                    buses: Some(vec![]),
                    main: false,
                },
            )));
        }
        ok(send(
            &mut other,
            EngineCommand::SetRundownThen {
                uuid: steps[0].clone(),
                secs: Some(0.05),
            },
        ));
        ok(take(&mut other, &steps[0], false));
        assert!(!nowhere.path().join(".varda").exists());
        let _ = other.load_workspace();
        assert!(rundown(&other).wait.is_none());
        std::thread::sleep(Duration::from_millis(80));
        other.advance_frame_state();
        assert_eq!(place(&other).0, Some("A".into()), "B was not taken");
    }

    // A show that opens only in part — its stage unreadable — leaves the
    // rundown that was open in place. Its wait is dropped all the same, and
    // none of its steps can be taken into the half-opened show.
    ok(send(
        &mut app,
        EngineCommand::SetRundownThen {
            uuid: steps[0].clone(),
            secs: Some(0.05),
        },
    ));
    ok(take(&mut app, &steps[0], false));
    assert!(rundown(&app).wait.is_some());
    std::fs::write(dir.path().join(".varda").join("stage.json"), "{ not a stage").unwrap();
    let _ = app.load_workspace();
    assert!(rundown(&app).wait.is_none());
    std::thread::sleep(Duration::from_millis(80));
    app.advance_frame_state();
    assert_eq!(place(&app).0, Some("A".into()), "B was not taken");
    let why = refused(take(&mut app, &steps[1], false));
    assert!(why.contains("the show did not open whole"), "{why}");
}

// ── What a step starts and stops ───────────────────────────────────

/// A step starts a count down from its top, pauses it, carries it on and
/// stops it back at its top.
#[test]
fn a_step_starts_pauses_and_stops_a_timer() {
    let Some(mut show) = show() else {
        return;
    };
    let timer = id(send(
        &mut show.app,
        EngineCommand::AddTimerDeck {
            channel_uuid: show.c.clone(),
            settings: Some(TimerSettings {
                mode: TimerMode::Countdown,
                duration_secs: 600,
                ..TimerSettings::default()
            }),
        },
    ));
    let mut step_that = |action: PlayAction| {
        let step = id(send(
            &mut show.app,
            EngineCommand::AddRundownStep {
                name: format!("{action:?}"),
                buses: Some(vec![]),
                main: false,
            },
        ));
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownPlay {
                uuid: step.clone(),
                deck_uuid: timer.clone(),
                action: Some(action),
            },
        ));
        step
    };
    let [start, resume, pause, stop] = [
        PlayAction::Start,
        PlayAction::Resume,
        PlayAction::Pause,
        PlayAction::Stop,
    ]
    .map(&mut step_that);
    let reading = |app: &VardaApp| {
        let state = app.build_engine_state();
        let decks = state.mixer.channels.iter().flat_map(|ch| &ch.decks);
        let deck = decks.into_iter().find(|d| d.uuid == timer);
        let timer = deck.and_then(|d| d.timer.clone()).expect("a timer");
        (timer.running, timer.elapsed)
    };
    let nap = || std::thread::sleep(Duration::from_millis(120));

    ok(take(&mut show.app, &resume, false));
    nap();
    let (running, run) = reading(&show.app);
    assert!(running && run > 0.1, "carried on from its top: {run}");

    ok(take(&mut show.app, &start, false));
    let (running, from_top) = reading(&show.app);
    assert!(running && from_top < 0.05, "from its top again: {from_top}");
    nap();

    ok(take(&mut show.app, &pause, false));
    let (running, held) = reading(&show.app);
    assert!(!running && held > 0.1, "stopped where it was: {held}");
    nap();
    assert!((reading(&show.app).1 - held).abs() < 1e-3);

    ok(take(&mut show.app, &resume, false));
    nap();
    let (running, on) = reading(&show.app);
    assert!(running && on > held + 0.1, "on from where it was: {held} → {on}");

    ok(take(&mut show.app, &stop, false));
    let (running, at_top) = reading(&show.app);
    assert!(!running && at_top.abs() < 1e-3, "stopped, at its top: {at_top}");
}

fn ffmpeg(args: &[&str]) -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

/// A six-second clip with a tone, or `None` without the ffmpeg tool.
fn clip(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let path = dir.join("sting.mp4");
    let made = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=64x36:r=25:d=6",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1000:duration=6",
        "-c:v",
        "mpeg4",
        "-c:a",
        "aac",
        "-shortest",
        path.to_str().unwrap(),
    ]);
    if !made {
        eprintln!("ffmpeg not available — skipping");
    }
    made.then_some(path)
}

/// Render frames for `secs`, the show's state moved on each.
fn run(app: &mut VardaApp, secs: f64) {
    let end = std::time::Instant::now() + Duration::from_secs_f64(secs);
    while std::time::Instant::now() < end {
        app.advance_frame_state();
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(8));
    }
}

/// A step plays a clip that is on no screen — a sting — from its beginning,
/// and it plays on unseen; taken again, from its beginning again. Pause
/// holds it where it is, Stop puts it back at its beginning. Once it has
/// been on a screen and left, it pauses and rewinds as any clip does.
#[test]
fn a_step_plays_a_clip_that_is_on_no_screen() {
    let Some(mut show) = show() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path()) else {
        return;
    };
    let sting = id(send(&mut show.app, EngineCommand::AddChannel));
    let deck = id(send(
        &mut show.app,
        EngineCommand::AddVideoDeck {
            channel_uuid: sting.clone(),
            path,
        },
    ));
    let mut step_that = |name: &str, action: PlayAction| {
        let step = id(send(
            &mut show.app,
            EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![]),
                main: false,
            },
        ));
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownPlay {
                uuid: step.clone(),
                deck_uuid: deck.clone(),
                action: Some(action),
            },
        ));
        step
    };
    let start = step_that("Start", PlayAction::Start);
    let pause = step_that("Pause", PlayAction::Pause);
    let resume = step_that("Resume", PlayAction::Resume);
    let stop = step_that("Stop", PlayAction::Stop);
    let playback = |app: &VardaApp| {
        let (ch, d) = app.mixer_ref().find_deck_by_uuid(&deck).expect("the deck");
        let snap = app.mixer_ref().channels()[ch].decks[d]
            .deck
            .playback_snapshot()
            .expect("a clip");
        (snap.playing, snap.position)
    };

    run(&mut show.app, 0.4);
    let (playing, at) = playback(&show.app);
    assert!(!playing && at < 0.1, "a new clip waits on its first frame: {at}");

    // Its beginning is its in-point, not the file's first frame.
    ok(send(
        &mut show.app,
        EngineCommand::VideoSetInPoint {
            deck_uuid: deck.clone(),
            secs: 2.0,
        },
    ));
    ok(take(&mut show.app, &start, false));
    run(&mut show.app, 1.0);
    let (playing, first) = playback(&show.app);
    assert!(playing && (2.5..3.6).contains(&first), "playing, unseen: {first}");

    // Again: from its beginning, not from where it had got to.
    ok(take(&mut show.app, &start, false));
    run(&mut show.app, 0.4);
    let (playing, again) = playback(&show.app);
    assert!(playing && (2.0..first - 0.2).contains(&again), "from its beginning: {first} then {again}");
    ok(take(&mut show.app, &stop, false));
    run(&mut show.app, 0.3);
    let (playing, at) = playback(&show.app);
    assert!(!playing && (1.9..2.1).contains(&at), "stopped at its in-point: {at}");
    ok(send(
        &mut show.app,
        EngineCommand::VideoClearInOutPoints {
            deck_uuid: deck.clone(),
        },
    ));
    ok(take(&mut show.app, &start, false));
    run(&mut show.app, 0.4);

    ok(take(&mut show.app, &pause, false));
    run(&mut show.app, 0.3);
    let (playing, held) = playback(&show.app);
    run(&mut show.app, 0.3);
    assert!(!playing && (playback(&show.app).1 - held).abs() < 0.05, "held at {held}");
    assert!(held > 0.2);

    ok(take(&mut show.app, &resume, false));
    run(&mut show.app, 0.5);
    let (playing, on) = playback(&show.app);
    assert!(playing && on > held + 0.2, "on from {held}: {on}");

    ok(take(&mut show.app, &stop, false));
    run(&mut show.app, 0.3);
    let (playing, at) = playback(&show.app);
    assert!(!playing && at < 0.1, "stopped at its beginning: {at}");

    // On a screen and off again, it is a clip like any other: it plays as
    // it goes on, and pauses and rewinds as it leaves.
    ok(take(&mut show.app, &start, false));
    let left = show.wall[0].clone();
    cut_bus(&mut show.app, &left, ch(&sting));
    run(&mut show.app, 0.6);
    assert!(playback(&show.app).0, "plays on, on the screen");
    cut_bus(&mut show.app, &left, ch(&show.a));
    run(&mut show.app, 0.4);
    let (playing, at) = playback(&show.app);
    assert!(!playing && at < 0.1, "paused and back at its start on leaving: {at}");

    // A step that puts the clip on a screen and says Stop means a still
    // picture there: Auto Play, which starts a clip as it goes on air, gives
    // way to the step — for that arrival, not for good.
    let still = id(send(
        &mut show.app,
        EngineCommand::AddRundownStep {
            name: "Still".into(),
            buses: Some(vec![]),
            main: false,
        },
    ));
    for cmd in [
        EngineCommand::SetRundownScreen {
            uuid: still.clone(),
            bus_uuid: left.clone(),
            input: Some(ch(&sting)),
        },
        EngineCommand::SetRundownPlay {
            uuid: still.clone(),
            deck_uuid: deck.clone(),
            action: Some(PlayAction::Stop),
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    for (fade, action) in [
        (false, PlayAction::Stop),
        (true, PlayAction::Stop),
        (false, PlayAction::Pause),
        (true, PlayAction::Pause),
    ] {
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownPlay {
                uuid: still.clone(),
                deck_uuid: deck.clone(),
                action: Some(action),
            },
        ));
        ok(take(&mut show.app, &still, fade));
        run(&mut show.app, 0.6);
        let (playing, at) = playback(&show.app);
        assert!(!playing && at < 0.1, "on the screen, still ({action:?}, fade {fade}): {at}");
        cut_bus(&mut show.app, &left, ch(&show.a));
        run(&mut show.app, 0.3);
    }

    // While something else is fading for a long time — another screen, say:
    let right = show.wall[2].clone();
    slow_fade(&mut show.app, &right, ch(&show.b));
    // - the hold is for the arrival the step brings about, and is used up
    //   by it: put on the screen again by hand, the clip plays;
    ok(take(&mut show.app, &still, false));
    run(&mut show.app, 0.4);
    assert!(!playback(&show.app).0, "brought on still");
    cut_bus(&mut show.app, &left, ch(&show.a));
    run(&mut show.app, 0.3);
    cut_bus(&mut show.app, &left, ch(&sting));
    run(&mut show.app, 0.5);
    assert!(playback(&show.app).0, "by hand again, it plays");
    // - and a clip paused while it is on a screen already has no arrival to
    //   come: taken off and put back by hand, it plays.
    ok(take(&mut show.app, &pause, false));
    run(&mut show.app, 0.3);
    assert!(!playback(&show.app).0, "paused where it is");
    cut_bus(&mut show.app, &left, ch(&show.a));
    run(&mut show.app, 0.3);
    cut_bus(&mut show.app, &left, ch(&sting));
    run(&mut show.app, 0.5);
    assert!(playback(&show.app).0, "off and on again by hand, it plays");
    cut_bus(&mut show.app, &right, ch(&show.a));
    // Put on the screen by hand, it plays as its Auto Play says.
    cut_bus(&mut show.app, &left, ch(&sting));
    run(&mut show.app, 0.5);
    assert!(playback(&show.app).0, "by hand, it plays");
    cut_bus(&mut show.app, &left, ch(&show.a));
    run(&mut show.app, 0.3);

    // Faded onto Main and stopped: a fade brings it on a frame or more
    // after the take, and it is still when it gets there, and stays so.
    for cmd in [
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
        EngineCommand::SetMainFadeTime { secs: 0.3 },
        EngineCommand::SetRundownScreen {
            uuid: still.clone(),
            bus_uuid: left.clone(),
            input: None,
        },
        EngineCommand::SetRundownMain {
            uuid: still.clone(),
            main: Some(MainSource::Channel(sting.clone())),
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    ok(take(&mut show.app, &still, true));
    run(&mut show.app, 0.8);
    let (playing, at) = playback(&show.app);
    assert!(!playing && at < 0.1, "faded onto Main, still: {at}");
    assert_eq!(on_main(&show.app), MainSource::Channel(sting.clone()));
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
    ));
    run(&mut show.app, 0.3);

    // On Main and playing, cut away by hand and faded back by the step in
    // the same moment — before a frame is drawn between: what the step goes
    // by is what is routed now, and it brings the clip back still.
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(sting.clone()),
            fade: false,
        },
    ));
    run(&mut show.app, 0.5);
    assert!(playback(&show.app).0, "on Main by hand, playing");
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
    ));
    ok(take(&mut show.app, &still, true));
    run(&mut show.app, 0.8);
    let (playing, at) = playback(&show.app);
    assert!(!playing && at < 0.1, "faded back to Main, still: {at}");
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
    ));
    run(&mut show.app, 0.3);

    // Put on Main by hand and, in the same moment, paused by a step that
    // has it on Main: the step, which came last, has the say.
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(sting.clone()),
            fade: false,
        },
    ));
    ok(take(&mut show.app, &still, false));
    run(&mut show.app, 0.5);
    let (playing, at) = playback(&show.app);
    assert!(!playing && at < 0.1, "on Main by hand, held still by the step: {at}");
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
    ));
    run(&mut show.app, 0.3);

    // A clip that plays only as it goes on Main, put on a screen still by a
    // step: nothing of the step's is left over to stop it playing when a
    // hand takes it to Main a moment later.
    let main_only = AutoPlayback {
        main_only: true,
        ..AutoPlayback::default()
    };
    for cmd in [
        EngineCommand::VideoSetAutoPlayback {
            deck_uuid: deck.clone(),
            auto: main_only,
        },
        EngineCommand::SetRundownMain {
            uuid: still.clone(),
            main: None,
        },
        EngineCommand::SetRundownScreen {
            uuid: still.clone(),
            bus_uuid: left.clone(),
            input: Some(ch(&sting)),
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    slow_fade(&mut show.app, &right, ch(&show.b));
    ok(take(&mut show.app, &still, false));
    run(&mut show.app, 0.3);
    assert!(!playback(&show.app).0, "on the screen, still");
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(sting.clone()),
            fade: false,
        },
    ));
    run(&mut show.app, 0.5);
    assert!(playback(&show.app).0, "taken to Main by hand, it plays");
    for cmd in [
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
        EngineCommand::VideoSetAutoPlayback {
            deck_uuid: deck.clone(),
            auto: AutoPlayback::default(),
        },
        EngineCommand::SetRundownScreen {
            uuid: still.clone(),
            bus_uuid: left.clone(),
            input: None,
        },
        EngineCommand::SetRundownMain {
            uuid: still.clone(),
            main: Some(MainSource::Channel(sting.clone())),
        },
    ] {
        ok(send(&mut show.app, cmd));
    }
    cut_bus(&mut show.app, &left, ch(&show.a));
    cut_bus(&mut show.app, &right, ch(&show.a));
    run(&mut show.app, 0.3);

    // A step that pauses a clip and puts it nowhere holds nothing back:
    // while another screen is fading, the clip put on a screen by hand
    // plays as its Auto Play says.
    slow_fade(&mut show.app, &right, ch(&show.b));
    ok(take(&mut show.app, &pause, false));
    run(&mut show.app, 0.2);
    cut_bus(&mut show.app, &left, ch(&sting));
    run(&mut show.app, 0.5);
    assert!(playback(&show.app).0, "by hand, with nothing of the step's to hold it");
    cut_bus(&mut show.app, &left, ch(&show.a));
    cut_bus(&mut show.app, &right, ch(&show.a));
    run(&mut show.app, 0.3);

    // Faded onto Main still by one step, while a second step that only
    // starts a countdown is taken in the same moment: the second step
    // leaves the clip alone, and so it stays still as it arrives.
    let timer = id(send(
        &mut show.app,
        EngineCommand::AddTimerDeck {
            channel_uuid: show.c.clone(),
            settings: None,
        },
    ));
    let countdown = id(send(
        &mut show.app,
        EngineCommand::AddRundownStep {
            name: "Countdown".into(),
            buses: Some(vec![]),
            main: false,
        },
    ));
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownPlay {
            uuid: countdown.clone(),
            deck_uuid: timer,
            action: Some(PlayAction::Start),
        },
    ));
    ok(take(&mut show.app, &still, true));
    ok(take(&mut show.app, &countdown, false));
    run(&mut show.app, 0.8);
    let (playing, at) = playback(&show.app);
    assert!(!playing && at < 0.1, "still on Main, the countdown going: {at}");
    ok(send(
        &mut show.app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
    ));
    run(&mut show.app, 0.3);

    // Paused by one step and put on a screen by the next in that moment, it
    // stays still: the pause is that moment's. A step that starts it is what
    // starts it.
    let shows_it = wall_step(&mut show, "Shows it", [&ch(&sting), &ch(&sting), &ch(&sting)]);
    ok(take(&mut show.app, &pause, false));
    ok(take(&mut show.app, &shows_it, false));
    run(&mut show.app, 0.5);
    assert!(!playback(&show.app).0, "still, as the step that paused it said");
    ok(take(&mut show.app, &resume, false));
    run(&mut show.app, 0.4);
    assert!(playback(&show.app).0, "carried on by the step that says so");
}

/// A clip that follows the timeline, or has a lane on it, is the timeline's
/// to start: a step cannot be made to start it, and one made before says so
/// instead of being taken.
#[test]
fn a_clip_on_the_timeline_is_not_a_steps_to_start() {
    use varda::video::{DeckTransportSync, TransportSyncMode};
    let Some(mut show) = show() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path()) else {
        return;
    };
    let deck = id(send(
        &mut show.app,
        EngineCommand::AddVideoDeck {
            channel_uuid: show.c.clone(),
            path,
        },
    ));
    let step = id(send(
        &mut show.app,
        EngineCommand::AddRundownStep {
            name: "A".into(),
            buses: Some(vec![]),
            main: false,
        },
    ));
    let start = |app: &mut VardaApp| {
        send(
            app,
            EngineCommand::SetRundownPlay {
                uuid: step.clone(),
                deck_uuid: deck.clone(),
                action: Some(PlayAction::Start),
            },
        )
    };
    let sync = |app: &mut VardaApp, mode: TransportSyncMode| {
        ok(send(
            app,
            EngineCommand::VideoSetTransportSync {
                deck_uuid: deck.clone(),
                sync: DeckTransportSync {
                    mode,
                    ..DeckTransportSync::default()
                },
            },
        ));
    };
    // The timeline stopped, a clip that follows it only while it runs is
    // free; one that always follows it is not.
    ok(start(&mut show.app));
    ok(take(&mut show.app, &step, false));
    sync(&mut show.app, TransportSyncMode::Always);
    let why = refused(take(&mut show.app, &step, false));
    assert!(why.contains("follows the timeline"), "{why}");
    assert!(refused(start(&mut show.app)).contains("follows the timeline"));
    sync(&mut show.app, TransportSyncMode::Never);
    ok(take(&mut show.app, &step, false));

    // With a lane of its own, the arrangement says when it decodes.
    ok(send(
        &mut show.app,
        EngineCommand::AddLane {
            deck_uuid: deck.clone(),
        },
    ));
    let why = refused(take(&mut show.app, &step, false));
    assert!(why.contains("follows the timeline"), "{why}");
    assert_eq!(step_named(&show.app, "A").missing.len(), 1);
    ok(send(
        &mut show.app,
        EngineCommand::RemoveLane {
            deck_uuid: deck.clone(),
        },
    ));
    ok(take(&mut show.app, &step, false));

    // A playlist with a lane is the timeline's as a clip is; a timer counts
    // whatever the timeline does with its picture, and can be started.
    let list = id(send(
        &mut show.app,
        EngineCommand::AddPlaylistDeck {
            channel_uuid: show.c.clone(),
            items: vec![],
            settings: None,
        },
    ));
    let timer = id(send(
        &mut show.app,
        EngineCommand::AddTimerDeck {
            channel_uuid: show.c.clone(),
            settings: None,
        },
    ));
    let plays = |app: &mut VardaApp, deck: &str| {
        send(
            app,
            EngineCommand::SetRundownPlay {
                uuid: step.clone(),
                deck_uuid: deck.to_string(),
                action: Some(PlayAction::Start),
            },
        )
    };
    ok(plays(&mut show.app, &list));
    for deck in [&list, &timer] {
        ok(send(
            &mut show.app,
            EngineCommand::AddLane {
                deck_uuid: deck.clone(),
            },
        ));
    }
    assert!(refused(plays(&mut show.app, &list)).contains("follows the timeline"));
    assert!(refused(take(&mut show.app, &step, false)).contains("follows the timeline"));
    ok(plays(&mut show.app, &timer));
    // The playlist taken out of the step, it is taken — the timer, lane and
    // all, with it.
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownPlay {
            uuid: step.clone(),
            deck_uuid: list,
            action: None,
        },
    ));
    ok(take(&mut show.app, &step, false));
}

/// A step that goes on by itself does not turn an overlay round before it
/// has been seen: the step that switches it off again waits for it to have
/// faded on, even when a slow frame brings its turn early.
#[test]
fn an_overlay_fading_on_is_seen_before_the_next_step_takes_it_off() {
    let Some(mut show) = show() else {
        return;
    };
    let logo = id(send(&mut show.app, EngineCommand::AddChannel));
    for cmd in [
        EngineCommand::SetChannelOverlay {
            channel_uuid: logo.clone(),
            overlay: true,
        },
        EngineCommand::TakeToMain {
            source: MainSource::Channel(show.a.clone()),
            fade: false,
        },
        EngineCommand::SetMainFadeTime { secs: 0.3 },
    ] {
        ok(send(&mut show.app, cmd));
    }
    let mut logo_step = |name: &str, on: bool| {
        let step = id(send(
            &mut show.app,
            EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![]),
                main: false,
            },
        ));
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownOverlay {
                uuid: step.clone(),
                channel_uuid: logo.clone(),
                on: Some(on),
            },
        ));
        step
    };
    let steps = [logo_step("On", true), logo_step("Off", false)];
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownThen {
            uuid: steps[0].clone(),
            secs: Some(0.05),
        },
    ));
    frame(&mut show.app);
    ok(take(&mut show.app, &steps[0], true));
    let counting = rundown(&show.app).wait.expect("counting");
    assert!(counting.secs_left > 0.25, "the logo's fade, not 0.05 s: {}", counting.secs_left);
    frame(&mut show.app);
    // A slow frame: the wait is up before the logo has moved at all.
    std::thread::sleep(Duration::from_millis(400));
    let level = |app: &VardaApp| app.mixer_ref().main().overlay_level(&logo);
    let mut seen = level(&show.app);
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while place(&show.app).0 != Some("Off".into()) {
        assert!(std::time::Instant::now() < until, "Off was never taken");
        frame(&mut show.app);
        if place(&show.app).0 != Some("Off".into()) {
            seen = seen.max(level(&show.app));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(seen > 0.99, "the logo was fully on before it was taken off: {seen}");
}

/// A step starts a playlist from its first item, pauses it, carries it on
/// and stops it back on its first item.
#[test]
fn a_step_starts_and_stops_a_playlist() {
    use varda::engine::value::playlist::PlaylistItem;
    let Some(mut show) = show() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items: Vec<PlaylistItem> = [("one.png", [255u8, 0, 0]), ("two.png", [0, 255, 0])]
        .into_iter()
        .map(|(name, rgb)| {
            let path = dir.path().join(name);
            image::RgbaImage::from_pixel(16, 9, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
                .save(&path)
                .unwrap();
            PlaylistItem::new(path.to_string_lossy())
        })
        .collect();
    let list = id(send(
        &mut show.app,
        EngineCommand::AddPlaylistDeck {
            channel_uuid: show.c.clone(),
            items,
            settings: None,
        },
    ));
    let mut step_that = |action: PlayAction| {
        let step = id(send(
            &mut show.app,
            EngineCommand::AddRundownStep {
                name: format!("{action:?}"),
                buses: Some(vec![]),
                main: false,
            },
        ));
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownPlay {
                uuid: step.clone(),
                deck_uuid: list.clone(),
                action: Some(action),
            },
        ));
        step
    };
    let [start, resume, pause, stop] = [
        PlayAction::Start,
        PlayAction::Resume,
        PlayAction::Pause,
        PlayAction::Stop,
    ]
    .map(&mut step_that);
    let state = |app: &VardaApp| {
        let state = app.build_engine_state();
        let decks = state.mixer.channels.iter().flat_map(|ch| &ch.decks);
        let deck = decks.into_iter().find(|d| d.uuid == list);
        let list = deck.and_then(|d| d.playlist.clone()).expect("a playlist");
        (list.playing, list.shown)
    };
    // Frames until the list shows `item` — its pictures open on a thread.
    let shows = |app: &mut VardaApp, item: usize| {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while state(app).1 != Some(item) {
            assert!(std::time::Instant::now() < until, "item {item} never shown");
            run(app, 0.05);
        }
    };
    shows(&mut show.app, 0);
    assert!(!state(&show.app).0, "a new list waits on its first item");

    // On its second item, stopped: started, it is on its first again.
    ok(send(
        &mut show.app,
        EngineCommand::ShowPlaylistItem {
            deck_uuid: list.clone(),
            index: 1,
        },
    ));
    shows(&mut show.app, 1);
    ok(take(&mut show.app, &start, false));
    assert!(state(&show.app).0, "playing");
    shows(&mut show.app, 0);

    ok(take(&mut show.app, &pause, false));
    assert!(!state(&show.app).0, "paused");
    ok(take(&mut show.app, &resume, false));
    assert!(state(&show.app).0, "carried on");

    ok(send(
        &mut show.app,
        EngineCommand::ShowPlaylistItem {
            deck_uuid: list.clone(),
            index: 1,
        },
    ));
    shows(&mut show.app, 1);
    ok(take(&mut show.app, &stop, false));
    assert!(!state(&show.app).0, "stopped");
    shows(&mut show.app, 0);
}

/// Music for one moment: a playlist a step starts "till the next step" is
/// faded out as the next step is faded to — playing on through the fade —
/// and stopped at its end, back on its first item; cut to, at once. A step
/// that carries it on itself keeps it playing.
#[test]
fn music_till_the_next_step_fades_out_and_stops_as_it_is_taken() {
    use varda::engine::value::playlist::PlaylistItem;
    let Some(mut show) = show() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items: Vec<PlaylistItem> = [("one.png", [255u8, 0, 0]), ("two.png", [0, 255, 0])]
        .into_iter()
        .map(|(name, rgb)| {
            let path = dir.path().join(name);
            image::RgbaImage::from_pixel(16, 9, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
                .save(&path)
                .unwrap();
            PlaylistItem::new(path.to_string_lossy())
        })
        .collect();
    let list = id(send(
        &mut show.app,
        EngineCommand::AddPlaylistDeck {
            channel_uuid: show.c.clone(),
            items,
            settings: None,
        },
    ));
    let mut new_step = |name: &str| {
        id(send(
            &mut show.app,
            EngineCommand::AddRundownStep {
                name: name.to_string(),
                buses: Some(vec![]),
                main: false,
            },
        ))
    };
    let (a, b, c) = (new_step("A"), new_step("B"), new_step("C"));
    for (step, action) in [(&a, PlayAction::Start), (&c, PlayAction::Resume)] {
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownPlay {
                uuid: step.clone(),
                deck_uuid: list.clone(),
                action: Some(action),
            },
        ));
    }
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownUntilNext {
            uuid: a.clone(),
            deck_uuid: list.clone(),
            on: true,
        },
    ));
    ok(send(&mut show.app, EngineCommand::SetRundownFade { secs: 1.0 }));
    let state = |app: &VardaApp| {
        let state = app.build_engine_state();
        let decks = state.mixer.channels.iter().flat_map(|ch| &ch.decks);
        let deck = decks.into_iter().find(|d| d.uuid == list);
        let list = deck.and_then(|d| d.playlist.clone()).expect("a playlist");
        (list.playing, list.shown)
    };
    let shows = |app: &mut VardaApp, item: usize| {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while state(app).1 != Some(item) {
            assert!(std::time::Instant::now() < until, "item {item} never shown");
            run(app, 0.05);
        }
    };
    shows(&mut show.app, 0);

    // Faded to: it plays on through B's fade, then stops on its first item.
    ok(take(&mut show.app, &a, false));
    assert!(state(&show.app).0, "A starts it");
    ok(send(
        &mut show.app,
        EngineCommand::ShowPlaylistItem {
            deck_uuid: list.clone(),
            index: 1,
        },
    ));
    shows(&mut show.app, 1);
    ok(take(&mut show.app, &b, true));
    run(&mut show.app, 0.4);
    assert!(state(&show.app).0, "playing on through the fade");
    run(&mut show.app, 1.0);
    assert!(!state(&show.app).0, "stopped once faded out");
    shows(&mut show.app, 0);

    // Cut to: stopped at once.
    ok(take(&mut show.app, &a, false));
    assert!(state(&show.app).0);
    ok(take(&mut show.app, &b, false));
    assert!(!state(&show.app).0, "a cut stops it at once");

    // A step that carries it on itself keeps it.
    ok(take(&mut show.app, &a, false));
    ok(take(&mut show.app, &c, true));
    run(&mut show.app, 1.4);
    assert!(state(&show.app).0, "C carries it on");

    // On no screen, stopped by a step faded to: it fades out first.
    let d = new_step_named(&mut show.app, "D");
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownPlay {
            uuid: d.clone(),
            deck_uuid: list.clone(),
            action: Some(PlayAction::Stop),
        },
    ));
    ok(take(&mut show.app, &d, true));
    run(&mut show.app, 0.4);
    assert!(state(&show.app).0, "stopping, it fades out first");
    run(&mut show.app, 1.0);
    assert!(!state(&show.app).0, "then stops");

    // The step taken last removed before the next is taken: what it played
    // till the next step stops all the same.
    ok(take(&mut show.app, &a, false));
    assert!(state(&show.app).0);
    ok(send(&mut show.app, EngineCommand::RemoveRundownStep { uuid: a.clone() }));
    ok(take(&mut show.app, &b, false));
    assert!(!state(&show.app).0, "stopped though its step is gone");
}

/// A clip a step played till the next step, which the next step puts on a
/// screen without starting or stopping it: stopped, and kept so — Auto
/// Play, as it arrives, does not start it again.
#[test]
fn a_clip_stopped_at_the_next_step_stays_stopped_as_that_step_shows_it() {
    let Some(mut show) = show() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path()) else {
        return;
    };
    let sting = id(send(&mut show.app, EngineCommand::AddChannel));
    let deck = id(send(
        &mut show.app,
        EngineCommand::AddVideoDeck {
            channel_uuid: sting,
            path,
        },
    ));
    let a = new_step_named(&mut show.app, "A");
    let b = new_step_named(&mut show.app, "B");
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownPlay {
            uuid: a.clone(),
            deck_uuid: deck.clone(),
            action: Some(PlayAction::Start),
        },
    ));
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownUntilNext {
            uuid: a.clone(),
            deck_uuid: deck.clone(),
            on: true,
        },
    ));
    ok(send(
        &mut show.app,
        EngineCommand::SetRundownScreen {
            uuid: b.clone(),
            bus_uuid: show.wall[0].clone(),
            input: Some(BusInputRef::Deck(deck.clone())),
        },
    ));
    let playing = |app: &VardaApp| {
        let (ch, d) = app.mixer_ref().find_deck_by_uuid(&deck).expect("the deck");
        app.mixer_ref().channels()[ch].decks[d]
            .deck
            .playback_snapshot()
            .expect("a clip")
            .playing
    };
    run(&mut show.app, 0.3);
    ok(take(&mut show.app, &a, false));
    run(&mut show.app, 0.3);
    assert!(playing(&show.app), "A starts it, on no screen");
    ok(take(&mut show.app, &b, false));
    run(&mut show.app, 0.5);
    assert!(!playing(&show.app), "stopped by B, and kept so as B shows it");
}

fn new_step_named(app: &mut VardaApp, name: &str) -> String {
    id(send(
        app,
        EngineCommand::AddRundownStep {
            name: name.to_string(),
            buses: Some(vec![]),
            main: false,
        },
    ))
}

/// Each talk its own timer: a step starts the timer counting down from a
/// length of its own, the next one counting up — and a count down of no
/// time, or a timer the step does not start, is refused.
#[test]
fn a_step_sets_the_timer_it_starts() {
    let Some(mut show) = show() else {
        return;
    };
    let timer = id(send(
        &mut show.app,
        EngineCommand::AddTimerDeck {
            channel_uuid: show.c.clone(),
            settings: Some(TimerSettings {
                mode: TimerMode::Countdown,
                duration_secs: 300,
                ..TimerSettings::default()
            }),
        },
    ));
    let mut step_with = |name: &str, set: TimerSet| {
        let step = id(send(
            &mut show.app,
            EngineCommand::AddRundownStep {
                name: name.to_string(),
                buses: Some(vec![]),
                main: false,
            },
        ));
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownPlay {
                uuid: step.clone(),
                deck_uuid: timer.clone(),
                action: Some(PlayAction::Start),
            },
        ));
        ok(send(
            &mut show.app,
            EngineCommand::SetRundownTimer {
                uuid: step.clone(),
                deck_uuid: timer.clone(),
                timer: Some(set),
            },
        ));
        step
    };
    let talk = step_with("Talk", TimerSet::Down { secs: 900 });
    let panel = step_with("Panel", TimerSet::Up);
    let reading = |app: &VardaApp| {
        let state = app.build_engine_state();
        let decks = state.mixer.channels.iter().flat_map(|ch| &ch.decks);
        let deck = decks.into_iter().find(|d| d.uuid == timer);
        let timer = deck.and_then(|d| d.timer.clone()).expect("a timer");
        (timer.settings.mode, timer.settings.duration_secs, timer.running)
    };
    ok(take(&mut show.app, &talk, false));
    assert_eq!(reading(&show.app), (TimerMode::Countdown, 900, true));
    ok(take(&mut show.app, &panel, false));
    let (mode, _, running) = reading(&show.app);
    assert_eq!((mode, running), (TimerMode::CountUp, true));

    // Undoing an edit made before the take keeps the talk's count down: the
    // step set it for this run, not in the timer's saved settings.
    ok(send(
        &mut show.app,
        EngineCommand::RenameRundownStep {
            uuid: panel.clone(),
            name: "Panel 2".into(),
        },
    ));
    ok(take(&mut show.app, &talk, false));
    assert_eq!(reading(&show.app), (TimerMode::Countdown, 900, true));
    ok(send(&mut show.app, EngineCommand::Undo));
    assert_eq!(reading(&show.app), (TimerMode::Countdown, 900, true), "kept by an undo");
    // Undoing an edit of the timer's own look — its saved settings changed —
    // keeps the count too.
    ok(send(
        &mut show.app,
        EngineCommand::SetTimerSettings {
            deck_uuid: timer.clone(),
            settings: TimerSettings {
                mode: TimerMode::Countdown,
                duration_secs: 300,
                warn_secs: 45,
                ..TimerSettings::default()
            },
        },
    ));
    ok(take(&mut show.app, &talk, false));
    // 400 s into the talk's 900 — past the timer's saved 300.
    ok(send(
        &mut show.app,
        EngineCommand::ControlTimer {
            deck_uuid: timer.clone(),
            action: varda::engine::value::timer::TimerAction::Adjust { secs: -400 },
        },
    ));
    ok(send(&mut show.app, EngineCommand::Undo));
    assert_eq!(
        reading(&show.app),
        (TimerMode::Countdown, 900, true),
        "kept by undoing the timer's look, counting on past its saved length"
    );
    // Set to show the time of day in its panel, the step still starts it
    // counting down from the talk's length.
    ok(send(
        &mut show.app,
        EngineCommand::SetTimerSettings {
            deck_uuid: timer.clone(),
            settings: TimerSettings {
                mode: TimerMode::Clock,
                ..TimerSettings::default()
            },
        },
    ));
    ok(take(&mut show.app, &talk, false));
    assert_eq!(reading(&show.app), (TimerMode::Countdown, 900, true), "over a clock");

    let zero = refused(send(
        &mut show.app,
        EngineCommand::SetRundownTimer {
            uuid: talk.clone(),
            deck_uuid: timer.clone(),
            timer: Some(TimerSet::Down { secs: 0 }),
        },
    ));
    assert!(zero.contains("longer than no time"), "{zero}");
    let elsewhere = refused(send(
        &mut show.app,
        EngineCommand::SetRundownTimer {
            uuid: talk.clone(),
            deck_uuid: show.c.clone(),
            timer: Some(TimerSet::Up),
        },
    ));
    assert!(elsewhere.contains("timer"), "{elsewhere}");
}
