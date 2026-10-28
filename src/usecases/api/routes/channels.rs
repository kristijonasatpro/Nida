//! Channel CRUD routes.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::engine::EngineCommand;
use crate::usecases::api::{command_response, SharedState};

#[derive(Deserialize, ToSchema)]
pub struct ChannelOpacityBody {
    /// Opacity value from 0.0 (transparent) to 1.0 (opaque).
    pub opacity: f32,
}

#[derive(Deserialize, ToSchema)]
pub struct ChannelBlendModeBody {
    /// Blend mode for compositing this channel.
    pub mode: crate::engine::BlendMode,
}

#[utoipa::path(post, path = "/api/channels", responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Channels")]
pub async fn add_channel(State(state): State<SharedState>) -> impl IntoResponse {
    match state.send_command(EngineCommand::AddChannel).await {
        Ok(result) => command_response(result),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct AddInputBody {
    /// What the input shows.
    pub source: crate::engine::InputSource,
    /// Its name; after the source when left out.
    #[serde(default)]
    pub name: Option<String>,
    /// Its own sound — a camera or capture card's — on and off air with it.
    #[serde(default)]
    pub audio: Option<crate::engine::InputAudio>,
}

/// Add an input (vMix's Add Input): a camera, NDI source, screen, stream or colour.
///
/// The input is a channel at the end of the row, named after its source.
/// Replies with the channel's UUID.
#[utoipa::path(post, path = "/api/inputs", request_body = AddInputBody, responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Channels")]
pub async fn add_input(
    State(state): State<SharedState>,
    Json(body): Json<AddInputBody>,
) -> impl IntoResponse {
    match state
        .send_command(EngineCommand::AddInput {
            source: body.source,
            name: body.name,
            audio: body.audio,
        })
        .await
    {
        Ok(result) => command_response(result),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[utoipa::path(delete, path = "/api/channels/{channel_uuid}", params(("channel_uuid" = String, Path, description = "Channel UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Channel not found")), tag = "Channels")]
pub async fn remove_channel(
    State(state): State<SharedState>,
    Path(channel_uuid): Path<String>,
) -> impl IntoResponse {
    match state
        .send_command(EngineCommand::RemoveChannel { channel_uuid })
        .await
    {
        Ok(result) => command_response(result),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[utoipa::path(put, path = "/api/channels/{channel_uuid}/opacity", params(("channel_uuid" = String, Path, description = "Channel UUID")), request_body = ChannelOpacityBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Channel not found")), tag = "Channels")]
pub async fn set_opacity(
    State(state): State<SharedState>,
    Path(channel_uuid): Path<String>,
    Json(body): Json<ChannelOpacityBody>,
) -> impl IntoResponse {
    match state
        .send_command(EngineCommand::SetChannelOpacity {
            channel_uuid,
            opacity: body.opacity,
        })
        .await
    {
        Ok(result) => command_response(result),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[utoipa::path(put, path = "/api/channels/{channel_uuid}/blend-mode", params(("channel_uuid" = String, Path, description = "Channel UUID")), request_body = ChannelBlendModeBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Channel not found")), tag = "Channels")]
pub async fn set_blend_mode(
    State(state): State<SharedState>,
    Path(channel_uuid): Path<String>,
    Json(body): Json<ChannelBlendModeBody>,
) -> impl IntoResponse {
    match state
        .send_command(EngineCommand::SetChannelBlendMode {
            channel_uuid,
            mode: body.mode,
        })
        .await
    {
        Ok(result) => command_response(result),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ChannelNameBody {
    /// The new name; at most 40 characters are kept.
    pub name: String,
}

#[utoipa::path(put, path = "/api/channels/{channel_uuid}/name", params(("channel_uuid" = String, Path, description = "Channel UUID")), request_body = ChannelNameBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Channel not found")), tag = "Channels")]
pub async fn rename(
    State(state): State<SharedState>,
    Path(channel_uuid): Path<String>,
    Json(body): Json<ChannelNameBody>,
) -> impl IntoResponse {
    match state
        .send_command(EngineCommand::RenameChannel {
            channel_uuid,
            name: body.name,
        })
        .await
    {
        Ok(result) => command_response(result),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ChannelColorBody {
    /// Red, green, blue (0–255); left out or null, the channel takes back
    /// the colour its place gives.
    #[serde(default)]
    pub color: Option<[u8; 3]>,
}

#[utoipa::path(put, path = "/api/channels/{channel_uuid}/color", params(("channel_uuid" = String, Path, description = "Channel UUID")), request_body = ChannelColorBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Channel not found")), tag = "Channels")]
pub async fn set_color(
    State(state): State<SharedState>,
    Path(channel_uuid): Path<String>,
    Json(body): Json<ChannelColorBody>,
) -> impl IntoResponse {
    match state
        .send_command(EngineCommand::SetChannelColor {
            channel_uuid,
            color: body.color,
        })
        .await
    {
        Ok(result) => command_response(result),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[derive(Deserialize, ToSchema, Default)]
pub struct ChannelMoveBody {
    /// The channel to go just before; left out, the channel goes last.
    #[serde(default)]
    pub before_channel_uuid: Option<String>,
}

#[utoipa::path(put, path = "/api/channels/{channel_uuid}/move", params(("channel_uuid" = String, Path, description = "Channel UUID")), request_body = ChannelMoveBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "Channel not found")), tag = "Channels")]
pub async fn move_channel(
    State(state): State<SharedState>,
    Path(channel_uuid): Path<String>,
    body: Option<Json<ChannelMoveBody>>,
) -> impl IntoResponse {
    let before_channel_uuid = body.and_then(|Json(b)| b.before_channel_uuid);
    match state
        .send_command(EngineCommand::MoveChannel {
            channel_uuid,
            before_channel_uuid,
        })
        .await
    {
        Ok(result) => command_response(result),
        Err(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}
