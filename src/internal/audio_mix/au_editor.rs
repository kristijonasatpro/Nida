//! A bus insert's own window: the plug-in's view — or Apple's generic list
//! of its controls, for a plug-in without one — in a window of its own,
//! made on the main thread through its dispatch queue (the frame and the
//! mixer never wait for it). What is set there is the plug-in's own
//! settings, read for the show every 2 s (see `insert`).

use std::cell::RefCell;
use std::collections::HashMap;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSFloatingWindowLevel, NSViewController, NSWindow};
use objc2_foundation::{NSPoint, NSString};

use super::au_host::SharedUnit;

// `requestViewControllerWithCompletionHandler:` and `AUGenericViewController`
// are CoreAudioKit's: linked so the runtime has them.
#[link(name = "CoreAudioKit", kind = "framework")]
unsafe extern "C" {}

/// The insert windows' bookkeeping, by bus: the windows made (kept, so one
/// closed opens again as it was), the views asked for that have not come,
/// and each bus's insert generation — moved on as its insert changes or
/// goes, so a view asked of the old one, coming after, is not shown.
struct Editors<W> {
    windows: HashMap<String, W>,
    pending: std::collections::HashSet<String>,
    generations: HashMap<String, u64>,
}

/// What Open does.
#[derive(Debug, PartialEq)]
enum Opening<W> {
    /// The window is made: to the front.
    Front(W),
    /// Ask the plug-in for its view, for the insert of this generation.
    Ask(u64),
    /// Asked already: it comes once.
    Wait,
}

impl<W: Clone> Editors<W> {
    fn new() -> Self {
        Self {
            windows: HashMap::new(),
            pending: std::collections::HashSet::new(),
            generations: HashMap::new(),
        }
    }

    fn generation(&self, key: &str) -> u64 {
        self.generations.get(key).copied().unwrap_or(0)
    }

    fn open(&mut self, key: &str) -> Opening<W> {
        if let Some(window) = self.windows.get(key) {
            return Opening::Front(window.clone());
        }
        if self.pending.insert(key.to_string()) {
            Opening::Ask(self.generation(key))
        } else {
            Opening::Wait
        }
    }

    /// The view asked for the insert of generation `asked` came: whether it
    /// is still that insert's.
    fn arrived(&mut self, key: &str, asked: u64) -> bool {
        // An old insert's view: the newer one's ask still stands.
        if self.generation(key) != asked {
            return false;
        }
        self.pending.remove(key);
        true
    }

    fn shown(&mut self, key: &str, window: W) {
        self.windows.insert(key.to_string(), window);
    }

    /// The bus's insert changed or went, or the bus did: its window, to
    /// close, and any view still on its way not to be shown.
    fn close(&mut self, key: &str) -> Option<W> {
        *self.generations.entry(key.to_string()).or_insert(0) += 1;
        self.pending.remove(key);
        self.windows.remove(key)
    }
}

thread_local! {
    /// The insert windows, on the main thread.
    static EDITORS: RefCell<Editors<Retained<NSWindow>>> = RefCell::new(Editors::new());
}

/// Buses whose windows were closed, for tests.
#[cfg(test)]
pub static CLOSED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A view controller handed from the plug-in's queue to the main thread.
struct Handed(Option<Retained<NSViewController>>);

// SAFETY: only handed over, and touched on the main thread alone.
unsafe impl Send for Handed {}

/// Open the window of bus `key`'s insert, titled `title`: to the front if
/// it is made already, else asked of the plug-in.
pub fn open(key: String, title: String, unit: SharedUnit) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let asked = match EDITORS.with(|e| e.borrow_mut().open(&key)) {
            Opening::Front(window) => {
                window.makeKeyAndOrderFront(None);
                return;
            }
            Opening::Wait => return,
            Opening::Ask(generation) => generation,
        };
        let theirs = unit.clone();
        // Called on a queue of the framework's: the window is made back on
        // the main thread.
        let handler = RcBlock::new(move |controller: *mut NSViewController| {
            // SAFETY: the framework's pointer, valid for the call, or null.
            let handed = Handed(unsafe { Retained::retain(controller) });
            let (key, title, unit) = (key.clone(), title.clone(), theirs.clone());
            dispatch2::DispatchQueue::main().exec_async(move || {
                let handed = handed;
                // Its insert changed or went meanwhile: not this one's.
                if EDITORS.with(|e| e.borrow_mut().arrived(&key, asked)) {
                    show(&key, &title, handed.0, &unit);
                }
            });
        });
        // SAFETY: CoreAudioKit's category on AUAudioUnit, sent with a block
        // of the type it takes; the block is retained by the call.
        unsafe {
            let () = msg_send![&*unit.0, requestViewControllerWithCompletionHandler: &*handler];
        }
    });
}

/// Make the window, on the main thread, with the plug-in's view, or the
/// generic one if it gave none.
fn show(key: &str, title: &str, controller: Option<Retained<NSViewController>>, unit: &SharedUnit) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let Some(controller) = controller.or_else(|| generic(unit)) else {
        log::warn!("Audio insert '{title}': no window to open (no view, no generic one)");
        return;
    };
    let window = NSWindow::windowWithContentViewController(&controller);
    window.setTitle(&NSString::from_str(title));
    beside_the_controls(&window);
    window.makeKeyAndOrderFront(None);
    log::info!("Audio insert window '{title}' opened");
    EDITORS.with(|e| e.borrow_mut().shown(key, window));
}

/// Put a plug-in's window on the screen of Nida's own window (its controls),
/// in its middle, above that window: never over a full-screen output (the
/// LED wall, the projector — the audience would see it), which sits higher
/// still, and never left wherever macOS puts a new window, perhaps under
/// such an output.
fn beside_the_controls(window: &NSWindow) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // Nida's own window: shown, at the normal level (a plug-in window floats
    // above it, a full-screen output higher still), not an output (titled as
    // one even when windowed), not this one. The main window first — if it
    // is that, not an output clicked last.
    let output = crate::renderer::context::output_title("");
    let is_controls = |w: &NSWindow| {
        w.isVisible()
            && w.level() < NSFloatingWindowLevel
            && !w.title().to_string().starts_with(&output)
            && !std::ptr::eq(std::ptr::from_ref::<NSWindow>(w), window)
    };
    let controls = app
        .mainWindow()
        .filter(|w| is_controls(w))
        .or_else(|| app.windows().iter().find(|w| is_controls(w)));
    window.setLevel(NSFloatingWindowLevel);
    let Some(screen) = controls.and_then(|w| w.screen()) else {
        window.center();
        return;
    };
    let area = screen.visibleFrame();
    let size = window.frame().size;
    window.setFrameOrigin(NSPoint::new(
        area.origin.x + (area.size.width - size.width) / 2.0,
        area.origin.y + (area.size.height - size.height) / 2.0,
    ));
}

/// Apple's generic view of a plug-in's controls.
fn generic(unit: &SharedUnit) -> Option<Retained<NSViewController>> {
    let class = AnyClass::get(c"AUGenericViewController")?;
    // SAFETY: CoreAudioKit's class, made with `new` and given the unit it
    // shows; on the main thread.
    unsafe {
        let controller: Retained<NSViewController> = msg_send![class, new];
        let () = msg_send![&*controller, setAuAudioUnit: &*unit.0];
        Some(controller)
    }
}

/// Close and forget bus `key`'s insert window, if made — its insert changed
/// or went, or the bus did — and any view still on its way for it.
pub fn close(key: String) {
    #[cfg(test)]
    if let Ok(mut closed) = CLOSED.lock() {
        closed.push(key.clone());
    }
    dispatch2::DispatchQueue::main().exec_async(move || {
        if let Some(window) = EDITORS.with(|e| e.borrow_mut().close(&key)) {
            window.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{Editors, Opening};

    /// Open asks once, shows what comes, then brings that window forward.
    #[test]
    fn a_window_is_asked_for_once_then_brought_forward() {
        let mut editors = Editors::<u32>::new();
        assert_eq!(editors.open("pa"), Opening::Ask(0));
        assert_eq!(editors.open("pa"), Opening::Wait, "asked already");
        assert!(editors.arrived("pa", 0));
        editors.shown("pa", 7);
        assert_eq!(editors.open("pa"), Opening::Front(7));
    }

    /// A view that comes after its insert changed or went is not shown;
    /// the next Open asks the new insert.
    #[test]
    fn a_view_for_an_insert_gone_is_not_shown() {
        let mut editors = Editors::<u32>::new();
        assert_eq!(editors.open("pa"), Opening::Ask(0));
        assert_eq!(editors.close("pa"), None);
        assert!(!editors.arrived("pa", 0), "the old insert's view");
        assert_eq!(editors.open("pa"), Opening::Ask(1));
        assert!(
            !editors.arrived("pa", 0),
            "the old view, coming after the new ask"
        );
        assert_eq!(
            editors.open("pa"),
            Opening::Wait,
            "the new ask still stands"
        );
        assert!(editors.arrived("pa", 1));
        editors.shown("pa", 9);
        assert_eq!(editors.close("pa"), Some(9), "its window, to close");
        assert_eq!(editors.open("pa"), Opening::Ask(2));
    }
}
