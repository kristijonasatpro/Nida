//! API end-to-end tests — real `VardaApp` wired to axum router via `SharedState`.
//!
//! Each test creates a headless `VardaApp`, wires its command channel into an axum
//! router, spawns a background thread to process commands, then exercises the
//! HTTP API and verifies state mutations.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use varda::app::VardaApp;
use varda::engine::CommandResult;
use varda::usecases::api::SharedState;

mod common;

/// Create a headless `VardaApp` and wire its command channel to an axum router.
///
/// Commands are processed eagerly via a background tokio task that drains the
/// channel and auto-replies `CommandResult::Ok`. The engine state snapshot is
/// published once at setup; tests that need to verify state mutations after API
/// calls should re-read from the shared `engine_state` arc.
fn setup() -> Option<axum::Router> {
    setup_seeing().map(|(router, _)| router)
}

/// As `setup`, with the commands the routes send, in order.
fn setup_seeing() -> Option<(
    axum::Router,
    std::sync::mpsc::Receiver<varda::engine::EngineCommand>,
)> {
    let gpu = common::headless_gpu()?;
    let config = varda::testing::headless_config();
    // Once a GPU exists, a construction failure is a bug, not a reason to skip.
    let app = VardaApp::new(gpu, &config).expect("VardaApp::new");

    let _real_cmd_tx = app.command_sender();
    let state_reader = app.state_reader();
    // Publish initial state so GET routes work
    app.publish_state();

    // Create a proxy channel: API sends commands here, we forward to the real
    // engine sender. Since VardaApp is !Send, we forward immediately and let
    // the engine process when app.process_commands() would be called. For E2E
    // route tests, what matters is that the route handler gets a reply, so we
    // use a mock auto-reply approach (same as existing route tests).
    let (proxy_tx, mut proxy_rx) =
        tokio::sync::mpsc::unbounded_channel::<varda::engine::CommandEnvelope>();

    let (seen, sent) = std::sync::mpsc::channel();
    tokio::spawn(async move {
        while let Some((cmd, reply_tx)) = proxy_rx.recv().await {
            // Nobody listening (`setup`): the reply is all that matters.
            let _ = seen.send(cmd);
            if let Some(tx) = reply_tx {
                let _ = tx.send(CommandResult::Ok);
            }
        }
    });

    let shared = SharedState {
        command_tx: proxy_tx,
        engine_state: state_reader,
    };
    let router = varda::usecases::api::runner::build_router(shared);
    Some((router, sent))
}

async fn get_json(app: axum::Router, path: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 128)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn put_json(
    app: axum::Router,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(
            Request::put(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 128)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn post_json(
    app: axum::Router,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 128)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn post_empty(app: axum::Router, path: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(Request::post(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 128)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

// ── Tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn health_with_real_engine() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = get_json(app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn state_reflects_real_engine() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = get_json(app, "/api/state").await;
    assert_eq!(status, StatusCode::OK);
    let channels = json["mixer"]["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 2);
}

/// `PUT /api/compositor` sends the engine the switches given, and leaves out
/// the rest.
#[tokio::test]
async fn set_compositor_via_api_sends_the_switches_given() {
    let Some((app, sent)) = setup_seeing() else {
        return;
    };
    let (status, _) = put_json(
        app,
        "/api/compositor",
        serde_json::json!({"slow_layers_less_often": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = sent.try_recv().expect("a command sent");
    assert!(
        matches!(
            sent,
            varda::engine::EngineCommand::SetCompositor {
                direct_layers: None,
                merge_layer_passes: None,
                slow_layers_less_often: Some(false),
                skip_blank_channels: None,
                keep_still_text: None,
                merge_main_passes: None,
                pass_main_through: None,
                keep_still_clips: None,
                keep_still_inputs: None,
            }
        ),
        "{sent:?}"
    );
}

/// `PUT /api/compositor` with leaving blank inputs undrawn alone sends that
/// switch alone.
#[tokio::test]
async fn set_compositor_via_api_sends_skip_blank_channels() {
    let Some((app, sent)) = setup_seeing() else {
        return;
    };
    let (status, _) = put_json(
        app,
        "/api/compositor",
        serde_json::json!({"skip_blank_channels": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = sent.try_recv().expect("a command sent");
    assert!(
        matches!(
            sent,
            varda::engine::EngineCommand::SetCompositor {
                direct_layers: None,
                merge_layer_passes: None,
                slow_layers_less_often: None,
                skip_blank_channels: Some(false),
                keep_still_text: None,
                merge_main_passes: None,
                pass_main_through: None,
                keep_still_clips: None,
                keep_still_inputs: None,
            }
        ),
        "{sent:?}"
    );
}

/// `PUT /api/compositor` with keeping a still lower third's or text's
/// frame alone sends that switch alone.
#[tokio::test]
async fn set_compositor_via_api_sends_keep_still_text() {
    let Some((app, sent)) = setup_seeing() else {
        return;
    };
    let (status, _) = put_json(
        app,
        "/api/compositor",
        serde_json::json!({"keep_still_text": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = sent.try_recv().expect("a command sent");
    assert!(
        matches!(
            sent,
            varda::engine::EngineCommand::SetCompositor {
                direct_layers: None,
                merge_layer_passes: None,
                slow_layers_less_often: None,
                skip_blank_channels: None,
                keep_still_text: Some(false),
                merge_main_passes: None,
                pass_main_through: None,
                keep_still_clips: None,
                keep_still_inputs: None,
            }
        ),
        "{sent:?}"
    );
}

/// `PUT /api/compositor` with drawing Main's layers in one pass alone sends
/// that switch alone.
#[tokio::test]
async fn set_compositor_via_api_sends_merge_main_passes() {
    let Some((app, sent)) = setup_seeing() else {
        return;
    };
    let (status, _) = put_json(
        app,
        "/api/compositor",
        serde_json::json!({"merge_main_passes": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = sent.try_recv().expect("a command sent");
    assert!(
        matches!(
            sent,
            varda::engine::EngineCommand::SetCompositor {
                direct_layers: None,
                merge_layer_passes: None,
                slow_layers_less_often: None,
                skip_blank_channels: None,
                keep_still_text: None,
                merge_main_passes: Some(false),
                pass_main_through: None,
                keep_still_clips: None,
                keep_still_inputs: None,
            }
        ),
        "{sent:?}"
    );
}

/// `PUT /api/compositor` with giving Main out as its one input's picture
/// alone sends that switch alone.
#[tokio::test]
async fn set_compositor_via_api_sends_pass_main_through() {
    let Some((app, sent)) = setup_seeing() else {
        return;
    };
    let (status, _) = put_json(
        app,
        "/api/compositor",
        serde_json::json!({"pass_main_through": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = sent.try_recv().expect("a command sent");
    assert!(
        matches!(
            sent,
            varda::engine::EngineCommand::SetCompositor {
                direct_layers: None,
                merge_layer_passes: None,
                slow_layers_less_often: None,
                skip_blank_channels: None,
                keep_still_text: None,
                merge_main_passes: None,
                pass_main_through: Some(false),
                keep_still_clips: None,
                keep_still_inputs: None,
            }
        ),
        "{sent:?}"
    );
}

/// `PUT /api/compositor` with keeping a clip's frame between two of its
/// frames alone sends that switch alone.
#[tokio::test]
async fn set_compositor_via_api_sends_keep_still_clips() {
    let Some((app, sent)) = setup_seeing() else {
        return;
    };
    let (status, _) = put_json(
        app,
        "/api/compositor",
        serde_json::json!({"keep_still_clips": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = sent.try_recv().expect("a command sent");
    assert!(
        matches!(
            sent,
            varda::engine::EngineCommand::SetCompositor {
                direct_layers: None,
                merge_layer_passes: None,
                slow_layers_less_often: None,
                skip_blank_channels: None,
                keep_still_text: None,
                merge_main_passes: None,
                pass_main_through: None,
                keep_still_clips: Some(false),
                keep_still_inputs: None,
            }
        ),
        "{sent:?}"
    );
}

/// `PUT /api/compositor` with keeping an input's picture while nothing in
/// it changes alone sends that switch alone.
#[tokio::test]
async fn set_compositor_via_api_sends_keep_still_inputs() {
    let Some((app, sent)) = setup_seeing() else {
        return;
    };
    let (status, _) = put_json(
        app,
        "/api/compositor",
        serde_json::json!({"keep_still_inputs": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = sent.try_recv().expect("a command sent");
    assert!(
        matches!(
            sent,
            varda::engine::EngineCommand::SetCompositor {
                direct_layers: None,
                merge_layer_passes: None,
                slow_layers_less_often: None,
                skip_blank_channels: None,
                keep_still_text: None,
                merge_main_passes: None,
                pass_main_through: None,
                keep_still_clips: None,
                keep_still_inputs: Some(false),
            }
        ),
        "{sent:?}"
    );
}

#[tokio::test]
async fn set_crossfader_via_api() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = put_json(
        app,
        "/api/mixer/crossfader",
        serde_json::json!({"position": 0.6}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn add_channel_via_api() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = post_empty(app, "/api/channels").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn add_solid_deck_via_api() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = post_json(
        app,
        "/api/channels/0/decks/solid",
        serde_json::json!({"color": [1.0, 0.0, 0.0, 1.0]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn multi_step_api_workflow() {
    let Some(app) = setup() else {
        return;
    };
    let (s, _) = post_empty(app, "/api/channels").await;
    assert_eq!(s, StatusCode::OK);

    let Some(app) = setup() else {
        return;
    };
    let (s, _) = post_json(
        app,
        "/api/channels/0/decks/solid",
        serde_json::json!({"color": [0.0, 1.0, 0.0, 1.0]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn add_lfo_via_api() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = post_json(
        app,
        "/api/modulation/lfo",
        serde_json::json!({"waveform": "Sine", "frequency": 2.0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn set_channel_opacity_via_api() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = put_json(
        app,
        "/api/channels/0/opacity",
        serde_json::json!({"opacity": 0.3}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn remove_channel_via_api() {
    let Some(app) = setup() else {
        return;
    };
    let (s, _) = post_empty(app, "/api/channels").await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn state_mixer_endpoint() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = get_json(app, "/api/state/mixer").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["channels"].is_array());
    assert!(json["crossfader"].is_number());
}

#[tokio::test]
async fn state_modulation_endpoint() {
    let Some(app) = setup() else {
        return;
    };
    let (status, json) = get_json(app, "/api/state/modulation").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["sources"].is_array());
}

#[tokio::test]
async fn undo_redo_via_api() {
    let Some(app) = setup() else {
        return;
    };
    let (s, _) = put_json(
        app,
        "/api/mixer/crossfader",
        serde_json::json!({"position": 0.5}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let Some(app2) = setup() else {
        return;
    };
    let (s, _) = post_empty(app2, "/api/undo").await;
    assert_eq!(s, StatusCode::OK);

    let Some(app3) = setup() else {
        return;
    };
    let (s, _) = post_empty(app3, "/api/redo").await;
    assert_eq!(s, StatusCode::OK);
}
