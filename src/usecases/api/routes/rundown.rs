//! Rundown routes: the show in order, as steps taken with one press — every
//! bus changed together, Main taken, clips started. The rundown itself is in
//! `GET /api/state` under `rundown`.
//!
//! A step is addressed by UUID in its path, as everything is — see
//! `/spec/api-addressing.md`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::bus::BusInputRef;
use crate::engine::EngineCommand;
use crate::usecases::api::{command_response, SharedState};

async fn dispatch(state: &SharedState, cmd: EngineCommand) -> axum::response::Response {
    match state.send_command(cmd).await {
        Ok(r) => command_response(r),
        Err(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

/// A new step, keeping what is on now.
#[derive(Deserialize, ToSchema)]
pub struct AddStepBody {
    /// What the operator calls it: "A", "B Walk-on".
    pub name: String,
    /// The buses it keeps, by UUID. Omit for every bus but one the stream
    /// or the recording takes; `[]` for none.
    #[serde(default)]
    pub buses: Option<Vec<String>>,
    /// Keep what is on Main too, and every overlay on or off.
    #[serde(default)]
    pub main: bool,
}

#[utoipa::path(post, path = "/api/rundown/steps", request_body = AddStepBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such bus")), tag = "Rundown")]
pub async fn add_step(
    State(s): State<SharedState>,
    Json(b): Json<AddStepBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::AddRundownStep {
            name: b.name,
            buses: b.buses,
            main: b.main,
        },
    )
    .await
}

#[utoipa::path(delete, path = "/api/rundown/steps/{step_uuid}", params(("step_uuid" = String, Path, description = "Step UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step")), tag = "Rundown")]
pub async fn remove_step(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RemoveRundownStep { uuid }).await
}

/// Put a copy of the step right after it; replies with the copy's UUID.
#[utoipa::path(post, path = "/api/rundown/steps/{step_uuid}/duplicate", params(("step_uuid" = String, Path, description = "Step UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step")), tag = "Rundown")]
pub async fn duplicate_step(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::DuplicateRundownStep { uuid }).await
}

/// A step's name.
#[derive(Deserialize, ToSchema)]
pub struct StepNameBody {
    pub name: String,
}

#[utoipa::path(put, path = "/api/rundown/steps/{step_uuid}/name", params(("step_uuid" = String, Path, description = "Step UUID")), request_body = StepNameBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step")), tag = "Rundown")]
pub async fn rename_step(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<StepNameBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RenameRundownStep { uuid, name: b.name }).await
}

/// Which way a step moves in the rundown.
#[derive(Deserialize, ToSchema)]
pub struct MoveStepBody {
    /// `true` one place later, `false` one place earlier.
    pub later: bool,
}

#[utoipa::path(post, path = "/api/rundown/steps/{step_uuid}/move", params(("step_uuid" = String, Path, description = "Step UUID")), request_body = MoveStepBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step")), tag = "Rundown")]
pub async fn move_step(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<MoveStepBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::MoveRundownStep {
            uuid,
            later: b.later,
        },
    )
    .await
}

/// What a bus shows in a step.
#[derive(Deserialize, ToSchema)]
pub struct StepScreenBody {
    /// `"Black"`, `"Master"`, `{"Channel": "<uuid>"}` or `{"Deck": "<uuid>"}`
    /// — or `null` to leave the bus as it is when the step is taken.
    pub input: Option<BusInputRef>,
}

#[utoipa::path(
    put,
    path = "/api/rundown/steps/{step_uuid}/screens/{bus_uuid}",
    params(
        ("step_uuid" = String, Path, description = "Step UUID"),
        ("bus_uuid" = String, Path, description = "Bus UUID")
    ),
    request_body = StepScreenBody,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step, bus or input")),
    tag = "Rundown"
)]
pub async fn set_screen(
    State(s): State<SharedState>,
    Path((uuid, bus_uuid)): Path<(String, String)>,
    Json(b): Json<StepScreenBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetRundownScreen {
            uuid,
            bus_uuid,
            input: b.input,
        },
    )
    .await
}

/// What a step takes to Main.
#[derive(Deserialize, ToSchema)]
pub struct StepMainBody {
    /// `{"Channel": "<uuid>"}`, `{"Deck": "<uuid>"}` or `"Black"` — or `null`
    /// to leave Main as it is.
    pub main: Option<crate::mixer::MainSource>,
}

#[utoipa::path(put, path = "/api/rundown/steps/{step_uuid}/main", params(("step_uuid" = String, Path, description = "Step UUID")), request_body = StepMainBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step, channel or deck")), tag = "Rundown")]
pub async fn set_main(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<StepMainBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetRundownMain { uuid, main: b.main }).await
}

/// Whether a step switches an overlay channel on or off.
#[derive(Deserialize, ToSchema)]
pub struct StepOverlayBody {
    /// `true` on, `false` off — or `null` to leave it as it is.
    pub on: Option<bool>,
}

#[utoipa::path(
    put,
    path = "/api/rundown/steps/{step_uuid}/overlays/{channel_uuid}",
    params(
        ("step_uuid" = String, Path, description = "Step UUID"),
        ("channel_uuid" = String, Path, description = "Overlay channel UUID")
    ),
    request_body = StepOverlayBody,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Not an overlay channel"), (status = 404, description = "No such step or channel")),
    tag = "Rundown"
)]
pub async fn set_overlay(
    State(s): State<SharedState>,
    Path((uuid, channel_uuid)): Path<(String, String)>,
    Json(b): Json<StepOverlayBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetRundownOverlay {
            uuid,
            channel_uuid,
            on: b.on,
        },
    )
    .await
}

/// What a step does to a clip, a playlist or a timer.
#[derive(Deserialize, ToSchema)]
pub struct StepPlayBody {
    /// `"start"` (from its beginning), `"resume"`, `"pause"` or `"stop"`
    /// (back at its beginning) — or `null` for nothing.
    pub action: Option<crate::rundown::PlayAction>,
}

#[utoipa::path(
    put,
    path = "/api/rundown/steps/{step_uuid}/plays/{deck_uuid}",
    params(
        ("step_uuid" = String, Path, description = "Step UUID"),
        ("deck_uuid" = String, Path, description = "UUID of a clip, playlist or timer deck")
    ),
    request_body = StepPlayBody,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Not something a step can start"), (status = 404, description = "No such step or deck")),
    tag = "Rundown"
)]
pub async fn set_play(
    State(s): State<SharedState>,
    Path((uuid, deck_uuid)): Path<(String, String)>,
    Json(b): Json<StepPlayBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetRundownPlay {
            uuid,
            deck_uuid,
            action: b.action,
        },
    )
    .await
}

/// Whether a clip or playlist a step plays is faded out at the next step.
#[derive(Deserialize, ToSchema)]
pub struct StepUntilNextBody {
    /// `true`: faded out — its sound over the next step's fade — and
    /// stopped, back at its beginning, when another step is taken.
    pub on: bool,
}

#[utoipa::path(
    put,
    path = "/api/rundown/steps/{step_uuid}/plays/{deck_uuid}/until-next",
    params(
        ("step_uuid" = String, Path, description = "Step UUID"),
        ("deck_uuid" = String, Path, description = "UUID of a clip or playlist deck the step plays")
    ),
    request_body = StepUntilNextBody,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Not a clip or playlist the step plays"), (status = 404, description = "No such step")),
    tag = "Rundown"
)]
pub async fn set_until_next(
    State(s): State<SharedState>,
    Path((uuid, deck_uuid)): Path<(String, String)>,
    Json(b): Json<StepUntilNextBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetRundownUntilNext {
            uuid,
            deck_uuid,
            on: b.on,
        },
    )
    .await
}

/// How a step sets a timer it starts.
#[derive(Deserialize, ToSchema)]
pub struct StepTimerBody {
    /// `{"down": {"secs": 900}}` — down from 15 minutes — or `"up"`; `null`
    /// for as the timer is set.
    pub timer: Option<crate::rundown::TimerSet>,
}

#[utoipa::path(
    put,
    path = "/api/rundown/steps/{step_uuid}/plays/{deck_uuid}/timer",
    params(
        ("step_uuid" = String, Path, description = "Step UUID"),
        ("deck_uuid" = String, Path, description = "UUID of a timer deck the step starts")
    ),
    request_body = StepTimerBody,
    responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Not a timer the step starts, or a count down of no time"), (status = 404, description = "No such step")),
    tag = "Rundown"
)]
pub async fn set_timer(
    State(s): State<SharedState>,
    Path((uuid, deck_uuid)): Path<(String, String)>,
    Json(b): Json<StepTimerBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetRundownTimer {
            uuid,
            deck_uuid,
            timer: b.timer,
        },
    )
    .await
}

/// The time of day a step is taken by itself at.
#[derive(Deserialize, ToSchema)]
pub struct StepAtBody {
    /// Seconds after midnight — 33300 for 9:15 — or `null` for none.
    pub secs: Option<u32>,
    /// Taken at its time even when GO would take another step next; left
    /// as it is when not given.
    #[serde(default)]
    pub always: Option<bool>,
}

#[utoipa::path(put, path = "/api/rundown/steps/{step_uuid}/at", params(("step_uuid" = String, Path, description = "Step UUID")), request_body = StepAtBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Not a time of day"), (status = 404, description = "No such step")), tag = "Rundown")]
pub async fn set_at(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<StepAtBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetRundownAt {
            uuid,
            at_secs: b.secs,
            always: b.always,
        },
    )
    .await
}

/// When a step goes on to the next by itself.
#[derive(Deserialize, ToSchema)]
pub struct StepThenBody {
    /// Seconds after the step is taken (more than none, an hour at most; no
    /// sooner than the step's own fade is done) — or `null` to wait for GO.
    pub secs: Option<f32>,
    /// Instead of a time: the UUID of a clip or playlist the step starts or
    /// carries on — the next step is taken when it has played to its end.
    #[serde(default)]
    pub when_ends: Option<String>,
}

#[utoipa::path(put, path = "/api/rundown/steps/{step_uuid}/then", params(("step_uuid" = String, Path, description = "Step UUID")), request_body = StepThenBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step")), tag = "Rundown")]
pub async fn set_then(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<StepThenBody>,
) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::SetRundownThen {
            uuid,
            secs: b.secs,
            when_ends: b.when_ends,
        },
    )
    .await
}

/// Make the step's picture what is on now, for the parts it sets.
///
/// The buses it lists, and Main and the overlays if it sets them.
#[utoipa::path(post, path = "/api/rundown/steps/{step_uuid}/capture", params(("step_uuid" = String, Path, description = "Step UUID")), responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step")), tag = "Rundown")]
pub async fn capture_step(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::CaptureRundownStep { uuid }).await
}

/// How a step is taken.
#[derive(Deserialize, ToSchema)]
pub struct TakeStepBody {
    /// Dissolve — every bus over the rundown's fade time, Main by its own
    /// fade — rather than cut.
    #[serde(default)]
    pub fade: bool,
}

/// Take a step, all of it or none; refused, the reply says why.
#[utoipa::path(post, path = "/api/rundown/steps/{step_uuid}/take", params(("step_uuid" = String, Path, description = "Step UUID")), request_body = TakeStepBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step"), (status = 503, description = "Something it sets is not in the show, or is still fading")), tag = "Rundown")]
pub async fn take_step(
    State(s): State<SharedState>,
    Path(uuid): Path<String>,
    Json(b): Json<TakeStepBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::TakeRundownStep { uuid, fade: b.fade }).await
}

/// Taking the next step.
#[derive(Deserialize, ToSchema)]
pub struct GoBody {
    /// Dissolve rather than cut. On unless said otherwise.
    #[serde(default = "yes")]
    pub fade: bool,
    /// The step this press is meant for, by UUID: refused if another is
    /// next by now, so a press never lands on the step after the one meant.
    #[serde(default)]
    pub expect: Option<String>,
}

fn yes() -> bool {
    true
}

/// Take the next step: the one after the step taken last, or the one set as next.
#[utoipa::path(post, path = "/api/rundown/go", request_body = GoBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 503, description = "At the end of the rundown, another step is next, or the step cannot be taken")), tag = "Rundown")]
pub async fn go(State(s): State<SharedState>, Json(b): Json<GoBody>) -> impl IntoResponse {
    dispatch(
        &s,
        EngineCommand::RundownGo {
            fade: b.fade,
            expect: b.expect,
        },
    )
    .await
}

/// The step GO takes next.
#[derive(Deserialize, ToSchema)]
pub struct NextBody {
    /// Step UUID.
    pub uuid: String,
}

/// Make a step the one GO takes next, without taking anything.
#[utoipa::path(put, path = "/api/rundown/next", request_body = NextBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 404, description = "No such step")), tag = "Rundown")]
pub async fn set_next(State(s): State<SharedState>, Json(b): Json<NextBody>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetRundownNext { uuid: b.uuid }).await
}

/// Stop a step going on to the next by itself; the rundown stays until told.
#[utoipa::path(post, path = "/api/rundown/hold", responses((status = 200, body = crate::usecases::api::ApiReply)), tag = "Rundown")]
pub async fn hold(State(s): State<SharedState>) -> impl IntoResponse {
    dispatch(&s, EngineCommand::RundownHold).await
}

/// The rundown's fade time.
#[derive(Deserialize, ToSchema)]
pub struct RundownFadeBody {
    /// Seconds the buses take to fade to a step. Finite and positive.
    pub secs: f32,
}

#[utoipa::path(put, path = "/api/rundown/fade-time", request_body = RundownFadeBody, responses((status = 200, body = crate::usecases::api::ApiReply), (status = 400, description = "Not a time")), tag = "Rundown")]
pub async fn set_fade_time(
    State(s): State<SharedState>,
    Json(b): Json<RundownFadeBody>,
) -> impl IntoResponse {
    dispatch(&s, EngineCommand::SetRundownFade { secs: b.secs }).await
}
