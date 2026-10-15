//! Main routes: the vMix-style Main — which mode the master is made in, what
//! is on Main, overlays, the fade time. What is on air is in `GET /api/state`
//! under `mixer.main`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::engine::{CommandResult, EngineCommand};
use crate::usecases::api::{command_response, SharedState};

async fn dispatch(state: &SharedState, cmd: EngineCommand) -> axum::response::Response {
    match state.send_command(cmd).await {
        Ok(r) => command_response(r),
        Err(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

/// How the master is made.
#[derive(Deserialize, ToSchema)]
pub struct MixModeBody {
    /// `program`: one channel or input on Main with overlays on top;
    /// `layered`: every channel at its fader.
    pub mode: crate::mixer::MixMode,
}

#[utoipa::path(put, path = "/api/main/mode", request_body = MixModeBody, responses((status = 200, body = CommandResult)), tag = "Main")]
pub async fn set_mode(
    State(s): State<SharedState>,
    Json(b): Json<MixModeBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetMixMode { mode: b.mode }).await
}

/// What to put on Main.
#[derive(Deserialize, ToSchema)]
pub struct TakeBody {
    /// `{"Channel": "<uuid>"}`, `{"Deck": "<uuid>"}` or `"Black"`.
    pub source: crate::mixer::MainSource,
    /// Dissolve over the fade time rather than cut.
    #[serde(default)]
    pub fade: bool,
}

#[utoipa::path(post, path = "/api/main/take", request_body = TakeBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such channel or deck")), tag = "Main")]
pub async fn take(State(s): State<SharedState>, Json(b): Json<TakeBody>) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::TakeToMain {
            source: b.source,
            fade: b.fade,
        },
    )
    .await
}

/// A fade time.
#[derive(Deserialize, ToSchema)]
pub struct FadeTimeBody {
    /// Seconds, 0–10.
    pub secs: f32,
}

#[utoipa::path(put, path = "/api/main/fade-time", request_body = FadeTimeBody, responses((status = 200, body = CommandResult)), tag = "Main")]
pub async fn set_fade_time(
    State(s): State<SharedState>,
    Json(b): Json<FadeTimeBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetMainFadeTime { secs: b.secs }).await
}

/// A transition.
#[derive(Deserialize, ToSchema)]
pub struct TransitionBody {
    pub transition: crate::mixer::MainTransition,
}

/// What a take to Main that is not a cut does: fade, merge, wipe, slide, zoom or iris.
#[utoipa::path(put, path = "/api/main/transition", request_body = TransitionBody, responses((status = 200, body = CommandResult)), tag = "Main")]
pub async fn set_transition(
    State(s): State<SharedState>,
    Json(b): Json<TransitionBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetMainTransition {
            transition: b.transition,
        },
    )
    .await
}

/// Whether a channel is an overlay.
#[derive(Deserialize, ToSchema)]
pub struct OverlayBody {
    pub overlay: bool,
}

#[utoipa::path(put, path = "/api/channels/{channel_uuid}/overlay", params(("channel_uuid" = String, Path, description = "Channel UUID")), request_body = OverlayBody, responses((status = 200, body = CommandResult), (status = 404, description = "No such channel")), tag = "Main")]
pub async fn set_overlay(
    State(s): State<SharedState>,
    Path(channel_uuid): Path<String>,
    Json(b): Json<OverlayBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetChannelOverlay {
            channel_uuid,
            overlay: b.overlay,
        },
    )
    .await
}

/// An overlay switched on or off.
#[derive(Deserialize, ToSchema)]
pub struct OverlayOnBody {
    pub on: bool,
    #[serde(default)]
    pub fade: bool,
}

#[utoipa::path(put, path = "/api/channels/{channel_uuid}/overlay-on", params(("channel_uuid" = String, Path, description = "Channel UUID")), request_body = OverlayOnBody, responses((status = 200, body = CommandResult), (status = 400, description = "Not an overlay"), (status = 404, description = "No such channel")), tag = "Main")]
pub async fn set_overlay_on(
    State(s): State<SharedState>,
    Path(channel_uuid): Path<String>,
    Json(b): Json<OverlayOnBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetOverlayOn {
            channel_uuid,
            on: b.on,
            fade: b.fade,
        },
    )
    .await
}
