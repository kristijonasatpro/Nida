//! Clips and pictures asked for over the bus — the API — are made on a
//! thread of their own and put in their channel a frame or more later: the
//! show's thread never opens the file (a 4K clip held it up 224 ms, a clip
//! on a disk asleep for as long as the disk took). The reply waits for them.
//!
//! Test clips are made with the `ffmpeg` command-line tool; without it these
//! tests skip.

use std::path::{Path, PathBuf};

use varda::app::VardaApp;
use varda::engine::{CommandResult, EngineCommand, ErrorCode};

mod common;

/// A two-second clip of `colour`, or `None` without the ffmpeg tool.
fn clip(dir: &Path, colour: &str) -> Option<PathBuf> {
    let path = dir.join(format!("{colour}.mp4"));
    let made = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c={colour}:s=64x36:r=25:d=2"))
        .args(["-c:v", "mpeg4"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    if made {
        Some(path)
    } else {
        eprintln!("ffmpeg not available — skipping");
        None
    }
}

fn app() -> Option<(VardaApp, String)> {
    let gpu = common::headless_gpu()?;
    let app = VardaApp::new(gpu, &varda::testing::headless_config()).expect("VardaApp::new");
    let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
    Some((app, channel))
}

/// Send `cmd` over the bus; its reply to come.
fn ask(app: &VardaApp, cmd: EngineCommand) -> tokio::sync::oneshot::Receiver<CommandResult> {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    app.command_sender().send((cmd, Some(reply_tx))).unwrap();
    reply_rx
}

fn decks(app: &VardaApp) -> Vec<String> {
    app.build_engine_state().mixer.channels[0]
        .decks
        .iter()
        .map(|d| d.uuid.clone())
        .collect()
}

fn add_video(channel: &str, path: &Path) -> EngineCommand {
    EngineCommand::AddVideoDeck {
        channel_uuid: channel.to_string(),
        path: path.to_path_buf(),
    }
}

/// The frame a clip is asked for goes on without it: the file is opened on
/// a thread of its own, and the clip put in, the reply answered, after.
#[test]
fn a_clip_asked_for_is_never_opened_on_the_shows_thread() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "red") else {
        return;
    };
    let Some((mut app, channel)) = app() else {
        return;
    };
    let before = decks(&app);
    let mut reply = ask(&app, add_video(&channel, &path));
    app.process_commands();
    assert!(
        matches!(
            reply.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "not answered in the frame it was asked in"
    );
    assert_eq!(decks(&app), before, "not put in the frame it was asked in");
    let CommandResult::OkWithId { uuid } = varda::testing::reply_of(&mut app, reply) else {
        panic!("added");
    };
    assert_eq!(decks(&app).last(), Some(&uuid), "put in its channel, last");
    // One undo step: the clip goes again.
    let asked = ask(&app, EngineCommand::Undo);
    let undone = varda::testing::reply_of(&mut app, asked);
    assert!(matches!(undone, CommandResult::Ok), "{undone:?}");
    assert_eq!(decks(&app), before, "undone");
}

/// A 4K clip of a second, slow to open, or `None` without the ffmpeg tool.
fn slow_clip(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("slow.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=s=3840x2160:r=25:d=1")
        .args(["-c:v", "libx264", "-preset", "ultrafast"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    made.then_some(path)
}

/// Asked for one after another, they are answered in that order however
/// long each takes: a broken file asked for after a 4K clip, though refused
/// at once, waits for the clip to be put in.
#[test]
fn they_are_put_in_and_answered_in_the_order_asked() {
    let dir = tempfile::tempdir().unwrap();
    let Some(slow) = slow_clip(dir.path()) else {
        eprintln!("ffmpeg with libx264 not available — skipping");
        return;
    };
    let broken = dir.path().join("broken.mp4");
    std::fs::write(&broken, b"not a video").unwrap();
    let Some((mut app, channel)) = app() else {
        return;
    };
    let mut first = ask(&app, add_video(&channel, &slow));
    let mut second = ask(&app, add_video(&channel, &broken));
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let (mut first_at, mut second_at) = (None, None);
    let mut pass = 0;
    while first_at.is_none() || second_at.is_none() {
        assert!(std::time::Instant::now() < until, "both answered");
        app.process_commands();
        pass += 1;
        if first_at.is_none() && first.try_recv().is_ok() {
            first_at = Some(pass);
        }
        if second_at.is_none() && second.try_recv().is_ok() {
            second_at = Some(pass);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        first_at <= second_at,
        "in the order asked: {first_at:?} {second_at:?}"
    );
}

/// Nida quitting answers what is still being made at once: a request left
/// waiting would hold its quitting up.
#[test]
fn quitting_answers_what_is_still_being_made() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "red") else {
        return;
    };
    let Some((mut app, channel)) = app() else {
        return;
    };
    let mut reply = ask(&app, add_video(&channel, &path));
    app.process_commands();
    app.refuse_commands();
    assert!(
        matches!(
            reply.try_recv(),
            Ok(CommandResult::Err {
                code: ErrorCode::Unavailable,
                ..
            })
        ),
        "answered as Nida quits"
    );
}

/// A file that will not open is said, as before — and nothing is added.
#[test]
fn a_file_that_will_not_open_is_said() {
    let dir = tempfile::tempdir().unwrap();
    let broken = dir.path().join("broken.mp4");
    std::fs::write(&broken, b"not a video").unwrap();
    let Some((mut app, channel)) = app() else {
        return;
    };
    let before = decks(&app);
    let asked = ask(&app, add_video(&channel, &broken));
    let reply = varda::testing::reply_of(&mut app, asked);
    assert!(
        matches!(reply, CommandResult::Err { .. }),
        "refused: {reply:?}"
    );
    assert_eq!(decks(&app), before);
    // A channel not there, at once.
    let asked = ask(&app, add_video("no-such-channel", &broken));
    let missing = varda::testing::reply_of(&mut app, asked);
    assert!(
        matches!(
            missing,
            CommandResult::Err {
                code: ErrorCode::NotFound,
                ..
            }
        ),
        "{missing:?}"
    );
}

/// A show opened over the one a clip was asked of: the clip is not put in
/// it.
#[test]
fn a_clip_asked_of_a_show_since_replaced_is_not_put_in() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "red") else {
        return;
    };
    let Some((mut app, channel)) = app() else {
        return;
    };
    // A show saved, to open over this one.
    let asked = ask(&app, EngineCommand::SaveWorkspace);
    let saved = varda::testing::reply_of(&mut app, asked);
    assert!(matches!(saved, CommandResult::Ok), "{saved:?}");
    let reply = ask(&app, add_video(&channel, &path));
    app.process_commands();
    let _ = app.load_workspace();
    let after = varda::testing::reply_of(&mut app, reply);
    assert!(
        matches!(
            after,
            CommandResult::Err {
                code: ErrorCode::Unavailable,
                ..
            }
        ),
        "{after:?}"
    );
}
