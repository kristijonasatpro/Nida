//! Preset persistence — save/load deck and channel presets from `.varda/presets/`.

use super::Workspace;
use crate::scene::{ChannelConfig, DeckConfig};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// A loaded deck preset with its name and config.
#[derive(Debug, Clone)]
pub struct DeckPreset {
    pub name: String,
    pub config: DeckConfig,
}

/// A loaded channel preset with its name and config.
#[derive(Debug, Clone)]
pub struct ChannelPreset {
    pub name: String,
    pub config: ChannelConfig,
}

/// In-memory collection of loaded presets from disk.
#[derive(Debug)]
pub struct PresetLibrary {
    pub deck_presets: Vec<DeckPreset>,
    pub channel_presets: Vec<ChannelPreset>,
}

impl PresetLibrary {
    /// Scan preset directories and load all valid JSON files.
    pub fn load(workspace: &Workspace) -> Self {
        Self::load_from(&workspace.presets_dir())
    }

    /// The presets in `presets` — a show's `presets` folder, its `decks`
    /// and `channels` — as a scan reads them.
    pub fn load_from(presets: &Path) -> Self {
        let mut lib = Self {
            deck_presets: Vec::new(),
            channel_presets: Vec::new(),
        };
        lib.scan_dir(&presets.join("decks"), true);
        lib.scan_dir(&presets.join("channels"), false);
        lib
    }

    /// A deck preset as saving writes it: where it goes in `workspace`, and
    /// its text.
    ///
    /// # Errors
    ///
    /// Returns an error if `config` cannot be serialized to JSON.
    pub fn deck_preset_file(
        workspace: &Workspace,
        name: &str,
        config: &DeckConfig,
    ) -> Result<(PathBuf, String)> {
        let errors = config.validate("deck_preset");
        for e in &errors {
            log::error!("Deck preset '{name}' save: {e}");
        }
        let path = workspace.deck_presets_dir().join(sanitize_filename(name));
        let json =
            serde_json::to_string_pretty(config).context("Failed to serialize deck preset")?;
        Ok((path, json))
    }

    /// A channel preset as saving writes it: where it goes in `workspace`,
    /// and its text.
    ///
    /// # Errors
    ///
    /// Returns an error if `config` cannot be serialized to JSON.
    pub fn channel_preset_file(
        workspace: &Workspace,
        name: &str,
        config: &ChannelConfig,
    ) -> Result<(PathBuf, String)> {
        let errors = config.validate("channel_preset");
        for e in &errors {
            log::error!("Channel preset '{name}' save: {e}");
        }
        let path = workspace
            .channel_presets_dir()
            .join(sanitize_filename(name));
        let json =
            serde_json::to_string_pretty(config).context("Failed to serialize channel preset")?;
        Ok((path, json))
    }

    /// Save a deck preset to disk, now.
    ///
    /// # Errors
    ///
    /// Returns an error if the `presets/decks/` directory cannot be created, if
    /// `config` cannot be serialized to JSON, or if the atomic write fails.
    pub fn save_deck_preset(workspace: &Workspace, name: &str, config: &DeckConfig) -> Result<()> {
        let (path, json) = Self::deck_preset_file(workspace, name, config)?;
        workspace.ensure_preset_dirs()?;
        super::atomic_write(&path, &json)?;
        log::info!("Saved deck preset '{}' to {}", name, path.display());
        Ok(())
    }

    /// Save a channel preset to disk, now.
    ///
    /// # Errors
    ///
    /// Returns an error if the `presets/channels/` directory cannot be created,
    /// if `config` cannot be serialized to JSON, or if the atomic write fails.
    pub fn save_channel_preset(
        workspace: &Workspace,
        name: &str,
        config: &ChannelConfig,
    ) -> Result<()> {
        let (path, json) = Self::channel_preset_file(workspace, name, config)?;
        workspace.ensure_preset_dirs()?;
        super::atomic_write(&path, &json)?;
        log::info!("Saved channel preset '{}' to {}", name, path.display());
        Ok(())
    }

    /// Take in a deck preset just saved to `path` as `json`, before its
    /// file has reached the disk: there to load at once, until the disk is
    /// read again once it has landed ([`Self::settle`]). Returns it as kept.
    pub fn keep_deck(&mut self, path: &Path, json: &str) -> Option<KeptPreset> {
        match serde_json::from_str::<DeckConfig>(json) {
            Ok(config) => {
                let kept = KeptPreset::Deck(DeckPreset {
                    name: preset_name(path),
                    config,
                });
                self.put(&kept);
                Some(kept)
            }
            Err(e) => {
                log::warn!("Deck preset {} not kept: {e}", path.display());
                None
            }
        }
    }

    /// Take in a channel preset just saved to `path` as `json`, as
    /// [`Self::keep_deck`] does a deck preset.
    pub fn keep_channel(&mut self, path: &Path, json: &str) -> Option<KeptPreset> {
        match serde_json::from_str::<ChannelConfig>(json) {
            Ok(config) => {
                let kept = KeptPreset::Channel(ChannelPreset {
                    name: preset_name(path),
                    config,
                });
                self.put(&kept);
                Some(kept)
            }
            Err(e) => {
                log::warn!("Channel preset {} not kept: {e}", path.display());
                None
            }
        }
    }

    /// The presets as the disk has them once a preset's file has landed —
    /// `scanned`, read off the engine — with those still on their way
    /// kept as they were saved: the names the disk gave, a file that did
    /// not land gone, as a rescan after a save always showed.
    pub fn settle<'a>(
        &mut self,
        scanned: PresetLibrary,
        on_way: impl Iterator<Item = &'a KeptPreset>,
    ) {
        self.deck_presets = scanned.deck_presets;
        self.channel_presets = scanned.channel_presets;
        for kept in on_way {
            self.put(kept);
        }
    }

    /// `kept` in the library, in place of a preset of its kind and name.
    fn put(&mut self, kept: &KeptPreset) {
        match kept {
            KeptPreset::Deck(preset) => {
                self.deck_presets.retain(|p| p.name != preset.name);
                self.deck_presets.push(preset.clone());
                self.deck_presets.sort_by(|a, b| a.name.cmp(&b.name));
            }
            KeptPreset::Channel(preset) => {
                self.channel_presets.retain(|p| p.name != preset.name);
                self.channel_presets.push(preset.clone());
                self.channel_presets.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }
    }

    /// Rescan directories to pick up new/removed presets.
    pub fn refresh(&mut self, workspace: &Workspace) {
        self.deck_presets.clear();
        self.channel_presets.clear();
        self.scan_dir(&workspace.deck_presets_dir(), true);
        self.scan_dir(&workspace.channel_presets_dir(), false);
    }

    fn scan_dir(&mut self, dir: &std::path::Path, is_deck: bool) {
        // Directory doesn't exist yet
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("Failed to read preset file {}: {}", path.display(), e);
                    continue;
                }
            };
            let stem = preset_name(&path);

            if is_deck {
                match serde_json::from_str::<DeckConfig>(&content) {
                    Ok(config) => {
                        let warnings = config.validate(&format!("deck_preset '{stem}'"));
                        for w in &warnings {
                            log::warn!("Preset {}: {}", path.display(), w);
                        }
                        self.deck_presets.push(DeckPreset { name: stem, config });
                    }
                    Err(e) => log::warn!("Failed to parse deck preset {}: {}", path.display(), e),
                }
            } else {
                match serde_json::from_str::<ChannelConfig>(&content) {
                    Ok(config) => {
                        let warnings = config.validate(&format!("channel_preset '{stem}'"));
                        for w in &warnings {
                            log::warn!("Preset {}: {}", path.display(), w);
                        }
                        self.channel_presets
                            .push(ChannelPreset { name: stem, config });
                    }
                    Err(e) => {
                        log::warn!("Failed to parse channel preset {}: {}", path.display(), e);
                    }
                }
            }
        }
        // Sort by name for stable ordering
        if is_deck {
            self.deck_presets.sort_by(|a, b| a.name.cmp(&b.name));
        } else {
            self.channel_presets.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }
}

/// A preset taken into the library before its file landed.
#[derive(Debug, Clone)]
pub enum KeptPreset {
    Deck(DeckPreset),
    Channel(ChannelPreset),
}

/// A preset's name: its file's, as a scan reads it.
fn preset_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Sanitize a user-provided name into a safe filename.
pub fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let truncated = if sanitized.len() > 64 {
        sanitized[..64].to_string()
    } else if sanitized.is_empty() {
        "preset".to_string()
    } else {
        sanitized
    };
    format!("{truncated}.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::*;

    fn sample_deck_config() -> DeckConfig {
        DeckConfig {
            uuid: crate::deck::generate_short_uuid(),
            name: "test_deck".to_string(),
            source: SourceConfig::SolidColor {
                color: [1.0, 0.0, 0.0, 1.0],
            },
            effects: vec![],
            opacity: 0.8,
            transparent: false,
            blend_mode: BlendModeConfig::Normal,
            mute: false,
            solo: false,
            z_index: 0,
            auto_transition: None,
            modulation: vec![],
            render_fps: crate::channel::DeckRenderFps::default(),
            render_scale: crate::deck::DeckRenderScale::default(),
        }
    }

    fn sample_channel_config() -> ChannelConfig {
        ChannelConfig {
            overlay: false,
            uuid: crate::deck::generate_short_uuid(),
            name: "test_channel".to_string(),
            opacity: 1.0,
            blend_mode: BlendModeConfig::Normal,
            decks: vec![sample_deck_config()],
            effects: vec![],
            modulation: vec![],
        }
    }

    #[test]
    fn test_save_load_deck_preset_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());
        let config = sample_deck_config();

        PresetLibrary::save_deck_preset(&ws, "my deck", &config).unwrap();

        let lib = PresetLibrary::load(&ws);
        assert_eq!(lib.deck_presets.len(), 1);
        assert_eq!(lib.deck_presets[0].name, "my_deck");
        assert_eq!(lib.deck_presets[0].config.name, "test_deck");
        assert_eq!(lib.deck_presets[0].config.opacity, 0.8);
    }

    #[test]
    fn test_save_load_channel_preset_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());
        let config = sample_channel_config();

        PresetLibrary::save_channel_preset(&ws, "my channel", &config).unwrap();

        let lib = PresetLibrary::load(&ws);
        assert_eq!(lib.channel_presets.len(), 1);
        assert_eq!(lib.channel_presets[0].name, "my_channel");
        assert_eq!(lib.channel_presets[0].config.decks.len(), 1);
    }

    #[test]
    fn test_preset_library_scan_ignores_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());
        ws.ensure_preset_dirs().unwrap();

        // Write a valid preset
        let config = sample_deck_config();
        PresetLibrary::save_deck_preset(&ws, "good", &config).unwrap();

        // Write an invalid JSON file
        std::fs::write(ws.deck_presets_dir().join("bad.json"), "not json").unwrap();

        // Write a non-json file (should be skipped entirely)
        std::fs::write(ws.deck_presets_dir().join("readme.txt"), "ignore me").unwrap();

        let lib = PresetLibrary::load(&ws);
        assert_eq!(lib.deck_presets.len(), 1);
        assert_eq!(lib.deck_presets[0].name, "good");
    }

    #[test]
    fn test_filename_sanitization() {
        assert_eq!(sanitize_filename("My Cool Preset!"), "My_Cool_Preset_.json");
        assert_eq!(sanitize_filename("hello-world_v2"), "hello-world_v2.json");
        assert_eq!(sanitize_filename(""), "preset.json");
        assert_eq!(sanitize_filename("a/b\\c:d"), "a_b_c_d.json");
        // Long name gets truncated
        let long = "a".repeat(100);
        let result = sanitize_filename(&long);
        assert!(result.len() <= 69); // 64 + ".json"
    }

    #[test]
    fn test_deck_preset_roundtrip_with_modulation() {
        use crate::modulation::ModulationSource;
        use crate::scene::{ModulationRecipe, ModulationRecipeAssignment};

        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());
        let mut config = sample_deck_config();
        config.modulation = vec![ModulationRecipe {
            source_uuid: "test0001".to_string(),
            source: ModulationSource::sine_lfo(2.0),
            timebase: crate::timebase::Timebase::FreeRun,
            assignments: vec![
                ModulationRecipeAssignment {
                    param: "brightness".into(),
                    amount: 0.5,
                    component: None,
                },
                ModulationRecipeAssignment {
                    param: "fx0:amount".into(),
                    amount: 0.3,
                    component: None,
                },
            ],
        }];
        PresetLibrary::save_deck_preset(&ws, "mod_test", &config).unwrap();
        let lib = PresetLibrary::load(&ws);
        assert_eq!(lib.deck_presets.len(), 1);
        let loaded = &lib.deck_presets[0].config;
        assert_eq!(loaded.modulation.len(), 1);
        assert_eq!(loaded.modulation[0].assignments.len(), 2);
        assert_eq!(loaded.modulation[0].assignments[0].param, "brightness");
        assert_eq!(loaded.modulation[0].assignments[0].amount, 0.5);
    }

    #[test]
    fn test_modulation_recipe_serde() {
        use crate::modulation::ModulationSource;
        use crate::scene::{ModulationRecipe, ModulationRecipeAssignment};

        let mut config = sample_deck_config();
        config.modulation = vec![ModulationRecipe {
            source_uuid: "test0002".to_string(),
            source: ModulationSource::adsr(0.1, 0.2, 0.7, 0.3),
            timebase: crate::timebase::Timebase::FreeRun,
            assignments: vec![ModulationRecipeAssignment {
                param: "scale".into(),
                amount: -0.8,
                component: Some(1),
            }],
        }];
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"modulation\""));
        assert!(json.contains("\"scale\""));
        let deser: DeckConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.modulation.len(), 1);
        assert_eq!(deser.modulation[0].assignments[0].component, Some(1));
    }

    #[test]
    fn test_deck_config_without_modulation_deserializes() {
        // Old presets without modulation field should deserialize fine
        let json = r#"{"name":"old","source":{"type":"SolidColor","color":[1,0,0,1]},"opacity":1.0,"blend_mode":"normal"}"#;
        let config: DeckConfig = serde_json::from_str(json).unwrap();
        assert!(config.modulation.is_empty());
    }

    #[test]
    fn test_preset_validation_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());
        // Deck with invalid opacity — should still save (logs error but doesn't block)
        let mut config = sample_deck_config();
        config.opacity = 5.0;
        assert!(PresetLibrary::save_deck_preset(&ws, "bad_opacity", &config).is_ok());
        // Verify it's loadable despite validation warnings
        let lib = PresetLibrary::load(&ws);
        assert_eq!(lib.deck_presets.len(), 1);
    }
}
