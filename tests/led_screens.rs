//! LED feeds end to end: screens placed at exact pixels of one output, each
//! showing its part of Main or its bus — what an LED processor reads back.

use varda::app::VardaApp;
use varda::engine::value::led::{LedFit, LedLayout, LedScreen};
use varda::engine::{BusInputRef, CommandResult, EngineCommand};
use varda::renderer::context::{OutputSource, OutputTarget, UnifiedOutput};

mod common;

const W: u32 = 320;
const H: u32 = 180;

fn headless_app() -> Option<VardaApp> {
    let gpu = common::headless_gpu()?;
    Some(VardaApp::new(gpu, &varda::testing::headless_config()).expect("VardaApp::new"))
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

/// A started NDI output: headless, at the render resolution.
fn feed_output(app: &mut VardaApp) -> String {
    let r = send(
        app,
        EngineCommand::CreateHeadlessOutput {
            target: OutputTarget::NdiSend {
                sender_name: "LED".into(),
            },
        },
    );
    assert!(matches!(r, CommandResult::Ok), "{r:?}");
    let uuid = app
        .build_engine_state()
        .outputs
        .windows
        .last()
        .unwrap()
        .uuid
        .clone();
    let r = send(
        app,
        EngineCommand::StartOutput {
            output_uuid: uuid.clone(),
        },
    );
    assert!(matches!(r, CommandResult::Ok), "{r:?}");
    uuid
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
    assert_eq!(tex.format(), wgpu::TextureFormat::Rgba8UnormSrgb);
    let ctx = app.gpu_context();
    let (w, h) = (tex.width(), tex.height());
    let padded =
        (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("led readback"),
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

/// Every channel of `got` within the output's dither of `want`.
fn near(got: [u8; 4], want: [u8; 3]) -> bool {
    (0..3).all(|c| got[c].abs_diff(want[c]) <= 2)
}

/// Main shows a picture whose every pixel differs from its neighbours; a
/// second channel is solid green on a CUBE bus.
fn show(app: &mut VardaApp) -> (image::RgbaImage, String, tempfile::TempDir) {
    send(
        app,
        EngineCommand::SetRenderResolution {
            width: W,
            height: H,
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wall.png");
    let picture = image::RgbaImage::from_fn(W, H, |x, y| {
        image::Rgba([
            ((x * 37 + y * 11) % 256) as u8,
            ((x * 5 + y * 29) % 256) as u8,
            ((x ^ y) % 256) as u8,
            255,
        ])
    });
    picture.save(&path).unwrap();
    let state = app.build_engine_state();
    let (a, b) = (
        state.mixer.channels[0].uuid.clone(),
        state.mixer.channels[1].uuid.clone(),
    );
    id(send(
        app,
        EngineCommand::AddImageDeck {
            channel_uuid: a,
            path,
        },
    ));
    id(send(
        app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: b.clone(),
            color: [0.0, 1.0, 0.0, 1.0],
        },
    ));
    send(app, EngineCommand::SetCrossfader(0.0));
    let cube = id(send(
        app,
        EngineCommand::AddBus {
            name: "CUBE".into(),
            width: W,
            height: H,
        },
    ));
    send(
        app,
        EngineCommand::SetBusPreview {
            uuid: cube.clone(),
            input: BusInputRef::Channel(b),
        },
    );
    send(app, EngineCommand::BusCut { uuid: cube.clone() });
    (picture, cube, dir)
}

fn render(app: &mut VardaApp) {
    for _ in 0..3 {
        app.render_mixer_frame();
        app.render_outputs();
    }
}

#[test]
fn screens_land_on_their_own_pixels_of_the_feed_showing_their_part() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let (picture, cube, _dir) = show(&mut app);
    let out = feed_output(&mut app);
    // The wall shows the 100×60 of Main from (150, 90), put at (10, 20); the
    // cube fills its 40×40 at (200, 100) with the CUBE bus.
    let wall = LedScreen {
        fit: LedFit::Part {
            x: 150,
            y: 90,
            width: 100,
            height: 60,
        },
        ..LedScreen::new("Wall", [100, 60], [10, 20], OutputSource::Master)
    };
    let cube_screen = LedScreen {
        fit: LedFit::Fill,
        ..LedScreen::new("Cube", [40, 40], [200, 100], OutputSource::BusProgram(cube))
    };
    let layout = LedLayout {
        feed: [W, H],
        screens: vec![wall, cube_screen],
        test_card: false,
    };
    let r = send(
        &mut app,
        EngineCommand::SetOutputLed {
            output_uuid: out.clone(),
            layout: Some(layout),
        },
    );
    assert!(matches!(r, CommandResult::Ok), "{r:?}");
    render(&mut app);

    let px = output_pixels(&app, &out);
    let at = |x: u32, y: u32| px[(y * W + x) as usize];
    let mut wrong = Vec::new();
    for y in 0..60 {
        for x in 0..100 {
            let want = picture.get_pixel(150 + x, 90 + y);
            if !near(at(10 + x, 20 + y), [want[0], want[1], want[2]]) {
                wrong.push((10 + x, 20 + y));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wall pixels are not Main's pixel for them; first {:?}: got {:?}",
        wrong.len(),
        wrong.first(),
        wrong.first().map(|&(x, y)| at(x, y))
    );
    for (x, y) in [(200, 100), (239, 100), (200, 139), (239, 139), (220, 120)] {
        assert!(
            near(at(x, y), [0, 255, 0]),
            "cube at {x},{y}: {:?}",
            at(x, y)
        );
    }
    // Just outside each screen, and elsewhere, the feed is black.
    for (x, y) in [
        (9, 20),
        (110, 20),
        (10, 19),
        (10, 80),
        (199, 100),
        (240, 100),
        (300, 10),
    ] {
        assert!(
            near(at(x, y), [0, 0, 0]),
            "outside at {x},{y}: {:?}",
            at(x, y)
        );
    }
}

#[test]
fn a_test_card_shows_each_screen_its_white_edge_and_nothing_of_the_show() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let (_picture, _cube, _dir) = show(&mut app);
    let out = feed_output(&mut app);
    let mut layout = LedLayout {
        feed: [W, H],
        screens: vec![LedScreen::new(
            "Cube L",
            [120, 90],
            [30, 40],
            OutputSource::Master,
        )],
        test_card: true,
    };
    layout.screens[0].fit = LedFit::Fill;
    send(
        &mut app,
        EngineCommand::SetOutputLed {
            output_uuid: out.clone(),
            layout: Some(layout),
        },
    );
    render(&mut app);
    let px = output_pixels(&app, &out);
    let at = |x: u32, y: u32| px[(y * W + x) as usize];
    for (x, y) in [(30, 40), (149, 40), (30, 129), (149, 129), (90, 40)] {
        assert!(
            near(at(x, y), [255, 255, 255]),
            "edge at {x},{y}: {:?}",
            at(x, y)
        );
    }
    assert!(
        near(at(29, 40), [0, 0, 0]) && near(at(150, 129), [0, 0, 0]),
        "outside"
    );
}

#[test]
fn an_output_with_no_layout_shows_its_source_again() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let (picture, _cube, _dir) = show(&mut app);
    let out = feed_output(&mut app);
    let mut layout = LedLayout::single([W, H]);
    layout.screens[0].size = [10, 10];
    send(
        &mut app,
        EngineCommand::SetOutputLed {
            output_uuid: out.clone(),
            layout: Some(layout),
        },
    );
    render(&mut app);
    assert!(near(
        output_pixels(&app, &out)[(H / 2 * W + W / 2) as usize],
        [0, 0, 0]
    ));
    send(
        &mut app,
        EngineCommand::SetOutputLed {
            output_uuid: out.clone(),
            layout: None,
        },
    );
    render(&mut app);
    let want = picture.get_pixel(W / 2, H / 2);
    let got = output_pixels(&app, &out)[(H / 2 * W + W / 2) as usize];
    assert!(
        near(got, [want[0], want[1], want[2]]),
        "Main again: {got:?}"
    );
}

#[test]
fn a_screen_gets_a_bus_of_its_own_named_after_it() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let out = feed_output(&mut app);
    let cube = |at| LedScreen::new("Cube", [300, 300], at, OutputSource::Master);
    let layout = LedLayout {
        feed: [1920, 1080],
        screens: vec![cube([0, 0]), cube([300, 0])],
        test_card: false,
    };
    send(
        &mut app,
        EngineCommand::SetOutputLed {
            output_uuid: out.clone(),
            layout: Some(layout),
        },
    );
    let screens = |app: &mut VardaApp| {
        app.build_engine_state()
            .outputs
            .windows
            .iter()
            .find(|w| w.uuid == out)
            .and_then(|w| w.led.clone())
            .unwrap()
            .screens
    };
    let ids: Vec<String> = screens(&mut app).iter().map(|s| s.uuid.clone()).collect();
    let mut buses = Vec::new();
    for screen in &ids {
        buses.push(id(send(
            &mut app,
            EngineCommand::CreateLedScreenBus {
                output_uuid: out.clone(),
                screen_uuid: screen.clone(),
            },
        )));
    }
    let names: Vec<String> = buses
        .iter()
        .map(|b| app.production_buses().get(b).unwrap().name.clone())
        .collect();
    assert_eq!(names, ["Cube", "Cube 2"]);
    let now = screens(&mut app);
    assert_eq!(now[0].source, OutputSource::BusProgram(buses[0].clone()));
    assert_eq!(now[1].source, OutputSource::BusProgram(buses[1].clone()));
    let r = send(
        &mut app,
        EngineCommand::CreateLedScreenBus {
            output_uuid: out,
            screen_uuid: "nope".into(),
        },
    );
    assert!(matches!(r, CommandResult::Err { .. }), "{r:?}");
}
