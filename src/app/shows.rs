//! Shows, as vMix's presets: the folder a show is saved in, and moving
//! between shows — saving one under a new name, starting an empty one,
//! opening another.
//!
//! A show is a folder holding `.varda/`. Nida saves the show it has open
//! whenever it closes, so leaving a show always saves it first, and a new
//! show gets a folder of its own at once: an empty show can never be saved
//! over the one before it.
//!
//! A new show starts fresh, as a show never set up: no outputs or surfaces,
//! the one bus, the mixer's Master with no inputs, the stream, recording and
//! timecode settings as new, two empty inputs and nothing on Main. It had
//! kept the room — the screens, the buses, the sound inputs — and the user
//! found the last show's microphone and screens still on in it: "new should
//! reset all to fresh" (2026-10-04). What belongs to the desk rather than a
//! show comes along: MIDI mappings, keys, OSC and controller profiles, the
//! presets, LUTs and shaders; and the size, frame rate and screen-rate
//! setting stay as they are.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use super::{VardaApp, WorkspaceLoad};
use crate::persistence::Workspace;
use crate::usecases::ui::UILayoutState;

/// Where shows go unless put somewhere else: `~/Documents/Nida Shows`.
pub fn shows_dir() -> PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Nida Shows")
}

/// The first "Untitled" folder in `dir` not already there: "Untitled",
/// "Untitled 2", …
pub fn untitled_in(dir: &Path) -> PathBuf {
    let name = |n: u32| {
        dir.join(if n == 1 {
            "Untitled".to_string()
        } else {
            format!("Untitled {n}")
        })
    };
    (1..10_000)
        .map(name)
        .find(|p| !p.exists())
        .unwrap_or_else(|| name(std::process::id()))
}

/// Whether `folder` holds a show.
pub fn is_show(folder: &Path) -> bool {
    Workspace::new(folder.to_path_buf()).exists()
}

/// The name of the show in `folder`: the folder's.
pub fn name_of(folder: &Path) -> String {
    folder
        .file_name()
        .map_or_else(|| "Show".to_string(), |n| n.to_string_lossy().into_owned())
}

/// A Save As on its way: the show stays in the folder it was saved from
/// until the copy has landed (see `persistence::writer`).
pub(crate) struct SavingAs {
    /// What the writer's word that it landed carries.
    id: u64,
    /// The show's folder when it was asked for, and the one it goes to.
    from: PathBuf,
    to: PathBuf,
    /// Whether to say the show is saved as `to` once it is.
    announce: bool,
    reply: Option<super::Reply>,
}

impl SavingAs {
    /// Whether the writer's word that `id` landed is about this one.
    pub(crate) fn is(&self, id: u64) -> bool {
        self.id == id
    }
}

/// Whether a show can be saved into `folder`: it is not there yet, is
/// empty, or already holds a show (to be replaced, as asked).
fn can_hold_a_show(folder: &Path) -> anyhow::Result<()> {
    if !folder.exists() || is_show(folder) {
        return Ok(());
    }
    let empty = std::fs::read_dir(folder)
        .with_context(|| format!("Cannot read {}", folder.display()))?
        .filter_map(Result::ok)
        .all(|entry| entry.file_name().to_string_lossy().starts_with('.'));
    if empty {
        Ok(())
    } else {
        anyhow::bail!(
            "{} already holds other files — pick a new name or an empty folder",
            folder.display()
        )
    }
}

/// Whether `a` and `b` are one folder, whatever they are called.
fn same_folder(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// Copy the files and folders in `from` into `to`, all but those named in
/// `skip`, making `to` if need be. A `from` not there — a show never saved
/// — is nothing to copy; one that cannot be read is an error, not a copy
/// with files missing.
fn copy_dir(from: &Path, to: &Path, skip: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    let entries = match std::fs::read_dir(from) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let (source, target) = (entry.path(), to.join(&name));
        if entry.file_type()?.is_dir() {
            copy_dir(&source, &target, &[])?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

impl VardaApp {
    /// The folder the open show is saved in.
    pub fn show_folder(&self) -> &Path {
        self.session.workspace.root()
    }

    /// The open show's name: its folder's.
    pub fn show_name(&self) -> String {
        name_of(self.show_folder())
    }

    /// Why the open show cannot be left now, if it cannot: leaving it would
    /// stop what is going out.
    pub fn show_switch_refused(&self) -> Option<&'static str> {
        if self.stream_status().live {
            Some("Stop the stream before opening another show")
        } else if self.record_status().recording {
            Some("Stop recording before opening another show")
        } else {
            None
        }
    }

    /// Save the show into `folder` — its presets, LUTs and shaders too — and
    /// carry on there. The show it was saved as before stays as it was.
    ///
    /// The copy is made on the writer's thread, so the outputs do not wait
    /// for the disk (see `persistence::writer`); the show moves to `folder`
    /// once it has landed, and says so then — or, if it did not land, stays
    /// where it was and says why.
    ///
    /// # Errors
    ///
    /// Returns an error if a Save As is still on its way.
    pub fn save_show_as(&mut self, folder: &Path, layout: &UILayoutState) -> anyhow::Result<()> {
        self.save_as(folder, layout, true, None)
    }

    /// `EngineCommand::SaveShowAs`, with the layout the engine saw last:
    /// `reply` is answered once the show has moved — or why it has not.
    pub(crate) fn save_show_as_for_command(&mut self, folder: &Path, reply: Option<super::Reply>) {
        let layout = self.session.last_layout.clone();
        let _ = self.save_as(folder, &layout, false, reply);
    }

    /// Start a Save As: `announce` says the show is saved as `folder` once
    /// it has moved; `reply` is answered then.
    pub(crate) fn save_as(
        &mut self,
        folder: &Path,
        layout: &UILayoutState,
        announce: bool,
        reply: Option<super::Reply>,
    ) -> anyhow::Result<()> {
        // One that has landed is taken in first: it may have moved the show.
        self.report_saves();
        if let Some(on_way) = &self.saving_as {
            let e = anyhow::anyhow!(
                "it is still being saved as {} — try again once it is",
                on_way.to.display()
            );
            self.session
                .notifications
                .error(format!("Show not saved as {}: {e:#}", folder.display()));
            if let Some(reply) = reply {
                let _ = reply.send(crate::engine::CommandResult::Err {
                    code: crate::engine::ErrorCode::Unavailable,
                    message: e.to_string(),
                });
            }
            return Err(e);
        }
        if folder == self.show_folder() {
            // The folder it is in: a save as any other.
            let then = reply.map(super::workspace::answer_when_landed);
            let save = self.show_save(layout, announce, then);
            self.show_writer.submit(save);
            return Ok(());
        }
        let target = Workspace::new(folder.to_path_buf());
        let save = self.show_save_into(&target, layout, false, None);
        let from = self.session.workspace.varda_dir();
        let copy: crate::persistence::writer::CopyShow = {
            let (from, to, folder) = (from.clone(), target.varda_dir(), folder.to_path_buf());
            Box::new(move || {
                can_hold_a_show(&folder)
                    .and_then(|()| {
                        // The show's own folder by another name — a link to
                        // it, or its name in other letters on a Mac's disk:
                        // nothing to copy, and each file copied onto itself
                        // would be emptied.
                        if same_folder(&from, &to) {
                            return Ok(());
                        }
                        copy_dir(&from, &to, &[])
                            .with_context(|| format!("Cannot write {}", folder.display()))
                    })
                    .map_err(|e| format!("{e:#}"))
            })
        };
        let id = self
            .show_writer
            .submit_move(crate::persistence::writer::ShowMove { from, copy, save });
        self.saving_as = Some(SavingAs {
            id,
            from: self.show_folder().to_path_buf(),
            to: folder.to_path_buf(),
            announce,
            reply,
        });
        Ok(())
    }

    /// A Save As has landed: the show carries on in its new folder — or,
    /// if it did not land whole, stays where it was and says why.
    pub(crate) fn saved_as(&mut self, landed: &crate::persistence::writer::Landed) {
        let Some(saving) = self.saving_as.take() else {
            return;
        };
        let answer = if landed.errors.is_empty() {
            // Only from the show it was saved from: a show opened since
            // stays open.
            if self.show_folder() == saving.from {
                let to = Workspace::new(saving.to.clone());
                // The cameras' LUTs in the show are the same files in the
                // copy: kept, not read again.
                let moves =
                    self.camera_lut_moves(&self.session.workspace.luts_dir(), &to.luts_dir());
                // Its shaders the copy's, as an opened show's.
                self.enter_show(to);
                self.camera_manager.relocate_luts(&moves);
            }
            // The engine names the new folder itself from now.
            self.show_writer.settled();
            log::info!("Saved the show as {}", saving.to.display());
            if saving.announce {
                self.session
                    .notifications
                    .info(format!("💾 Saved as “{}”", name_of(&saving.to)));
            }
            crate::engine::CommandResult::Ok
        } else {
            let message = format!(
                "Show not saved as {}: {}",
                saving.to.display(),
                landed.errors.join("; ")
            );
            self.session.notifications.error(message.clone());
            crate::engine::CommandResult::Err {
                code: crate::engine::ErrorCode::InvalidInput,
                message,
            }
        };
        if let Some(reply) = saving.reply {
            let _ = reply.send(answer);
        }
    }

    /// Save the show, then start an empty one in `folder`, keeping the room
    /// setup (see the module notes).
    ///
    /// # Errors
    ///
    /// Returns an error if the stream or recording is running, the show
    /// cannot be saved first, or `folder` cannot hold the new one; the show
    /// open stays open then.
    pub fn new_show(&mut self, folder: &Path, layout: &UILayoutState) -> anyhow::Result<()> {
        let result = self.try_new_show(folder, layout);
        if let Err(e) = &result {
            self.session
                .notifications
                .error(format!("No new show: {e:#}"));
        }
        result
    }

    fn try_new_show(&mut self, folder: &Path, layout: &UILayoutState) -> anyhow::Result<()> {
        if let Some(why) = self.show_switch_refused() {
            anyhow::bail!(why);
        }
        self.save_workspace(layout)
            .context("the open show could not be saved first")?;
        can_hold_a_show(folder)?;
        let mut empty = self.empty_scene();
        empty.free_control_screen = !self.control_screen_rate();
        empty.free_night_shift = !self.night_shift_off();
        // The new show is written first, the desk's setup copied beside a
        // fresh stage and an empty scene; only then is it opened, as any show
        // is, so a show that cannot be written leaves the one open as it is.
        let target = Workspace::new(folder.to_path_buf());
        copy_dir(
            &self.session.workspace.varda_dir(),
            &target.varda_dir(),
            &["scene.json", "stage.json"],
        )
        .map_err(anyhow::Error::from)
        .and_then(|()| crate::persistence::StagePrefs::default().save(target.stage_path()))
        .and_then(|()| empty.save(target.scene_path()))
        .with_context(|| format!("Cannot write {}", folder.display()))?;
        self.enter_show(target);
        let opened = self.load_workspace();
        if let Some(why) = opened.error_message() {
            log::warn!("New show opened in part: {why}");
        }
        self.session.history.clear();
        self.save_workspace(layout)?;
        log::info!("New show in {}", folder.display());
        Ok(())
    }

    /// Save the show, then open the one in `folder`. The layout it was saved
    /// with comes back in the result, for the UI to put in place.
    ///
    /// # Errors
    ///
    /// Returns an error if the stream or recording is running, `folder`
    /// holds no show, or the show open cannot be saved first; the show open
    /// stays open then. A show that opens only in part is open, and says so
    /// in the result.
    pub fn open_show(
        &mut self,
        folder: &Path,
        layout: &UILayoutState,
    ) -> anyhow::Result<WorkspaceLoad> {
        let result = self.try_open_show(folder, layout);
        if let Err(e) = &result {
            self.session
                .notifications
                .error(format!("Show not opened: {e:#}"));
        }
        result
    }

    fn try_open_show(
        &mut self,
        folder: &Path,
        layout: &UILayoutState,
    ) -> anyhow::Result<WorkspaceLoad> {
        if let Some(why) = self.show_switch_refused() {
            anyhow::bail!(why);
        }
        if !is_show(folder) {
            anyhow::bail!("{} holds no Nida show", folder.display());
        }
        let workspace = Workspace::new(folder.to_path_buf());
        // A show whose scene or stage cannot be read is not opened: the show
        // open now stays. Opened, its folder became the show's while Nida
        // still held the show before, and the next save — Quit saves — wrote
        // that over the files that would not load (a show saved by a later
        // Nida, a damaged file).
        let scene = if workspace.has_scene() {
            let scene = crate::scene::SceneConfig::load(workspace.scene_path());
            Some(scene.context("its scene cannot be read")?)
        } else {
            None
        };
        if workspace.has_stage() {
            crate::persistence::StagePrefs::load(workspace.stage_path())
                .context("its stage cannot be read")?;
        }
        self.save_workspace(layout)
            .context("the open show could not be saved first")?;
        // A show made at another size: everything is resized to it first,
        // as a size picked by hand would be.
        if let Some(scene) = &scene {
            if let (Some(w), Some(h)) = (scene.render_width, scene.render_height) {
                if w > 0 && h > 0 && (w, h) != (self.render_width, self.render_height) {
                    self.set_render_resolution(w, h);
                }
            }
        }
        self.enter_show(workspace);
        let loaded = self.load_workspace();
        log::info!("Opened show {}", folder.display());
        Ok(loaded)
    }

    /// Start in the show in `folder` rather than the one Nida was started
    /// with, before the show is loaded: the one open last time.
    pub fn start_in_show(&mut self, folder: &Path) {
        self.enter_show(Workspace::new(folder.to_path_buf()));
    }

    /// Make `workspace` the show's folder, its own shaders in the library
    /// in place of the show's before (see `ShaderRegistry::set_show_library`):
    /// left there, a shader of the show before stood in for this one's of the
    /// same name — added to it, or for a file gone (see
    /// `persistence::saved_shader`).
    fn enter_show(&mut self, workspace: Workspace) {
        let shaders = workspace.shaders_dir();
        if self.registry.set_show_library(&shaders) || shaders.is_dir() {
            if let Err(e) = self.registry.scan() {
                log::warn!("Show shaders not read: {e}");
            }
        }
        self.session.workspace = workspace;
    }

    /// The scene a new show starts with: two empty inputs, and Main as it is
    /// set up — its mode and fade time — with nothing on it. The size stays
    /// as it is.
    fn empty_scene(&self) -> crate::scene::SceneConfig {
        let mut scene = crate::persistence::snapshot_scene(
            &self.mixer,
            Some(&self.transport_config()),
            self.render_width,
            self.render_height,
        );
        scene.render_resolution_auto = self.render_auto;
        scene.frames_in_flight = (self.frames_in_flight != 1).then_some(self.frames_in_flight);
        let empty_channel = |name: String| crate::scene::ChannelConfig {
            uuid: crate::deck::generate_short_uuid(),
            name,
            opacity: 1.0,
            blend_mode: crate::scene::BlendModeConfig::default(),
            overlay: false,
            color: None,
            decks: Vec::new(),
            effects: Vec::new(),
            modulation: Vec::new(),
        };
        scene.channels = vec![empty_channel("Ch 0".into()), empty_channel("Ch 1".into())];
        scene.crossfader = 0.0;
        scene.active_transition = None;
        scene.master_effects.clear();
        scene.modulation = crate::modulation::ModulationEngine::default();
        scene.macros = crate::macros::MacroBank::default();
        scene.transition_sequences.clear();
        scene.arrangement = None;
        if let Some(main) = &mut scene.main {
            main.program = crate::mixer::MainSource::Black;
            main.overlays_on.clear();
        }
        scene
    }
}

// ── Recent shows ────────────────────────────────────────────────────

/// How many shows the Recent list keeps.
const RECENT: usize = 8;

/// Where the Recent list is kept: `recent-shows.json` in Nida's settings
/// folder.
pub fn recent_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Nida").join("recent-shows.json"))
}

/// The shows opened lately, the latest first; those gone since are left out.
pub fn recent_shows() -> Vec<PathBuf> {
    recent_file().map_or_else(Vec::new, |file| recent_shows_in(&file))
}

/// The shows in the Recent list kept in `file`, those gone left out.
pub fn recent_shows_in(file: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<PathBuf>>(&text).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| is_show(p))
        .collect()
}

/// `list` with `show` at its head, once, and no longer than [`RECENT`].
pub fn with_recent(list: Vec<PathBuf>, show: &Path) -> Vec<PathBuf> {
    std::iter::once(show.to_path_buf())
        .chain(list.into_iter().filter(|p| p != show))
        .take(RECENT)
        .collect()
}

impl VardaApp {
    /// Write a file of Nida's own — the saved layouts, a window's choices —
    /// in the background (see `persistence::writer`): the outputs need not
    /// wait for it. `said` is said once it lands; `failed` puts a failure to
    /// the operator, before why; `then` is told what came of it.
    pub fn keep_file(
        &mut self,
        label: &str,
        path: &Path,
        content: String,
        said: Option<String>,
        failed: String,
        then: Option<crate::persistence::writer::Then>,
    ) {
        self.show_writer
            .submit_file(crate::persistence::writer::FileSave {
                label: label.into(),
                path: path.to_path_buf(),
                content,
                said,
                failed: Some(failed),
                presets: false,
                then,
            });
    }

    /// Keep `list` as the Recent list in `path`, written in the background
    /// (see `persistence::writer`): a file the outputs need not wait for.
    /// Only the log hears if it cannot be written.
    pub fn keep_recent_shows(&mut self, list: &[PathBuf], path: &Path) {
        let path = path.to_path_buf();
        let content = match serde_json::to_string_pretty(list) {
            Ok(text) => text,
            Err(e) => {
                log::warn!("Recent shows not kept: {e}");
                return;
            }
        };
        self.show_writer
            .submit_file(crate::persistence::writer::FileSave {
                label: "the Recent list".into(),
                path,
                content,
                said: None,
                failed: None,
                presets: false,
                then: None,
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untitled_takes_the_first_free_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(untitled_in(dir.path()), dir.path().join("Untitled"));
        std::fs::create_dir(dir.path().join("Untitled")).unwrap();
        assert_eq!(untitled_in(dir.path()), dir.path().join("Untitled 2"));
    }

    #[test]
    fn a_show_goes_into_a_new_empty_or_show_folder_only() {
        let dir = tempfile::tempdir().unwrap();
        assert!(can_hold_a_show(&dir.path().join("new")).is_ok());
        assert!(can_hold_a_show(dir.path()).is_ok(), "empty");
        std::fs::write(dir.path().join("taxes.pdf"), b"").unwrap();
        assert!(can_hold_a_show(dir.path()).is_err(), "someone's files");
        std::fs::create_dir(dir.path().join(".varda")).unwrap();
        assert!(can_hold_a_show(dir.path()).is_ok(), "a show, replaced");
    }

    /// A folder that cannot be read is not copied as if it were empty: the
    /// copy says so. One not there is nothing to copy.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_read_is_not_copied_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        std::fs::create_dir_all(from.join("luts")).unwrap();
        std::fs::write(from.join("luts/look.cube"), "LUT").unwrap();
        let mode = |bits| std::fs::Permissions::from_mode(bits);
        std::fs::set_permissions(from.join("luts"), mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(from.join("luts")).is_err();
        let copied = copy_dir(&from, &to, &[]);
        std::fs::set_permissions(from.join("luts"), mode(0o755)).unwrap();
        if unreadable {
            assert!(copied.is_err(), "copied as if empty");
        }
        assert!(copy_dir(&dir.path().join("never"), &to, &[]).is_ok());
    }

    #[test]
    fn the_recent_list_puts_the_latest_first_once() {
        let (a, b) = (PathBuf::from("/a"), PathBuf::from("/b"));
        let list = with_recent(vec![a.clone(), b.clone()], &b);
        assert_eq!(list, vec![b.clone(), a.clone()]);
        let long: Vec<PathBuf> = (0..20).map(|i| PathBuf::from(format!("/{i}"))).collect();
        assert_eq!(with_recent(long, &a).len(), RECENT);
    }
}
