//! The Show menu's work: file dialogs on their own threads, then saving,
//! opening or starting a show (see [`crate::app::shows`]), the layout that
//! comes with an opened show put in place, and the Recent list kept.

use std::path::PathBuf;
use std::sync::mpsc;

use crate::app::shows;
use crate::usecases::ui::ShowAction;

/// What a dialog open on its thread is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShowPick {
    Open,
    SaveAs,
}

/// A show dialog open on its thread, and what it will answer.
pub(super) type PendingPick = (ShowPick, mpsc::Receiver<Option<PathBuf>>);

impl super::UIRunner {
    /// Do what the Show menu asked.
    pub(super) fn start_show_action(&mut self, action: ShowAction) {
        let Some(varda) = self.varda.as_mut() else {
            return;
        };
        match action {
            ShowAction::New => {
                let dir = shows::shows_dir();
                let folder = shows::untitled_in(&dir);
                if varda.new_show(&folder, &self.layout).is_ok() {
                    // The selection named channels that are gone.
                    self.layout.selected_deck = None;
                    self.layout.selected_channel = None;
                    varda.notify_info(format!(
                        "New show “{}” — in {}",
                        varda.show_name(),
                        dir.display()
                    ));
                    self.show_changed();
                }
            }
            ShowAction::Open | ShowAction::SaveAs if self.show_pick.is_some() => {}
            ShowAction::Open => {
                let start = shows::shows_dir();
                self.show_pick = Some((
                    ShowPick::Open,
                    ask(move || {
                        rfd::FileDialog::new()
                            .set_title("Open a show — pick its folder")
                            .set_directory(start)
                            .pick_folder()
                    }),
                ));
            }
            ShowAction::SaveAs => {
                let start = shows::shows_dir();
                let name = format!("{} copy", varda.show_name());
                self.show_pick = Some((
                    ShowPick::SaveAs,
                    ask(move || {
                        // Made here, not on the engine: the dialog opens in it.
                        let _ = std::fs::create_dir_all(&start);
                        rfd::FileDialog::new()
                            .set_title("Save the show as")
                            .set_directory(start)
                            .set_file_name(name)
                            .save_file()
                    }),
                ));
            }
            ShowAction::OpenRecent(folder) => self.open_show(&folder),
            ShowAction::Reveal => super::reveal_in_file_manager(varda.show_folder()),
        }
    }

    /// Take a show dialog's answer, once it has one.
    pub(super) fn poll_show_pick(&mut self) {
        let Some((pick, rx)) = &self.show_pick else {
            return;
        };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        let pick = *pick;
        self.show_pick = None;
        let Some(folder) = answer else {
            return;
        };
        match pick {
            ShowPick::Open => self.open_show(&folder),
            // Copied in the background: the show moves, and says so, once
            // the copy has landed (see `show_moved`).
            ShowPick::SaveAs => {
                if let Some(varda) = self.varda.as_mut() {
                    let _ = varda.save_show_as(&folder, &self.layout);
                }
            }
        }
    }

    pub(super) fn open_show(&mut self, folder: &std::path::Path) {
        let Some(varda) = self.varda.as_mut() else {
            return;
        };
        let Ok(loaded) = varda.open_show(folder, &self.layout) else {
            return;
        };
        let whole = loaded.is_ok();
        if let Some(layout) = loaded.layout {
            self.layout = layout;
            self.saved_layouts.reconnect(&mut self.layout.screen);
        } else {
            self.layout.selected_deck = None;
            self.layout.selected_channel = None;
        }
        if whole {
            varda.notify_info(format!("Opened “{}”", varda.show_name()));
        }
        self.show_changed();
    }

    /// The show open is another: keep it at the head of the Recent list,
    /// and name the window after it.
    pub(super) fn show_changed(&mut self) {
        let Some(varda) = self.varda.as_mut() else {
            return;
        };
        let folder = varda.show_folder().to_path_buf();
        let recent: Vec<PathBuf> = shows::with_recent(self.recent_shows.to_vec(), &folder)
            .into_iter()
            .filter(|show| shows::is_show(show))
            .collect();
        // Written in the background: the list is the one kept here.
        if let Some(file) = &self.recent_file {
            varda.keep_recent_shows(&recent, file);
        }
        self.recent_shows = std::sync::Arc::new(recent);
        if let Some(window) = self.window {
            window.set_title(&format!("Nida — {}", varda.show_name()));
        }
        self.named_after = folder;
    }

    /// The show has moved without the Show menu knowing — a Save As whose
    /// copy has landed, a show the API opened: as for any other.
    pub(super) fn show_moved(&mut self) {
        if self
            .varda
            .as_ref()
            .is_some_and(|varda| varda.show_folder() != self.named_after)
        {
            self.show_changed();
        }
    }
}

/// Run a dialog on a thread of its own; its answer comes on the channel.
fn ask(
    dialog: impl FnOnce() -> Option<PathBuf> + Send + 'static,
) -> mpsc::Receiver<Option<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("show-dialog".into())
        .spawn(move || {
            let _ = tx.send(dialog());
        });
    if let Err(e) = spawned {
        log::warn!("Show dialog not opened: {e}");
    }
    rx
}
