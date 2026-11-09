//! A smaller show when no output needs more.
//!
//! A show is drawn at its own size and then shown, on every output, at that
//! output's: a 4K show feeding a 1080p LED wall, a 1080p recording and a
//! 1080p Blackmagic output draws four times the pixels any of them shows.
//! On Max5's conference show (M5 Max) 1080p took half the GPU 4K did, the
//! picture on every output the same. Nida does not change the size itself
//! (an operator may have a 4K output coming): it says so, and offers it.

use crate::engine::value::led::{LedFit, LedLayout};
use crate::engine::value::surface::ContentMapping;
use crate::renderer::context::{OutputSource, OutputTarget, UnifiedOutput};

/// A smaller size for the show that every output still gets all it shows
/// from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RenderHint {
    /// The size offered.
    pub size: (u32, u32),
    /// Its pixels as a share of the show's now, in percent.
    pub pixels_pct: u32,
    /// What follows the show's size — an NDI or Syphon send, recordings set
    /// to the show's size — by name: at the smaller size it is that size too.
    pub following: Vec<String>,
}

/// The sizes offered, as the resolution menu offers them.
const SIZES: [(u32, u32); 8] = [
    (1280, 720),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (1080, 1920),
    (2160, 3840),
    (1080, 1350),
    (1080, 1080),
];

/// How much of a `picture`'s detail an output area of `out` pixels shows:
/// its larger ratio either way — 0.5 for a 4K picture on a 1080p screen, 1
/// or more where every pixel of it counts.
fn share(out: (f32, f32), picture: (f32, f32)) -> f32 {
    (out.0 / picture.0.max(1.0)).max(out.1 / picture.1.max(1.0))
}

/// The size to offer a show of `master` whose outputs show at most `need`
/// of its detail: the smallest offered size of the show's shape no smaller
/// than that either way — if it draws at most 60% of the pixels, else none
/// (too little to be worth a change).
pub(crate) fn suggest(master: (u32, u32), need: f32) -> Option<(u32, u32)> {
    let need = need.min(1.0);
    #[allow(clippy::cast_precision_loss)]
    let wanted = (master.0 as f32 * need, master.1 as f32 * need);
    let area = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
    // The same shape, within half a percent.
    let same_shape = |(w, h): (u32, u32)| {
        let (a, b) = (u64::from(w) * u64::from(master.1), u64::from(h) * u64::from(master.0));
        a.abs_diff(b) * 200 <= a
    };
    #[allow(clippy::cast_precision_loss)]
    let size = SIZES
        .into_iter()
        .filter(|&size| same_shape(size))
        .filter(|&(w, h)| w as f32 + 0.5 >= wanted.0 && h as f32 + 0.5 >= wanted.1)
        .min_by_key(|&size| area(size))?;
    (area(size) * 10 <= area(master) * 6).then_some(size)
}

impl super::VardaApp {
    /// A smaller size for the show, when every output with a size of its
    /// own — screens, LED feeds, a Blackmagic output, the recording and the
    /// stream as set or as running — shows less of its detail than its size
    /// gives (see [`suggest`]). None while that is not known for sure: a
    /// window waiting for its screen, a send with an encoder running that a
    /// new size would stop, or nothing shown at a size of its own.
    pub fn render_hint(&self) -> Option<RenderHint> {
        let master = (self.render_width, self.render_height);
        #[allow(clippy::cast_precision_loss)]
        let whole = (master.0 as f32, master.1 as f32);
        // A source's picture in the show's pixels: a bus's region of it.
        #[allow(clippy::cast_precision_loss)]
        let picture = |source: &OutputSource| match source {
            OutputSource::BusProgram(uuid) => self.production_buses.get(uuid).map_or(whole, |bus| {
                let canvas = bus.canvas();
                (
                    canvas.width.clamp(1, master.0.max(1)) as f32,
                    canvas.height.clamp(1, master.1.max(1)) as f32,
                )
            }),
            _ => whole,
        };
        // An LED feed drawn over an output area of `out` pixels: each screen
        // at its share of it (Codex: a 1080p feed in a 4K window).
        #[allow(clippy::cast_precision_loss)]
        let led_need = |layout: &LedLayout, out: (f32, f32)| {
            let k = (
                out.0 / layout.feed[0].max(1) as f32,
                out.1 / layout.feed[1].max(1) as f32,
            );
            layout
                .screens
                .iter()
                .map(|screen| {
                    let size = (screen.size[0] as f32 * k.0, screen.size[1] as f32 * k.1);
                    match screen.fit {
                        // A part is in its picture's pixels, the show's.
                        LedFit::Part { width, height, .. } => share(size, (width as f32, height as f32)),
                        _ => share(size, picture(&screen.source)),
                    }
                })
                .fold(0.0, f32::max)
        };
        let surfaces = &self.output.surface_manager.surfaces;
        // An output area of `out` pixels showing its surfaces `assigned` —
        // all of them when none is (as it draws them). None for a warped
        // surface, which may magnify any part of its picture (Codex).
        let surfaces_need = |out: (f32, f32), assigned: &[crate::renderer::context::SurfaceAssignment]| {
            if surfaces.is_empty() {
                // The whole show, fitted.
                return Some(share(out, whole));
            }
            let mut need = 0.0_f32;
            for surface in surfaces.iter().filter(|surface| {
                assigned.is_empty() || assigned.iter().any(|a| a.enabled && a.surface_uuid == surface.uuid)
            }) {
                // A warp, a path, or a warp made from its shape (Codex).
                if surface.warp.is_some() || surface.path.is_some() || surface.warp_bound {
                    return None;
                }
                let source = picture(&surface.source);
                need = need.max(match surface.content_mapping {
                    // Its place on the output is its part of the source.
                    ContentMapping::Mapped => share(out, source),
                    ContentMapping::Fill => {
                        let bb = surface.bounding_box();
                        share((bb.width * out.0, bb.height * out.1), source)
                    }
                });
            }
            Some(need)
        };
        #[allow(clippy::cast_precision_loss)]
        let area = |(w, h): (u32, u32)| (w as f32, h as f32);
        // The dome's picture is the show projected — turned, curved — so
        // what an output of it needs from the show is not its size (Codex).
        let domed = |source: &OutputSource| matches!(source, OutputSource::Domemaster);
        let leds = self.output.outputs.iter().filter_map(UnifiedOutput::led);
        if surfaces.iter().any(|surface| domed(&surface.source))
            || self.output.outputs.iter().filter_map(UnifiedOutput::source).any(domed)
            || leds.flat_map(|led| &led.screens).any(|screen| domed(&screen.source))
            || domed(&self.recording.settings.options.source)
            || domed(&self.streaming.settings.options.source)
        {
            return None;
        }
        let mut need = 0.0_f32;
        let mut shown = false;
        let mut following = Vec::new();
        for output in &self.output.outputs {
            match output {
                UnifiedOutput::Window(w) => {
                    // Waiting for its screen, at a size not its own.
                    if w.awaiting_display.is_some() {
                        return None;
                    }
                    shown = true;
                    // Drawn turned, at its size turned (see `effective_dimensions`).
                    let out = area(w.rotation.effective_dimensions(w.size.width, w.size.height));
                    need = need.max(match &w.led {
                        Some(led) => led_need(led, out),
                        None => surfaces_need(out, &w.surface_assignments)?,
                    });
                }
                // A Blackmagic output keeps its format's size.
                UnifiedOutput::Headless(h) if h.decklink.is_some() => {
                    shown = true;
                    // Drawn at its own size, turned or not (Codex).
                    let out = area((h.width, h.height));
                    need = need.max(match &h.led {
                        Some(led) => led_need(led, out),
                        None if h.surface_assignments.is_empty() => share(out, picture(&h.source)),
                        None => surfaces_need(out, &h.surface_assignments)?,
                    });
                }
                // A Blackmagic output whose device is not here: back, it
                // takes its format's size again, which is not known now (Codex).
                UnifiedOutput::Headless(h) if matches!(h.target, OutputTarget::DeckLink { .. }) => {
                    return None;
                }
                // Any other is kept at the show's size: it follows it — but
                // one with its encoder running is stopped by a new size.
                UnifiedOutput::Headless(h) => {
                    if h.subprocess.is_some() {
                        return None;
                    }
                    following.push(h.name.clone());
                }
            }
        }
        // Still opening: an LED feed's needs are known from its layout; a
        // window's are not until its screen is.
        #[allow(clippy::cast_precision_loss)]
        for config in &self.output.pending_output_creates {
            let led = config.led.as_ref()?;
            shown = true;
            need = need.max(led_need(led, (led.feed[0] as f32, led.feed[1] as f32)));
        }
        // The recording and the stream: running, at the size they run at —
        // a new size of the show does not change it; else as set.
        let record = &self.recording.settings.options;
        let recording = self.recording.output.first();
        #[allow(clippy::cast_precision_loss)]
        match (recording, record.size) {
            (Some(UnifiedOutput::Headless(h)), _) => {
                shown = true;
                need = need.max(share((h.width as f32, h.height as f32), picture(&record.source)));
            }
            (_, Some((w, h))) => {
                shown = true;
                need = need.max(share((w as f32, h as f32), picture(&record.source)));
            }
            (_, None) => following.push("Recordings".to_string()),
        }
        let stream = &self.streaming.settings;
        let options = &stream.options;
        #[allow(clippy::cast_precision_loss)]
        if let Some(UnifiedOutput::Headless(h)) = self.streaming.output.first() {
            shown = true;
            need = need.max(share((h.width as f32, h.height as f32), picture(&options.source)));
        } else if stream.destinations.iter().any(|d| d.enabled) {
            shown = true;
            let out = (options.width as f32, options.height as f32);
            need = need.max(share(out, picture(&options.source)));
        }
        if !shown {
            return None;
        }
        // Effects on Main or a whole input — a zoom — work on the picture
        // already drawn at the show's size: smaller, they have less of it to
        // work on (Codex).
        let channels = self.mixer.channels();
        if self.mixer.master_effects().iter().any(|e| e.enabled)
            || channels.iter().flat_map(|c| &c.effects).any(|e| e.enabled)
        {
            return None;
        }
        let decks = || channels.iter().flat_map(|channel| &channel.decks);
        // A picture at its native size (Center): the show smaller, it covers
        // more of it — framed differently, not only less sharp (Codex).
        if decks().any(|slot| slot.deck.scaling_mode() == Some(crate::deck::ScalingMode::Center)) {
            return None;
        }
        // A layer drawn at half or a quarter of the show's size: the show
        // smaller, it is smaller still (Codex).
        let finest = decks()
            .map(|slot| match slot.deck.render_scale() {
                crate::deck::DeckRenderScale::Full => 1.0,
                crate::deck::DeckRenderScale::Half => 0.5,
                crate::deck::DeckRenderScale::Quarter => 0.25,
            })
            .fold(1.0_f32, f32::min);
        let size = suggest(master, need / finest)?;
        let pixels_pct = u64::from(size.0) * u64::from(size.1) * 100
            / (u64::from(master.0) * u64::from(master.1)).max(1);
        Some(RenderHint {
            size,
            pixels_pct: u32::try_from(pixels_pct).unwrap_or(100),
            following,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4K outputs keep 4K; 1080p ones are offered 1080p, a quarter of the
    /// pixels; between, the next size up that is enough; a show already
    /// small, or one barely larger, is left alone; another shape gets a
    /// size of its own shape.
    #[test]
    fn the_smallest_size_enough_for_every_output() {
        let uhd = (3840, 2160);
        assert_eq!(suggest(uhd, 1.0), None);
        assert_eq!(suggest(uhd, 0.5), Some((1920, 1080)));
        assert_eq!(suggest(uhd, 0.3), Some((1280, 720)));
        assert_eq!(suggest(uhd, 0.55), Some((2560, 1440)));
        assert_eq!(suggest((1920, 1080), 0.9), None, "1080p for 1080p: nothing to save");
        assert_eq!(suggest((2560, 1440), 0.75), Some((1920, 1080)), "56% of the pixels");
        assert_eq!(suggest((2560, 1440), 0.8), None, "no smaller size enough");
        assert_eq!(suggest((1920, 1080), 0.6), Some((1280, 720)), "1152 wide: 720p is enough");
        assert_eq!(suggest((1920, 1080), 0.7), None, "1344 wide: only 1080p is enough");
        assert_eq!(suggest((2160, 3840), 0.5), Some((1080, 1920)));
        assert_eq!(suggest((3000, 2000), 0.4), None, "no size of its shape offered");
    }

    fn app_at_4k() -> Option<crate::app::VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        let mut app = crate::app::VardaApp::new(gpu, &crate::testing::headless_config()).ok()?;
        app.set_render_resolution(3840, 2160);
        Some(app)
    }

    /// A 4K show recorded at 1080p, and nothing else showing it: 1080p is
    /// offered, a quarter of the pixels. Recorded at the show's own size
    /// instead, nothing shows it at a size of its own: no hint.
    #[test]
    fn a_4k_show_recorded_at_1080p_is_offered_1080p() {
        let Some(mut app) = app_at_4k() else {
            return;
        };
        app.recording.settings.options.size = Some((1920, 1080));
        assert_eq!(
            app.render_hint(),
            Some(RenderHint {
                size: (1920, 1080),
                pixels_pct: 25,
                following: Vec::new()
            })
        );
        // Said over the API too, with the show's size.
        let render = app.build_engine_state().render;
        assert_eq!((render.width, render.height), (3840, 2160));
        assert_eq!(render.hint.map(|h| h.size), Some((1920, 1080)));
        // Recording the dome's picture: projected, what it needs from the
        // show is not its size.
        app.recording.settings.options.source = OutputSource::Domemaster;
        assert_eq!(app.render_hint(), None, "the dome's picture");
        app.recording.settings.options.source = OutputSource::Master;
        // An effect on Main — a zoom — works on the picture already drawn.
        let shader = crate::isf::ISFShader::from_file(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders/transform.fs"),
        )
        .expect("the transform shader");
        let zoom = crate::deck::Effect::new(&app.context, shader).expect("an effect");
        app.mixer.master_effects_mut().push(zoom);
        assert_eq!(app.render_hint(), None, "an effect on Main");
        app.mixer.master_effects_mut().clear();
        assert!(app.render_hint().is_some(), "the effect gone");
        app.recording.settings.options.size = None;
        assert_eq!(app.render_hint(), None, "nothing at a size of its own");
    }

    /// An LED feed taking a 3840-wide strip of a 4K show onto a 1920-pixel
    /// wall needs only half its detail: 1080p, the recordings at the show's
    /// size said to follow it. A send at the show's size follows it too; a
    /// 4K stream needs it all; a window waiting for its screen is not known.
    #[test]
    fn what_each_output_needs_counts() {
        use crate::engine::value::led::{LedFit, LedLayout};
        let Some(mut app) = app_at_4k() else {
            return;
        };
        app.recording.settings.options.size = None;
        let mut layout = LedLayout::single([1920, 1080]);
        layout.screens[0].size = [1920, 688];
        layout.screens[0].fit = LedFit::Part {
            x: 0,
            y: 736,
            width: 3840,
            height: 1376,
        };
        let crate::engine::CommandResult::OkWithId { .. } = app.cmd_create_led_output(None, layout)
        else {
            panic!("an LED output");
        };
        let hint = app.render_hint().expect("a hint");
        assert_eq!(hint.size, (1920, 1080));
        assert_eq!(hint.following, vec!["Recordings".to_string()]);

        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::CreateHeadlessOutput {
                target: crate::renderer::context::OutputTarget::SyphonServer {
                    server_name: "Out".into(),
                },
            }),
            crate::engine::CommandResult::Ok
        ));
        let hint = app.render_hint().expect("still a hint");
        assert_eq!(hint.following.len(), 2, "the send follows too: {:?}", hint.following);

        // A layer drawn at half the show's size: at 1080p it would be half
        // of that, below what the wall shows.
        let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
        let crate::engine::CommandResult::OkWithId { uuid: deck } =
            app.execute_command(crate::engine::EngineCommand::AddSolidColorDeck {
                channel_uuid: channel,
                color: [1.0, 0.0, 0.0, 1.0],
            })
        else {
            panic!("a layer");
        };
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::SetDeckRenderScale {
                deck_uuid: deck.clone(),
                scale: crate::engine::types::DeckRenderScale::Half,
            }),
            crate::engine::CommandResult::Ok
        ));
        assert_eq!(app.render_hint(), None, "a half-size layer needs the 4K show");
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::SetDeckRenderScale {
                deck_uuid: deck.clone(),
                scale: crate::engine::types::DeckRenderScale::Full,
            }),
            crate::engine::CommandResult::Ok
        ));
        assert!(app.render_hint().is_some(), "back at full size");

        // A picture at its native size (Center) would cover more of a
        // smaller show: framed differently, so no hint.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logo.png");
        image::RgbaImage::from_pixel(64, 36, image::Rgba([255, 255, 255, 255]))
            .save(&path)
            .unwrap();
        let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
        let crate::engine::CommandResult::OkWithId { uuid: logo } =
            app.execute_command(crate::engine::EngineCommand::AddImageDeck {
                channel_uuid: channel,
                path,
            })
        else {
            panic!("a picture");
        };
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::SetDeckScalingMode {
                deck_uuid: logo.clone(),
                mode: crate::deck::ScalingMode::Center,
            }),
            crate::engine::CommandResult::Ok
        ));
        assert_eq!(app.render_hint(), None, "a picture at its native size");
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::RemoveDeck { deck_uuid: logo }),
            crate::engine::CommandResult::Ok
        ));
        assert!(app.render_hint().is_some(), "the picture gone");

        let stream = &mut app.streaming.settings;
        stream.destinations[0].enabled = true;
        (stream.options.width, stream.options.height) = (3840, 2160);
        assert_eq!(app.render_hint(), None, "a 4K stream needs 4K");
        app.streaming.settings.destinations[0].enabled = false;

        app.output.pending_output_creates.push(crate::scene::OutputConfig::default_windowed());
        assert_eq!(app.render_hint(), None, "a window's screen not known yet");
    }

    #[test]
    fn share_is_the_larger_ratio() {
        assert!((share((1920.0, 1080.0), (3840.0, 2160.0)) - 0.5).abs() < 1e-6);
        assert!((share((1920.0, 688.0), (1920.0, 1080.0)) - 1.0).abs() < 1e-6);
    }
}
