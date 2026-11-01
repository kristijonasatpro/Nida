//! How lively the control window has to be: whether anything in the show
//! moves by itself. The window is drawn every frame while pictures move —
//! a clip or playlist playing, a live source, an animated generator or
//! effect, a take, fade or crossfade, the timeline running — at a middle
//! rate while only sound moves (the meters), and at an idle rate when the
//! show stands still: an empty or static show then costs the GPU a fraction
//! of a window drawn at the display's rate (an M5 Max with a 4K window spent
//! most of its GPU time redrawing an unchanged desk). Hands on the window
//! keep it at full rate whatever the show does (see the runner).

use super::super::VardaApp;

/// What moves in the show, for how often the control window is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum UiMotion {
    /// Nothing moves by itself.
    #[default]
    Still,
    /// Only sound: meters move.
    Sound,
    /// Pictures move.
    Pictures,
}

/// A camera or captured screen moves while a new picture came within this.
const FRESH: std::time::Duration = std::time::Duration::from_secs(1);

/// A meter above this many dBFS is moving: below it is silence, or the
/// least of noise.
const SOUND_FLOOR_DB: f32 = -60.0;

impl VardaApp {
    /// What moves in the show now.
    pub(crate) fn ui_motion(&self) -> UiMotion {
        self.ui_motion_why().0
    }

    /// What moves in the show now, and the first thing found moving — for
    /// the performance figures to say why the control window is drawn as
    /// often as it is.
    pub(crate) fn ui_motion_why(&self) -> (UiMotion, Option<String>) {
        if let Some(why) = self.moving_picture() {
            (UiMotion::Pictures, Some(why))
        } else if self.audio_mix.sounding(SOUND_FLOOR_DB) {
            (UiMotion::Sound, Some("a meter above -60 dBFS".to_string()))
        } else {
            (UiMotion::Still, None)
        }
    }

    /// The first picture found changing by itself — what a tile, a monitor
    /// or a preview would show differently next frame without a hand on
    /// anything — said for a person; `None` when none does.
    fn moving_picture(&self) -> Option<String> {
        let modulation = self.mixer.modulation();
        // Something modulated, the timeline running, a crossfade, a take or
        // a fade: all move until they are done.
        if modulation.has_modulation_for_any() {
            return Some("something is modulated".into());
        }
        if self.transport.running() {
            return Some("the timeline runs".into());
        }
        if self.mixer.auto_crossfade().is_some() {
            return Some("a crossfade".into());
        }
        if self.mixer.main().fade.is_some() {
            return Some("Main is fading".into());
        }
        if let Some(bus) = self.production_buses.iter().find(|b| b.transition().is_some()) {
            return Some(format!("bus '{}' is taking", bus.name));
        }
        let main = self.mixer.main();
        let animated = |effects: &[crate::deck::Effect]| {
            effects
                .iter()
                .find(|e| e.enabled && e.offline().is_none() && e.shader.animates())
                .map(crate::deck::Effect::name)
        };
        if let Some(effect) = animated(self.mixer.master_effects()) {
            return Some(format!("master effect '{effect}' animates"));
        }
        for channel in self.mixer.channels() {
            if main.overlay_settle_secs(channel.uuid()) > 0.0 {
                return Some(format!("overlay '{}' is fading", channel.name));
            }
            if let Some(effect) = animated(&channel.effects) {
                return Some(format!("effect '{effect}' on '{}' animates", channel.name));
            }
            for slot in &channel.decks {
                let deck = slot.deck.source_name();
                if matches!(
                    slot.transition_phase(),
                    crate::channel::DeckTransitionPhase::Transitioning { .. }
                ) {
                    return Some(format!("'{deck}' in '{}' is transitioning", channel.name));
                }
                if let Some(effect) = animated(&slot.deck.effects) {
                    return Some(format!("effect '{effect}' on '{deck}' animates"));
                }
                if self.deck_moving(&slot.deck) {
                    return Some(format!("'{deck}' in '{}' moves", channel.name));
                }
            }
        }
        None
    }

    /// Whether a deck's picture changes by itself now: as
    /// `Deck::moves_by_itself` says, but a camera, a captured screen, an NDI
    /// source or a stream — alone or as a talk of a Slides input — only
    /// while new pictures come: a camera not plugged in or without signal, a
    /// screen not there, a sender not sending, a still PowerPoint slide
    /// (macOS sends a capture's frames only as it changes) all stand still.
    fn deck_moving(&self, deck: &crate::deck::Deck) -> bool {
        use crate::deck::ExternalSourceKind as K;
        if let Some(slides) = deck.slides() {
            let mut moving = slides.in_transition();
            slides.for_each_deck(|talk| moving |= self.deck_moving(talk));
            return moving;
        }
        match deck.external_source_kind() {
            Some(K::Camera(id)) => self.camera_manager.receiving(id, FRESH),
            Some(K::ScreenCapture(id)) => self.screen_capture_manager.receiving(id, FRESH),
            Some(K::Ndi(idx)) => self.external_io.ndi_manager.receiving(idx, FRESH),
            Some(K::Srt(idx) | K::Hls(idx) | K::Dash(idx) | K::Rtmp(idx)) => {
                self.external_io.stream_manager.receiving(idx, FRESH)
            }
            _ => deck.moves_by_itself(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CommandResult, EngineCommand};

    fn headless_app() -> Option<VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        VardaApp::new(gpu, &crate::testing::headless_config()).ok()
    }

    fn id(result: CommandResult) -> String {
        match result {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("expected an id, got {other:?}"),
        }
    }

    /// An empty show stands still; a solid colour or a playlist stopped
    /// keeps it so; a playlist playing, a fade on Main or a running take on
    /// a bus moves it.
    #[test]
    fn a_still_show_is_still_until_something_plays_or_fades() {
        let Some(mut app) = headless_app() else {
            return;
        };
        assert_eq!(app.ui_motion(), UiMotion::Still, "an empty show");
        let channel = app.mixer_ref().channels()[0].uuid().to_string();
        id(app.execute_command(EngineCommand::AddSolidColorDeck {
            channel_uuid: channel.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        }));
        assert_eq!(app.ui_motion(), UiMotion::Still, "a solid colour");
        let list = id(app.execute_command(EngineCommand::AddPlaylistDeck {
            channel_uuid: channel.clone(),
            items: vec![],
            settings: None,
        }));
        assert_eq!(app.ui_motion(), UiMotion::Still, "a playlist stopped");
        app.execute_command(EngineCommand::SetPlaylistPlaying {
            deck_uuid: list.clone(),
            playing: true,
        });
        assert_eq!(app.ui_motion(), UiMotion::Pictures, "a playlist playing");
        app.execute_command(EngineCommand::SetPlaylistPlaying {
            deck_uuid: list,
            playing: false,
        });
        assert_eq!(app.ui_motion(), UiMotion::Still);

        // A take on a bus, running.
        let bus = app.production_buses().iter().next().unwrap().uuid().to_string();
        app.execute_command(EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: crate::bus::BusInputRef::Channel(channel),
        });
        app.execute_command(EngineCommand::BusAuto {
            uuid: bus,
            duration_secs: Some(5.0),
        });
        assert_eq!(app.ui_motion(), UiMotion::Pictures, "a take running");
    }

    /// The library's own shaders: crop and transform, on every layer, and a
    /// colour correction stand still though every shader declares the clock;
    /// a generator that reads it moves.
    #[test]
    fn the_librarys_shaders_move_only_when_they_read_the_clock() {
        let library = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders");
        let animates = |name: &str| {
            crate::isf::ISFShader::from_file(library.join(format!("{name}.fs")))
                .unwrap_or_else(|e| panic!("{name}: {e}"))
                .animates()
        };
        for still in ["crop", "transform", "color_correction", "blur", "levels"] {
            assert!(!animates(still), "{still} stands still");
        }
        assert!(animates("aurora_borealis"), "a generator reading TIME moves");
    }

    /// A live input moves only while new pictures come: a camera not plugged
    /// in — or one whose last picture is all its texture holds — stands
    /// still; an input sending pictures moves, and the reason names it.
    /// Another input shown as a layer moves with what it shows, not by
    /// itself.
    #[test]
    fn a_live_input_moves_only_while_pictures_come() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let channel = app.mixer_ref().channels()[0].uuid().to_string();
        let ch = app.mixer_ref().find_channel_by_uuid(&channel).unwrap();
        let live = |app: &VardaApp, kind, name: &str| {
            crate::deck::Deck::new_from_external(
                app.gpu_context(),
                kind,
                name.into(),
                1920,
                1080,
                64,
                36,
            )
            .unwrap()
        };
        let camera = live(&app, crate::deck::ExternalSourceKind::Camera(7), "CAM");
        app.mixer.channel_mut(ch).unwrap().add_deck(camera);
        assert_eq!(app.ui_motion(), UiMotion::Still, "a camera with no device");
        // A picture in its slot, but none new from a device: still.
        let picture = app.mixer_ref().channels()[1].composite_view.clone();
        app.mixer.channel_mut(ch).unwrap().decks.last_mut().unwrap().deck.external_source_view =
            Some(picture.clone());
        assert_eq!(app.ui_motion(), UiMotion::Still, "a camera sending nothing new");

        // An NDI source with a texture but no sender sending: still.
        let mut ndi = live(&app, crate::deck::ExternalSourceKind::Ndi(0), "Stage NDI");
        ndi.external_source_view = Some(picture.clone());
        app.mixer.channel_mut(ch).unwrap().add_deck(ndi);
        assert_eq!(app.ui_motion(), UiMotion::Still, "an NDI sender not sending");

        // A web page with its picture: live, and named.
        let mut page = live(&app, crate::deck::ExternalSourceKind::Html(0), "Lower third page");
        page.external_source_view = Some(picture);
        app.mixer.channel_mut(ch).unwrap().add_deck(page);
        let (motion, why) = app.ui_motion_why();
        assert_eq!(motion, UiMotion::Pictures, "an input with its picture");
        assert!(why.as_deref().is_some_and(|w| w.contains("'Lower third page'")), "{why:?}");
        app.mixer.channel_mut(ch).unwrap().decks.last_mut().unwrap().deck.external_source_view =
            None;
        assert_eq!(app.ui_motion(), UiMotion::Still, "its picture gone");

        // The master shown as a layer of the other channel: still with it.
        let other = app.mixer_ref().channels()[1].uuid().to_string();
        id(app.execute_command(EngineCommand::AddTapDeck {
            channel_uuid: other,
            source: crate::scene::TapSourceConfig::MasterProgram,
        }));
        assert_eq!(app.ui_motion_why(), (UiMotion::Still, None), "a tap of a still show");
    }

    /// The performance figures say how often the control window is drawn and
    /// why: still with nothing moving, and what keeps it at full rate.
    #[test]
    fn the_performance_figures_say_why() {
        let Some(mut app) = headless_app() else {
            return;
        };
        app.set_control_rate(60.0, false, None);
        let rate = |app: &VardaApp| crate::app::snapshot::build_engine_state(app).pacing.control_rate;
        let now = rate(&app);
        assert_eq!((now.motion.as_str(), now.why.as_deref(), now.period_ms), ("still", None, 60.0));
        let channel = app.mixer_ref().channels()[0].uuid().to_string();
        let list = id(app.execute_command(EngineCommand::AddPlaylistDeck {
            channel_uuid: channel,
            items: vec![],
            settings: None,
        }));
        app.execute_command(EngineCommand::SetPlaylistPlaying {
            deck_uuid: list,
            playing: true,
        });
        app.set_control_rate(0.0, true, Some(40.0));
        let now = rate(&app);
        assert_eq!(now.motion, "pictures");
        assert!(now.why.as_deref().is_some_and(|w| w.ends_with("moves")), "{:?}", now.why);
        assert!(now.hands_on);
        assert_eq!(now.widgets_ms, Some(40.0));
    }

    /// A generator moves only if its shader reads the clock.
    #[test]
    fn a_shader_moves_when_it_reads_the_clock() {
        let still = crate::isf::ISFShader::from_string(
            "/*{\"INPUTS\":[]}*/\nuniform U {\n    float TIME;\n};\n// TIME in a comment\nvoid main() {\n    float timeSum = TIMEDELTA + float(FRAMEINDEX);\n    gl_FragColor = vec4(1.0);\n}",
        )
        .unwrap();
        let moving = crate::isf::ISFShader::from_string(
            "/*{\"INPUTS\":[]}*/\nvoid main() { gl_FragColor = vec4(sin(TIME)); }",
        )
        .unwrap();
        let counting = crate::isf::ISFShader::from_string(
            "/*{\"INPUTS\":[]}*/\nvoid main() { gl_FragColor = vec4(float(FRAMEINDEX)); }",
        )
        .unwrap();
        assert!(!still.animates());
        assert!(moving.animates());
        assert!(counting.animates());
        // A phase uniform reached through a macro, pasted or not.
        let pasted = crate::isf::ISFShader::from_string(
            "/*{\"INPUTS\":[]}*/\n#define PHASE(i) PHASE_TIME_ ## i\nvoid main() { gl_FragColor = vec4(PHASE(0)); }",
        )
        .unwrap();
        assert!(pasted.animates(), "a phase uniform through a macro");
        let glued = crate::isf::ISFShader::from_string(
            "/*{\"INPUTS\":[]}*/\n#define CAT(a, b) a ## b\nvoid main() { gl_FragColor = vec4(CAT(TI, ME)); }",
        )
        .unwrap();
        assert!(glued.animates(), "a name pasted together is taken as moving");
        // A compute simulation keeping its storage from frame to frame.
        let simulation = crate::isf::ISFShader::from_string(
            "/*{\"TYPE\":\"compute\",\"COMPUTE\":{\"WORKGROUP_SIZE\":[16,16,1],\"DISPATCH\":\"resolution\"},\"BUFFERS\":[{\"NAME\":\"cells\",\"TYPE\":\"storage\",\"STRUCT\":\"Cell\",\"COUNT\":64,\"STRIDE\":16,\"PERSISTENT\":true}]}*/\nvoid main() {}",
        )
        .unwrap();
        assert!(simulation.animates(), "persistent storage moves");
    }

    /// A Slides input moves while a talk comes in, or while a talk of its
    /// own moves — a clip talk playing — and stands still on a picture.
    #[test]
    fn a_slides_input_moves_with_its_talk() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let picture = dir.path().join("break.png");
        image::RgbImage::from_pixel(64, 36, image::Rgb([0, 0, 255]))
            .save(&picture)
            .unwrap();
        let clip = dir.path().join("tone.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("color=c=red:s=64x36:r=25:d=6")
            .args(["-c:v", "mpeg4"])
            .arg(&clip)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        id(app.execute_command(EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![
                picture.display().to_string(),
                clip.display().to_string(),
            ],
            at: None,
        }));
        let slides_moving = |app: &VardaApp| {
            app.mixer_ref()
                .channels()
                .iter()
                .flat_map(|c| &c.decks)
                .find(|slot| slot.deck.slides().is_some())
                .is_some_and(|slot| slot.deck.moves_by_itself())
        };
        // Frames until `done`, or three seconds.
        let until = |app: &mut VardaApp, done: &dyn Fn(&VardaApp) -> bool| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < end && !done(app) {
                app.render_mixer_frame();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            done(app)
        };
        let start = |app: &mut VardaApp| {
            app.execute_command(EngineCommand::StartTalk {
                deck_uuid: None,
                to: crate::engine::value::slides::TalkRef::Next,
            });
        };
        start(&mut app);
        assert!(
            until(&mut app, &|app| !slides_moving(app)),
            "a picture talk on, its transition done: still"
        );
        assert_eq!(app.ui_motion(), UiMotion::Still);
        start(&mut app);
        assert!(
            until(&mut app, &|app| {
                let mut playing = false;
                for slot in app.mixer_ref().channels().iter().flat_map(|c| &c.decks) {
                    if let Some(slides) = slot.deck.slides() {
                        slides.for_each_deck(|d| {
                            playing |= d.playback_snapshot().is_some_and(|s| s.playing);
                        });
                    }
                }
                playing
            }),
            "the clip talk plays"
        );
        assert!(slides_moving(&app), "a clip talk playing moves");
        assert_eq!(app.ui_motion(), UiMotion::Pictures);
    }
}
