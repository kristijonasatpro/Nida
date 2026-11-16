//! Configurable keyboard shortcut system.
//!
//! Mirrors the MIDI mapping architecture: a data-driven keymap with learn mode,
//! persistence to `.varda/keymap.json`, and default bindings that can be overridden.
//! This module is framework-free. egui conversion lives in `usecases::ui::keyboard`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Key names persisted in `.varda/keymap.json` (`egui::Key` Debug spellings).
pub const SUPPORTED_KEY_NAMES: &[&str] = &[
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "Num0",
    "Num1",
    "Num2",
    "Num3",
    "Num4",
    "Num5",
    "Num6",
    "Num7",
    "Num8",
    "Num9",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Insert",
    "Delete",
    "Backspace",
    "Enter",
    "Tab",
    "Space",
    "Escape",
    "Minus",
    "Plus",
];

/// Whether `name` is a persistable key identity.
pub fn is_supported_key_name(name: &str) -> bool {
    SUPPORTED_KEY_NAMES.contains(&name)
}

/// A key combination: a key + modifier state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyCombo {
    pub key: String,
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
}

/// What a key binding targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyTarget {
    /// A discrete application action.
    Action(ActionId),
    /// A `param_path` (same addressing as MIDI).
    ParamPath(String),
}

/// All discrete actions that can be keyboard-mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActionId {
    Undo,
    Redo,
    Save,
    ToggleLibrary,
    ToggleStageEditor,
    ToolSelect,
    ToolRectangle,
    ToolPolygon,
    ToolCircle,
    DuplicateSurface,
    FlipHorizontal,
    FlipVertical,
    DeleteSurface,
    ClearDrawing,
    CombineSurfaces,
    ToggleMidiLearn,
    ToggleKeyboardLearn,
    /// Copy, paste, and duplicate the current selection: the deck, channel, or
    /// effect the bottom bar is already following. See /spec/clipboard.md.
    Copy,
    Paste,
    Duplicate,
    /// Main mode: put the selected channel on Main at once (an overlay is
    /// switched on or off). Space by default.
    CutToMain,
    /// Main mode: fade the selected channel onto Main. Enter by default.
    FadeToMain,
    /// Take the rundown's next step with a fade: GO. No key by default —
    /// one is learned on the Rundown panel's GO button.
    RundownGo,
    /// A PowerPoint input's show on air (else the one moved last): the next
    /// step, `PageDown` by default — what a presentation clicker sends.
    PresentationNext,
    /// Back a step: `PageUp` by default.
    PresentationPrevious,
    /// Black the screen out, and back: B by default, a clicker's
    /// black-screen button.
    PresentationBlack,
}

impl ActionId {
    /// Whether every public Nida (0.9 on) reads it: a show keeps a binding to
    /// any other apart (see [`KeymapConfig::split`]), for an older Nida on
    /// another Mac — the same show in iCloud — or kept to go back to. An
    /// action added later answers `false` here.
    #[must_use]
    pub fn read_by_older(self) -> bool {
        match self {
            Self::Undo
            | Self::Redo
            | Self::Save
            | Self::ToggleLibrary
            | Self::ToggleStageEditor
            | Self::ToolSelect
            | Self::ToolRectangle
            | Self::ToolPolygon
            | Self::ToolCircle
            | Self::DuplicateSurface
            | Self::FlipHorizontal
            | Self::FlipVertical
            | Self::DeleteSurface
            | Self::ClearDrawing
            | Self::CombineSurfaces
            | Self::ToggleMidiLearn
            | Self::ToggleKeyboardLearn
            | Self::Copy
            | Self::Paste
            | Self::Duplicate
            | Self::CutToMain
            | Self::FadeToMain
            | Self::RundownGo => true,
            Self::PresentationNext | Self::PresentationPrevious | Self::PresentationBlack => false,
        }
    }
}

/// Persistent keymap store. Mirrors `MidiMappingStore` pattern.
#[derive(Debug, Clone)]
pub struct KeymapStore {
    pub bindings: HashMap<KeyCombo, KeyTarget>,
    pub learn_mode: bool,
    pub learn_target: Option<KeyTarget>,
}

impl Default for KeymapStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeymapStore {
    /// Create an empty keymap store.
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            learn_mode: false,
            learn_target: None,
        }
    }

    /// Create a keymap store populated with default bindings.
    pub fn with_defaults() -> Self {
        let mut store = Self::new();
        store.bindings = Self::defaults();
        store
    }

    /// Static default bindings.
    pub fn defaults() -> HashMap<KeyCombo, KeyTarget> {
        let mut m = HashMap::new();
        let action = |id: ActionId| KeyTarget::Action(id);

        // Main mode takes, as a vMix operator binds them; a presentation
        // clicker's buttons.
        for (key, id) in [
            ("Space", ActionId::CutToMain),
            ("Enter", ActionId::FadeToMain),
            ("PageDown", ActionId::PresentationNext),
            ("PageUp", ActionId::PresentationPrevious),
            ("B", ActionId::PresentationBlack),
        ] {
            m.insert(
                KeyCombo {
                    key: key.into(),
                    command: false,
                    shift: false,
                    alt: false,
                },
                action(id),
            );
        }

        // Global shortcuts
        m.insert(
            KeyCombo {
                key: "Z".into(),
                command: true,
                shift: false,
                alt: false,
            },
            action(ActionId::Undo),
        );
        m.insert(
            KeyCombo {
                key: "Z".into(),
                command: true,
                shift: true,
                alt: false,
            },
            action(ActionId::Redo),
        );
        m.insert(
            KeyCombo {
                key: "S".into(),
                command: true,
                shift: false,
                alt: false,
            },
            action(ActionId::Save),
        );
        m.insert(
            KeyCombo {
                key: "L".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::ToggleLibrary),
        );
        for (key, id) in [
            ("C", ActionId::Copy),
            ("V", ActionId::Paste),
            ("D", ActionId::Duplicate),
        ] {
            m.insert(
                KeyCombo {
                    key: key.into(),
                    command: true,
                    shift: false,
                    alt: false,
                },
                action(id),
            );
        }

        // Stage editor tools (context-checked at dispatch)
        m.insert(
            KeyCombo {
                key: "S".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::ToolSelect),
        );
        m.insert(
            KeyCombo {
                key: "R".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::ToolRectangle),
        );
        m.insert(
            KeyCombo {
                key: "P".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::ToolPolygon),
        );
        m.insert(
            KeyCombo {
                key: "C".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::ToolCircle),
        );
        m.insert(
            KeyCombo {
                key: "D".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::DuplicateSurface),
        );
        m.insert(
            KeyCombo {
                key: "H".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::FlipHorizontal),
        );
        m.insert(
            KeyCombo {
                key: "V".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::FlipVertical),
        );
        m.insert(
            KeyCombo {
                key: "Delete".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::DeleteSurface),
        );
        m.insert(
            KeyCombo {
                key: "Backspace".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::DeleteSurface),
        );
        m.insert(
            KeyCombo {
                key: "Escape".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::ClearDrawing),
        );
        m.insert(
            KeyCombo {
                key: "G".into(),
                command: false,
                shift: false,
                alt: false,
            },
            action(ActionId::CombineSurfaces),
        );

        m
    }

    /// Add or replace a binding.
    pub fn set(&mut self, combo: KeyCombo, target: KeyTarget) {
        log::info!("Keyboard mapped {combo:?} → {target:?}");
        self.bindings.insert(combo, target);
    }

    /// Remove a binding.
    pub fn remove(&mut self, combo: &KeyCombo) {
        self.bindings.remove(combo);
    }

    /// Look up a binding.
    pub fn get(&self, combo: &KeyCombo) -> Option<&KeyTarget> {
        self.bindings.get(combo)
    }

    /// Toggle learn mode on/off.
    pub fn toggle_learn(&mut self) {
        self.learn_mode = !self.learn_mode;
        if !self.learn_mode {
            self.learn_target = None;
        }
        log::info!(
            "Keyboard learn mode: {}",
            if self.learn_mode { "ON" } else { "OFF" }
        );
    }

    /// Select a learn target (must be in learn mode).
    pub fn select_learn_target(&mut self, target: KeyTarget) {
        if self.learn_mode {
            log::info!("Keyboard learn target: {target:?}");
            self.learn_target = Some(target);
        }
    }

    /// Cancel learn mode.
    pub fn cancel_learn(&mut self) {
        self.learn_mode = false;
        self.learn_target = None;
    }

    /// Process a key press in learn mode. Binds combo to current target.
    /// Returns true if a mapping was created. Stays in learn mode.
    pub fn process_learn(&mut self, combo: KeyCombo) -> bool {
        if let Some(target) = self.learn_target.take() {
            self.set(combo, target);
            true
        } else {
            false
        }
    }

    /// Serialize to a persistable config.
    pub fn to_config(&self) -> KeymapConfig {
        let bindings = self
            .bindings
            .iter()
            .map(|(combo, target)| KeyBinding {
                key: combo.key.clone(),
                command: combo.command,
                shift: combo.shift,
                alt: combo.alt,
                target: target.clone(),
            })
            .collect();
        KeymapConfig {
            version: 1,
            bindings,
        }
    }

    /// Load bindings from config, merging over defaults.
    pub fn load_config(&mut self, config: &KeymapConfig) {
        // Start from defaults, then overlay custom bindings
        self.bindings = Self::defaults();
        for binding in &config.bindings {
            let combo = KeyCombo {
                key: binding.key.clone(),
                command: binding.command,
                shift: binding.shift,
                alt: binding.alt,
            };
            if is_supported_key_name(&binding.key) {
                self.bindings.insert(combo, binding.target.clone());
            } else {
                log::warn!("Keymap: skipping unknown key '{}'", binding.key);
            }
        }
    }

    /// Reset to default bindings.
    pub fn reset_to_defaults(&mut self) {
        self.bindings = Self::defaults();
        log::info!("Keyboard shortcuts reset to defaults");
    }
}

/// Serializable keymap config, read leniently (see [`KeymapFile`]). A show
/// keeps it as two files (see [`KeymapConfig::split`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "KeymapFile")]
pub struct KeymapConfig {
    pub version: u32,
    pub bindings: Vec<KeyBinding>,
}

/// A keymap file as read: a binding this Nida does not know — a later
/// Nida's action — is left out with a warning, not the whole keymap refused.
#[derive(Deserialize)]
struct KeymapFile {
    #[serde(default = "default_keymap_version")]
    version: u32,
    #[serde(default, deserialize_with = "known_bindings")]
    bindings: Vec<KeyBinding>,
}

impl From<KeymapFile> for KeymapConfig {
    fn from(file: KeymapFile) -> Self {
        Self {
            version: file.version,
            bindings: file.bindings,
        }
    }
}

/// The bindings in a list that this Nida knows; another — a later Nida's
/// action — is left out with a warning, not the keymap refused.
fn known_bindings<'de, D>(deserializer: D) -> Result<Vec<KeyBinding>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let listed = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(listed
        .into_iter()
        .filter_map(|entry| match serde_json::from_value(entry) {
            Ok(binding) => Some(binding),
            Err(e) => {
                log::warn!("Keymap: skipping a binding this Nida does not know: {e}");
                None
            }
        })
        .collect())
}

fn default_keymap_version() -> u32 {
    1
}

/// A single key binding entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBinding {
    pub key: String,
    #[serde(default)]
    pub command: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
    pub target: KeyTarget,
}

impl KeyBinding {
    /// Validate a single key binding. Returns a list of errors (empty = valid).
    pub fn validate(&self, prefix: &str) -> Vec<String> {
        let mut errors = Vec::new();
        if self.key.trim().is_empty() {
            errors.push(format!("{prefix}: key is empty"));
        }
        errors
    }
}

impl KeymapConfig {
    /// The keymap as a show keeps it: `keymap.json`, the bindings every
    /// public Nida reads, and `keymap-newer.json`, those to later actions
    /// (see [`ActionId::read_by_older`]). An older Nida refuses a whole
    /// keymap naming an action it does not know — the show opens with
    /// "Failed to load workspace" — and, never opening the second file,
    /// leaves it be when it saves the show over: the later keys are there
    /// again when this Nida opens it.
    #[must_use]
    pub fn split(self) -> (Self, Self) {
        let read_by_older = |binding: &KeyBinding| match binding.target {
            KeyTarget::Action(id) => id.read_by_older(),
            KeyTarget::ParamPath(_) => true,
        };
        let (bindings, newer) = self.bindings.into_iter().partition(read_by_older);
        (
            Self {
                version: self.version,
                bindings,
            },
            Self {
                version: self.version,
                bindings: newer,
            },
        )
    }

    /// The keymap `newer`'s file adds to this one's: its bindings last, so
    /// they win a key both bind.
    #[must_use]
    pub fn with(mut self, newer: Self) -> Self {
        self.bindings.extend(newer.bindings);
        self
    }

    /// Validate the keymap config for semantic correctness. Returns a list of errors.
    /// An empty list means the config is valid.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (i, binding) in self.bindings.iter().enumerate() {
            errors.extend(binding.validate(&format!("bindings[{i}]")));
        }
        errors
    }

    /// Load from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or if its contents are not
    /// valid keymap JSON. Semantic validation issues are logged as warnings and
    /// do not fail the load.
    pub fn load<P: AsRef<std::path::Path>>(path: P) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .map_err(|e| anyhow::anyhow!("Failed to read keymap config: {e}"))?;
        let config: KeymapConfig = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse keymap config: {e}"))?;
        let warnings = config.validate();
        for w in &warnings {
            log::warn!("Keymap config {}: {}", path.as_ref().display(), w);
        }
        Ok(config)
    }

    /// Save to a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the config cannot be serialized to JSON or if the
    /// atomic write to `path` fails (missing directory, permissions, disk full).
    pub fn save<P: AsRef<std::path::Path>>(&self, path: P) -> anyhow::Result<()> {
        crate::persistence::atomic_write(path.as_ref(), &self.to_json()?)
    }

    /// The text [`KeymapConfig::save`] writes, its problems logged.
    ///
    /// # Errors
    ///
    /// Returns an error if the config cannot be serialized to JSON.
    pub fn to_json(&self) -> anyhow::Result<String> {
        let errors = self.validate();
        for e in &errors {
            log::error!("Keymap config save: {e}");
        }
        serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize keymap config: {e}"))
    }
}

/// Display a `KeyCombo` as a user-friendly string (e.g. "Cmd+Shift+Z").
impl std::fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.command {
            write!(f, "Cmd+")?;
        }
        if self.shift {
            write!(f, "Shift+")?;
        }
        if self.alt {
            write!(f, "Alt+")?;
        }
        write!(f, "{}", self.key)
    }
}

/// Display a `KeyTarget` as a user-friendly string.
impl std::fmt::Display for KeyTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyTarget::Action(id) => write!(f, "{id:?}"),
            KeyTarget::ParamPath(path) => write!(f, "{path}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(key: &str, command: bool, shift: bool, alt: bool) -> KeyCombo {
        KeyCombo {
            key: key.into(),
            command,
            shift,
            alt,
        }
    }

    #[test]
    fn test_default_bindings_complete() {
        let store = KeymapStore::with_defaults();
        assert!(store
            .bindings
            .values()
            .any(|t| *t == KeyTarget::Action(ActionId::Undo)));
        assert!(store
            .bindings
            .values()
            .any(|t| *t == KeyTarget::Action(ActionId::Save)));
        assert!(store
            .bindings
            .values()
            .any(|t| *t == KeyTarget::Action(ActionId::ToggleLibrary)));
        assert!(store
            .bindings
            .values()
            .any(|t| *t == KeyTarget::Action(ActionId::ToolSelect)));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let store = KeymapStore::with_defaults();
        let config = store.to_config();
        let json = serde_json::to_string(&config).unwrap();
        let loaded: KeymapConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.bindings.len(), loaded.bindings.len());
    }

    #[test]
    fn test_custom_binding_overrides_default() {
        let mut store = KeymapStore::with_defaults();
        let c = combo("L", false, false, false);
        store.set(c.clone(), KeyTarget::ParamPath("ch/0/opacity".into()));
        assert_eq!(
            store.get(&c),
            Some(&KeyTarget::ParamPath("ch/0/opacity".into()))
        );
    }

    #[test]
    fn test_conflict_replaces() {
        let mut store = KeymapStore::with_defaults();
        let c = combo("Z", true, false, false);
        store.set(c.clone(), KeyTarget::Action(ActionId::Save));
        assert_eq!(store.get(&c), Some(&KeyTarget::Action(ActionId::Save)));
    }

    #[test]
    fn test_learn_mode_state_machine() {
        let mut store = KeymapStore::with_defaults();
        assert!(!store.learn_mode);

        store.toggle_learn();
        assert!(store.learn_mode);
        assert!(store.learn_target.is_none());

        store.select_learn_target(KeyTarget::Action(ActionId::Save));
        assert!(store.learn_target.is_some());

        let c = combo("F1", false, false, false);
        let created = store.process_learn(c.clone());
        assert!(created);
        assert!(store.learn_mode); // stays in learn mode
        assert!(store.learn_target.is_none()); // target consumed
        assert_eq!(store.get(&c), Some(&KeyTarget::Action(ActionId::Save)));
    }

    #[test]
    fn test_reset_to_defaults() {
        let mut store = KeymapStore::with_defaults();
        let c = combo("L", false, false, false);
        store.set(c.clone(), KeyTarget::ParamPath("custom".into()));
        store.reset_to_defaults();
        assert_eq!(
            store.get(&c),
            Some(&KeyTarget::Action(ActionId::ToggleLibrary))
        );
    }

    #[test]
    fn test_key_combo_display() {
        let c = combo("Z", true, true, false);
        assert_eq!(format!("{c}"), "Cmd+Shift+Z");
    }

    #[test]
    fn default_bindings_use_supported_key_names() {
        for combo in KeymapStore::defaults().keys() {
            assert!(
                is_supported_key_name(&combo.key),
                "default binding uses unsupported key {}",
                combo.key
            );
        }
        assert!(is_supported_key_name("Z"));
        assert!(is_supported_key_name("Delete"));
        assert!(is_supported_key_name("F1"));
        assert!(!is_supported_key_name("NotAKey"));
        assert!(!is_supported_key_name("z"));
    }

    #[test]
    fn load_config_skips_unknown_key_names() {
        let mut store = KeymapStore::with_defaults();
        let before = store.bindings.len();
        store.load_config(&KeymapConfig {
            version: 1,
            bindings: vec![KeyBinding {
                key: "NotAKey".into(),
                command: false,
                shift: false,
                alt: false,
                target: KeyTarget::Action(ActionId::Save),
            }],
        });
        assert_eq!(store.bindings.len(), before);
        assert!(store.get(&combo("NotAKey", false, false, false)).is_none());
    }

    #[test]
    fn test_load_config_merges_over_defaults() {
        let mut store = KeymapStore::with_defaults();
        let config = KeymapConfig {
            version: 1,
            bindings: vec![KeyBinding {
                key: "L".into(),
                command: false,
                shift: false,
                alt: false,
                target: KeyTarget::ParamPath("ch/0/opacity".into()),
            }],
        };
        store.load_config(&config);
        // Custom binding overrides default
        let c = combo("L", false, false, false);
        assert_eq!(
            store.get(&c),
            Some(&KeyTarget::ParamPath("ch/0/opacity".into()))
        );
        // Other defaults remain
        let undo = combo("Z", true, false, false);
        assert_eq!(store.get(&undo), Some(&KeyTarget::Action(ActionId::Undo)));
    }

    #[test]
    fn test_keymap_config_validate_valid() {
        let config = KeymapConfig {
            version: 1,
            bindings: vec![KeyBinding {
                key: "Z".into(),
                command: true,
                shift: false,
                alt: false,
                target: KeyTarget::Action(ActionId::Undo),
            }],
        };
        assert!(config.validate().is_empty());
    }

    #[test]
    fn test_keymap_config_validate_empty_key() {
        let config = KeymapConfig {
            version: 1,
            bindings: vec![KeyBinding {
                key: String::new(),
                command: false,
                shift: false,
                alt: false,
                target: KeyTarget::Action(ActionId::Save),
            }],
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("key is empty")));
    }

    /// The keymap as public Nida 0.9 reads it: the whole file refused at an
    /// action it does not know.
    #[allow(dead_code)]
    mod older {
        use serde::Deserialize;

        #[derive(Deserialize)]
        pub enum ActionId {
            Undo,
            Redo,
            Save,
            ToggleLibrary,
            ToggleStageEditor,
            ToolSelect,
            ToolRectangle,
            ToolPolygon,
            ToolCircle,
            DuplicateSurface,
            FlipHorizontal,
            FlipVertical,
            DeleteSurface,
            ClearDrawing,
            CombineSurfaces,
            ToggleMidiLearn,
            ToggleKeyboardLearn,
            Copy,
            Paste,
            Duplicate,
            CutToMain,
            FadeToMain,
            RundownGo,
        }

        #[derive(Deserialize)]
        pub enum KeyTarget {
            Action(ActionId),
            ParamPath(String),
        }

        #[derive(Deserialize)]
        pub struct KeyBinding {
            pub key: String,
            #[serde(default)]
            pub command: bool,
            #[serde(default)]
            pub shift: bool,
            #[serde(default)]
            pub alt: bool,
            pub target: KeyTarget,
        }

        #[derive(Deserialize)]
        pub struct KeymapConfig {
            #[serde(default)]
            pub version: u32,
            #[serde(default)]
            pub bindings: Vec<KeyBinding>,
        }
    }

    /// A show saved here opens in an older Nida — on the other Mac, the same
    /// show in iCloud — its keymap whole but for the later actions, which wait
    /// in their own file: this Nida reads them all back, an older Nida's save
    /// of keymap.json between included.
    #[test]
    fn an_older_nida_reads_the_keymap_this_one_saves() {
        let mut store = KeymapStore::with_defaults();
        // A GO key learned, which 0.9 knows, and a clicker's own key.
        store.bindings.insert(
            combo("G", false, false, false),
            KeyTarget::Action(ActionId::RundownGo),
        );
        store.bindings.insert(
            combo("N", false, false, false),
            KeyTarget::Action(ActionId::PresentationNext),
        );
        let (keymap, newer) = store.to_config().split();
        let keymap = keymap.to_json().unwrap();
        let newer = newer.to_json().unwrap();

        let older: older::KeymapConfig =
            serde_json::from_str(&keymap).expect("an older Nida reads it");
        // The clicker's keys, its three and N, are this Nida's alone.
        assert_eq!(older.bindings.len(), store.bindings.len() - 4);

        let mut back = KeymapStore::new();
        back.load_config(
            &serde_json::from_str::<KeymapConfig>(&keymap)
                .unwrap()
                .with(serde_json::from_str(&newer).unwrap()),
        );
        assert_eq!(back.bindings, store.bindings);
    }

    /// A later Nida's binding is left out, not the whole keymap refused; a
    /// later action in keymap.json, as a try-out build wrote it, still reads.
    #[test]
    fn a_later_nidas_binding_is_left_out_and_the_rest_kept() {
        let json = r#"{
            "version": 1,
            "bindings": [
                { "key": "S", "command": true, "target": { "Action": "Save" } },
                { "key": "L", "target": { "Action": "SomethingLater" } },
                { "key": "PageUp", "target": { "Action": "PresentationPrevious" } }
            ]
        }"#;
        let config: KeymapConfig = serde_json::from_str(json).expect("not refused");
        let targets: Vec<_> = config.bindings.iter().map(|b| b.target.clone()).collect();
        assert_eq!(
            targets,
            [
                KeyTarget::Action(ActionId::Save),
                KeyTarget::Action(ActionId::PresentationPrevious),
            ]
        );
    }
}
