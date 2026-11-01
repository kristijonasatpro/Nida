//! Production bus routes.
//!
//! Every route addresses a bus by UUID in its path. There is deliberately no
//! "current bus" and no unaddressed cut/auto, so an API caller cannot switch a
//! bus it did not name — see `/spec/api-addressing.md` and `src/internal/bus`.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::bus::BusInputRef;
use crate::engine::EngineCommand;
use crate::usecases::api::{command_response, SharedState};

/// Body for creating a bus.
#[derive(Deserialize, ToSchema)]
pub struct CreateBusBody {
    /// Operator-facing label, e.g. `STREAM`, `LED`, `TV`.
    pub name: String,
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
}

/// Body for renaming a bus.
#[derive(Deserialize, ToSchema)]
pub struct RenameBusBody {
    /// New label.
    pub name: String,
}

/// Body for staging an input on Preview.
#[derive(Deserialize, ToSchema)]
pub struct BusPreviewBody {
    /// What to stage. Channels and decks are addressed by stable UUID.
    pub input: BusInputRef,
}

/// Body for resizing a bus canvas.
#[derive(Deserialize, ToSchema)]
pub struct BusCanvasBody {
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
}

/// Body for an `AUTO` take.
#[derive(Deserialize, ToSchema)]
pub struct BusAutoBody {
    /// Transition duration in seconds. Omit to use the bus's own default.
    #[serde(default)]
    pub duration_secs: Option<f32>,
}

/// Body for hiding or showing one mixer layer on a bus.
#[derive(Deserialize, ToSchema)]
pub struct BusLayerBody {
    /// `true` leaves the layer out of this bus's Master; `false` puts it back.
    pub hidden: bool,
}

/// Body for putting a bus on a screen.
#[derive(Deserialize, ToSchema)]
pub struct BusScreenBody {
    /// Monitor name as listed in the engine state's monitors, or `null` to
    /// take the bus off screen.
    pub monitor_name: Option<String>,
}

/// Body for putting a bus on a Blackmagic device.
#[derive(Deserialize, ToSchema)]
pub struct BusBlackmagicBody {
    /// Device name as listed in the engine state's `decklink_devices`, or
    /// `null` to take the bus off its device.
    pub device: Option<String>,
}

/// Body for setting a bus's default `AUTO` duration.
#[derive(Deserialize, ToSchema)]
pub struct BusAutoDurationBody {
    /// Duration in seconds. Must be finite and positive.
    pub duration_secs: f32,
}

async fn dispatch(state: &SharedState, cmd: EngineCommand) -> axum::response::Response {
    match state.send_command(cmd).await {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[utoipa::path(post, path = "/api/buses", request_body = CreateBusBody, responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Buses")]
pub async fn create(
    State(s): State<SharedState>,
    Json(body): Json<CreateBusBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::AddBus {
            name: body.name,
            width: body.width,
            height: body.height,
        },
    )
    .await
}

#[utoipa::path(delete, path = "/api/buses/{bus_uuid}", params(("bus_uuid" = String, Path, description = "Bus UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus not found")), tag = "Buses")]
pub async fn delete(State(s): State<SharedState>, Path(uuid): Path<String>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RemoveBus { uuid }).await
}

#[utoipa::path(put, path = "/api/buses/{bus_uuid}/name", params(("bus_uuid" = String, Path, description = "Bus UUID")), request_body = RenameBusBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus not found")), tag = "Buses")]
pub async fn rename(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(body): Json<RenameBusBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::RenameBus {
            uuid,
            name: body.name,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/buses/{bus_uuid}/preview", params(("bus_uuid" = String, Path, description = "Bus UUID")), request_body = BusPreviewBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus not found")), tag = "Buses")]
pub async fn set_preview(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(body): Json<BusPreviewBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetBusPreview {
            uuid,
            input: body.input,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/buses/{bus_uuid}/canvas", params(("bus_uuid" = String, Path, description = "Bus UUID")), request_body = BusCanvasBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus not found")), tag = "Buses")]
pub async fn set_canvas(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(body): Json<BusCanvasBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetBusCanvas {
            uuid,
            width: body.width,
            height: body.height,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/buses/{bus_uuid}/auto-duration", params(("bus_uuid" = String, Path, description = "Bus UUID")), request_body = BusAutoDurationBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Duration must be finite and positive"), (status = 404, description = "Bus not found")), tag = "Buses")]
pub async fn set_auto_duration(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(body): Json<BusAutoDurationBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetBusAutoDuration {
            uuid,
            duration_secs: body.duration_secs,
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/buses/{bus_uuid}/cut", params(("bus_uuid" = String, Path, description = "Bus UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus not found")), tag = "Buses")]
pub async fn cut(State(s): State<SharedState>, Path(uuid): Path<String>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::BusCut { uuid }).await
}

#[utoipa::path(post, path = "/api/buses/{bus_uuid}/auto", params(("bus_uuid" = String, Path, description = "Bus UUID")), request_body = BusAutoBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus not found")), tag = "Buses")]
pub async fn auto(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(body): Json<BusAutoBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::BusAuto {
            uuid,
            duration_secs: body.duration_secs,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/buses/{bus_uuid}/screen", params(("bus_uuid" = String, Path, description = "Bus UUID")), request_body = BusScreenBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus not found")), tag = "Buses")]
pub async fn set_screen(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(body): Json<BusScreenBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::ShowBusOnScreen {
            uuid,
            monitor_name: body.monitor_name,
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/buses/{bus_uuid}/blackmagic", params(("bus_uuid" = String, Path, description = "Bus UUID")), request_body = BusBlackmagicBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus or device not found")), tag = "Buses")]
pub async fn set_blackmagic(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(body): Json<BusBlackmagicBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::ShowBusOnBlackmagic {
            uuid,
            device: body.device,
        },
    )
    .await
}

#[utoipa::path(
    put,
    path = "/api/buses/{bus_uuid}/layers/{channel_uuid}",
    params(
        ("bus_uuid" = String, Path, description = "Bus UUID"),
        ("channel_uuid" = String, Path, description = "Channel (mixer layer) UUID")
    ),
    request_body = BusLayerBody,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Bus or channel not found")),
    tag = "Buses"
)]
pub async fn set_layer(
    State(s): State<SharedState>,
    Path((uuid, channel_uuid)): Path<(String, String)>,
    Json(body): Json<BusLayerBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetBusLayerHidden {
            uuid,
            channel_uuid,
            hidden: body.hidden,
        },
    )
    .await
}
