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

use crate::engine::{CommandResult, EngineCommand};
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
    /// First output of the pair (0 = outputs 1-2).
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
}

/// A send switch.
#[derive(Deserialize, ToSchema)]
pub struct AudioSendBody {
    pub on: bool,
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

#[utoipa::path(post, path = "/api/audio-buses", request_body = AudioNameBody, responses((status = 200, body = CommandResult)), tag = "Audio Mixer")]
pub async fn create_bus(
    State(s): State<SharedState>,
    Json(b): Json<AudioNameBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::AddAudioBus { name: b.name }).await
}

#[utoipa::path(delete, path = "/api/audio-buses/{bus_uuid}", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), responses((status = 200, body = CommandResult), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn delete_bus(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RemoveAudioBus { uuid }).await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/name", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioNameBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
pub async fn rename_bus(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioNameBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RenameAudioBus { uuid, name: b.name }).await
}

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/gain", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioGainBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/mute", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioMuteBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/delay", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioDelayBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-buses/{bus_uuid}/device", params(("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioDeviceBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
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

#[utoipa::path(post, path = "/api/audio-inputs", request_body = AudioInputBody, responses((status = 200, body = CommandResult)), tag = "Audio Mixer")]
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
#[utoipa::path(post, path = "/api/audio-inputs/app", request_body = AppSoundBody, responses((status = 200, body = CommandResult)), tag = "Audio Mixer")]
pub async fn create_app_sound(
    State(s): State<SharedState>,
    Json(b): Json<AppSoundBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::AddAppSound {
            app: b.app,
            name: b.name,
        },
    )
    .await
}

#[utoipa::path(delete, path = "/api/audio-inputs/{input_uuid}", params(("input_uuid" = String, Path, description = "Audio input UUID")), responses((status = 200, body = CommandResult), (status = 404, description = "No such input")), tag = "Audio Mixer")]
pub async fn delete_input(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RemoveAudioInput { uuid }).await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/gain", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioGainBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such source")), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/mute", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioMuteBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such source")), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/sends/{bus_uuid}", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)"), ("bus_uuid" = String, Path, description = "Audio bus UUID")), request_body = AudioSendBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such source or bus")), tag = "Audio Mixer")]
pub async fn set_send(
    State(s): State<SharedState>,
    Path((uuid, bus_uuid)): Path<(String, String)>,
    Json(b): Json<AudioSendBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetAudioSend {
            uuid,
            bus_uuid,
            on: b.on,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/follow", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioFollowBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such source or video bus")), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/auto", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioAutoBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such source")), tag = "Audio Mixer")]
/// Audio Auto: the source's sound switches on as it goes on air and off as
/// it leaves, fading with the picture.
pub async fn set_auto(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioAutoBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioAuto { uuid, auto: b.auto }).await
}

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/balance", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioBalanceBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such source")), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/delay", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioDelayBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such source")), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/outputs/{output_uuid}/audio-bus", params(("output_uuid" = String, Path, description = "Output UUID")), request_body = OutputAudioBusBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such output or bus")), tag = "Outputs")]
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

#[utoipa::path(post, path = "/api/audio-devices/rescan", responses((status = 200, body = CommandResult)), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-sources/{source_uuid}/solo", params(("source_uuid" = String, Path, description = "Source UUID (the deck's, for a clip)")), request_body = AudioSoloBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such source")), tag = "Audio Mixer")]
pub async fn set_solo(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<AudioSoloBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetAudioSolo { uuid, solo: b.solo }).await
}

#[utoipa::path(put, path = "/api/audio-monitor/device", request_body = AudioDeviceBody, responses((status = 200, body = CommandResult)), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-monitor/gain", request_body = AudioGainBody, responses((status = 200, body = CommandResult)), tag = "Audio Mixer")]
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

#[utoipa::path(put, path = "/api/audio-monitor/listen", request_body = AudioListenBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such bus")), tag = "Audio Mixer")]
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
