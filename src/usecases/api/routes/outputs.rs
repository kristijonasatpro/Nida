//! Output management write routes.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::engine::EngineCommand;
use crate::usecases::api::{command_response, SharedState};

#[derive(Deserialize, ToSchema)]
pub struct SetDisplayBody {
    /// Name of the display monitor to target.
    pub monitor_name: String,
}

#[derive(Deserialize, ToSchema)]
pub struct AssignSurfaceBody {
    /// UUID of the surface to assign to this output.
    pub surface_uuid: String,
}

#[utoipa::path(post, path = "/api/outputs", responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Outputs")]
pub async fn create(State(s): State<SharedState>) -> impl IntoResponse {
    match s.send_command(EngineCommand::CreateOutput).await {
        Ok(r) => command_response(r),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[utoipa::path(delete, path = "/api/outputs/{output_uuid}", params(("output_uuid" = String, Path, description = "Output UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")), tag = "Outputs")]
pub async fn close(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::CloseOutput { output_uuid })
        .await
    {
        Ok(r) => command_response(r),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[utoipa::path(put, path = "/api/outputs/{output_uuid}/display", params(("output_uuid" = String, Path, description = "Output UUID")), request_body = SetDisplayBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")), tag = "Outputs")]
pub async fn set_display(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<SetDisplayBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::SetOutputDisplay {
            output_uuid,
            monitor_name: b.monitor_name,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[utoipa::path(post, path = "/api/outputs/{output_uuid}/surfaces", params(("output_uuid" = String, Path, description = "Output UUID")), request_body = AssignSurfaceBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output or surface not found")), tag = "Outputs")]
pub async fn assign_surface(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<AssignSurfaceBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::AssignSurfaceToOutput {
            output_uuid,
            surface_uuid: b.surface_uuid,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[utoipa::path(delete, path = "/api/outputs/{output_uuid}/surfaces/{surface_uuid}", params(("output_uuid" = String, Path, description = "Output UUID"), ("surface_uuid" = String, Path, description = "Surface UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output or surface assignment not found")), tag = "Outputs")]
pub async fn unassign_surface(
    State(s): State<SharedState>,
    Path((output_uuid, surface_uuid)): Path<(String, String)>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::UnassignSurfaceFromOutput {
            output_uuid,
            surface_uuid,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

// ── Headless / Start / Stop / Calibration / Warp ───────────────────

#[derive(Deserialize, ToSchema)]
pub struct CreateHeadlessBody {
    /// Output target configuration for the headless output.
    pub target: crate::renderer::context::OutputTarget,
}

#[utoipa::path(post, path = "/api/outputs/headless", request_body = CreateHeadlessBody, responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Outputs")]
pub async fn create_headless(
    State(s): State<SharedState>,
    Json(b): Json<CreateHeadlessBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::CreateHeadlessOutput { target: b.target })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}
#[utoipa::path(post, path = "/api/outputs/{output_uuid}/start", params(("output_uuid" = String, Path, description = "Output UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")), tag = "Outputs")]
pub async fn start(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::StartOutput { output_uuid })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}
#[utoipa::path(post, path = "/api/outputs/{output_uuid}/stop", params(("output_uuid" = String, Path, description = "Output UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")), tag = "Outputs")]
pub async fn stop(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::StopOutput { output_uuid })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}
#[derive(Deserialize, ToSchema)]
pub struct SetCalibrationModeBody {
    /// Calibration display mode (Off / Projector / Surfaces).
    pub mode: crate::renderer::context::CalibrationMode,
}

#[utoipa::path(put, path = "/api/outputs/{output_uuid}/calibration", params(("output_uuid" = String, Path, description = "Output UUID")), request_body = SetCalibrationModeBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")), tag = "Outputs")]
pub async fn set_calibration_mode(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<SetCalibrationModeBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::SetCalibrationMode {
            output_uuid,
            mode: b.mode,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

// ── Missing Parity Routes ─────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct SetOutputTargetBody {
    /// Output target configuration.
    pub target: crate::renderer::context::OutputTarget,
}

#[utoipa::path(put, path = "/api/outputs/{output_uuid}/target", params(("output_uuid" = String, Path, description = "Output UUID")), request_body = SetOutputTargetBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")), tag = "Outputs")]
pub async fn set_target(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<SetOutputTargetBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::SetOutputTarget {
            output_uuid,
            target: b.target,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/api/outputs/{output_uuid}/presentation",
    params(("output_uuid" = String, Path, description = "Output UUID")),
    request_body = crate::engine::value::render::PresentationRequest,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")),
    tag = "Outputs"
)]
pub async fn set_presentation(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(request): Json<crate::engine::value::render::PresentationRequest>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::SetOutputPresentation {
            output_uuid,
            request,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

/// Body for routing a headless output.
#[derive(Deserialize, ToSchema)]
pub struct SetOutputSourceBody {
    /// `"Master"` or `{"BusProgram": "<bus uuid>"}`.
    pub source: crate::renderer::context::OutputSource,
}

#[utoipa::path(
    put,
    path = "/api/outputs/{output_uuid}/source",
    params(("output_uuid" = String, Path, description = "Output UUID")),
    request_body = SetOutputSourceBody,
    responses(
        (status = 200, body = crate::usecases::api::ApiReply),
        (status = 400, description = "Not Master or a bus, or the output is a window"),
        (status = 404, description = "Output or bus not found")
    ),
    tag = "Outputs"
)]
pub async fn set_source(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<SetOutputSourceBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::SetOutputSource {
            output_uuid,
            source: b.source,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

// ── Edge Blending ────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct SetEdgeBlendBody {
    /// Edge blending configuration (four edges with enabled, width, gamma).
    pub config: crate::renderer::edge_blend::EdgeBlendConfig,
}

#[utoipa::path(
    put,
    path = "/api/outputs/{output_uuid}/edge-blend",
    params(("output_uuid" = String, Path, description = "Output UUID")),
    request_body = SetEdgeBlendBody,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")),
    tag = "Outputs"
)]
pub async fn set_edge_blend(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<SetEdgeBlendBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::SetEdgeBlend {
            output_uuid,
            config: b.config,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct SetEdgeBlendModeBody {
    /// Edge blend mode: "Auto" or "Manual".
    pub mode: crate::renderer::edge_blend::EdgeBlendMode,
}

#[utoipa::path(
    put,
    path = "/api/outputs/{output_uuid}/edge-blend-mode",
    params(("output_uuid" = String, Path, description = "Output UUID")),
    request_body = SetEdgeBlendModeBody,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Output not found")),
    tag = "Outputs"
)]
pub async fn set_edge_blend_mode(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<SetEdgeBlendModeBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::SetEdgeBlendMode {
            output_uuid,
            mode: b.mode,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

// ── LED feeds ────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct CreateLedOutputBody {
    /// Monitor to go fullscreen on (by name); a window when absent.
    #[serde(default)]
    pub monitor_name: Option<String>,
    /// The screens the feed carries.
    pub layout: crate::engine::value::led::LedLayout,
}

#[utoipa::path(
    post,
    path = "/api/outputs/led",
    request_body = CreateLedOutputBody,
    responses(
        (status = 200, body = crate::usecases::api::ApiReply),
        (status = 400, description = "A screen of no size, too many screens, or a bus that does not exist")
    ),
    tag = "Outputs"
)]
pub async fn create_led(
    State(s): State<SharedState>,
    Json(b): Json<CreateLedOutputBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::CreateLedOutput {
            monitor_name: b.monitor_name,
            layout: b.layout,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct SetOutputLedBody {
    /// The screens the feed carries; `null` makes it an ordinary output.
    pub layout: Option<crate::engine::value::led::LedLayout>,
}

#[utoipa::path(
    put,
    path = "/api/outputs/{output_uuid}/led",
    params(("output_uuid" = String, Path, description = "Output UUID")),
    request_body = SetOutputLedBody,
    responses(
        (status = 200, body = crate::usecases::api::ApiReply),
        (status = 400, description = "A screen of no size, too many screens, or a bus that does not exist"),
        (status = 404, description = "Output not found")
    ),
    tag = "Outputs"
)]
pub async fn set_led(
    State(s): State<SharedState>,
    Path(output_uuid): Path<String>,
    Json(b): Json<SetOutputLedBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::SetOutputLed {
            output_uuid,
            layout: b.layout,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/api/outputs/{output_uuid}/led/screens/{screen_uuid}/bus",
    params(
        ("output_uuid" = String, Path, description = "Output UUID"),
        ("screen_uuid" = String, Path, description = "LED screen UUID")
    ),
    responses(
        (status = 200, body = crate::usecases::api::ApiReply),
        (status = 404, description = "Output, LED feed or screen not found")
    ),
    tag = "Outputs"
)]
pub async fn create_led_screen_bus(
    State(s): State<SharedState>,
    Path((output_uuid, screen_uuid)): Path<(String, String)>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::CreateLedScreenBus {
            output_uuid,
            screen_uuid,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

/// Body for naming a screen.
#[derive(Deserialize, ToSchema)]
pub struct ScreenNameBody {
    /// The screen, as the engine state's monitors list it (`name`).
    pub monitor_name: String,
    /// Its name on this computer ("Stage LED"), or `null` to take it away.
    pub name: Option<String>,
}

#[utoipa::path(put, path = "/api/screens/name", request_body = ScreenNameBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such screen connected")), tag = "Outputs")]
pub async fn set_screen_name(
    State(s): State<SharedState>,
    Json(b): Json<ScreenNameBody>,
) -> impl IntoResponse {
    match s
        .send_command(EngineCommand::RenameScreen {
            monitor_name: b.monitor_name,
            name: b.name,
        })
        .await
    {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[utoipa::path(post, path = "/api/screens/identify", responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Outputs")]
pub async fn identify_screens(State(s): State<SharedState>) -> impl IntoResponse {
    match s.send_command(EngineCommand::IdentifyScreens).await {
        Ok(r) => command_response(r),
        Err(m) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}
