//! Persistence and startup policy, without a device, GPU, or audio engine.
use super::{config_to_target, target_to_config};
use crate::engine::value::render::OutputTarget;
use crate::scene::OutputConfig;

fn legacy_output(device: &str) -> serde_json::Value {
    serde_json::json!({
        "uuid": "out00001",
        "name": "Room SDI",
        "target": {
            "type": "deck_link", "device": device,
            "mode": "1080p59.94", "full_range": true
        },
        "audio_bus": "master01",
        "running": true,
        "rotation": "Deg0",
        "source": {"BusProgram": "bus00001"},
        "surface_assignments": [{"surface_uuid": "surf0001", "enabled": true}]
    })
}

#[test]
fn legacy_outputs_with_saved_buses_restore_video_only_on_both_platforms() {
    for device in ["UltraStudio Monitor 3G", "DeckLink 4K Extreme 12G"] {
        for bus in [Some("master01"), Some("removed1"), None] {
            let mut json = legacy_output(device);
            json["audio_bus"] = serde_json::json!(bus);
            let saved: OutputConfig = serde_json::from_value(json).unwrap();
            let target = config_to_target(&saved.target);
            assert_eq!(target.mixer_audio_bus(saved.audio_bus.as_deref()), None);
            assert!(matches!(
                target,
                OutputTarget::DeckLink {
                    embedded_audio: false,
                    ..
                }
            ));
            assert!(saved.running, "auto-start does not imply audio consent");
            let resaved = serde_json::to_value(&saved).unwrap();
            assert!(resaved["target"].get("embedded_audio").is_none());
            assert_eq!(resaved["audio_bus"].as_str(), bus);
        }
    }
}

#[test]
fn embedded_audio_opt_in_survives_both_target_conversions_and_output_roundtrip() {
    for enabled in [false, true] {
        let mut json = legacy_output("UltraStudio Monitor 3G");
        json["target"]["embedded_audio"] = serde_json::json!(enabled);
        let saved: OutputConfig = serde_json::from_value(json).unwrap();
        let live = config_to_target(&saved.target);
        assert_eq!(
            live.mixer_audio_bus(saved.audio_bus.as_deref()),
            enabled.then_some("master01")
        );
        let api_roundtrip: OutputTarget =
            serde_json::from_str(&serde_json::to_string(&live).unwrap()).unwrap();
        assert_eq!(api_roundtrip, live);
        let restored = OutputConfig {
            target: target_to_config(&live),
            ..saved
        };
        let json = serde_json::to_value(&restored).unwrap();
        assert_eq!(
            json["target"]["embedded_audio"].as_bool().unwrap_or(false),
            enabled
        );
        assert_eq!(json["target"]["full_range"], true);
        assert_eq!(json["audio_bus"], "master01");
        assert_eq!(json["running"], true);
        assert_eq!(json["source"]["BusProgram"], "bus00001");
        let again: OutputConfig = serde_json::from_value(json).unwrap();
        let again = config_to_target(&again.target);
        assert_eq!(again, live);
        assert_eq!(
            again.mixer_audio_bus(None),
            None,
            "no default bus is invented"
        );
    }
}

#[test]
fn legacy_api_targets_are_video_only_and_other_output_audio_is_unchanged() {
    let legacy: OutputTarget = serde_json::from_str(
        r#"{"DeckLink":{"device":"UltraStudio Monitor 3G","mode":"1080p60"}}"#,
    )
    .unwrap();
    assert_eq!(legacy.mixer_audio_bus(Some("master01")), None);
    for json in [
        serde_json::json!({"type":"ndi_send", "sender_name":"Room"}),
        serde_json::json!({"type":"recording", "path":"room.mp4", "codec":"H264"}),
    ] {
        let mut output = legacy_output("unused");
        output["target"] = json;
        let output: OutputConfig = serde_json::from_value(output).unwrap();
        let target = config_to_target(&output.target);
        assert_eq!(
            target.mixer_audio_bus(output.audio_bus.as_deref()),
            Some("master01")
        );
    }
}
