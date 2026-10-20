//! Record routes: what is recorded and how, start and stop. How it is doing
//! is in `GET /api/record` (and `GET /api/state` under `record`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;

use crate::engine::EngineCommand;
use crate::usecases::api::{command_response, SharedState};

async fn dispatch(state: &SharedState, cmd: EngineCommand) -> axum::response::Response {
    match state.send_command(cmd).await {
        Ok(r) => command_response(r),
        Err(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[utoipa::path(get, path = "/api/record", responses((status = 200, body = crate::record::RecordSnapshot), (status = 503, description = "Engine not yet initialized")), tag = "Record")]
pub async fn get_record(State(state): State<SharedState>) -> impl IntoResponse {
    match state.engine_state.read() {
        Ok(guard) => match guard.as_ref() {
            Some(engine_state) => Json(engine_state.record.clone()).into_response(),
            None => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Engine not yet initialized",
            )
                .into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "State lock poisoned").into_response(),
    }
}

#[utoipa::path(put, path = "/api/record/options", request_body = crate::record::RecordOptions, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Only the source can change while recording")), tag = "Record")]
pub async fn set_options(
    State(s): State<SharedState>,
    Json(options): Json<crate::record::RecordOptions>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetRecordOptions { options }).await
}

#[utoipa::path(post, path = "/api/record/start", responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Record")]
pub async fn start(State(s): State<SharedState>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::StartRecording).await
}

#[utoipa::path(post, path = "/api/record/stop", responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Record")]
pub async fn stop(State(s): State<SharedState>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::StopRecording).await
}
