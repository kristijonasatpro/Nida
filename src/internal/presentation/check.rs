//! A talk's file looked over before the show, for what goes wrong on the day
//! and can still be put right: a video a deck links to that was not copied
//! with it, fonts this Mac does not have (PowerPoint draws others), a deck of
//! another shape than the show, a web page that needs the internet or a
//! picture of its that is not there. Warnings only — the talk plays all the
//! same.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use crate::engine::value::slides::TalkKind;

/// The shapes are told apart from this far on (4:3 against 16:9 is 25 %).
const ASPECT_SLACK: f64 = 0.03;

/// A font installed while Nida runs is seen by checks this long after.
const FONTS_FRESH: Duration = Duration::from_secs(5);

/// A part of a deck read for looking over is at most this big, and all the
/// parts read of one deck together this much: a deck's XML is small, a zip
/// may claim anything, and a look over must never be what fills the memory
/// of the show (running out of it ends Nida, whatever catches panics).
const MAX_PART: u64 = 16 * 1024 * 1024;
const MAX_READ: u64 = 128 * 1024 * 1024;

/// A zip of more parts than this, or a longer list of them, is no deck to
/// look into.
const MAX_PARTS: u64 = 20_000;
const MAX_LIST: u64 = 64 * 1024 * 1024;

/// Of a web page, at most this much is looked over.
const MAX_PAGE: u64 = 64 * 1024 * 1024;

/// The slide size PowerPoint takes when a deck names none: 4:3, EMU.
const DEFAULT_SLIDE: (f64, f64) = (9_144_000.0, 6_858_000.0);

/// What may go wrong with the talk at `path`, played in a picture of
/// width over height `aspect`; nothing for a file that looks right.
#[must_use]
pub fn check(path: &Path, aspect: f64) -> Vec<String> {
    check_with(path, aspect, &mut installed_fonts)
}

/// [`check`], the fonts this Mac has asked of `mac_fonts` (only for a deck).
fn check_with(path: &Path, aspect: f64, mac_fonts: &mut dyn FnMut() -> BTreeSet<String>) -> Vec<String> {
    let Some(kind) = TalkKind::of(&path.to_string_lossy()) else {
        return Vec::new();
    };
    if !path.is_file() {
        return Vec::new();
    }
    match kind {
        TalkKind::PowerPoint => check_pptx(path, aspect, mac_fonts),
        TalkKind::Pdf => check_pdf(path, aspect),
        TalkKind::Web => read_page(path)
            .map(|html| check_html(&html, path.parent().unwrap_or(path)))
            .unwrap_or_default(),
        TalkKind::Clip | TalkKind::Picture => Vec::new(),
    }
}

/// A deck of `deck` width over height in a picture of `picture`'s, when
/// they differ: said in words — the shapes by their names where they have
/// one (a PDF of A4 pages standing up too), and how much of the picture the
/// talk fills.
fn shape_warning(deck: f64, picture: f64) -> Option<String> {
    if !deck.is_finite() || !picture.is_finite() || deck <= 0.0 || picture <= 0.0 {
        return None;
    }
    if (deck / picture - 1.0).abs() <= ASPECT_SLACK {
        return None;
    }
    let name = |a: f64| {
        for (shape, of) in [
            ("16:9", 16.0 / 9.0),
            ("4:3", 4.0 / 3.0),
            ("16:10", 16.0 / 10.0),
            ("1:1", 1.0),
            ("3:4", 3.0 / 4.0),
            ("9:16", 9.0 / 16.0),
            ("A4 portrait", 1.0 / std::f64::consts::SQRT_2),
            ("A4 landscape", std::f64::consts::SQRT_2),
            ("Letter portrait", 8.5 / 11.0),
        ] {
            if (a / of - 1.0).abs() <= ASPECT_SLACK {
                return shape.to_string();
            }
        }
        if a < 1.0 {
            format!("portrait {a:.2}:1")
        } else {
            format!("{a:.2}:1")
        }
    };
    let (talk, show) = (name(deck), name(picture));
    let a = |name: &str| if name.starts_with('A') { "an" } else { "a" };
    let (share, along, side) = if deck < picture {
        (deck / picture, "width", "at its sides")
    } else {
        (picture / deck, "height", "above and below it")
    };
    Some(format!(
        "{} {talk} talk in {} {show} show: it fills {:.0} % of the {along}, black bars {side}",
        a(&talk),
        a(&show),
        share * 100.0
    ))
}

fn check_pdf(path: &Path, aspect: f64) -> Vec<String> {
    match crate::presentation::pdf::PdfPages::open(path) {
        Ok(pages) => pages
            .aspect(1)
            .and_then(|a| shape_warning(a, aspect))
            .into_iter()
            .collect(),
        Err(e) => vec![format!("{e:#}")],
    }
}

/// The fonts that come with Microsoft Office for Mac: PowerPoint has them
/// though macOS does not list them.
const OFFICE_FONTS: &[&str] = &[
    "aptos",
    "aptos display",
    "aptos narrow",
    "aptos serif",
    "aptos mono",
    "calibri",
    "calibri light",
    "cambria",
    "cambria math",
    "candara",
    "consolas",
    "constantia",
    "corbel",
    "franklin gothic",
    "franklin gothic book",
    "franklin gothic medium",
    "gill sans mt",
    "segoe ui",
    "segoe ui light",
    "segoe ui semibold",
    "segoe ui semilight",
    "century gothic",
    "tw cen mt",
    "garamond",
    "bookman old style",
    "book antiqua",
    "wingdings",
    "wingdings 2",
    "wingdings 3",
    "webdings",
    "symbol",
    "arial",
    "arial black",
    "arial narrow",
    "times new roman",
    "courier new",
    "georgia",
    "verdana",
    "tahoma",
    "trebuchet ms",
    "impact",
    "comic sans ms",
];

fn check_pptx(path: &Path, aspect: f64, mac_fonts: &mut dyn FnMut() -> BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(mut file) = std::fs::File::open(path) else {
        return out;
    };
    // Not a zip (an old .ppt, which PowerPoint plays), or one claiming more
    // parts than a deck has — counted before it is opened, as opening it
    // reserves room for every one: nothing to look at.
    if !crate::presentation::pptx::zip_fits(&mut file, MAX_PARTS, MAX_LIST) {
        return out;
    }
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return out;
    };
    let mut budget = MAX_READ;
    // The slide size, and the fonts the deck carries — from its
    // presentation part alone.
    let mut embedded = BTreeSet::new();
    if let Some(text) = read_part(&mut zip, "ppt/presentation.xml", MAX_PART, &mut budget) {
        if let Ok(doc) = roxmltree::Document::parse(&text) {
            let size = doc
                .descendants()
                .find(|n| n.tag_name().name() == "sldSz")
                .and_then(|n| {
                    let emu = |name| n.attribute(name)?.parse::<f64>().ok().filter(|v| *v > 0.0);
                    Some((emu("cx")?, emu("cy")?))
                })
                .unwrap_or(DEFAULT_SLIDE);
            out.extend(shape_warning(size.0 / size.1, aspect));
            for font in doc.descendants().filter(|n| n.tag_name().name() == "embeddedFont") {
                for face in font.children().filter(|n| n.tag_name().name() == "font") {
                    if let Some(name) = face.attribute("typeface") {
                        embedded.insert(name.trim().to_lowercase());
                    }
                }
            }
        }
    }
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let folder = path.parent().unwrap_or(path);
    let mut missing_media = BTreeSet::new();
    let mut fonts = BTreeSet::new();
    // Part names are matched without regard to case, as the format has them.
    let ends = |name: &str, ext: &str| {
        Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    };
    for name in &names {
        let rels = name.starts_with("ppt/slides/_rels/") && ends(name, "rels");
        let xml = (name.starts_with("ppt/slides/slide")
            || name.starts_with("ppt/theme/")
            || name.starts_with("ppt/slideMasters/")
            || name.starts_with("ppt/slideLayouts/"))
            && ends(name, "xml");
        if !rels && !xml {
            continue;
        }
        if budget == 0 {
            // As much read as one deck is given: what is found so far.
            break;
        }
        let Some(text) = read_part(&mut zip, name, MAX_PART, &mut budget) else {
            continue;
        };
        let Ok(doc) = roxmltree::Document::parse(&text) else {
            continue;
        };
        if rels {
            for rel in doc.descendants().filter(|n| n.tag_name().name() == "Relationship") {
                let external = rel.attribute("TargetMode") == Some("External");
                let kind = rel.attribute("Type").unwrap_or_default();
                let media = ["/video", "/audio", "/media"].iter().any(|k| kind.ends_with(k));
                if external && media {
                    if let Some(target) = rel.attribute("Target") {
                        if let Some(file) = linked_file(target, folder) {
                            if !file.exists() {
                                let shown = file
                                    .file_name()
                                    .map_or_else(|| target.to_string(), |f| f.to_string_lossy().into_owned());
                                missing_media.insert(shown);
                            }
                        }
                    }
                }
            }
        } else {
            for node in doc.descendants() {
                if matches!(node.tag_name().name(), "latin" | "ea" | "cs") {
                    if let Some(face) = node.attribute("typeface") {
                        let face = face.trim();
                        // Theme references (+mj-lt) and empty names name
                        // no font of their own.
                        if !face.is_empty() && !face.starts_with('+') {
                            fonts.insert(face.to_string());
                        }
                    }
                }
            }
        }
    }
    if !missing_media.is_empty() {
        let list: Vec<String> = missing_media.into_iter().collect();
        out.push(format!(
            "linked videos or sounds not there (copy them beside the deck): {}",
            list.join(", ")
        ));
    }
    // A font the deck carries is no font missing; any other may be.
    fonts.retain(|f| !embedded.contains(&f.to_lowercase()));
    if !fonts.is_empty() {
        let installed = mac_fonts();
        if !installed.is_empty() {
            let missing: Vec<String> = fonts
                .into_iter()
                .filter(|f| !font_is_there(f, &installed))
                .collect();
            if !missing.is_empty() {
                out.push(format!(
                    "fonts not on this Mac — PowerPoint will use others: {}",
                    missing.join(", ")
                ));
            }
        }
    }
    out
}

/// Whether the font named `face` is one this Mac (or Office) has: by its
/// family's name, or a family whose name begins it (`Montserrat` for
/// `Montserrat SemiBold`).
fn font_is_there(face: &str, installed: &BTreeSet<String>) -> bool {
    let face = face.to_lowercase();
    OFFICE_FONTS.contains(&face.as_str())
        || installed.contains(&face)
        || installed.iter().any(|family| {
            face.len() > family.len()
                && face.starts_with(family.as_str())
                && face[family.len()..].starts_with(' ')
        })
}

/// The file a deck's linked media names: a `file://` URL, or a path beside
/// the deck.
fn linked_file(target: &str, folder: &Path) -> Option<std::path::PathBuf> {
    if let Some(rest) = target.strip_prefix("file://") {
        let path = percent_decode(rest.trim_start_matches("localhost"));
        return Some(std::path::PathBuf::from(path));
    }
    if target.contains("://") {
        // On the internet: not looked for.
        return None;
    }
    Some(folder.join(percent_decode(target)))
}

fn percent_decode(text: &str) -> String {
    let hex = |b: u8| char::from(b).to_digit(16);
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(&h), Some(&l)) = (bytes.get(i + 1), bytes.get(i + 2)) {
                if let (Some(h), Some(l)) = (hex(h), hex(l)) {
                    out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The part `name` of a deck, as text: none when bigger than `cap` — what
/// it really holds, whatever the zip claims — or than what is left of the
/// deck's `budget`, which what is read comes off.
fn read_part<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    cap: u64,
    budget: &mut u64,
) -> Option<String> {
    let entry = zip.by_name(name).ok()?;
    let cap = cap.min(*budget);
    if entry.size() > cap {
        return None;
    }
    let mut bytes = Vec::new();
    let read = entry.take(cap.saturating_add(1)).read_to_end(&mut bytes);
    *budget = budget.saturating_sub(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    read.ok()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// A web page's text: as much of it as is looked over.
fn read_page(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_PAGE)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A file's size and when it was last written: changed, it is checked again.
pub type Stamp = (u64, Option<SystemTime>);

/// Talks' files looked over on a thread of their own — each once, until its
/// file changes or it is asked again — and what was found read back at once
/// by the frame that shows it.
#[derive(Default)]
pub struct Checker {
    found: Arc<Mutex<Found>>,
    asks: Mutex<Option<mpsc::Sender<Ask>>>,
}

#[derive(Default)]
struct Found {
    /// By file: the stamp, shape and round it was looked over at, and what
    /// was found.
    done: HashMap<String, Done>,
    /// Asked, not yet answered.
    asked: HashSet<(String, Stamp, u32, u64)>,
    /// How many times each file, and every file, has been asked to be looked
    /// over again: an answer to an earlier round is let go.
    again: HashMap<String, u64>,
    again_all: u64,
}

struct Done {
    stamp: Stamp,
    shape: u32,
    round: u64,
    warnings: Vec<String>,
}

impl Found {
    fn round(&self, path: &str) -> u64 {
        self.again_all + self.again.get(path).copied().unwrap_or(0)
    }
}

struct Ask {
    path: String,
    stamp: Stamp,
    shape: u32,
    round: u64,
    aspect: f64,
}

/// The picture's shape to a thousandth: the same show asks the same.
fn shape_key(aspect: f64) -> u32 {
    if aspect.is_finite() && aspect > 0.0 {
        // Bounded above: a cast that cannot wrap.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let key = (aspect * 1000.0).round().min(f64::from(u32::MAX)) as u32;
        key
    } else {
        0
    }
}

fn lock(found: &Mutex<Found>) -> MutexGuard<'_, Found> {
    found.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Checker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What was found in the file at `path`, as it is (`stamp`), shown in a
    /// picture of `aspect`: asked of the thread the first time, and its last
    /// findings shown meanwhile.
    pub fn warnings(&self, path: &str, stamp: Stamp, aspect: f64) -> Vec<String> {
        let shape = shape_key(aspect);
        let (known, round, ask) = {
            let mut found = lock(&self.found);
            let round = found.round(path);
            let known = found.done.get(path);
            if known.is_some_and(|d| d.stamp == stamp && d.shape == shape && d.round == round) {
                return known.map(|d| d.warnings.clone()).unwrap_or_default();
            }
            let known = known.map(|d| d.warnings.clone()).unwrap_or_default();
            let ask = found.asked.insert((path.to_string(), stamp, shape, round));
            (known, round, ask)
        };
        if ask {
            self.ask(Ask {
                path: path.to_string(),
                stamp,
                shape,
                round,
                aspect,
            });
        }
        known
    }

    /// Look these files over again — or every file — when next shown: a
    /// video copied beside a deck, or a font installed, changes no talk's
    /// file.
    /// What was found shows on until the new look has been had; an answer
    /// to a look asked before this is let go.
    pub fn check_again(&self, paths: Option<&[String]>) {
        let mut found = lock(&self.found);
        match paths {
            Some(paths) => {
                for path in paths {
                    *found.again.entry(path.clone()).or_default() += 1;
                }
            }
            None => found.again_all += 1,
        }
    }

    fn ask(&self, ask: Ask) {
        let mut asks = self.asks.lock().unwrap_or_else(PoisonError::into_inner);
        if asks.is_none() {
            let (tx, rx) = mpsc::channel::<Ask>();
            let found = Arc::clone(&self.found);
            let spawned = std::thread::Builder::new()
                .name("talk-check".into())
                .spawn(move || look_over(&rx, &found));
            if let Err(e) = spawned {
                log::warn!("Talks' files cannot be looked over: {e}");
                return;
            }
            *asks = Some(tx);
        }
        if let Some(tx) = asks.as_ref() {
            let _ = tx.send(ask);
        }
    }
}

/// The checker's thread: each file asked, looked over; ends with the
/// checker.
fn look_over(asks: &mpsc::Receiver<Ask>, found: &Mutex<Found>) {
    let mut fonts: Option<(Instant, BTreeSet<String>)> = None;
    while let Ok(ask) = asks.recv() {
        let mut fonts_now = || {
            if fonts.as_ref().is_none_or(|(at, _)| at.elapsed() > FONTS_FRESH) {
                fonts = Some((Instant::now(), installed_fonts()));
            }
            fonts.as_ref().map(|(_, f)| f.clone()).unwrap_or_default()
        };
        let warnings = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check_with(Path::new(&ask.path), ask.aspect, &mut fonts_now)
        }))
        .unwrap_or_else(|_| vec!["Nida could not look this file over".to_string()]);
        record(found, ask, warnings);
    }
}

/// What a look over found, kept — unless the file has been asked to be
/// looked over again since it was asked: then it is let go, and the next
/// frame that shows the file asks anew.
fn record(found: &Mutex<Found>, ask: Ask, warnings: Vec<String>) {
    let mut found = lock(found);
    found
        .asked
        .remove(&(ask.path.clone(), ask.stamp, ask.shape, ask.round));
    if ask.round == found.round(&ask.path) {
        found.done.insert(
            ask.path,
            Done {
                stamp: ask.stamp,
                shape: ask.shape,
                round: ask.round,
                warnings,
            },
        );
    }
}

/// The font families this Mac has, lower case; none off macOS (fonts not
/// looked at then).
#[cfg(target_os = "macos")]
fn installed_fonts() -> BTreeSet<String> {
    use objc2_core_foundation::{CFArray, CFRetained, CFString};
    #[link(name = "CoreText", kind = "framework")]
    extern "C" {
        fn CTFontManagerCopyAvailableFontFamilyNames() -> Option<std::ptr::NonNull<CFArray>>;
    }
    // SAFETY: CoreText's documented copy function: an array of strings we
    // own, released by the CFRetained.
    let Some(array) = (unsafe { CTFontManagerCopyAvailableFontFamilyNames() }) else {
        return BTreeSet::new();
    };
    let array: CFRetained<CFArray> = unsafe { CFRetained::from_raw(array) };
    // SAFETY: the array holds CFStrings, as documented.
    let array: CFRetained<CFArray<CFString>> = unsafe { CFRetained::cast_unchecked(array) };
    array.iter().map(|name| name.to_string().to_lowercase()).collect()
}

#[cfg(not(target_os = "macos"))]
fn installed_fonts() -> BTreeSet<String> {
    BTreeSet::new()
}

/// A web page's needs: the hosts it loads from (it needs the internet for
/// them) and the files of its own it names that are not beside it.
#[must_use]
pub fn check_html(html: &str, folder: &Path) -> Vec<String> {
    let mut hosts = BTreeSet::new();
    let mut missing = BTreeSet::new();
    for reference in references(html) {
        if let Some(rest) = reference
            .strip_prefix("https://")
            .or_else(|| reference.strip_prefix("http://"))
            .or_else(|| reference.strip_prefix("//"))
        {
            let host = rest.split(['/', '?', '#', ':']).next().unwrap_or_default();
            if !host.is_empty() {
                hosts.insert(host.to_lowercase());
            }
        } else if !reference.contains(':') && !reference.starts_with('#') && !reference.is_empty() {
            let file = reference.split(['?', '#']).next().unwrap_or_default();
            if !file.is_empty() && !folder.join(percent_decode(file)).exists() {
                missing.insert(file.to_string());
            }
        }
    }
    let mut out = Vec::new();
    if !hosts.is_empty() {
        let list: Vec<String> = hosts.into_iter().collect();
        out.push(format!(
            "needs the internet when shown — it loads from {}",
            list.join(", ")
        ));
    }
    if !missing.is_empty() {
        let list: Vec<String> = missing.into_iter().take(5).collect();
        out.push(format!("files it shows are not beside it: {}", list.join(", ")));
    }
    out
}

/// What a page loads: its `src` and stylesheet `href` attributes, and the
/// `url(…)`s and `@import`s of its styles.
fn references(html: &str) -> Vec<String> {
    // ASCII case folding only: its byte offsets are the page's (a full one
    // changes some letters' lengths, and the page is sliced by them).
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut take_quoted = |from: usize| {
        let rest = &html[from..];
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            return;
        };
        if let Some(end) = rest[1..].find(quote) {
            out.push(rest[1..=end].trim().to_string());
        }
    };
    for key in ["src=", "src =", "<link"] {
        let mut at = 0;
        while let Some(found) = lower[at..].find(key) {
            let start = at + found;
            if key == "<link" {
                // A stylesheet or font a <link> brings in, not a page it
                // merely names.
                let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
                let tag = &lower[start..end];
                if tag.contains("stylesheet") || tag.contains("preload") || tag.contains("icon") {
                    if let Some(h) = tag.find("href=") {
                        take_quoted(start + h + "href=".len());
                    }
                }
            } else {
                take_quoted(start + key.len());
            }
            at = start + key.len();
        }
    }
    let mut at = 0;
    while let Some(found) = lower[at..].find("url(") {
        let start = at + found + "url(".len();
        let end = lower[start..].find(')').map_or(lower.len(), |e| start + e);
        let inner = html[start..end].trim().trim_matches(['"', '\'']).trim();
        if !inner.starts_with("data:") {
            out.push(inner.to_string());
        }
        at = end;
    }
    let mut at = 0;
    while let Some(found) = lower[at..].find("@import") {
        let start = at + found + "@import".len();
        let rest = html[start..].trim_start();
        if let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
            if let Some(end) = rest[1..].find(q) {
                out.push(rest[1..=end].to_string());
            }
        }
        at = start;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Said in words an operator reads at a glance: the shapes by name — a
    /// PDF of A4 pages standing up too — and how much of the picture the
    /// talk fills.
    #[test]
    fn a_talk_of_another_shape_than_the_show_is_said_so() {
        assert_eq!(shape_warning(16.0 / 9.0, 16.0 / 9.0), None);
        assert_eq!(
            shape_warning(4.0 / 3.0, 16.0 / 9.0).as_deref(),
            Some("a 4:3 talk in a 16:9 show: it fills 75 % of the width, black bars at its sides")
        );
        assert_eq!(
            shape_warning(16.0 / 9.0, 4.0 / 3.0).as_deref(),
            Some(
                "a 16:9 talk in a 4:3 show: it fills 75 % of the height, black bars above and \
                 below it"
            )
        );
        // A PDF of A4 pages standing up, as a document is: 595 × 842 points.
        assert_eq!(
            shape_warning(595.0 / 842.0, 16.0 / 9.0).as_deref(),
            Some(
                "an A4 portrait talk in a 16:9 show: it fills 40 % of the width, black bars at \
                 its sides"
            )
        );
        assert_eq!(
            shape_warning(0.62, 16.0 / 9.0).as_deref(),
            Some(
                "a portrait 0.62:1 talk in a 16:9 show: it fills 35 % of the width, black bars \
                 at its sides"
            )
        );
        assert_eq!(
            shape_warning(16.0 / 9.0, 595.0 / 842.0).as_deref(),
            Some(
                "a 16:9 talk in an A4 portrait show: it fills 40 % of the height, black bars \
                 above and below it"
            )
        );
        assert_eq!(shape_warning(0.0, 16.0 / 9.0), None, "no shape");
    }

    #[test]
    fn a_web_page_says_what_it_loads_from_the_internet_and_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("img")).unwrap();
        std::fs::write(dir.path().join("img/here.png"), b"x").unwrap();
        let html = r#"<html><head>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter">
<link rel="canonical" href="https://example.com/talk">
<script src="https://cdn.jsdelivr.net/npm/reveal.js/dist/reveal.js"></script>
<style>@import 'https://unpkg.com/x.css'; body{background:url(img/bg.jpg)} .a{background:url("data:image/png;base64,xx")}</style>
</head><body><img src="img/here.png"><img src='img/gone.png'><a href="next.html">next</a></body></html>"#;
        let found = check_html(html, dir.path());
        assert_eq!(
            found,
            vec![
                "needs the internet when shown — it loads from cdn.jsdelivr.net, fonts.googleapis.com, unpkg.com".to_string(),
                "files it shows are not beside it: img/bg.jpg, img/gone.png".to_string(),
            ]
        );
        assert!(check_html("<html><body><h1>Hi</h1></body></html>", dir.path()).is_empty());
    }

    #[test]
    fn a_font_is_there_by_its_family_or_office_has_it() {
        let installed: BTreeSet<String> = ["helvetica neue", "montserrat"].iter().map(|s| (*s).to_string()).collect();
        assert!(font_is_there("Helvetica Neue", &installed));
        assert!(font_is_there("Montserrat SemiBold", &installed));
        assert!(font_is_there("Calibri", &installed), "Office brings it");
        assert!(!font_is_there("Gotham", &installed));
        assert!(!font_is_there("Montserratish", &installed), "a whole word only");
    }

    /// What was found for `path` at `stamp`, once the thread has looked.
    fn settled(checker: &Checker, path: &str, stamp: Stamp) -> Vec<String> {
        for _ in 0..500 {
            let _ = checker.warnings(path, stamp, 16.0 / 9.0);
            {
                let found = lock(&checker.found);
                if let Some(done) = found.done.get(path) {
                    if done.stamp == stamp && done.round == found.round(path) {
                        return done.warnings.clone();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{path} was not looked over");
    }

    fn stamp_of(path: &Path) -> Stamp {
        let meta = std::fs::metadata(path).unwrap();
        (meta.len(), meta.modified().ok())
    }

    #[test]
    fn a_file_is_looked_over_once_until_it_changes_or_is_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("talk.html");
        std::fs::write(&page, r#"<img src="gone.png">"#).unwrap();
        let path = page.to_string_lossy().into_owned();
        let checker = Checker::new();
        let missing = vec!["files it shows are not beside it: gone.png".to_string()];
        assert_eq!(settled(&checker, &path, stamp_of(&page)), missing);
        // The picture put beside it: the page is not looked at again unasked.
        std::fs::write(dir.path().join("gone.png"), b"x").unwrap();
        assert_eq!(checker.warnings(&path, stamp_of(&page), 16.0 / 9.0), missing);
        checker.check_again(Some(std::slice::from_ref(&path)));
        assert!(settled(&checker, &path, stamp_of(&page)).is_empty());
        // The page changed: looked over again by itself.
        std::fs::write(&page, r#"<script src="https://cdn.example.com/deck.js"></script>"#).unwrap();
        assert_eq!(
            settled(&checker, &path, stamp_of(&page)),
            ["needs the internet when shown — it loads from cdn.example.com"]
        );
    }

    /// An answer to a look asked before "Check again" is let go: it cannot
    /// stand for the look asked for after a video was copied beside a deck.
    #[test]
    fn an_answer_from_before_check_again_is_let_go() {
        let checker = Checker::new();
        let stamp: Stamp = (10, None);
        let ask = |round| Ask {
            path: "/talks/deck.pptx".into(),
            stamp,
            shape: shape_key(16.0 / 9.0),
            round,
            aspect: 16.0 / 9.0,
        };
        checker.check_again(Some(&["/talks/deck.pptx".to_string()]));
        record(&checker.found, ask(0), vec!["linked videos or sounds not there".into()]);
        assert!(!lock(&checker.found).done.contains_key("/talks/deck.pptx"));
        record(&checker.found, ask(1), Vec::new());
        assert!(lock(&checker.found).done["/talks/deck.pptx"].warnings.is_empty());
        // Every file asked again: the same.
        checker.check_again(None);
        record(&checker.found, ask(1), vec!["stale".into()]);
        assert!(lock(&checker.found).done["/talks/deck.pptx"].warnings.is_empty());
    }

    /// A part bigger than allowed — as it really is, whatever the zip claims
    /// — or than what is left of the deck's allowance, is not read; what is
    /// read comes off the allowance.
    #[test]
    fn a_decks_parts_are_read_within_their_allowance() {
        use std::io::Write as _;
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut bytes);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("small.xml", options).unwrap();
            zip.write_all(&[b'a'; 100]).unwrap();
            zip.start_file("big.xml", options).unwrap();
            zip.write_all(&[b'b'; 5000]).unwrap();
            zip.finish().unwrap();
        }
        let mut zip = zip::ZipArchive::new(bytes).unwrap();
        let mut budget = 1000;
        assert_eq!(read_part(&mut zip, "small.xml", 4000, &mut budget).map(|t| t.len()), Some(100));
        assert_eq!(budget, 900);
        assert_eq!(read_part(&mut zip, "big.xml", 4000, &mut budget), None, "over the part's cap");
        assert_eq!(read_part(&mut zip, "big.xml", 10_000, &mut budget), None, "over what is left");
        let mut budget = 50;
        assert_eq!(read_part(&mut zip, "small.xml", 4000, &mut budget), None);
    }

    /// A page with letters whose lower case is longer or shorter than they
    /// are is looked over all the same.
    #[test]
    fn a_page_in_any_letters_is_looked_over() {
        let dir = tempfile::tempdir().unwrap();
        let found = check_html("İ<style>body{background:url(é.png)}</style>", dir.path());
        assert_eq!(found, ["files it shows are not beside it: é.png"]);
    }

    #[test]
    fn percent_signs_that_are_not_escapes_are_kept() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zzé%C3%A9"), "%zzéé");
        assert_eq!(percent_decode("%aé"), "%aé");
    }

    #[test]
    fn a_decks_linked_media_is_found_by_url_or_beside_it() {
        let folder = Path::new("/Talks");
        assert_eq!(
            linked_file("file:///Users/x/My%20Clip.mp4", folder),
            Some(std::path::PathBuf::from("/Users/x/My Clip.mp4"))
        );
        assert_eq!(
            linked_file("media/clip.mp4", folder),
            Some(std::path::PathBuf::from("/Talks/media/clip.mp4"))
        );
        assert_eq!(linked_file("https://youtube.com/x", folder), None);
    }

    /// A deck linking a video beside it that is not there, in fonts this
    /// Mac has none of, 4:3: each said — but not a font the deck carries,
    /// though it carries only that one.
    #[test]
    fn a_deck_with_a_missing_video_strange_fonts_and_4_3_is_said_so() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talk.pptx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let p = "http://schemas.openxmlformats.org/presentationml/2006/main";
        let a = "http://schemas.openxmlformats.org/drawingml/2006/main";
        let r = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let rels = "http://schemas.openxmlformats.org/package/2006/relationships";
        let parts = [
            ("ppt/presentation.xml", format!(r#"<p:presentation xmlns:p="{p}" xmlns:r="{r}"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/><p:embeddedFontLst><p:embeddedFont><p:font typeface="Carried Along Zz"/><p:regular r:id="rId9"/></p:embeddedFont></p:embeddedFontLst></p:presentation>"#)),
            ("ppt/fonts/font1.fntdata", "x".to_string()),
            ("ppt/_rels/presentation.xml.rels", format!(r#"<Relationships xmlns="{rels}"><Relationship Id="rId1" Type="{r}/slide" Target="slides/slide1.xml"/></Relationships>"#)),
            ("ppt/slides/slide1.xml", format!(r#"<p:sld xmlns:p="{p}" xmlns:a="{a}"><p:cSld><p:spTree><a:latin typeface="Gotham Rounded Zz"/><a:latin typeface="Carried Along Zz"/><a:latin typeface="+mj-lt"/><a:latin typeface="Calibri"/></p:spTree></p:cSld></p:sld>"#)),
            ("ppt/slides/_rels/slide1.xml.rels", format!(r#"<Relationships xmlns="{rels}"><Relationship Id="rId1" Type="{r}/video" Target="clip%20one.mp4" TargetMode="External"/></Relationships>"#)),
        ];
        for (name, text) in parts {
            zip.start_file(name, options).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let found = check(&path, 16.0 / 9.0);
        assert_eq!(
            found[0],
            "a 4:3 talk in a 16:9 show: it fills 75 % of the width, black bars at its sides"
        );
        assert_eq!(
            found[1],
            "linked videos or sounds not there (copy them beside the deck): clip one.mp4"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            found[2],
            "fonts not on this Mac — PowerPoint will use others: Gotham Rounded Zz"
        );
        // The video copied beside it: no longer said.
        std::fs::write(dir.path().join("clip one.mp4"), b"x").unwrap();
        assert!(!check(&path, 16.0 / 9.0).iter().any(|w| w.contains("linked")));
    }
}
