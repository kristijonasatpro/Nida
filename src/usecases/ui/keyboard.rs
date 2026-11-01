//! egui adapter for the domain keymap.
//!
//! `internal/keymap` stores string key names and modifier flags. This module
//! is the only place that converts those names to and from `egui::Key`.

use crate::keymap::KeyCombo;

impl KeyCombo {
    /// Build a domain combo from an egui key event.
    pub fn from_egui(key: egui::Key, modifiers: &egui::Modifiers) -> Self {
        Self {
            key: format!("{key:?}"),
            command: modifiers.command,
            shift: modifiers.shift,
            alt: modifiers.alt,
        }
    }
}

/// Collect non-repeat key-down events this frame as domain combos.
///
/// Each with the modifiers its own event carries: a Shift let go before the
/// frame still held for the key pressed with it.
pub fn collect_pressed_keys(ctx: &egui::Context) -> Vec<KeyCombo> {
    ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } => Some(KeyCombo::from_egui(*key, modifiers)),
                _ => None,
            })
            .collect()
    })
}

/// Every key down this frame, and whether it is a repeat — the key held —
/// where [`collect_pressed_keys`] gives the first presses only: a key a
/// shortcut owns is taken from the panels while it repeats too, though acted
/// on once.
pub fn collect_key_downs(ctx: &egui::Context) -> Vec<(KeyCombo, bool)> {
    ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat,
                    modifiers,
                    ..
                } => Some((KeyCombo::from_egui(*key, modifiers), *repeat)),
                _ => None,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{is_supported_key_name, SUPPORTED_KEY_NAMES};

    #[test]
    fn from_egui_uses_debug_names_and_modifiers() {
        let mods = egui::Modifiers {
            command: true,
            shift: true,
            alt: false,
            ..egui::Modifiers::default()
        };
        let combo = KeyCombo::from_egui(egui::Key::Z, &mods);
        assert_eq!(combo.key, "Z");
        assert!(combo.command);
        assert!(combo.shift);
        assert!(!combo.alt);
        assert!(is_supported_key_name(&combo.key));
    }

    #[test]
    fn supported_names_cover_mapped_egui_keys() {
        let keys = [
            egui::Key::A,
            egui::Key::Z,
            egui::Key::Num0,
            egui::Key::F12,
            egui::Key::ArrowLeft,
            egui::Key::Delete,
            egui::Key::Backspace,
            egui::Key::Escape,
            egui::Key::Space,
            egui::Key::Minus,
            egui::Key::Plus,
        ];
        for key in keys {
            let name = format!("{key:?}");
            assert!(
                SUPPORTED_KEY_NAMES.contains(&name.as_str()),
                "{name} must stay in SUPPORTED_KEY_NAMES"
            );
        }
    }
}
