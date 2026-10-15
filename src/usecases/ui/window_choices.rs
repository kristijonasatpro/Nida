//! Choices made in Nida's windows that outlast a run, for every show — kept
//! in `windows.json` in its settings folder. So far, where the Input
//! Settings window puts its controls.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What has been chosen. A choice the file does not hold, or a damaged
/// file, gives the window's own default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowChoices {
    /// The Input Settings window's controls under its picture rather than
    /// beside it.
    pub settings_controls_below: bool,
}

/// The choices and the file they are kept in.
#[derive(Debug, Clone, Default)]
pub struct WindowChoiceStore {
    /// None: kept for this run only.
    path: Option<PathBuf>,
    choices: WindowChoices,
}

impl WindowChoiceStore {
    /// The choices in the user's file: `windows.json` in Nida's settings
    /// folder.
    pub fn open_default() -> Self {
        match dirs::config_dir() {
            Some(dir) => Self::open(dir.join("Nida").join("windows.json")),
            None => Self::default(),
        }
    }

    /// The choices kept at `path`, which changes are written to.
    pub fn open(path: PathBuf) -> Self {
        let choices = load(&path);
        Self {
            path: Some(path),
            choices,
        }
    }

    pub fn choices(&self) -> WindowChoices {
        self.choices
    }

    /// Keep `choices`, writing the file when they change.
    ///
    /// # Errors
    ///
    /// When the file cannot be written: the choices hold for this run all
    /// the same.
    pub fn set(&mut self, choices: WindowChoices) -> Result<(), String> {
        self.set_with(choices, |path, text| {
            store(path, &text).map_err(|e| format!("{}: {e}", not_kept(path)))
        })
    }

    /// As [`Self::set`], the file handed to `write` — the show's writer, so
    /// the outputs do not wait for the disk (see `persistence::writer`).
    ///
    /// # Errors
    ///
    /// When `write` says the file cannot be written: the choices hold for
    /// this run all the same.
    pub fn set_with(
        &mut self,
        choices: WindowChoices,
        write: impl FnOnce(&Path, String) -> Result<(), String>,
    ) -> Result<(), String> {
        if choices == self.choices {
            return Ok(());
        }
        self.choices = choices;
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = serde_json::to_string_pretty(&choices)
            .map_err(|e| format!("{}: {e}", not_kept(path)))?;
        write(path, text)
    }
}

/// How a choice that could not be written to `path` is put to the
/// operator, before why.
pub fn not_kept(path: &Path) -> String {
    format!("Could not keep the window's choice in {}", path.display())
}

fn load(path: &Path) -> WindowChoices {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return WindowChoices::default(),
        Err(e) => {
            log::warn!(
                "Could not read the windows' choices {}: {e}",
                path.display()
            );
            return WindowChoices::default();
        }
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        log::warn!("The windows' choices {} are damaged: {e}", path.display());
        WindowChoices::default()
    })
}

fn store(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::persistence::atomic_write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Handed to the show's writer, the file is what a save to it would
    /// write; the choice holds at once, and an unchanged choice writes
    /// nothing.
    #[test]
    fn a_choice_hands_its_file_to_be_written_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("windows.json");
        let mut store = WindowChoiceStore::open(path.clone());
        let below = WindowChoices {
            settings_controls_below: true,
        };
        let mut handed = Vec::new();
        store
            .set_with(below, |to, text| {
                handed.push((to.to_path_buf(), text));
                Ok(())
            })
            .unwrap();
        store
            .set_with(below, |_, _| panic!("unchanged, nothing to write"))
            .unwrap();
        assert_eq!(store.choices(), below);
        assert_eq!(handed.len(), 1);
        assert_eq!(handed[0].0, path);
        std::fs::write(&path, &handed[0].1).unwrap();
        assert_eq!(WindowChoiceStore::open(path).choices(), below);
    }

    /// Chosen in one run, there in the next; nothing chosen yet, or the file
    /// damaged, the window's own default.
    #[test]
    fn a_choice_is_there_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Nida").join("windows.json");
        assert_eq!(
            WindowChoiceStore::open(path.clone()).choices(),
            WindowChoices::default()
        );
        let mut store = WindowChoiceStore::open(path.clone());
        let below = WindowChoices {
            settings_controls_below: true,
        };
        store.set(below).unwrap();
        assert_eq!(WindowChoiceStore::open(path.clone()).choices(), below);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            WindowChoiceStore::open(path).choices(),
            WindowChoices::default()
        );
    }
}
