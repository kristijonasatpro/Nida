//! Screen layouts: which panels the control window shows, and where.
//!
//! Everything below the top bar is a dock of panels. Three layouts come built
//! in — **Classic** (Varda's own), **Broadcast** (Preview and Program on top,
//! the inputs below) and **VJ** (layers as rows, the output top right, the
//! browser bottom right) — and the operator can drag any panel anywhere,
//! resize the dividers, then save the arrangement under a name.
//!
//! Saved layouts belong to the operator rather than to a show, so they live in
//! one file for the user ([`saved_layouts_path`]) and are there for the next
//! workspace too. The arrangement in use is kept with the workspace, in
//! `stage.json`.

use std::path::{Path, PathBuf};

use egui_dock::{DockState, Node, NodeIndex, Surface, Tree};
use serde::{Deserialize, Serialize};

/// A panel of the control window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pane {
    /// Generators, effects, files, cameras, streams: what decks are made from.
    Library,
    /// The channels side by side, their decks stacked in each (Classic).
    Channels,
    /// The channels as rows, their decks across, as layers and clips (VJ).
    Layers,
    /// One tile per channel, taken to Main from the tile (Broadcast).
    Inputs,
    /// Preview, the take buttons, and Program side by side — Broadcast's top half.
    Monitors,
    /// The Main output picture.
    Output,
    /// The buses and their Preview / Program.
    Switcher,
    /// The rundown: the show's steps in order, each taken with one press.
    Rundown,
    /// The audio mixer.
    Audio,
    /// Whatever is selected: a deck's settings, a channel's or the master's
    /// effects.
    Details,
    /// Tonemap, modulation, MIDI, stage layout and outputs.
    Setup,
    /// A timer's controls — start, pause, reset, its length — by its
    /// panel number (see [`crate::engine::value::timer::panel_of`]). One
    /// per timer, opened from the timer.
    Timer(u64),
}

impl Pane {
    pub const ALL: [Self; 11] = [
        Self::Library,
        Self::Channels,
        Self::Layers,
        Self::Inputs,
        Self::Monitors,
        Self::Output,
        Self::Switcher,
        Self::Rundown,
        Self::Audio,
        Self::Details,
        Self::Setup,
    ];

    /// The name on the panel's tab.
    pub fn title(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Channels => "Channels",
            Self::Layers => "Layers",
            Self::Inputs => "Inputs",
            Self::Monitors => "Preview / Program",
            Self::Output => "Main Output",
            Self::Switcher => "Switcher",
            Self::Rundown => "Rundown",
            Self::Audio => "Audio Mixer",
            Self::Details => "Properties",
            Self::Setup => "Setup",
            // Named after its timer where the timer is known (see `dock`).
            Self::Timer(_) => "Timer",
        }
    }

    /// One line on what the panel is, for the Panels menu.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Library => "Generators, effects, cameras, streams — drag them into a channel",
            Self::Channels => "Channels side by side, decks stacked in each (Classic)",
            Self::Layers => "Channels as rows, decks across, top layer on top (VJ)",
            Self::Inputs => "A tile per channel with Cut and Fade on it (Broadcast)",
            Self::Monitors => "Preview and Program with the take buttons between (Broadcast)",
            Self::Output => "The Main output picture",
            Self::Switcher => "Buses: what each screen shows, Cut and Auto",
            Self::Rundown => "The show in order: steps taken with one press, and GO",
            Self::Audio => "Audio mixer: faders, meters, monitoring",
            Self::Details => "Settings of whatever is selected",
            Self::Setup => "Tonemap, modulation, MIDI, stage layout, outputs",
            Self::Timer(_) => "A timer's start, pause, reset and length",
        }
    }

    /// The panels that show the channels and their decks.
    pub fn is_desk(self) -> bool {
        matches!(self, Self::Channels | Self::Layers | Self::Inputs)
    }
}

/// A layout that comes with Varda.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Classic,
    Broadcast,
    Vj,
}

impl Builtin {
    pub const ALL: [Self; 3] = [Self::Classic, Self::Broadcast, Self::Vj];

    pub fn name(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::Broadcast => "Broadcast",
            Self::Vj => "VJ",
        }
    }

    /// The name it had before, which shows and saved layouts written then
    /// still give: Broadcast was "vMix" and VJ "Resolume", after the programs
    /// they are laid out like.
    fn former_name(self) -> Option<&'static str> {
        match self {
            Self::Classic => None,
            Self::Broadcast => Some("vMix"),
            Self::Vj => Some("Resolume"),
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Classic => "Library left, channel columns, Main Output and switcher right",
            Self::Broadcast => "Preview and Program on top, input tiles below, audio beside them",
            Self::Vj => "Layers as rows with the output top right, properties and browser below",
        }
    }

    /// The built-in called `name` in a layout written before Broadcast and
    /// VJ were built-ins (naming 0): Classic, vMix or Resolume. A "Broadcast"
    /// or "VJ" then was a layout the operator saved.
    fn named_before(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL.into_iter().find(|b| {
            b.former_name()
                .unwrap_or(b.name())
                .eq_ignore_ascii_case(name)
        })
    }

    /// The built-in called `name`, ignoring case — by its name or the one it
    /// had before.
    pub fn named(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL.into_iter().find(|b| {
            b.name().eq_ignore_ascii_case(name)
                || b.former_name()
                    .is_some_and(|former| former.eq_ignore_ascii_case(name))
        })
    }

    /// The arrangement. Every split is made as `split_right` / `split_below`,
    /// whose fraction is the share of the part left or above.
    pub fn dock(self) -> DockState<Pane> {
        match self {
            Self::Classic => {
                let mut dock = DockState::new(vec![Pane::Library]);
                let tree = dock.main_surface_mut();
                let [_, rest] = tree.split_right(NodeIndex::root(), 0.13, vec![Pane::Channels]);
                let [center, right] = tree.split_right(rest, 0.8, vec![Pane::Output]);
                tree.split_below(center, 0.78, vec![Pane::Details]);
                // The switcher and the audio mixer both in view, as they were
                // stacked in the old right panel; setup behind the mixer.
                let [_, tools] =
                    tree.split_below(right, 0.3, vec![Pane::Switcher, Pane::Rundown]);
                tree.split_below(tools, 0.55, vec![Pane::Audio, Pane::Setup]);
                dock
            }
            Self::Broadcast => {
                let mut dock = DockState::new(vec![Pane::Monitors]);
                let tree = dock.main_surface_mut();
                let [_, below] = tree.split_below(NodeIndex::root(), 0.46, vec![Pane::Inputs]);
                tree.split_right(
                    below,
                    0.7,
                    vec![
                        Pane::Audio,
                        Pane::Switcher,
                        Pane::Rundown,
                        Pane::Details,
                        Pane::Library,
                        Pane::Setup,
                    ],
                );
                dock
            }
            Self::Vj => {
                let mut dock = DockState::new(vec![Pane::Layers]);
                let tree = dock.main_surface_mut();
                let [top, bottom] = tree.split_below(NodeIndex::root(), 0.58, vec![Pane::Details]);
                tree.split_right(top, 0.68, vec![Pane::Output]);
                tree.split_right(
                    bottom,
                    0.62,
                    vec![
                        Pane::Library,
                        Pane::Switcher,
                        Pane::Rundown,
                        Pane::Audio,
                        Pane::Setup,
                    ],
                );
                dock
            }
        }
    }
}

/// The arrangement in use, kept with the workspace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenLayout {
    /// The layout last chosen or saved — a built-in's name or a saved one's —
    /// which the arrangement may since have been changed from.
    pub name: String,
    #[serde(with = "portable_dock")]
    pub dock: DockState<Pane>,
    /// Panels can't be dragged or closed, so a show can't knock them about.
    #[serde(default)]
    pub locked: bool,
    /// The built-ins' names `name` was written under: 0 in a show from before
    /// Broadcast and VJ were built-ins (a "Broadcast" then was the operator's
    /// own), [`NAMING`] since.
    #[serde(default)]
    pub naming: u32,
}

/// The built-ins' names layouts are written under now (see
/// [`ScreenLayout::naming`]).
pub const NAMING: u32 = 1;

impl Default for ScreenLayout {
    fn default() -> Self {
        Self::builtin(Builtin::Classic)
    }
}

impl ScreenLayout {
    pub fn builtin(builtin: Builtin) -> Self {
        Self {
            name: builtin.name().to_string(),
            dock: builtin.dock(),
            locked: false,
            naming: NAMING,
        }
    }

    /// Whether a panel is anywhere in the arrangement.
    pub fn has(&self, pane: Pane) -> bool {
        has_pane(&self.dock, pane)
    }

    /// Made safe to show, however it was written: a built-in goes by its name
    /// as it is now (a show saved before says "vMix" or "Resolume"; its
    /// "Broadcast" or "VJ" was the operator's own, left for
    /// [`SavedLayoutStore::reconnect`]), a panel twice keeps its first place,
    /// and an arrangement `egui_dock` could not draw is replaced by Classic.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        let builtin = if self.naming < NAMING {
            Builtin::named_before(&self.name)
        } else {
            Builtin::named(&self.name)
        };
        if let Some(builtin) = builtin {
            builtin.name().clone_into(&mut self.name);
            self.naming = NAMING;
        }
        dedupe(&mut self.dock);
        if !is_drawable(&self.dock) {
            log::warn!(
                "Screen layout \"{}\" is damaged — using Classic instead",
                self.name
            );
            let locked = self.locked;
            self = Self::builtin(Builtin::Classic);
            self.locked = locked;
        }
        self
    }
}

/// A layout the operator saved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedLayout {
    pub name: String,
    #[serde(with = "portable_dock")]
    pub dock: DockState<Pane>,
    /// The name it was saved under before a built-in took it, which shows
    /// from then still give (see [`SavedLayoutStore::reconnect`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
}

/// A dock written without the sizes the last frame gave its panels. They are
/// recomputed every frame, and a panel not yet drawn has an infinite one,
/// which JSON cannot hold.
mod portable_dock {
    use super::{DockState, Node, Pane};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        dock: &DockState<Pane>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut dock = dock.clone();
        for (_, node) in dock.iter_all_nodes_mut() {
            match node {
                Node::Leaf(leaf) => {
                    leaf.rect = egui::Rect::ZERO;
                    leaf.viewport = egui::Rect::ZERO;
                }
                Node::Vertical(split) | Node::Horizontal(split) => split.rect = egui::Rect::ZERO,
                Node::Empty => {}
            }
        }
        dock.serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<DockState<Pane>, D::Error> {
        DockState::deserialize(deserializer)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SavedFile {
    #[serde(default)]
    layouts: Vec<SavedLayout>,
}

/// The operator's saved layouts and the file they are kept in.
#[derive(Clone, Debug, Default)]
pub struct SavedLayoutStore {
    path: Option<PathBuf>,
    layouts: std::sync::Arc<Vec<SavedLayout>>,
}

impl SavedLayoutStore {
    /// The layouts in the user's file ([`saved_layouts_path`]).
    pub fn open_default() -> Self {
        match saved_layouts_path() {
            Some(path) => Self::open(path),
            None => Self::default(),
        }
    }

    /// The layouts in `path`, which saves go to.
    pub fn open(path: PathBuf) -> Self {
        let layouts = std::sync::Arc::new(load_saved(&path));
        Self {
            path: Some(path),
            layouts,
        }
    }

    /// Shared, so handing them to each frame's UI data costs nothing.
    pub fn layouts(&self) -> std::sync::Arc<Vec<SavedLayout>> {
        self.layouts.clone()
    }

    /// The layouts as their file holds them, read again: a change whose
    /// write did not land is not kept.
    pub fn reload(&mut self) {
        if let Some(path) = &self.path {
            self.layouts = std::sync::Arc::new(load_saved(path));
        }
    }

    /// A show from before Broadcast and VJ were built-ins that used a layout
    /// the operator had saved under one of those names: named after that
    /// layout again, as it is kept now. From then on the show is written
    /// under today's names.
    pub fn reconnect(&self, screen: &mut ScreenLayout) {
        if screen.naming >= NAMING {
            return;
        }
        screen.naming = NAMING;
        if Builtin::named(&screen.name).is_none() {
            return;
        }
        let was = |l: &&SavedLayout| {
            l.was
                .as_deref()
                .is_some_and(|was| was.trim().eq_ignore_ascii_case(screen.name.trim()))
        };
        if let Some(saved) = self.layouts.iter().find(was) {
            screen.name.clone_from(&saved.name);
        }
    }

    /// Save `dock` as `name` (replacing one of that name) and write the file.
    /// Returns the name it was saved under.
    ///
    /// # Errors
    ///
    /// A message for the operator if the name won't do or the file can't be
    /// written; nothing changes then.
    pub fn save_as(&mut self, name: &str, dock: &DockState<Pane>) -> Result<String, String> {
        self.save_as_with(name, dock, |path, text| write_now(path, &text))
    }

    /// As [`Self::save_as`], the file handed to `write` — the show's writer,
    /// so the outputs do not wait for the disk (see `persistence::writer`).
    ///
    /// # Errors
    ///
    /// A message for the operator if the name won't do, or `write` says the
    /// file can't be written; nothing changes then.
    pub fn save_as_with(
        &mut self,
        name: &str,
        dock: &DockState<Pane>,
        write: impl FnOnce(&Path, String) -> Result<(), String>,
    ) -> Result<String, String> {
        let name = valid_name(name)?.to_string();
        let mut layouts = self.layouts.as_ref().clone();
        upsert(&mut layouts, &name, dock.clone());
        self.write(layouts, write)?;
        Ok(name)
    }

    /// Forget the layout called `name` and write the file.
    ///
    /// # Errors
    ///
    /// A message for the operator if there is no such layout or the file
    /// can't be written.
    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        self.delete_with(name, |path, text| write_now(path, &text))
    }

    /// As [`Self::delete`], the file handed to `write`.
    ///
    /// # Errors
    ///
    /// A message for the operator if there is no such layout, or `write`
    /// says the file can't be written.
    pub fn delete_with(
        &mut self,
        name: &str,
        write: impl FnOnce(&Path, String) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut layouts = self.layouts.as_ref().clone();
        if !remove(&mut layouts, name) {
            return Err(format!("No saved layout called \"{name}\""));
        }
        self.write(layouts, write)
    }

    fn write(
        &mut self,
        layouts: Vec<SavedLayout>,
        write: impl FnOnce(&Path, String) -> Result<(), String>,
    ) -> Result<(), String> {
        let path = self
            .path
            .as_ref()
            .ok_or("There is no folder to keep saved layouts in")?;
        let text = saved_text(&layouts).map_err(|e| format!("{}: {e}", not_saved(path)))?;
        write(path, text)?;
        self.layouts = std::sync::Arc::new(layouts);
        Ok(())
    }
}

/// Write the saved layouts' `text` to `path` now.
fn write_now(path: &Path, text: &str) -> Result<(), String> {
    store_text(path, text).map_err(|e| format!("{}: {e}", not_saved(path)))
}

/// How saved layouts that could not be written to `path` are put to the
/// operator, before why.
pub fn not_saved(path: &Path) -> String {
    format!("Could not save layouts to {}", path.display())
}

/// Where saved layouts are kept: `VARDA_LAYOUTS_FILE` if set, otherwise
/// `layouts.json` in Varda's folder of the user's application data
/// (`~/Library/Application Support/Varda` on a Mac).
pub fn saved_layouts_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("VARDA_LAYOUTS_FILE") {
        return Some(PathBuf::from(path));
    }
    dirs::data_dir().map(|dir| dir.join("Varda").join("layouts.json"))
}

/// The saved layouts in `path`; none if it doesn't exist yet. A damaged file
/// loses its layouts rather than stopping Varda, and a damaged layout in it
/// is left out. One saved under a name a built-in has since taken (Broadcast,
/// VJ) is kept under a name of its own.
pub fn load_saved(path: &Path) -> Vec<SavedLayout> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            log::warn!("Could not read saved layouts {}: {e}", path.display());
            return Vec::new();
        }
    };
    match serde_json::from_str::<SavedFile>(&text) {
        Ok(file) => {
            let mut names: Vec<String> = file.layouts.iter().map(|l| l.name.clone()).collect();
            file.layouts
                .into_iter()
                .filter_map(|mut layout| {
                    dedupe(&mut layout.dock);
                    if Builtin::named(&layout.name).is_some() {
                        let own = own_name(&layout.name, &names);
                        log::info!(
                            "Saved layout \"{}\" is now \"{own}\": a built-in has its name",
                            layout.name
                        );
                        names.push(own.clone());
                        layout.was = Some(std::mem::replace(&mut layout.name, own));
                    }
                    let ok = is_drawable(&layout.dock) && valid_name(&layout.name).is_ok();
                    if !ok {
                        log::warn!("Leaving out damaged saved layout \"{}\"", layout.name);
                    }
                    ok.then_some(layout)
                })
                .collect()
        }
        Err(e) => {
            log::warn!("Saved layouts {} are damaged: {e}", path.display());
            Vec::new()
        }
    }
}

/// A name for a saved layout whose own a built-in has taken, none of `taken`:
/// "VJ" becomes "VJ (saved)", or "VJ (saved 2)" if that is taken too.
fn own_name(name: &str, taken: &[String]) -> String {
    let name = name.trim();
    // One of the first `taken.len() + 1` is free: no more are taken.
    (1..=taken.len() + 1)
        .map(|n| match n {
            1 => format!("{name} (saved)"),
            n => format!("{name} (saved {n})"),
        })
        .find(|own| !taken.iter().any(|t| t.trim().eq_ignore_ascii_case(own)))
        .unwrap_or_else(|| format!("{name} (saved)"))
}

/// Write the saved layouts to `path`, making its folder if need be.
///
/// # Errors
///
/// If the folder can't be made or the file can't be written.
pub fn store_saved(path: &Path, layouts: &[SavedLayout]) -> anyhow::Result<()> {
    store_text(path, &saved_text(layouts)?)
}

/// The saved layouts as their file holds them.
fn saved_text(layouts: &[SavedLayout]) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&SavedFile {
        layouts: layouts.to_vec(),
    })
}

fn store_text(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::persistence::atomic_write(path, text)
}

/// The name a layout would be saved under, or why it can't be.
///
/// # Errors
///
/// If the name is empty, too long, or a built-in's.
pub fn valid_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the layout a name".into());
    }
    if name.chars().count() > 40 {
        return Err("Layout names are at most 40 characters".into());
    }
    if let Some(builtin) = Builtin::named(name) {
        return Err(if builtin.name().eq_ignore_ascii_case(name) {
            format!(
                "\"{}\" is a built-in layout — choose another name",
                builtin.name()
            )
        } else {
            format!(
                "\"{name}\" is the built-in \"{}\" layout's old name — choose another name",
                builtin.name()
            )
        });
    }
    Ok(name)
}

/// Save `dock` as `name`, replacing a saved layout of the same name (ignoring
/// case). Returns whether one was replaced.
pub fn upsert(layouts: &mut Vec<SavedLayout>, name: &str, dock: DockState<Pane>) -> bool {
    if let Some(existing) = layouts
        .iter_mut()
        .find(|l| l.name.eq_ignore_ascii_case(name))
    {
        existing.name = name.to_string();
        existing.dock = dock;
        true
    } else {
        layouts.push(SavedLayout {
            name: name.to_string(),
            dock,
            was: None,
        });
        false
    }
}

/// Forget the saved layout called `name`. Returns whether there was one.
pub fn remove(layouts: &mut Vec<SavedLayout>, name: &str) -> bool {
    let before = layouts.len();
    layouts.retain(|l| !l.name.eq_ignore_ascii_case(name));
    layouts.len() != before
}

/// Whether `pane` is anywhere in `dock`, including a floating window.
pub fn has_pane(dock: &DockState<Pane>, pane: Pane) -> bool {
    dock.find_tab(&pane).is_some()
}

/// Close `pane` wherever it is. Returns whether it was there.
pub fn remove_pane(dock: &mut DockState<Pane>, pane: Pane) -> bool {
    match dock.find_tab(&pane) {
        Some(path) => {
            dock.remove_tab(path);
            true
        }
        None => false,
    }
}

/// Open `pane` where it is most likely wanted: the Library down the left,
/// Preview / Program across the top, Properties along the bottom, a desk
/// beside the other desks, anything else among the side tools. The operator
/// can drag it from there.
pub fn add_pane(dock: &mut DockState<Pane>, pane: Pane) {
    if has_pane(dock, pane) {
        return;
    }
    if dock.main_surface().num_tabs() == 0 {
        *dock.main_surface_mut() = Tree::new(vec![pane]);
        return;
    }
    let tree = dock.main_surface_mut();
    match pane {
        Pane::Library => {
            tree.split_left(NodeIndex::root(), 0.13, vec![pane]);
        }
        Pane::Monitors => {
            tree.split_above(NodeIndex::root(), 0.42, vec![pane]);
        }
        Pane::Details => {
            tree.split_below(NodeIndex::root(), 0.78, vec![pane]);
        }
        _ => {
            let beside: &[Pane] = if pane.is_desk() {
                &[Pane::Channels, Pane::Layers, Pane::Inputs]
            } else {
                &[
                    Pane::Switcher,
                    Pane::Rundown,
                    Pane::Audio,
                    Pane::Setup,
                    Pane::Library,
                    Pane::Output,
                ]
            };
            let leaf = beside.iter().find_map(|p| tree.find_tab(p).map(|(n, _)| n));
            match leaf {
                Some(node) => {
                    if let Node::Leaf(leaf) = &mut tree[node] {
                        leaf.append_tab(pane);
                    }
                }
                None => tree.push_to_first_leaf(pane),
            }
        }
    }
}

/// Open `pane` if it is closed, close it if it is open.
pub fn toggle_pane(dock: &mut DockState<Pane>, pane: Pane) {
    if !remove_pane(dock, pane) {
        add_pane(dock, pane);
    }
}

/// Whether two arrangements put the same panels in the same places, with
/// dividers within a whisker of each other. Which tab is in front, sizes the
/// window gave them, and scrolling don't count.
pub fn same_arrangement(a: &DockState<Pane>, b: &DockState<Pane>) -> bool {
    let shape = |dock: &DockState<Pane>| -> Vec<Vec<NodeShape>> {
        dock.iter_surfaces()
            .filter_map(|surface| match surface {
                Surface::Main(tree) | Surface::Window(tree, _) => Some(tree_shape(tree)),
                Surface::Empty => None,
            })
            .collect()
    };
    let (a, b) = (shape(a), shape(b));
    a.len() == b.len()
        && a.iter()
            .zip(&b)
            .all(|(x, y)| x.len() == y.len() && x.iter().zip(y).all(|(m, n)| m.same_as(n)))
}

#[derive(Debug)]
enum NodeShape {
    Empty,
    Leaf(Vec<Pane>),
    Split { vertical: bool, fraction: f32 },
}

impl NodeShape {
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty, Self::Empty) => true,
            (Self::Leaf(a), Self::Leaf(b)) => a == b,
            (
                Self::Split {
                    vertical: va,
                    fraction: fa,
                },
                Self::Split {
                    vertical: vb,
                    fraction: fb,
                },
            ) => va == vb && (fa - fb).abs() < 0.01,
            _ => false,
        }
    }
}

fn tree_shape(tree: &Tree<Pane>) -> Vec<NodeShape> {
    let mut shape: Vec<NodeShape> = tree
        .iter()
        .map(|node| match node {
            Node::Empty => NodeShape::Empty,
            Node::Leaf(leaf) => NodeShape::Leaf(leaf.tabs.clone()),
            Node::Vertical(split) => NodeShape::Split {
                vertical: true,
                fraction: split.fraction,
            },
            Node::Horizontal(split) => NodeShape::Split {
                vertical: false,
                fraction: split.fraction,
            },
        })
        .collect();
    // Trailing empty slots are room the tree kept, not part of the shape.
    while matches!(shape.last(), Some(NodeShape::Empty)) {
        shape.pop();
    }
    shape
}

/// Keep each panel's first appearance only: the tab id is the panel, and two
/// tabs with one id confuse the dock.
fn dedupe(dock: &mut DockState<Pane>) {
    let mut seen = std::collections::HashSet::new();
    dock.retain_tabs(|pane| seen.insert(*pane));
}

/// Whether `egui_dock` can draw `dock` without tripping over itself: every split
/// has both halves, every leaf has a tab in front that exists. A file edited
/// by hand, or written by a later Varda, may not hold to that.
fn is_drawable(dock: &DockState<Pane>) -> bool {
    dock.iter_surfaces().all(|surface| match surface {
        Surface::Empty => true,
        Surface::Main(tree) | Surface::Window(tree, _) => tree_is_drawable(tree),
    })
}

fn tree_is_drawable(tree: &Tree<Pane>) -> bool {
    let nodes: Vec<&Node<Pane>> = tree.iter().collect();
    let present = |i: usize| nodes.get(i).is_some_and(|n| !n.is_empty());
    nodes.iter().enumerate().all(|(i, node)| match node {
        Node::Empty => true,
        Node::Leaf(leaf) => leaf.tabs.is_empty() || leaf.active.0 < leaf.tabs.len(),
        Node::Vertical(split) | Node::Horizontal(split) => {
            (0.0..=1.0).contains(&split.fraction) && present(2 * i + 1) && present(2 * i + 2)
        }
    }) && nodes.iter().enumerate().skip(1).all(|(i, node)| {
        // Nothing hangs below a leaf or an empty slot.
        node.is_empty() || matches!(nodes.get((i - 1) / 2), Some(n) if n.is_parent())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panes(dock: &DockState<Pane>) -> Vec<Pane> {
        dock.iter_all_tabs().map(|(_, p)| *p).collect()
    }

    #[test]
    fn each_builtin_has_a_desk_and_no_panel_twice() {
        for builtin in Builtin::ALL {
            let dock = builtin.dock();
            let all = panes(&dock);
            let unique: std::collections::HashSet<_> = all.iter().collect();
            assert_eq!(unique.len(), all.len(), "{builtin:?}: {all:?}");
            assert!(
                all.iter().any(|p| p.is_desk()),
                "{builtin:?} shows no channels"
            );
            assert!(is_drawable(&dock), "{builtin:?}");
        }
    }

    #[test]
    fn each_builtin_is_what_its_name_says() {
        assert!(has_pane(&Builtin::Broadcast.dock(), Pane::Monitors));
        assert!(has_pane(&Builtin::Broadcast.dock(), Pane::Inputs));
        assert!(has_pane(&Builtin::Vj.dock(), Pane::Layers));
        assert!(has_pane(&Builtin::Vj.dock(), Pane::Output));
        assert!(has_pane(&Builtin::Classic.dock(), Pane::Channels));
        assert!(!has_pane(&Builtin::Classic.dock(), Pane::Monitors));
    }

    #[test]
    fn builtins_are_found_by_name_whatever_the_case() {
        assert_eq!(Builtin::named("broadcast"), Some(Builtin::Broadcast));
        assert_eq!(Builtin::named(" vj "), Some(Builtin::Vj));
        assert_eq!(Builtin::named("My show"), None);
    }

    /// Shows and saved layouts written before the built-ins were renamed
    /// still name them as they were.
    #[test]
    fn builtins_are_found_by_the_names_they_had_before() {
        assert_eq!(Builtin::named("vmix"), Some(Builtin::Broadcast));
        assert_eq!(Builtin::named(" RESOLUME "), Some(Builtin::Vj));
        assert_eq!(Builtin::named("Classic"), Some(Builtin::Classic));
    }

    #[test]
    fn a_show_saved_with_a_builtins_old_name_opens_it_by_its_new_one() {
        // As Nida 0.5 wrote it: no `naming`.
        let mut json = serde_json::to_value(ScreenLayout::builtin(Builtin::Vj)).unwrap();
        json["name"] = "Resolume".into();
        json.as_object_mut().unwrap().remove("naming");
        let back = serde_json::from_value::<ScreenLayout>(json)
            .unwrap()
            .sanitized();
        assert_eq!((back.name.as_str(), back.naming), ("VJ", NAMING));
        assert!(same_arrangement(&back.dock, &Builtin::Vj.dock()));
        let mut layout = ScreenLayout::builtin(Builtin::Broadcast);
        (layout.name, layout.naming) = ("vMix".into(), 0);
        assert_eq!(layout.sanitized().name, "Broadcast");
        // A "Broadcast" then was the operator's own: left for reconnect.
        let mut layout = ScreenLayout::builtin(Builtin::Broadcast);
        layout.naming = 0;
        let layout = layout.sanitized();
        assert_eq!((layout.name.as_str(), layout.naming), ("Broadcast", 0));
    }

    /// A show from before Broadcast was a built-in, on a layout the
    /// operator saved as "Broadcast", is named after that layout again —
    /// changed since or not, and after the layouts' file was written again.
    /// A show since, on the built-in, stays on it, however its panels lie;
    /// so does an old show that was on vMix.
    #[test]
    fn a_show_on_a_layout_saved_under_a_builtins_new_name_finds_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layouts.json");
        let mut mine = Builtin::Classic.dock();
        remove_pane(&mut mine, Pane::Setup);
        let file = SavedFile {
            layouts: vec![SavedLayout {
                name: "broadcast".into(),
                dock: mine.clone(),
                was: None,
            }],
        };
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();
        let old_show = |dock: &DockState<Pane>| {
            ScreenLayout {
                name: "Broadcast".into(),
                dock: dock.clone(),
                locked: false,
                naming: 0,
            }
            .sanitized()
        };
        let mut store = SavedLayoutStore::open(path.clone());
        let mut show = old_show(&mine);
        store.reconnect(&mut show);
        assert_eq!(
            (show.name.as_str(), show.naming),
            ("broadcast (saved)", NAMING)
        );
        let mut changed = mine.clone();
        remove_pane(&mut changed, Pane::Audio);
        let mut show = old_show(&changed);
        store.reconnect(&mut show);
        assert_eq!(show.name, "broadcast (saved)");

        store.save_as("Other", &Builtin::Vj.dock()).unwrap();
        let store = SavedLayoutStore::open(path);
        let mut show = old_show(&mine);
        store.reconnect(&mut show);
        assert_eq!(
            show.name, "broadcast (saved)",
            "the old name is kept in the file"
        );

        let mut since = ScreenLayout {
            dock: mine.clone(),
            ..ScreenLayout::builtin(Builtin::Broadcast)
        };
        store.reconnect(&mut since);
        assert_eq!(since.name, "Broadcast");
        let mut on_vmix = ScreenLayout {
            name: "vMix".into(),
            dock: mine,
            locked: false,
            naming: 0,
        }
        .sanitized();
        store.reconnect(&mut on_vmix);
        assert_eq!(on_vmix.name, "Broadcast");
    }

    /// A layout saved as "VJ" before VJ was a built-in is not lost.
    #[test]
    fn a_saved_layout_with_a_builtins_new_name_is_kept_under_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layouts.json");
        let saved = |name: &str| SavedLayout {
            name: name.into(),
            dock: Builtin::Classic.dock(),
            was: None,
        };
        let file = SavedFile {
            layouts: vec![
                saved("VJ"),
                saved("vj (saved)"),
                saved("Broadcast"),
                saved("Mine"),
            ],
        };
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();
        let names: Vec<String> = load_saved(&path).into_iter().map(|l| l.name).collect();
        assert_eq!(
            names,
            ["VJ (saved 2)", "vj (saved)", "Broadcast (saved)", "Mine"]
        );
    }

    #[test]
    fn a_closed_panel_comes_back_where_it_belongs() {
        let mut dock = Builtin::Classic.dock();
        assert!(remove_pane(&mut dock, Pane::Library));
        assert!(!has_pane(&dock, Pane::Library));
        add_pane(&mut dock, Pane::Library);
        assert!(has_pane(&dock, Pane::Library));
        // Down the left: the root now splits sideways with the Library first.
        let tree = dock.main_surface();
        assert!(tree[NodeIndex::root()].is_horizontal());
        assert_eq!(
            tree[NodeIndex::root().left()].tabs(),
            Some(&[Pane::Library][..])
        );
        assert!(is_drawable(&dock));
    }

    #[test]
    fn a_desk_joins_the_other_desk() {
        let mut dock = Builtin::Classic.dock();
        add_pane(&mut dock, Pane::Inputs);
        let (channels, _) = dock.main_surface().find_tab(&Pane::Channels).unwrap();
        let (inputs, _) = dock.main_surface().find_tab(&Pane::Inputs).unwrap();
        assert_eq!(channels, inputs, "same tab group");
    }

    #[test]
    fn toggling_twice_leaves_the_panel_open_somewhere() {
        let mut dock = Builtin::Broadcast.dock();
        toggle_pane(&mut dock, Pane::Output);
        assert!(has_pane(&dock, Pane::Output));
        toggle_pane(&mut dock, Pane::Output);
        assert!(!has_pane(&dock, Pane::Output));
    }

    #[test]
    fn a_panel_opens_in_an_empty_window() {
        let mut dock = Builtin::Broadcast.dock();
        for pane in Pane::ALL {
            remove_pane(&mut dock, pane);
        }
        assert_eq!(dock.main_surface().num_tabs(), 0);
        add_pane(&mut dock, Pane::Switcher);
        assert_eq!(panes(&dock), vec![Pane::Switcher]);
        add_pane(&mut dock, Pane::Library);
        assert!(is_drawable(&dock));
    }

    #[test]
    fn an_arrangement_equals_itself_but_not_a_moved_divider_or_panel() {
        let a = Builtin::Vj.dock();
        assert!(same_arrangement(&a, &Builtin::Vj.dock()));
        assert!(!same_arrangement(&a, &Builtin::Broadcast.dock()));

        let mut moved = Builtin::Vj.dock();
        if let Node::Vertical(split) = &mut moved.main_surface_mut()[NodeIndex::root()] {
            split.fraction += 0.1;
        }
        assert!(!same_arrangement(&a, &moved));

        let mut closed = Builtin::Vj.dock();
        remove_pane(&mut closed, Pane::Audio);
        assert!(!same_arrangement(&a, &closed));
    }

    #[test]
    fn a_panel_twice_keeps_its_first_place() {
        let mut dock = Builtin::Classic.dock();
        dock.push_to_first_leaf(Pane::Output);
        let layout = ScreenLayout {
            name: "x".into(),
            dock,
            locked: false,
            naming: NAMING,
        }
        .sanitized();
        let outputs = panes(&layout.dock)
            .into_iter()
            .filter(|p| *p == Pane::Output)
            .count();
        assert_eq!(outputs, 1);
    }

    #[test]
    fn a_damaged_arrangement_falls_back_to_classic() {
        let mut dock = Builtin::Classic.dock();
        // A split whose halves are gone.
        let tree = dock.main_surface_mut();
        *tree = Tree::new(vec![Pane::Library]);
        tree[NodeIndex::root()] = Node::Horizontal(egui_dock::SplitNode::new(
            egui::Rect::NOTHING,
            0.5,
            false,
            0,
        ));
        let layout = ScreenLayout {
            name: "Broken".into(),
            dock,
            locked: true,
            naming: NAMING,
        }
        .sanitized();
        assert_eq!(layout.name, "Classic");
        assert!(layout.locked, "the lock is the operator's, kept");
        assert!(same_arrangement(&layout.dock, &Builtin::Classic.dock()));
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(valid_name("  My show "), Ok("My show"));
        assert!(valid_name("   ").is_err());
        assert!(valid_name("Broadcast").is_err(), "a built-in's name");
        assert!(valid_name("vMix").is_err(), "a built-in's old name");
        assert!(valid_name(&"x".repeat(41)).is_err());
    }

    #[test]
    fn saving_a_name_again_replaces_it() {
        let mut saved = Vec::new();
        assert!(!upsert(&mut saved, "Show", Builtin::Broadcast.dock()));
        assert!(upsert(&mut saved, "show", Builtin::Vj.dock()));
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "show");
        assert!(same_arrangement(&saved[0].dock, &Builtin::Vj.dock()));
        assert!(remove(&mut saved, "SHOW"));
        assert!(saved.is_empty());
        assert!(!remove(&mut saved, "Show"));
    }

    #[test]
    fn saved_layouts_survive_a_round_trip_through_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("layouts.json");
        assert!(load_saved(&path).is_empty(), "no file yet");
        let mut dock = Builtin::Broadcast.dock();
        remove_pane(&mut dock, Pane::Setup);
        let saved = vec![SavedLayout {
            name: "Conference".into(),
            dock,
            was: None,
        }];
        store_saved(&path, &saved).unwrap();
        let loaded = load_saved(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Conference");
        assert!(same_arrangement(&loaded[0].dock, &saved[0].dock));
    }

    #[test]
    fn the_store_writes_every_change_to_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layouts.json");
        let mut store = SavedLayoutStore::open(path.clone());
        assert_eq!(
            store.save_as("  Show day ", &Builtin::Broadcast.dock()),
            Ok("Show day".to_string())
        );
        assert!(store.save_as("VJ", &Builtin::Broadcast.dock()).is_err());
        assert_eq!(SavedLayoutStore::open(path.clone()).layouts().len(), 1);
        assert!(store.delete("show day").is_ok());
        assert!(store.delete("show day").is_err(), "already gone");
        assert!(SavedLayoutStore::open(path).layouts().is_empty());
    }

    /// Handed to the show's writer, the file is what a save to it would
    /// write, and the layout is kept at once; a write refused keeps nothing.
    #[test]
    fn the_store_hands_its_file_to_be_written_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layouts.json");
        let mut store = SavedLayoutStore::open(path.clone());
        let mut handed = None;
        let saved = store.save_as_with("Show day", &Builtin::Broadcast.dock(), |to, text| {
            handed = Some((to.to_path_buf(), text));
            Ok(())
        });
        assert_eq!(saved, Ok("Show day".to_string()));
        assert_eq!(store.layouts().len(), 1, "kept at once");
        assert!(!path.exists(), "written by whoever it was handed to");
        let (to, text) = handed.unwrap();
        assert_eq!(to, path);
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            SavedLayoutStore::open(path.clone()).layouts()[0].name,
            "Show day"
        );

        let refused = store.delete_with("Show day", |_, _| Err("disk full".into()));
        assert_eq!(refused, Err("disk full".to_string()));
        assert_eq!(store.layouts().len(), 1, "nothing changes");
    }

    #[test]
    fn a_damaged_saved_layouts_file_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layouts.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_saved(&path).is_empty());
    }

    #[test]
    fn a_screen_layout_survives_json() {
        let mut layout = ScreenLayout::builtin(Builtin::Vj);
        layout.locked = true;
        let json = serde_json::to_string(&layout).unwrap();
        let back: ScreenLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "VJ");
        assert!(back.locked);
        assert!(same_arrangement(&back.dock, &layout.dock));
    }
}
