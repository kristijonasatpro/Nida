//! Native MIDI discovery must not hold the event/render thread. Saved bindings
//! remain named until the worker's manager is adopted, before messages drain.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::VardaApp;
use crate::midi::{MidiConfig, MidiDeviceManager};

pub(super) type Discovery = Receiver<anyhow::Result<MidiDeviceManager>>;

pub(super) fn discover() -> Option<Discovery> {
    // Unit tests inject detached devices; never open the developer's ports.
    if cfg!(test) {
        return None;
    }
    spawn_discovery(MidiDeviceManager::new)
}

fn spawn_discovery(
    scan: impl FnOnce() -> anyhow::Result<MidiDeviceManager> + Send + 'static,
) -> Option<Discovery> {
    let (send, receive) = mpsc::channel();
    match std::thread::Builder::new()
        .name("midi-discovery".into())
        .spawn(move || {
            let result = scan();
            // Dropping the receiver on quit does not wait for a stuck driver.
            // A late result and its connections are then dropped here.
            let _ = send.send(result);
        }) {
        Ok(_) => Some(receive),
        Err(error) => {
            log::warn!("Could not start MIDI discovery: {error}");
            None
        }
    }
}

impl VardaApp {
    pub(super) fn poll_midi_discovery(&mut self) {
        let Some(pending) = &self.input.midi_pending else {
            return;
        };
        let result = match pending.try_recv() {
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("MIDI discovery stopped")),
            Ok(result) => result,
        };
        self.input.midi_pending = None;
        match result {
            Ok(mut manager) => {
                manager.load_user_profiles(&self.session.workspace.controller_profiles_dir());
                self.input.controller_led_mgr.sync_devices(&manager);
                self.input.auto_map_engine.sync_devices(&manager);
                if let Some(config) = &self.input.saved_midi {
                    self.input
                        .midi_mappings
                        .load_from_config(config, &manager.devices);
                }
                log::info!("[STARTUP] MIDI ready: {} device(s)", manager.devices.len());
                self.input.midi_devices = Some(manager);
                if let Some(device) = self.input.pending_mtc.take() {
                    // Keep LTC's current patch: it may have been edited while
                    // discovery was pending. Only resolve the deferred port.
                    let mut config = self.timecode_config();
                    config.preference = crate::timecode::PreferenceConfig::ForceMtc { device };
                    self.apply_timecode_config(&config);
                }
            }
            Err(error) => {
                log::warn!("Failed to initialize MIDI: {error}");
                self.session
                    .notifications
                    .warn(format!("MIDI unavailable: {error}"));
            }
        }
    }

    pub(super) fn midi_config_for_save(&self) -> Option<MidiConfig> {
        let Some(manager) = &self.input.midi_devices else {
            return self.input.saved_midi.clone();
        };
        let mut config = self
            .input
            .midi_mappings
            .to_config(&manager.devices, &self.input.auto_map_engine);
        // Retain bindings for absent controllers. Connected controllers use
        // live mappings, so deleting a binding does not resurrect it on save.
        if let Some(saved) = &self.input.saved_midi {
            config.mappings.extend(
                saved
                    .mappings
                    .iter()
                    .filter(|entry| {
                        !manager
                            .devices
                            .values()
                            .any(|device| device.name == entry.device_name)
                    })
                    .cloned(),
            );
        }
        Some(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineCommand;
    use crate::midi::{MidiKey, MidiMessage};
    use crate::timecode::{PreferenceConfig, TimecodeConfig, TimecodePreference};

    fn app() -> Option<VardaApp> {
        let gpu = crate::renderer::GpuContext::new_headless().ok()?;
        VardaApp::new(gpu, &crate::testing::headless_config()).ok()
    }

    fn config(name: &str) -> MidiConfig {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "mappings": [{"device_name": name, "msg_type": "cc",
                "channel": 0, "number": 7, "param_path": "master/opacity"}]
        }))
        .unwrap()
    }

    fn device() -> MidiDeviceManager {
        let mut manager = MidiDeviceManager::detached();
        manager.inject(MidiMessage::ControlChange {
            device_id: 42,
            channel: 0,
            cc: 7,
            value: 100,
        });
        manager
    }

    #[test]
    fn pending_discovery_preserves_save_and_adopts_named_bindings_and_mtc() {
        let Some(mut app) = app() else { return };
        let (send, receive) = mpsc::channel();
        app.input.midi_pending = Some(receive);
        app.input.saved_midi = Some(config("Test device 42"));
        let patch = TimecodeConfig {
            preference: PreferenceConfig::ForceMtc {
                device: "Test device 42".into(),
            },
            ..TimecodeConfig::default()
        };
        app.apply_timecode_config(&patch);
        app.poll_midi_discovery();
        assert!(app.input.midi_pending.is_some());
        assert!(app.input.midi_devices.is_none());
        assert_eq!(app.timecode_config().preference, patch.preference);
        app.save_workspace(&crate::usecases::ui::UILayoutState::default())
            .unwrap();
        app.flush_saves();
        let path = app.session.workspace.midi_path();
        assert_eq!(
            MidiConfig::load(&path).unwrap().mappings[0].device_name,
            "Test device 42"
        );
        assert!(send.send(Ok(device())).is_ok());
        app.poll_midi_discovery();
        assert!(app.input.midi_pending.is_none());
        assert_eq!(
            app.input
                .midi_mappings
                .get(&MidiKey::CC(42, 0, 7))
                .map(String::as_str),
            Some("master/opacity")
        );
        assert_eq!(
            app.input.timecode.preference(),
            TimecodePreference::ForceMtc { device_id: 42 }
        );
        // Deletion for a connected device must not resurrect its saved entry.
        app.input.midi_mappings.remove(&MidiKey::CC(42, 0, 7));
        assert!(app.midi_config_for_save().unwrap().mappings.is_empty());
    }

    #[test]
    fn failed_or_disconnected_discovery_keeps_saved_config() {
        let Some(mut app) = app() else { return };
        app.input.saved_midi = Some(config("Absent controller"));
        for result in [Some(Err(anyhow::anyhow!("driver failed"))), None] {
            let (send, receive) = mpsc::channel();
            app.input.midi_pending = Some(receive);
            if let Some(result) = result {
                assert!(send.send(result).is_ok());
            }
            drop(send);
            app.poll_midi_discovery();
            assert!(app.input.midi_pending.is_none());
            assert_eq!(app.midi_config_for_save().unwrap().mappings.len(), 1);
        }
        app.input.midi_devices = Some(device());
        assert_eq!(
            app.midi_config_for_save().unwrap().mappings[0].device_name,
            "Absent controller"
        );
    }

    #[test]
    fn explicit_changes_while_pending_are_not_overwritten() {
        let Some(mut app) = app() else { return };
        let (send, receive) = mpsc::channel();
        app.input.midi_pending = Some(receive);
        app.input.saved_midi = Some(config("Test device 42"));
        app.apply_timecode_config(&TimecodeConfig {
            preference: PreferenceConfig::ForceMtc {
                device: "Test device 42".into(),
            },
            ..TimecodeConfig::default()
        });
        app.execute_command(EngineCommand::SetTimecodePreference {
            preference: TimecodePreference::Off,
        });
        app.execute_command(EngineCommand::ClearMidiMappings);
        assert!(app.midi_config_for_save().unwrap().mappings.is_empty());
        assert!(send.send(Ok(device())).is_ok());
        app.poll_midi_discovery();
        assert_eq!(app.input.timecode.preference(), TimecodePreference::Off);
        assert!(app.input.midi_mappings.sorted_mappings().is_empty());
    }

    #[test]
    fn leaving_while_discovery_is_pending_does_not_wait_for_worker() {
        let (entered, started) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let receive = spawn_discovery(move || {
            entered.send(()).unwrap();
            wait.recv().unwrap();
            finished.send(()).unwrap();
            Ok(MidiDeviceManager::detached())
        })
        .expect("worker starts");
        started
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(matches!(receive.try_recv(), Err(TryRecvError::Empty)));
        drop(receive);
        release.send(()).unwrap();
        done.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
    }
}
