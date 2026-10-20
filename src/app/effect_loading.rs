//! Non-blocking interactive effect additions at all three tiers.

use super::VardaApp;
use crate::deck::Effect;
use crate::engine::types::EffectLoadState;
use crate::isf::ISFShader;

impl VardaApp {
    pub(super) fn load_effect(&mut self, shader: ISFShader) -> anyhow::Result<Effect> {
        if self.effect_loader.is_none() {
            self.effect_loader = Some(crate::deck::loading::EffectLoader::new(&self.context)?);
        }
        self.effect_loader
            .as_ref()
            .expect("loader initialized")
            .load(shader, self.context.compositing_format)
    }

    pub(crate) fn poll_effect_loading(&mut self) {
        if self.effect_loader.is_none() {
            return;
        }
        let mut failures = Vec::new();
        let mut released_sensors = Vec::new();
        let mut poll = |effect: &mut Effect| {
            let changed = effect.poll_load();
            if changed {
                if let EffectLoadState::Failed { message } = effect.load_state() {
                    failures.push(format!(
                        "Could not load effect '{}': {message}",
                        effect.shader.name()
                    ));
                }
            }
            changed
        };
        for channel in self.mixer.channels_mut() {
            for slot in &mut channel.decks {
                // Do not short-circuit: every pending reply gets a turn.
                let mut changed = false;
                for effect in &mut slot.deck.effects {
                    changed |= poll(effect);
                }
                if changed {
                    slot.deck
                        .ensure_preprocessor_analyzers(&self.analyzer_registry);
                    slot.deck.rebind_depth_preprocessor_slots();
                    if let Some(id) = slot.deck.detach_depth_preprocessor_if_unused() {
                        released_sensors.push(id);
                    }
                }
            }
            for effect in &mut channel.effects {
                poll(effect);
            }
        }
        for effect in self.mixer.master_effects_mut() {
            poll(effect);
        }
        for id in released_sensors {
            self.depth_manager.release(id);
        }
        for failure in failures {
            log::error!("{failure}");
            self.session.notifications.error(failure);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EffectTarget, MixerCommands, MixerQueries};
    use crate::params::ParamValue;
    use std::time::{Duration, Instant};

    fn channel_pixel(app: &VardaApp) -> [u16; 4] {
        let buffer = app.context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pending effect pixel"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = app
            .context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            app.mixer.channels()[0].composite_texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        app.context.submit([encoder.finish()]);
        let (send, reply) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                send.send(result).unwrap();
            });
        app.context
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(10)),
            })
            .unwrap();
        reply
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let bytes = buffer.slice(..).get_mapped_range().unwrap();
        std::array::from_fn(|i| u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]))
    }

    #[test]
    fn background_effects_keep_rendering_and_keep_edits_order_and_identity() {
        let gpu = match crate::renderer::GpuContext::new_headless() {
            Ok(gpu) => gpu,
            Err(error) => {
                assert!(
                    std::env::var_os("VARDA_REQUIRE_GPU").is_none(),
                    "headless GPU required: {error}"
                );
                return;
            }
        };
        let mut app = VardaApp::new(gpu.clone(), &crate::testing::headless_config()).unwrap();
        app.mixer.resize(&gpu, 64, 64);
        let channel_uuid = app.mixer.channels()[0].uuid().to_string();
        let deck_uuid = app
            .add_solid_color_deck(&channel_uuid, [0.0, 0.0, 1.0, 1.0])
            .unwrap();
        let (release, gate) = crossbeam_channel::bounded::<()>(8);
        app.effect_loader = Some(
            crate::deck::loading::EffectLoader::with_builder(move |shader, format| {
                gate.recv().unwrap();
                Effect::new_with_format(&gpu, shader, format)
            })
            .unwrap(),
        );
        let targets = [
            EffectTarget::Deck(deck_uuid.clone()),
            EffectTarget::Channel(channel_uuid.clone()),
            EffectTarget::Master,
        ];
        let mut ids = Vec::new();
        for target in targets {
            ids.push(app.add_effect(target, "blur").unwrap());
        }
        let second = app
            .add_effect(EffectTarget::Channel(channel_uuid), "invert")
            .unwrap();
        app.move_effect(
            EffectTarget::Channel(app.mixer.channels()[0].uuid().into()),
            0,
            1,
        )
        .unwrap();
        // A change made before completion belongs to the live shell.
        let location = app.resolve_effect(&ids[1]).unwrap();
        let effect = app.mixer.effect_at_mut(location).unwrap();
        let param = effect
            .params
            .param_order
            .iter()
            .find(|name| matches!(effect.params.values[*name], ParamValue::Float(_)))
            .unwrap()
            .clone();
        effect.params.set(&param, ParamValue::Float(2.0));
        effect.enabled = false;
        let saved = crate::scene::EffectConfig::of(effect);
        assert_eq!(saved.uuid, ids[1]);
        assert!(!saved.enabled);
        assert!(matches!(saved.params[&param], ParamValue::Float(v) if (v - 2.0).abs() < 1e-6));
        assert!(serde_json::to_value(&saved)
            .unwrap()
            .get("load_state")
            .is_none());
        let fault_count = app.context.errors.fault_count();
        for _ in 0..4 {
            app.poll_effect_loading();
            app.render_mixer_frame();
        }
        for id in &ids {
            let location = app.resolve_effect(id).unwrap();
            let (chain, index) = app.mixer.effect_chain_at_mut(location);
            assert_eq!(chain[index].load_state(), EffectLoadState::Building);
        }
        assert_eq!(
            app.context.errors.fault_count(),
            fault_count,
            "pending tiers must bypass without invalid ping-pong output"
        );
        assert_eq!(
            channel_pixel(&app),
            [0, 0, 0x3c00, 0x3c00],
            "the blue picture must survive pending effects"
        );
        // Undo restores a removed shell from its persisted settings. Its old
        // reply must neither replace that runtime nor duplicate the UUID.
        let before_remove = app.history_snapshot_default();
        app.push_history(before_remove);
        app.remove_effect(&second).unwrap();
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::Undo),
            crate::engine::CommandResult::Ok
        ));
        // Removal must not be undone by the arriving result.
        app.remove_effect(&ids[0]).unwrap();
        for _ in 0..4 {
            release.send(()).unwrap();
        }
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            app.poll_effect_loading();
            let snapshot = app.mixer_snapshot();
            let waiting = snapshot
                .channels
                .iter()
                .flat_map(|ch| ch.decks.iter().flat_map(|d| &d.effects).chain(&ch.effects))
                .chain(&snapshot.master_effects)
                .any(|e| e.load_state == EffectLoadState::Building);
            if !waiting {
                break;
            }
            assert!(Instant::now() < until, "effect builds did not complete");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            app.resolve_effect(&ids[0]).is_err(),
            "removed effect returned"
        );
        let effects = &app.mixer.channels()[0].effects;
        assert_eq!(effects[0].uuid(), second);
        assert_eq!(effects[1].uuid(), ids[1]);
        assert_eq!(effects[1].load_state(), EffectLoadState::Ready);
        assert!(!effects[1].enabled);
        assert!(
            matches!(effects[1].params.values[&param], ParamValue::Float(v) if (v - 2.0).abs() < 1e-6)
        );
        app.render_mixer_frame();
        assert_eq!(app.context.errors.fault_count(), fault_count);
    }
}
