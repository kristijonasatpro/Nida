//! What a screen is called. Outputs address a screen by the platform's name
//! for it (see `monitors`), which on a Mac is the display's model number —
//! `Monitor #0` — and, for two identical displays, the same for both. So each
//! screen is also shown by what macOS calls it ("Color LCD", "BMD HDMI"), its
//! size and rate, and — given one — the operator's own name for it ("Stage
//! LED"), kept for this computer: the screens belong to the desk, not the show.
//!
//! A name given is kept by the display's identity: its UUID, which macOS keeps
//! across restarts, or — two connected displays sharing one, identical models
//! with the same serial — that and its place on the desktop.

use std::collections::BTreeMap;
use std::path::PathBuf;

use winit::monitor::MonitorHandle;

/// What is known of a connected screen beyond the name outputs address it by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScreenFacts {
    /// What the platform calls it: "Color LCD", "BMD HDMI".
    pub model: Option<String>,
    /// What a name given to it is kept by (see the module's doc).
    pub identity: String,
    /// Its refresh rate.
    pub refresh_hz: Option<f64>,
    /// Its place on the desktop.
    pub position: (i32, i32),
    /// The platform's number for it (a `CGDirectDisplayID` on a Mac).
    pub display_id: Option<u32>,
}

/// The facts of `handles`, in order — asked of the platform on the main
/// thread (as `refresh_monitors` is), with `keys` their names as outputs
/// address them.
pub(crate) fn facts(handles: &[MonitorHandle], keys: &[String]) -> Vec<ScreenFacts> {
    let mut facts: Vec<ScreenFacts> = handles
        .iter()
        .zip(keys)
        .map(|(handle, key)| {
            let position = handle.position();
            let display_id = display_id(handle);
            ScreenFacts {
                // A display with no name of its own reads as macOS's blank.
                model: display_id
                    .and_then(platform::model_name)
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty()),
                identity: display_id
                    .and_then(platform::uuid)
                    .unwrap_or_else(|| key.clone()),
                refresh_hz: handle
                    .refresh_rate_millihertz()
                    .map(|mhz| f64::from(mhz) / 1000.0),
                position: (position.x, position.y),
                display_id,
            }
        })
        .collect();
    qualify_twins(&mut facts);
    facts
}

/// Two connected screens with one identity — identical displays, the same
/// serial — are told apart by their place.
fn qualify_twins(facts: &mut [ScreenFacts]) {
    let identities: Vec<String> = facts.iter().map(|f| f.identity.clone()).collect();
    for fact in facts.iter_mut() {
        if identities.iter().filter(|i| **i == fact.identity).count() > 1 {
            let (x, y) = fact.position;
            fact.identity = format!("{} @ {x},{y}", fact.identity);
        }
    }
}

// None elsewhere than on a Mac.
#[allow(clippy::unnecessary_wraps)]
fn display_id(handle: &MonitorHandle) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::MonitorHandleExtMacOS as _;
        Some(handle.native_id())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = handle;
        None
    }
}

/// How a screen is shown: the name given to it, or what the platform calls
/// it, or the name outputs address it by — then what it is: "Stage LED (BMD
/// HDMI · 1920×1080 · 60 Hz)", "Color LCD (3456×2234 · 120 Hz)". `twin` when
/// another connected screen would read the same: then its place too.
pub fn label(
    key: &str,
    facts: Option<&ScreenFacts>,
    name: Option<&str>,
    (width, height): (u32, u32),
    twin: bool,
) -> String {
    let model = facts
        .and_then(|f| f.model.as_deref())
        .filter(|m| !m.trim().is_empty());
    let mut details = Vec::new();
    let title = match name {
        Some(name) => {
            details.push(model.unwrap_or(key).to_string());
            name.to_string()
        }
        None => match (model, twin, facts) {
            (Some(model), true, Some(f)) => format!("{model} @ {},{}", f.position.0, f.position.1),
            (Some(model), _, _) => model.to_string(),
            (None, _, _) => key.to_string(),
        },
    };
    details.push(format!("{width}×{height}"));
    if let Some(hz) = facts.and_then(|f| f.refresh_hz) {
        details.push(format!("{hz:.0} Hz"));
    }
    format!("{title} ({})", details.join(" · "))
}

/// The labels of the screens `keys` — with their facts, names and sizes —
/// in order, a screen's place added where two would read the same.
pub fn labels(
    keys: &[String],
    facts: &[ScreenFacts],
    names: &ScreenNames,
    sizes: &[(u32, u32)],
) -> Vec<String> {
    let base = |i: usize| -> (Option<String>, Option<String>) {
        let fact = facts.get(i);
        let name = fact.and_then(|f| names.get(&f.identity)).map(str::to_string);
        let model = fact
            .and_then(|f| f.model.clone())
            .filter(|m| !m.trim().is_empty());
        (name, model)
    };
    let titles: Vec<String> = (0..keys.len())
        .map(|i| {
            let (name, model) = base(i);
            name.or(model).unwrap_or_else(|| keys[i].clone())
        })
        .collect();
    (0..keys.len())
        .map(|i| {
            let (name, _) = base(i);
            let twin = name.is_none() && titles.iter().filter(|t| **t == titles[i]).count() > 1;
            label(
                &keys[i],
                facts.get(i),
                name.as_deref(),
                sizes.get(i).copied().unwrap_or_default(),
                twin,
            )
        })
        .collect()
}

/// The names given to screens on this computer, by identity.
#[derive(Debug, Default)]
pub struct ScreenNames {
    names: BTreeMap<String, String>,
    /// Where they are kept; none, kept nowhere (a test).
    path: Option<PathBuf>,
}

impl ScreenNames {
    /// The names kept on this computer (`…/Nida/screen-names.json` in its
    /// settings folder); none yet, none.
    pub fn load() -> Self {
        Self::load_from(dirs::config_dir().map(|d| d.join("Nida").join("screen-names.json")))
    }

    /// The names kept at `path`.
    pub fn load_from(path: Option<PathBuf>) -> Self {
        let names = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { names, path }
    }

    /// The name given to the screen with `identity`.
    pub fn get(&self, identity: &str) -> Option<&str> {
        self.names.get(identity).map(String::as_str)
    }

    /// Give the screen with `identity` a name — or, with none or a blank one,
    /// take its name away — and keep it.
    ///
    /// # Errors
    ///
    /// When the names cannot be written.
    pub fn set(&mut self, identity: &str, name: Option<&str>) -> std::io::Result<()> {
        let Some((path, text)) = self.name(identity, name) else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)
    }

    /// As [`Self::set`], the name held at once; the file the names are kept
    /// in, and its text, returned for the caller to write — the show's
    /// writer, so the outputs do not wait for the disk. None when they are
    /// kept nowhere.
    pub fn name(&mut self, identity: &str, name: Option<&str>) -> Option<(PathBuf, String)> {
        match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => {
                self.names.insert(identity.to_string(), name.to_string());
            }
            None => {
                self.names.remove(identity);
            }
        }
        let path = self.path.clone()?;
        match serde_json::to_string_pretty(&self.names) {
            Ok(text) => Some((path, text)),
            Err(e) => {
                log::warn!("The screens' names not kept: {e}");
                None
            }
        }
    }
}

/// Put each screen's label big on it for a couple of seconds — `screens`,
/// (display, text) — so two alike can be told apart. A Mac's screens only.
pub fn identify(screens: Vec<(u32, String)>) {
    platform::identify(screens);
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ptr::NonNull;

    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly as _};
    use objc2_app_kit::{
        NSBackingStoreType, NSColor, NSFont, NSScreen, NSTextAlignment, NSTextField, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_core_foundation::{CFRetained, CFUUID};
    use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, NSString};

    #[link(name = "ColorSync", kind = "framework")]
    extern "C" {
        fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> Option<NonNull<CFUUID>>;
    }

    /// The display's UUID, kept by macOS across restarts.
    pub(super) fn uuid(display: u32) -> Option<String> {
        // SAFETY: any display number; a new reference, or null.
        let uuid = unsafe { CGDisplayCreateUUIDFromDisplayID(display) }?;
        // SAFETY: created above (the Create rule), released once.
        let uuid = unsafe { CFRetained::from_raw(uuid) };
        CFUUID::new_string(None, Some(&uuid)).map(|s| s.to_string())
    }

    /// The screens and their display numbers — on the main thread only.
    fn screens(mtm: MainThreadMarker) -> Vec<(u32, Retained<NSScreen>)> {
        let key = NSString::from_str("NSScreenNumber");
        NSScreen::screens(mtm)
            .iter()
            .filter_map(|screen| {
                let number = screen.deviceDescription().objectForKey(&key)?;
                let number = number.downcast::<NSNumber>().ok()?;
                Some((number.unsignedIntValue(), screen))
            })
            .collect()
    }

    /// What macOS calls the display ("Color LCD"); off the main thread, none.
    pub(super) fn model_name(display: u32) -> Option<String> {
        let mtm = MainThreadMarker::new()?;
        screens(mtm)
            .into_iter()
            .find(|(number, _)| *number == display)
            .map(|(_, screen)| screen.localizedName().to_string())
    }

    /// How long each screen's label stays up.
    const SHOWN_FOR: std::time::Duration = std::time::Duration::from_secs(3);

    pub(super) fn identify(labels: Vec<(u32, String)>) {
        dispatch2::DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            let windows: Vec<Retained<NSWindow>> = screens(mtm)
                .into_iter()
                .filter_map(|(number, screen)| {
                    let text = labels.iter().find(|(d, _)| *d == number)?.1.clone();
                    Some(overlay(mtm, &screen, &text))
                })
                .collect();
            let windows = dispatch2::MainThreadBound::new(windows, mtm);
            let _ = dispatch2::DispatchQueue::main().after(
                dispatch2::DispatchTime::try_from(SHOWN_FOR).unwrap_or(dispatch2::DispatchTime::NOW),
                move || {
                    if let Some(mtm) = MainThreadMarker::new() {
                        for window in windows.get(mtm) {
                            window.close();
                        }
                    }
                },
            );
        });
    }

    /// A see-through window over all of `screen` with `text` big in its
    /// middle; it lets clicks through.
    fn overlay(mtm: MainThreadMarker, screen: &NSScreen, text: &str) -> Retained<NSWindow> {
        let frame = screen.frame();
        // SAFETY: a borderless window over the screen's frame, on the main
        // thread; kept by the caller until it closes it.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer_screen(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
                Some(screen),
            )
        };
        // SAFETY: closed by the caller, which keeps it until then.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(0.0, 0.75)));
        window.setIgnoresMouseEvents(true);
        // Above full-screen output windows.
        window.setLevel(1000);
        let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        label.setFont(Some(&NSFont::boldSystemFontOfSize(frame.size.height / 12.0)));
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setAlignment(NSTextAlignment::Center);
        let height = frame.size.height / 4.0;
        label.setFrame(NSRect::new(
            NSPoint::new(0.0, (frame.size.height - height) / 2.0),
            NSSize::new(frame.size.width, height),
        ));
        if let Some(content) = window.contentView() {
            content.addSubview(&label);
        }
        window.setFrame_display(frame, true);
        window.orderFrontRegardless();
        window
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn uuid(_display: u32) -> Option<String> {
        None
    }

    pub(super) fn model_name(_display: u32) -> Option<String> {
        None
    }

    pub(super) fn identify(_labels: Vec<(u32, String)>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(model: Option<&str>, identity: &str, hz: Option<f64>, at: (i32, i32)) -> ScreenFacts {
        ScreenFacts {
            model: model.map(str::to_string),
            identity: identity.into(),
            refresh_hz: hz,
            position: at,
            display_id: None,
        }
    }

    /// A screen reads by the name given to it, what macOS calls it, its size
    /// and rate; without a name, by what macOS calls it; knowing nothing, by
    /// the name outputs address it by.
    #[test]
    fn a_screen_reads_by_its_given_name_then_macos_name_then_key() {
        let lcd = fact(Some("Color LCD"), "u1", Some(120.0), (0, 0));
        assert_eq!(
            label("Monitor #41041", Some(&lcd), Some("Laptop"), (3456, 2234), false),
            "Laptop (Color LCD · 3456×2234 · 120 Hz)"
        );
        assert_eq!(
            label("Monitor #41041", Some(&lcd), None, (3456, 2234), false),
            "Color LCD (3456×2234 · 120 Hz)"
        );
        assert_eq!(label("HDMI-A-1", None, None, (1920, 1080), false), "HDMI-A-1 (1920×1080)");
        // A display macOS gives a blank name (seen on a Mac mini's screen).
        let blank = fact(Some(""), "u2", Some(60.0), (0, 0));
        assert_eq!(
            label("Monitor #1986622068", Some(&blank), None, (1920, 1080), false),
            "Monitor #1986622068 (1920×1080 · 60 Hz)"
        );
    }

    /// Two identical displays — one model, one serial, one UUID — are told
    /// apart by their place: a name given to one stays with it, and without
    /// names each reads with its place.
    #[test]
    fn identical_displays_are_told_apart_by_their_place() {
        let mut facts = vec![
            fact(Some("BMD HDMI"), "same", Some(60.0), (1728, 0)),
            fact(Some("BMD HDMI"), "same", Some(60.0), (-1920, 0)),
            fact(Some("Color LCD"), "lcd", Some(120.0), (0, 0)),
        ];
        qualify_twins(&mut facts);
        assert_eq!(facts[0].identity, "same @ 1728,0");
        assert_eq!(facts[1].identity, "same @ -1920,0");
        assert_eq!(facts[2].identity, "lcd");
        let keys: Vec<String> = ["Monitor #0 @ 1728,0", "Monitor #0 @ -1920,0", "Monitor #41041"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let sizes = [(1920, 1080), (1920, 1080), (3456, 2234)];
        let mut names = ScreenNames::load_from(None);
        let unnamed = labels(&keys, &facts, &names, &sizes);
        assert_eq!(unnamed[0], "BMD HDMI @ 1728,0 (1920×1080 · 60 Hz)");
        assert_eq!(unnamed[1], "BMD HDMI @ -1920,0 (1920×1080 · 60 Hz)");
        assert_eq!(unnamed[2], "Color LCD (3456×2234 · 120 Hz)");
        names.set("same @ -1920,0", Some("ATEM in 2")).unwrap();
        let with_a_name = labels(&keys, &facts, &names, &sizes);
        assert_eq!(with_a_name[1], "ATEM in 2 (BMD HDMI · 1920×1080 · 60 Hz)");
        assert_eq!(
            with_a_name[0],
            "BMD HDMI (1920×1080 · 60 Hz)",
            "its twin alone, no place needed"
        );
    }

    /// macOS gives a display a UUID — the same each time it is asked — that
    /// a name given to it is kept by. (A machine with no display skips.)
    #[cfg(target_os = "macos")]
    #[test]
    fn a_display_has_a_lasting_uuid() {
        let main = objc2_core_graphics::CGMainDisplayID();
        if main == 0 {
            return;
        }
        let Some(first) = platform::uuid(main) else {
            return;
        };
        assert_eq!(first.len(), 36, "{first}");
        assert_eq!(platform::uuid(main).as_deref(), Some(first.as_str()));
    }

    /// Names given are kept on this computer, and read back; a blank name
    /// takes the name away.
    #[test]
    fn names_given_are_kept_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Nida").join("screen-names.json");
        let mut names = ScreenNames::load_from(Some(path.clone()));
        names.set("u1", Some("  Stage LED ")).unwrap();
        names.set("u2", Some("Confidence TV")).unwrap();
        let again = ScreenNames::load_from(Some(path.clone()));
        assert_eq!((again.get("u1"), again.get("u2")), (Some("Stage LED"), Some("Confidence TV")));
        names.set("u1", Some("   ")).unwrap();
        let after = ScreenNames::load_from(Some(path));
        assert_eq!((after.get("u1"), after.get("u2")), (None, Some("Confidence TV")));
    }
}
