//! Audio mixer routes: buses, inputs and source strips.
//!
//! Every write names what it changes by UUID in the path. A clip's strip is
//! addressed by its deck's UUID. The mixer's state, meters included, is in
//! `GET /api/state` under `audio_mix`.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::engine::EngineCommand;
use crate::usecases::api::{command_response, SharedState};

async fn dispatch(state: &SharedState, cmd: EngineCommand) -> axum::response::Response {
    match state.send_command(cmd).await {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

/// A name.
#[derive(Deserialize, ToSchema)]
pub struct AudioNameBody {
    pub name: String,
}

/// A fader level.
#[derive(Deserialize, ToSchema)]
pub struct AudioGainBody {
    /// dB; -96 is off, at most +12.
    pub gain_db: f32,
}

/// A mute switch.
#[derive(Deserialize, ToSchema)]
pub struct AudioMuteBody {
    pub muted: bool,
}

/// A bus's or a source's delay.
#[derive(Deserialize, ToSchema)]
pub struct AudioDelayBody {
    /// Milliseconds, 0–1000.
    pub delay_ms: f32,
}

/// The sound device a bus plays on.
#[derive(Deserialize, ToSchema)]
pub struct AudioDeviceBody {
    /// Device name as listed in `audio_mix.output_devices`; `null` for none.
    pub device: Option<String>,
    /// First output of the pair (0 = outputs 1-2). A pair past the
    /// device's outputs (`audio_mix.output_channels`) plays on its last pair,
    /// said in the bus's `device_note`.
    #[serde(default)]
    pub first_channel: u16,
}

/// A new input source.
#[derive(Deserialize, ToSchema)]
pub struct AudioInputBody {
    pub name: String,
    /// Device name as listed in `audio_mix.input_devices`.
    pub device: String,
    /// Device channel for the left side (0-based).
    pub left: u16,
    /// Device channel for the right side; the same as `left` for mono.
    pub right: u16,
}

/// An app whose sound to take.
#[derive(Deserialize, ToSchema)]
pub struct AppSoundBody {
    /// The app's bundle ID, as listed in `audio_mix.apps` ("us.zoom.xos").
    pub app: String,
    /// The strip's name; the app's when empty.
    #[serde(default)]
    pub name: String,
    /// The input whose sound it is, by deck UUID (PowerPoint's, of a
    /// PowerPoint input): heard with its picture on air, removed with it.
    #[serde(default)]
    pub deck_uuid: Option<String>,
}

/// A send switch.
#[derive(Deserialize, ToSchema)]
pub struct AudioSendBody {
    /// On or off; left out, as it was.
    #[serde(default)]
    pub on: Option<bool>,
    /// Its level, −96 (silent) to +10 dB; left out, as it was. Setting it
    /// does not turn the send on.
    #[serde(default)]
    pub gain_db: Option<f32>,
    /// Where it takes the source: "inherit" (as its bus), "pre" or "post"
    /// the fader; left out, as it was.
    #[serde(default)]
    pub mode: Option<crate::audio_mix::SendMode>,
}

/// A bus as a monitor mix, or not.
#[derive(Deserialize, ToSchema)]
pub struct AudioPreFaderBody {
    pub pre_fader: bool,
}

/// Where a source's Audio Auto looks for it being on air.
#[derive(Deserialize, ToSchema)]
pub struct AudioFollowBody {
    /// A video bus UUID, `"main"` for Main, or `null` for Main and every bus.
    pub bus_uuid: Option<String>,
}

/// Audio Auto on or off.
#[derive(Deserialize, ToSchema)]
pub struct AudioAutoBody {
    pub auto: bool,
}

/// Left–right balance.
#[derive(Deserialize, ToSchema)]
pub struct AudioBalanceBody {
    /// -1 (left only) to 1 (right only); 0 is centre.
    pub balance: f32,
}

/// The audio bus an output carries.
#[derive(Deserialize, ToSchema)]
pub struct OutputAudioBusBody {
    /// Audio bus UUID; `null` for none.
    pub bus_uuid: Option<String>,
}

#[utoipa::path(post, path = "/api/audio-buses", request_body = AudioNameBody, responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn create_bus(
    State(s): State<SharedState>,
    Json(b): Json<AudioNameBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::AddAudioBus { name: b.name }).await
}

#[utoipa::path(delete, path = "/api/audio-buses/{bus_uuid}", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn delete_bus(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RemoveAudioBus { uuid }).await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/name", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioNameBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn rename_bus(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioNameBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RenameAudioBus { uuid, name: b.name }).await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/gain", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioGainBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_gain(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioGainBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioBusGain {
            uuid,
            gain_db: b.gain_db,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/mute", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioMuteBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_mute(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioMuteBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioBusMute {
            uuid,
            muted: b.muted,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/delay", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioDelayBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_delay(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioDelayBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioBusDelay {
            uuid,
            delay_ms: b.delay_ms,
        },
    )
    .await
}

/// A bus played live or not.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct AudioLiveBody {
    /// Live: the mics on the bus's own sound device mixed inside that
    /// device, about 7 ms from mic to speakers; the rest of the bus at the
    /// usual delay. Its state shows as the bus's `live` (true while it plays
    /// live) and `device_note` (why not).
    pub live: bool,
    /// The device's buffer, frames a call: 32, 64, 128 or 256 (48 kHz: 0.7,
    /// 1.3, 2.7, 5.3 ms). Left as it was when not given; 64 at first.
    #[serde(default)]
    pub frames: Option<u32>,
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/live", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioLiveBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_live(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioLiveBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioBusLive {
            uuid,
            live: b.live,
            frames: b.frames,
        },
    )
    .await
}

/// A bus's Audio Unit insert.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct AudioInsertBody {
    /// The effect's codes as listed in `audio_mix.effects`
    /// ("aufx:lpas:appl"), or null to take it out. First in the bus's
    /// processing; the bus plays 20 ms later with one; not on a live bus.
    pub component: Option<String>,
}

/// A bus's insert bypassed, or not.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct AudioBypassBody {
    pub bypass: bool,
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/insert", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioInsertBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_insert(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioInsertBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioBusInsert {
            uuid,
            component: b.component,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/insert/bypass", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioBypassBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_insert_bypass(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioBypassBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioBusInsertBypass {
            uuid,
            bypass: b.bypass,
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/audio-buses/{bus_uuid}/insert/window", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus"), (status = 503, description = "No effect on it, or not loaded")), tag = "Audio Mixer")]
pub async fn open_bus_insert(State(s): State<SharedState>, Path(uuid): Path<String>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::OpenAudioBusInsert { uuid }).await
}

#[utoipa::path(delete, path = "/api/audio-buses/{bus_uuid}/insert/window", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn close_bus_insert(State(s): State<SharedState>, Path(uuid): Path<String>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::CloseAudioBusInsert { uuid }).await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/device", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioDeviceBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_device(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioDeviceBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioBusDevice {
            uuid,
            device: b.device,
            first_channel: b.first_channel,
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/audio-inputs", request_body = AudioInputBody, responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn create_input(
    State(s): State<SharedState>,
    Json(b): Json<AudioInputBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::AddAudioInput {
            name: b.name,
            device: b.device,
            left: b.left,
            right: b.right,
        },
    )
    .await
}

/// Take an app's sound — a Zoom or Teams call's — as a source, straight from
/// the app while it runs (macOS 14.2 and later); replies with its UUID.
#[utoipa::path(post, path = "/api/audio-inputs/app", request_body = AppSoundBody, responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn create_app_sound(
    State(s): State<SharedState>,
    Json(b): Json<AppSoundBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::AddAppSound {
            app: b.app,
            name: b.name,
            deck_uuid: b.deck_uuid,
        },
    )
    .await
}

#[utoipa::path(delete, path = "/api/audio-inputs/{input_uuid}", params(("input_uuid" = String, Path, description = "Audio input UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such input")), tag = "Audio Mixer")]
pub async fn delete_input(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RemoveAudioInput { uuid }).await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/gain", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioGainBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_gain(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioGainBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioSourceGain {
            uuid,
            gain_db: b.gain_db,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/mute", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioMuteBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_mute(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioMuteBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioSourceMute {
            uuid,
            muted: b.muted,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/sends/{bus_uuid}", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)"), ("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioSendBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source or bus")), tag = "Audio Mixer")]
pub async fn set_send(
    State(s): State<SharedState>,
    Path((uuid, bus_uuid)): Path<(String, String)>,
    Json(b): Json<AudioSendBody>,
) -> impl IntoResponse {
    let command = match (b.on, b.gain_db, b.mode) {
        // On or off alone: as it always was.
        (Some(on), None, None) => EngineCommand::SetAudioSend { uuid, bus_uuid, on },
        (on, gain_db, mode) => EngineCommand::SetAudioSendSettings {
            uuid,
            bus_uuid,
            on,
            gain_db,
            mode,
        },
    };
    dispatch(&s, command).await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/pre-fader", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioPreFaderBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_pre_fader(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioPreFaderBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioBusPreFader { uuid, pre_fader: b.pre_fader }).await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/follow", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioFollowBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source or video bus")), tag = "Audio Mixer")]
pub async fn set_follow(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioFollowBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioFollow {
            uuid,
            bus_uuid: b.bus_uuid,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/auto", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioAutoBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source")), tag = "Audio Mixer")]
/// Audio Auto: the source's sound switches on as it goes on air and off as
/// it leaves, fading with the picture.
pub async fn set_auto(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioAutoBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioAuto { uuid, auto: b.auto }).await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/balance", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioBalanceBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_balance(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioBalanceBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioBalance {
            uuid,
            balance: b.balance,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/delay", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioDelayBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_delay(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioDelayBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioSourceDelay {
            uuid,
            delay_ms: b.delay_ms,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/outputs/{output_uuid}/audio-bus", params(("output_uuid" = String, Path, description = "Output UUID")), request_body = OutputAudioBusBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such output or bus")), tag = "Outputs")]
pub async fn set_output_audio_bus(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<OutputAudioBusBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetOutputAudioBus {
            output_uuid,
            bus_uuid: b.bus_uuid,
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/audio-devices/rescan", responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn rescan_devices(State(s): State<SharedState>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RescanAudioDevices).await
}

/// A solo switch.
#[derive(Deserialize, ToSchema)]
pub struct AudioSoloBody {
    pub solo: bool,
}

/// What the monitor listens to.
#[derive(Deserialize, ToSchema)]
pub struct AudioListenBody {
    /// Audio bus UUID; `null` for the first bus.
    pub bus_uuid: Option<String>,
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/solo", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioSoloBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_solo(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioSoloBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioSolo { uuid, solo: b.solo }).await
}

#[utoipa::path(put, path = "/api/audio-monitor/device", request_body = AudioDeviceBody, responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn set_monitor_device(
    State(s): State<SharedState>,
    Json(b): Json<AudioDeviceBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioMonitorDevice {
            device: b.device,
            first_channel: b.first_channel,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-monitor/gain", request_body = AudioGainBody, responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn set_monitor_gain(
    State(s): State<SharedState>,
    Json(b): Json<AudioGainBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioMonitorGain { gain_db: b.gain_db },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-monitor/listen", request_body = AudioListenBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_monitor_listen(
    State(s): State<SharedState>,
    Json(b): Json<AudioListenBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioMonitorListen {
            bus_uuid: b.bus_uuid,
        },
    )
    .await
}

// ── Processing ─────────────────────────────────────────────────────────
//
// Each takes the whole setting: fields left out take their defaults (`enabled`
// false among them), so send every field you mean to keep.

const SOURCE: &str = "Source UUID (the deck's, for a clip)";

/// A source's de-esser: an "s" over its threshold turns the top down.
#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/de-esser", params(("source_uuid" = String, Path, description = SOURCE)), request_body = crate::audio_mix::DeEsserConfig, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Out of 3000–10 000 Hz, threshold −60–0 dBFS or range 0–20 dB"), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_de_esser(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(config): Json<crate::audio_mix::DeEsserConfig>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioSourceDeEsser { uuid, config }).await
}

/// A source's EQ: a low shelf, two bells and a high shelf.
#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/eq", params(("source_uuid" = String, Path, description = SOURCE)), request_body = crate::audio_mix::EqConfig, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "A band out of 20–20 000 Hz, −18–18 dB or Q 0.3–10"), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_eq(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(config): Json<crate::audio_mix::EqConfig>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioSourceEq { uuid, config }).await
}

/// A source's gate: the room turned down in a pause.
#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/gate", params(("source_uuid" = String, Path, description = SOURCE)), request_body = crate::audio_mix::GateConfig, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Threshold out of −80–−10 dBFS, range 0–30 dB, attack 0.1–50 ms, hold 0–500 ms or release 10–2000 ms"), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_gate(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(config): Json<crate::audio_mix::GateConfig>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioSourceGate { uuid, config }).await
}

/// A source's low cut: rumble and hum under a voice.
#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/low-cut", params(("source_uuid" = String, Path, description = SOURCE)), request_body = crate::audio_mix::LowCutConfig, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Frequency out of 20–400 Hz, or a slope other than 12 or 24"), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_low_cut(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(config): Json<crate::audio_mix::LowCutConfig>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioSourceLowCut { uuid, config }).await
}

/// A source's leveler: each speaker brought slowly to the same loudness.
#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/leveler", params(("source_uuid" = String, Path, description = SOURCE)), request_body = crate::audio_mix::LevelerConfig, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "A value out of its range"), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_leveler(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(config): Json<crate::audio_mix::LevelerConfig>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioSourceLeveler { uuid, config }).await
}

/// A source's compressor, after its leveler.
#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/compressor", params(("source_uuid" = String, Path, description = SOURCE)), request_body = crate::audio_mix::CompressorConfig, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "A value out of its range"), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_source_compressor(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(config): Json<crate::audio_mix::CompressorConfig>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioSourceCompressor { uuid, config }).await
}

/// The Speech preset on a source: low cut at 100 Hz, the leveler to −20 dB,
/// gentle compression — what a talk's microphone needs, in one call.
#[utoipa::path(post, path = "/api/audio-sources/{source_uuid}/speech-preset", params(("source_uuid" = String, Path, description = SOURCE)), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn apply_speech_preset(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> axum::response::Response {
    use crate::audio_mix::{CompressorConfig, LevelerConfig, LowCutConfig};
    let commands = [
        EngineCommand::SetAudioSourceLowCut {
            uuid: uuid.clone(),
            config: LowCutConfig::speech(),
        },
        EngineCommand::SetAudioSourceLeveler {
            uuid: uuid.clone(),
            config: LevelerConfig::speech(),
        },
        EngineCommand::SetAudioSourceCompressor {
            uuid,
            config: CompressorConfig::speech(),
        },
    ];
    let mut last = crate::engine::CommandResult::Ok;
    for command in commands {
        match s.send_command(command).await {
            Ok(result @ crate::engine::CommandResult::Err { .. }) => {
                return command_response(result)
            }
            Ok(result) => last = result,
            Err(m) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
        }
    }
    command_response(last)
}

/// A bus's compressor, before its fader.
#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/compressor", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = crate::audio_mix::CompressorConfig, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "A value out of its range"), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_compressor(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(config): Json<crate::audio_mix::CompressorConfig>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioBusCompressor { uuid, config }).await
}

/// A bus's limiter, after its fader: no sample past its ceiling.
#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/limiter", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = crate::audio_mix::LimiterConfig, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Ceiling out of −12–0 dBFS or release out of 10–1000 ms"), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn set_bus_limiter(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(config): Json<crate::audio_mix::LimiterConfig>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioBusLimiter { uuid, config }).await
}

/// Reset a bus's held peak (its loudness readings stay).
#[utoipa::path(post, path = "/api/audio-buses/{bus_uuid}/meters/reset", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn reset_bus_meters(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::ResetAudioMeter { uuid, bus: true }).await
}

/// Start a bus's loudness readings again — integrated, short-term and
/// momentary — as a show starts.
#[utoipa::path(post, path = "/api/audio-buses/{bus_uuid}/loudness/reset", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn reset_bus_loudness(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::ResetAudioLoudness { uuid }).await
}

/// Reset a source's held peak.
#[utoipa::path(post, path = "/api/audio-sources/{source_uuid}/meters/reset", params(("source_uuid" = String, Path, description = SOURCE)), responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Audio Mixer")]
pub async fn reset_source_meters(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::ResetAudioMeter { uuid, bus: false }).await
}
