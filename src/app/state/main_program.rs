//! Main, the vMix way: take a channel or input to Main with a cut or a fade,
//! switch overlay channels on and off over it. See [`crate::mixer::MainProgram`].

use crate::engine::{CommandResult, ErrorCode};
use crate::mixer::{MainSource, MixMode};

/// Longest fade offered, in seconds.
const MAX_FADE_SECS: f32 = 10.0;

fn not_found(what: &str, uuid: &str) -> CommandResult {
    CommandResult::Err {
        code: ErrorCode::NotFound,
        message: format!("No {what} {uuid}"),
    }
}

impl super::super::VardaApp {
    /// Make the master Main with overlays, or the layered channel mix.
    ///
    /// Switching to Main with nothing on it takes the channel the layered mix
    /// was showing on top — the topmost with a deck and its fader up — so the
    /// output does not go black at the switch.
    pub fn cmd_set_mix_mode(&mut self, mode: MixMode) -> CommandResult {
        if mode == MixMode::Program
            && self.mixer.mix_mode() == MixMode::Layered
            && self.mixer.main().program == MainSource::Black
        {
            let showing = (0..self.mixer.channel_count()).rev().find_map(|i| {
                let ch = self.mixer.channel(i)?;
                (!ch.overlay && !ch.decks.is_empty() && self.mixer.channel_opacity(i) > 0.0)
                    .then(|| ch.uuid().to_string())
            });
            if let Some(uuid) = showing {
                self.mixer.main_mut().take(MainSource::Channel(uuid), false);
            }
        }
        self.mixer.set_mix_mode(mode);
        CommandResult::Ok
    }

    /// Put a channel or deck on Main — or black — at once or with a fade.
    /// An overlay channel is switched on instead, as it is shown over Main
    /// rather than on it.
    pub fn cmd_take_to_main(&mut self, source: MainSource, fade: bool) -> CommandResult {
        if !self.mixer.main_source_exists(&source) {
            let kind = if matches!(source, MainSource::Deck(_)) {
                "deck"
            } else {
                "channel"
            };
            return not_found(kind, source.uuid().unwrap_or_default());
        }
        if let MainSource::Channel(uuid) = &source {
            let overlay = self
                .mixer
                .find_channel_by_uuid(uuid)
                .and_then(|i| self.mixer.channel(i))
                .is_some_and(|ch| ch.overlay);
            if overlay {
                self.mixer.main_mut().set_overlay(uuid, true, fade);
                return CommandResult::Ok;
            }
        }
        // Only a take that starts says what it will do: taking what is
        // already on its way changes nothing.
        let starting = fade && source != self.mixer.main().program;
        self.mixer.main_mut().take(source, fade);
        // What a merge will do, in the log; and when it must dissolve
        // instead, why, for the operator: a merge that faded where it
        // should have moved said nothing of it.
        match self.mixer.merge_plan().filter(|_| starting) {
            Some(Ok(plan)) => {
                log::info!("Merge {}", plan.text);
                // A layer both show that fades across all the same: the
                // operator saw it fade where it should have moved, unsaid.
                if !plan.crossfaded.is_empty() {
                    let named: Vec<String> = plan
                        .crossfaded
                        .iter()
                        .map(|(name, why)| format!("'{name}': {why}"))
                        .collect();
                    let text = format!("Merge fades across — {}", named.join("; "));
                    log::info!("{text}");
                    self.session.notifications.info(text);
                }
            }
            Some(Err(why)) => {
                let text = format!("Merge dissolves instead: {why}");
                log::warn!("{text}");
                self.session.notifications.info(text);
            }
            None => {}
        }
        CommandResult::Ok
    }

    /// Make a channel an overlay (switched on and off over Main) or a normal
    /// one (taken to Main). A channel that stops being an overlay leaves Main.
    pub fn cmd_set_channel_overlay(&mut self, channel_uuid: &str, overlay: bool) -> CommandResult {
        let Some(idx) = self.mixer.find_channel_by_uuid(channel_uuid) else {
            return not_found("channel", channel_uuid);
        };
        if let Some(ch) = self.mixer.channel_mut(idx) {
            ch.overlay = overlay;
        }
        if !overlay {
            self.mixer
                .main_mut()
                .set_overlay(channel_uuid, false, false);
        }
        CommandResult::Ok
    }

    /// Switch an overlay channel on or off over Main.
    pub fn cmd_set_overlay_on(
        &mut self,
        channel_uuid: &str,
        on: bool,
        fade: bool,
    ) -> CommandResult {
        let Some(idx) = self.mixer.find_channel_by_uuid(channel_uuid) else {
            return not_found("channel", channel_uuid);
        };
        if !self.mixer.channel(idx).is_some_and(|ch| ch.overlay) {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "That channel is not an overlay — take it to Main instead".into(),
            };
        }
        self.mixer.main_mut().set_overlay(channel_uuid, on, fade);
        CommandResult::Ok
    }

    /// Seconds a fade to Main takes, 0–10.
    pub fn cmd_set_main_fade_time(&mut self, secs: f32) -> CommandResult {
        if !secs.is_finite() {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "The fade time must be a number of seconds".into(),
            };
        }
        self.mixer.main_mut().fade_secs = secs.clamp(0.0, MAX_FADE_SECS);
        CommandResult::Ok
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::{CommandResult, EngineCommand};
    use crate::mixer::{MainSource, MainTransition};

    fn run(app: &mut crate::app::VardaApp, cmd: EngineCommand) -> CommandResult {
        app.execute_command(cmd)
    }

    /// A merge that cannot take an input apart dissolves, and says why: the
    /// operator saw a picture fade where it should have moved and nothing
    /// told them so. One that can merge says nothing.
    #[test]
    fn a_merge_that_must_dissolve_says_why() {
        let Ok(gpu) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        let Ok(mut app) = crate::app::VardaApp::new(gpu, &crate::testing::headless_config()) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let picture = dir.path().join("picture.png");
        image::RgbaImage::from_pixel(16, 9, image::Rgba([0, 200, 255, 255]))
            .save(&picture)
            .unwrap();
        run(&mut app, EngineCommand::SetMixMode { mode: crate::mixer::MixMode::Program });
        let state = app.build_engine_state();
        for deck in state.mixer.channels.iter().flat_map(|c| &c.decks) {
            run(&mut app, EngineCommand::RemoveDeck { deck_uuid: deck.uuid.clone() });
        }
        while app.build_engine_state().mixer.channels.len() < 2 {
            run(&mut app, EngineCommand::AddChannel);
        }
        let channels: Vec<String> = app
            .build_engine_state()
            .mixer
            .channels
            .iter()
            .map(|c| c.uuid.clone())
            .collect();
        let mut layers = Vec::new();
        for channel in &channels {
            match run(
                &mut app,
                EngineCommand::AddImageDeck {
                    channel_uuid: channel.clone(),
                    path: picture.clone(),
                },
            ) {
                CommandResult::OkWithId { uuid } => layers.push(uuid),
                other => panic!("no layer: {other:?}"),
            }
        }
        let take = |app: &mut crate::app::VardaApp| {
            run(
                app,
                EngineCommand::TakeToMain {
                    source: MainSource::Channel(channels[0].clone()),
                    fade: false,
                },
            );
            run(app, EngineCommand::SetMainTransition { transition: MainTransition::Merge });
            run(
                app,
                EngineCommand::TakeToMain {
                    source: MainSource::Channel(channels[1].clone()),
                    fade: true,
                },
            );
        };
        let said = |app: &crate::app::VardaApp| {
            app.session
                .notifications
                .visible()
                .iter()
                .find(|n| n.message.starts_with("Merge dissolves instead"))
                .map(|n| n.message.clone())
        };

        take(&mut app);
        assert_eq!(said(&app), None, "a merge it can do says nothing");

        run(
            &mut app,
            EngineCommand::SetDeckBlendMode {
                deck_uuid: layers[1].clone(),
                mode: crate::channel::BlendMode::Add,
            },
        );
        take(&mut app);
        let message = said(&app).expect("a dissolve says why");
        assert!(
            message.contains("is blended as Add") && message.contains("picture"),
            "{message}"
        );
        // Taking what is already on its way starts nothing, and says nothing
        // again.
        run(
            &mut app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channels[1].clone()),
                fade: true,
            },
        );
        let told = app
            .session
            .notifications
            .visible()
            .iter()
            .filter(|n| n.message.starts_with("Merge dissolves instead"))
            .count();
        assert_eq!(told, 1, "said once");
    }

    /// A merge that fades a layer both inputs show across — a picture
    /// cropped differently in the two, which does not travel — says so, and
    /// why: the operator saw a camera fade where it should have moved, and
    /// the log said it moved. Drawn alike, it says nothing.
    #[test]
    fn a_merge_that_fades_a_layer_across_says_so() {
        let Ok(gpu) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        let Ok(mut app) = crate::app::VardaApp::new(gpu, &crate::testing::headless_config()) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let picture = dir.path().join("picture.png");
        image::RgbaImage::from_pixel(16, 9, image::Rgba([0, 200, 255, 255]))
            .save(&picture)
            .unwrap();
        run(&mut app, EngineCommand::SetMixMode { mode: crate::mixer::MixMode::Program });
        let state = app.build_engine_state();
        for deck in state.mixer.channels.iter().flat_map(|c| &c.decks) {
            run(&mut app, EngineCommand::RemoveDeck { deck_uuid: deck.uuid.clone() });
        }
        while app.build_engine_state().mixer.channels.len() < 2 {
            run(&mut app, EngineCommand::AddChannel);
        }
        let channels: Vec<String> = app
            .build_engine_state()
            .mixer
            .channels
            .iter()
            .map(|c| c.uuid.clone())
            .collect();
        let mut layers = Vec::new();
        for channel in &channels {
            match run(
                &mut app,
                EngineCommand::AddImageDeck {
                    channel_uuid: channel.clone(),
                    path: picture.clone(),
                },
            ) {
                CommandResult::OkWithId { uuid } => layers.push(uuid),
                other => panic!("no layer: {other:?}"),
            }
        }
        let take = |app: &mut crate::app::VardaApp| {
            run(
                app,
                EngineCommand::TakeToMain {
                    source: MainSource::Channel(channels[0].clone()),
                    fade: false,
                },
            );
            run(app, EngineCommand::SetMainTransition { transition: MainTransition::Merge });
            run(
                app,
                EngineCommand::TakeToMain {
                    source: MainSource::Channel(channels[1].clone()),
                    fade: true,
                },
            );
        };
        let said = |app: &crate::app::VardaApp| {
            app.session
                .notifications
                .visible()
                .iter()
                .find(|n| n.message.starts_with("Merge fades"))
                .map(|n| n.message.clone())
        };

        take(&mut app);
        assert_eq!(said(&app), None, "drawn alike: it moves, and nothing is said");

        let crop = app
            .build_engine_state()
            .mixer
            .channels
            .iter()
            .flat_map(|c| &c.decks)
            .find(|d| d.uuid == layers[1])
            .and_then(|d| d.effects.iter().find(|e| e.name == "crop"))
            .expect("a layer has a crop")
            .uuid
            .clone();
        run(
            &mut app,
            EngineCommand::SetEffectParam {
                effect_uuid: crop,
                name: "crop_left".into(),
                value: crate::params::ParamValue::Float(0.5),
            },
        );
        take(&mut app);
        let message = said(&app).expect("a layer faded across is said");
        assert!(message.contains("'picture.png': not a live input"), "{message}");
    }

    /// A tap not keeping its input's transparency moves all the same — the
    /// operator's duplicated layout lost the flag and every merge faded —
    /// and one mirrored in one input only fades, the notice saying so.
    #[test]
    fn a_merge_moves_a_flattened_tap_and_names_why_a_mirrored_one_fades() {
        for mirrored in [false, true] {
            let Some((mut app, channels)) = taps_of_a_picture() else {
                return;
            };
            let state = app.build_engine_state();
            let tap = &state.mixer.channels[2].decks[0];
            let transform = tap
                .effects
                .iter()
                .find(|e| e.name == "transform")
                .expect("a tap has a transform")
                .uuid
                .clone();
            run(
                &mut app,
                EngineCommand::SetDeckTransparent {
                    deck_uuid: tap.uuid.clone(),
                    transparent: false,
                },
            );
            if mirrored {
                run(
                    &mut app,
                    EngineCommand::SetEffectParam {
                        effect_uuid: transform,
                        name: "flip_x".into(),
                        value: crate::params::ParamValue::Bool(true),
                    },
                );
            }
            run(
                &mut app,
                EngineCommand::TakeToMain {
                    source: MainSource::Channel(channels[2].clone()),
                    fade: true,
                },
            );
            let said = app
                .session
                .notifications
                .visible()
                .iter()
                .find(|n| n.message.starts_with("Merge fades"))
                .map(|n| n.message.clone());
            if mirrored {
                let said = said.expect("a mirrored tap fades, and it is said");
                assert!(said.contains(": mirrored in one input only"), "{said}");
            } else {
                assert_eq!(said, None, "the flattened tap moves");
            }
        }
    }

    /// Three channels: a picture, a tap of it in the second, and a tap of it
    /// cropped to the right half in the third — added last, with no frame
    /// drawn since. The app on Main, Merge chosen, and the channels' UUIDs.
    fn taps_of_a_picture() -> Option<(crate::app::VardaApp, Vec<String>)> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        let mut app = crate::app::VardaApp::new(gpu, &crate::testing::headless_config()).ok()?;
        let dir = tempfile::tempdir().unwrap();
        let picture = dir.path().join("picture.png");
        image::RgbaImage::from_pixel(16, 9, image::Rgba([0, 200, 255, 255]))
            .save(&picture)
            .unwrap();
        run(&mut app, EngineCommand::SetMixMode { mode: crate::mixer::MixMode::Program });
        let state = app.build_engine_state();
        for deck in state.mixer.channels.iter().flat_map(|c| &c.decks) {
            run(&mut app, EngineCommand::RemoveDeck { deck_uuid: deck.uuid.clone() });
        }
        while app.build_engine_state().mixer.channels.len() < 3 {
            run(&mut app, EngineCommand::AddChannel);
        }
        let channels: Vec<String> = app
            .build_engine_state()
            .mixer
            .channels
            .iter()
            .map(|c| c.uuid.clone())
            .collect();
        run(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channels[0].clone(),
                path: picture,
            },
        );
        let tap = |app: &mut crate::app::VardaApp, channel: &str| match run(
            app,
            EngineCommand::AddTapDeck {
                channel_uuid: channel.to_string(),
                source: crate::scene::TapSourceConfig::Channel {
                    uuid: channels[0].clone(),
                },
            },
        ) {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("no tap: {other:?}"),
        };
        tap(&mut app, &channels[1]);
        run(
            &mut app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channels[1].clone()),
                fade: false,
            },
        );
        run(&mut app, EngineCommand::SetMainTransition { transition: MainTransition::Merge });
        run(&mut app, EngineCommand::SetMainFadeTime { secs: 10.0 });
        app.render_mixer_frame();
        let cropped = tap(&mut app, &channels[2]);
        let crop = app
            .build_engine_state()
            .mixer
            .channels
            .iter()
            .flat_map(|c| &c.decks)
            .find(|d| d.uuid == cropped)
            .and_then(|d| d.effects.iter().find(|e| e.name == "crop"))
            .expect("a tap has a crop")
            .uuid
            .clone();
        run(
            &mut app,
            EngineCommand::SetEffectParam {
                effect_uuid: crop,
                name: "crop_left".into(),
                value: crate::params::ParamValue::Float(0.5),
            },
        );
        Some((app, channels))
    }

    /// A take onto a tap just added, before a frame has bound its picture,
    /// says nothing of a cross-fade: the tap moves with its framing all the
    /// same once drawn.
    #[test]
    fn a_merge_onto_a_tap_just_added_says_nothing_of_a_crossfade() {
        let Some((mut app, channels)) = taps_of_a_picture() else {
            return;
        };
        run(
            &mut app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channels[2].clone()),
                fade: true,
            },
        );
        let said = app
            .session
            .notifications
            .visible()
            .iter()
            .find(|n| n.message.starts_with("Merge fades"))
            .map(|n| n.message.clone());
        assert_eq!(said, None, "the tap moves");
    }

    /// The frames a merge draws its travelling pictures into go when the
    /// merge ends, at a resize, when the master is switched to the layered
    /// mix mid-merge, and while the merge dissolves instead: left, a dozen
    /// pairs at 4K kept some 700 MB after their merge.
    #[test]
    fn a_merge_keeps_no_frames_once_it_ends() {
        let Some((mut app, channels)) = taps_of_a_picture() else {
            return;
        };
        app.render_mixer_frame();
        run(
            &mut app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channels[2].clone()),
                fade: true,
            },
        );
        let hold = |app: &mut crate::app::VardaApp| {
            let fade = app.mixer_mut().main_mut().fade.as_mut().expect("taking");
            fade.progress = 0.5;
            fade.duration = 1.0e9;
            app.render_mixer_frame();
        };
        hold(&mut app);
        assert_eq!(app.mixer_ref().merge_frames_kept(), 1, "one pair travels");
        let context = app.gpu_context().clone();
        app.mixer_mut().resize(&context, 1280, 720);
        assert_eq!(app.mixer_ref().merge_frames_kept(), 0, "dropped at a resize");
        hold(&mut app);
        assert_eq!(app.mixer_ref().merge_frames_kept(), 1, "made again at the new size");
        run(&mut app, EngineCommand::SetMixMode { mode: crate::mixer::MixMode::Layered });
        hold(&mut app);
        assert_eq!(app.mixer_ref().merge_frames_kept(), 0, "nothing draws the merge in the layered mix");
        run(&mut app, EngineCommand::SetMixMode { mode: crate::mixer::MixMode::Program });
        hold(&mut app);
        assert_eq!(app.mixer_ref().merge_frames_kept(), 1, "drawn again on Main");
        // A layer blended as Add: the merge dissolves now, nothing travels.
        let cropped = app
            .build_engine_state()
            .mixer
            .channels
            .iter()
            .find(|c| c.uuid == channels[2])
            .map(|c| c.decks[0].uuid.clone())
            .expect("the cropped tap");
        let blend = |app: &mut crate::app::VardaApp, mode| {
            run(
                app,
                EngineCommand::SetDeckBlendMode {
                    deck_uuid: cropped.clone(),
                    mode,
                },
            );
        };
        blend(&mut app, crate::channel::BlendMode::Add);
        hold(&mut app);
        assert_eq!(app.mixer_ref().merge_frames_kept(), 0, "none kept for a dissolve");
        blend(&mut app, crate::channel::BlendMode::Normal);
        hold(&mut app);
        assert_eq!(app.mixer_ref().merge_frames_kept(), 1, "travelling again");
        run(
            &mut app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channels[2].clone()),
                fade: false,
            },
        );
        app.render_mixer_frame();
        assert!(app.mixer_ref().main().fade.is_none(), "the take is done");
        assert_eq!(app.mixer_ref().merge_frames_kept(), 0, "none kept after it");
    }
}
