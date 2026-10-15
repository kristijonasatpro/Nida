//! Stream routes: what is streamed, the four destinations, start and stop.
//!
//! How the stream and each destination are doing is in `GET /api/stream`
//! (and `GET /api/state` under `stream`). Stream keys and passwords can be
//! set here but are never read back.

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

#[utoipa::path(get, path = "/api/stream", responses((status = 200, body = crate::streaming::StreamSnapshot), (status = 503, description = "Engine not yet initialized")), tag = "Stream")]
pub async fn get_stream(State(state): State<SharedState>) -> impl IntoResponse {
    match state.engine_state.read() {
        Ok(guard) => match guard.as_ref() {
            Some(engine_state) => Json(engine_state.stream.clone()).into_response(),
            None => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Engine not yet initialized",
            )
                .into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "State lock poisoned").into_response(),
    }
}

#[utoipa::path(put, path = "/api/stream/options", request_body = crate::streaming::StreamOptions, responses((status = 200, body = CommandResult), (status = 400, description = "While the stream is on only the source changes — and, for a stream encoded in Nida (`rate_live`), the video bit rate and `adaptive`")), tag = "Stream")]
pub async fn set_options(
    State(s): State<SharedState>,
    Json(options): Json<crate::streaming::StreamOptions>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetStreamOptions { options }).await
}

/// One destination's settings.
#[derive(Deserialize, ToSchema)]
pub struct StreamDestinationBody {
    pub enabled: bool,
    #[serde(default)]
    pub name: String,
    /// `rtmp://` or `rtmps://` server and application.
    pub url: String,
    /// Stream key; leave out to keep the one saved.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub username: String,
    /// Leave out to keep the one saved.
    #[serde(default)]
    pub password: Option<String>,
}

#[utoipa::path(put, path = "/api/stream/destinations/{index}", params(("index" = usize, Path, description = "Destination 0–3")), request_body = StreamDestinationBody, responses((status = 200, body = CommandResult), (status = 400, description = "No such destination")), tag = "Stream")]
pub async fn set_destination(
    State(s): State<SharedState>,
    Path(index): Path<usize>,
    Json(b): Json<StreamDestinationBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetStreamDestination {
            index,
            enabled: b.enabled,
            name: b.name,
            url: b.url,
            key: b.key,
            username: b.username,
            password: b.password,
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/stream/start", responses((status = 200, body = CommandResult), (status = 400, description = "No destination switched on, or none can be sent to")), tag = "Stream")]
pub async fn start(State(s): State<SharedState>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::StartStream).await
}

#[utoipa::path(post, path = "/api/stream/stop", responses((status = 200, body = CommandResult)), tag = "Stream")]
pub async fn stop(State(s): State<SharedState>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::StopStream).await
}
