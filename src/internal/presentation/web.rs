//! Web talks: an HTML presentation — an AI-made deck, reveal.js — played by
//! the Mac's own web engine (`WebKit`, as Safari plays it) in a window of
//! Nida's kept off every screen, and captured as PowerPoint's show window is.
//!
//! Measured on the mini (macOS 27, 2026-10-26): such a page draws on at 60
//! frames a second covered and off every screen once `WebKit` is told not to
//! go by whether its window can be seen (`_setWindowOcclusionDetectionEnabled:`
//! — `WebKit`'s own switch for it, not public: without it the page stops
//! drawing as soon as nothing of its window shows); its capture is the
//! window's own size, whatever the screens; keys sent to the view move the
//! page as a clicker's would. A page opened ahead has its media paused, so
//! nothing of it is heard before it is on.
//!
//! A page is ready only once it has loaded and set itself up — its document
//! and fonts loaded, a reveal.js deck ready — and gone to the slide it was
//! left on: only then is its window captured, so a talk started never shows
//! a page half drawn. Each opening of a page, and each capture of it, has a
//! number that what it finds carries: what an opening or capture since
//! replaced finds is let go.
//!
//! `WebKit` lives on the main thread: every call is sent there, and what it
//! finds comes back as [`WebEvent`]s.

use std::path::Path;
use std::time::Duration;

use crate::engine::value::presentation::Go;
use crate::screen_capture::backend::{CaptureConfig, CaptureTargetInfo, ScreenCaptureBackend};

/// A page not set up in this long is given up on, and said why.
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// A deck walked to a slide a key at a time goes at most this far.
const MAX_WALK: u32 = 300;

/// What the web engine found. Each carries the opening of the page it is of
/// (`generation`); a capture's, the capture asked (`attempt`).
pub enum WebEvent {
    /// The page has loaded, set itself up and gone to the slide asked: its
    /// window can be captured.
    Ready { key: String, generation: u64 },
    /// Its window captured, to adopt: the talk is on or coming.
    Captured {
        key: String,
        generation: u64,
        attempt: u64,
        target: CaptureTargetInfo,
        capture: Box<dyn ScreenCaptureBackend>,
        config: CaptureConfig,
    },
    /// It cannot be played, and why: its opening (no `attempt`), or a
    /// capture of it.
    Failed {
        key: String,
        generation: u64,
        attempt: Option<u64>,
        why: String,
    },
    /// Where the page is, when it says (reveal.js): its slide and how many
    /// it has.
    Position {
        key: String,
        generation: u64,
        at: u32,
        of: u32,
    },
}

impl std::fmt::Debug for WebEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready { key, generation } => write!(f, "Ready({key}#{generation})"),
            Self::Captured {
                key,
                generation,
                attempt,
                target,
                ..
            } => write!(f, "Captured({key}#{generation}/{attempt}, {})", target.label),
            Self::Failed {
                key,
                generation,
                attempt,
                why,
            } => write!(f, "Failed({key}#{generation}/{attempt:?}, {why})"),
            Self::Position {
                key,
                generation,
                at,
                of,
            } => write!(f, "Position({key}#{generation}, {at}/{of})"),
        }
    }
}

/// What plays web talks: the Mac's `WebKit`, or a stand-in in tests.
pub trait WebEngine: Send {
    /// Open the page at `path` for `key` — the opening numbered
    /// `generation` — its picture `pixels` big, its media paused; once set
    /// up it goes to `slide` (when given) and says [`WebEvent::Ready`].
    fn load(&mut self, key: &str, generation: u64, path: &Path, pixels: (u32, u32), slide: Option<u32>);
    /// Its media playing (on or coming), or paused (ready only).
    fn set_playing(&mut self, key: &str, playing: bool);
    /// Its sound silenced, or heard again.
    fn set_muted(&mut self, key: &str, muted: bool);
    /// Capture its window: a [`WebEvent::Captured`], or a `Failed`, with
    /// this `attempt` follows.
    fn capture(&mut self, key: &str, attempt: u64);
    /// Its picture made `pixels` big — the page laid out again at that size,
    /// where it is kept. A capture since is that size.
    fn resize(&mut self, key: &str, pixels: (u32, u32));
    /// Move the page on or back, as a clicker would.
    fn go(&mut self, key: &str, to: Go);
    /// Close the page.
    fn unload(&mut self, key: &str);
    /// What it found since last asked.
    fn poll(&mut self) -> Vec<WebEvent>;
}

/// The key a key press on the page stands for: (macOS key code, the
/// character `AppKit` gives it).
fn key_of(to: Go) -> Option<(u16, u32)> {
    match to {
        // Arrow keys: what an HTML deck most surely answers to.
        Go::Next => Some((124, 0xF703)),
        Go::Previous => Some((123, 0xF702)),
        Go::First => Some((115, 0xF729)),
        Go::Last => Some((119, 0xF72B)),
        Go::Slide(_) => None,
    }
}

/// Whether the program at `path` is `WebKit`'s process for a page — not
/// the one it plays every page's media in, nor its network's.
#[must_use]
pub fn is_page_program(path: &str) -> bool {
    crate::audio::app_sound::is_webkit_helper(path)
        && Path::new(path)
            .file_name()
            .is_some_and(|name| name == "com.apple.WebKit.WebContent")
}

/// How many steps on from the first a walk to slide `n` takes, at most.
fn walk(n: u32) -> u32 {
    n.clamp(1, MAX_WALK) - 1
}

/// `JavaScript` saying how far the page has got: `"ready"` once its document
/// and fonts have loaded (and a reveal.js deck is ready), `"loading"`
/// before, `"blank"` while what shows is not the file yet (opening, or
/// failed to).
pub const READY_JS: &str = r"(function(){try{
if(location.protocol!=='file:')return 'blank';
if(document.readyState!=='complete')return 'loading';
if(document.fonts&&document.fonts.status!=='loaded')return 'loading';
if(window.Reveal&&Reveal.isReady&&!Reveal.isReady())return 'loading';
return 'ready';}catch(e){return 'ready';}})()";

/// `JavaScript` that says where the page is, when it is a deck that says:
/// `[slide, slides]` from reveal.js, else `null`.
pub const POSITION_JS: &str = r"(function(){try{
if(window.Reveal&&Reveal.getTotalSlides){var past=Reveal.getSlidePastCount?Reveal.getSlidePastCount():Reveal.getIndices().h;return [past+1,Reveal.getTotalSlides()];}
}catch(e){}return null;})()";

/// `JavaScript` that goes to slide `n` (from 1) of a deck that can be told —
/// reveal.js — and says whether it could.
#[must_use]
pub fn go_to_js(n: u32) -> String {
    format!(
        r"(function(n){{try{{
if(window.Reveal&&Reveal.getSlides){{var all=Reveal.getSlides();if(all[n-1]){{var i=Reveal.getIndices(all[n-1]);Reveal.slide(i.h,i.v);return true;}}}}
}}catch(e){{}}return false;}})({n})"
    )
}

/// `JavaScript` that puts a reveal.js deck at slide `n` (from 1) at once —
/// every transition in it held still for the jump (a slide's own
/// `data-transition` too), let go once it has been drawn there, when
/// `window.__nidaStill` turns true — and says whether it could.
#[must_use]
pub fn restore_js(n: u32) -> String {
    format!(
        r"(function(n){{try{{if(window.Reveal&&Reveal.getSlides){{
var all=Reveal.getSlides();var s=all[n-1];if(!s)return false;
var still=document.createElement('style');
still.textContent='.reveal *,.reveal *::before,.reveal *::after{{transition:none!important}}';
document.head.appendChild(still);window.__nidaStill=false;
var i=Reveal.getIndices(s);Reveal.slide(i.h,i.v);
requestAnimationFrame(function(){{requestAnimationFrame(function(){{still.remove();window.__nidaStill=true;}});}});
return true;}}}}catch(e){{}}return false;}})({n})"
    )
}

/// `JavaScript` saying where a deck put at its slide is, and whether it
/// has been drawn there with its transitions let go: `[slide, drawn]`.
pub const SETTLED_JS: &str = r"(function(){try{
var past=Reveal.getSlidePastCount?Reveal.getSlidePastCount():Reveal.getIndices().h;
return [past+1,window.__nidaStill===true];}catch(e){}return null;})()";

/// `JavaScript` that steps a reveal.js deck itself — on through its
/// fragments and down its columns, as its own Page Down does, where the
/// arrow keys would skip the rest of a column — and says whether it could.
#[must_use]
pub fn step_js(to: Go) -> Option<String> {
    let what = match to {
        Go::Next => "Reveal.next()",
        Go::Previous => "Reveal.prev()",
        // Its builds hidden again too: given no fragment, reveal.js keeps
        // them as they were left.
        Go::First => "Reveal.slide(0,0,-1)",
        Go::Last => {
            "var all=Reveal.getSlides();var i=Reveal.getIndices(all[all.length-1]);Reveal.slide(i.h,i.v)"
        }
        Go::Slide(_) => return None,
    };
    Some(format!(
        r"(function(){{try{{if(window.Reveal&&Reveal.next&&Reveal.getSlides){{{what};return true;}}}}catch(e){{}}return false;}})()"
    ))
}

#[cfg(target_os = "macos")]
pub use mac::WebKit;

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeSet, HashMap};
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{class, msg_send, sel, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSBackingStoreType, NSScreen, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    use super::{
        go_to_js, is_page_program, key_of, restore_js, step_js, walk, WebEngine, WebEvent,
        LOAD_TIMEOUT, POSITION_JS, READY_JS, SETTLED_JS,
    };
    use crate::engine::value::presentation::Go;
    use crate::screen_capture::backend::{CaptureTargetInfo, CaptureTargetKind};

    #[link(name = "WebKit", kind = "framework")]
    extern "C" {}

    /// Where web talks' windows are: off every screen.
    const OFF_SCREEN: f64 = -40_000.0;

    /// How often a page opening is looked at, until it is set up.
    const WATCH_EVERY: Duration = Duration::from_millis(150);

    /// A page set up is given this long to be drawn before it is ready.
    const DRAWN: Duration = Duration::from_millis(200);

    /// A deck walked to its slide a key at a time is given this long to get
    /// there (it cannot say when it has).
    const WALKED: Duration = Duration::from_millis(900);

    /// A deck told to go to its slide is asked where it is this often, until
    /// it says it is there or this long has gone by.
    const ARRIVING_EVERY: Duration = Duration::from_millis(100);
    const ARRIVE_WITHIN: Duration = Duration::from_secs(3);

    /// A page not ready this long after it was opened is given up on —
    /// whether or not its scripts still answer.
    const OVERDUE: Duration = Duration::from_secs(LOAD_TIMEOUT.as_secs() + 2);

    /// A page open on the main thread.
    struct Page {
        window: Retained<NSWindow>,
        web: Retained<AnyObject>,
        /// The picture's size it is laid out for.
        pixels: (u32, u32),
        /// Which opening of the page this is.
        generation: u64,
        /// The slide to go to once set up.
        slide: Option<u32>,
        opened: Instant,
        /// Set up, at its slide, said ready.
        ready: bool,
    }

    /// A page asked to answer before its process is ended: by its token,
    /// and whether it has.
    type Asked = (u64, std::rc::Rc<Cell<bool>>);

    thread_local! {
        /// The pages open, by key — on the main thread only.
        static PAGES: RefCell<HashMap<String, Page>> = RefCell::new(HashMap::new());
        /// Whether the sound side is being told the pages' processes, and
        /// what it was told last.
        static TELLING: Cell<bool> = const { Cell::new(false) };
        static TOLD: RefCell<BTreeSet<i32>> = const { RefCell::new(BTreeSet::new()) };
        /// Pages asked to answer before their process is ended: each by a
        /// token, with whether it did.
        static STUCK: RefCell<Vec<Asked>> = const { RefCell::new(Vec::new()) };
        static NEXT_STUCK: Cell<u64> = const { Cell::new(0) };
    }

    /// Web talks played by the Mac's `WebKit`.
    pub struct WebKit {
        tx: mpsc::Sender<WebEvent>,
        rx: mpsc::Receiver<WebEvent>,
    }

    impl Default for WebKit {
        fn default() -> Self {
            let (tx, rx) = mpsc::channel();
            Self { tx, rx }
        }
    }

    impl WebKit {
        /// Whether this Mac's `WebKit` has the switch that keeps a page
        /// drawing when its window cannot be seen: without it a web talk
        /// stops moving off screen.
        #[must_use]
        pub fn can_draw_off_screen() -> bool {
            AnyClass::get(c"WKWebView").is_some_and(|class| {
                class.instance_method(sel!(_setWindowOcclusionDetectionEnabled:)).is_some()
            })
        }
    }

    /// Run `work` on the main thread, its marker given.
    fn on_main(work: impl FnOnce(MainThreadMarker) + Send + 'static) {
        dispatch2::DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                work(mtm);
            }
        });
    }

    /// Run `work` on the main thread `wait` from now.
    fn after(wait: Duration, work: impl FnOnce() + Send + 'static) {
        let _ = dispatch2::DispatchQueue::main().after(
            dispatch2::DispatchTime::try_from(wait).unwrap_or(dispatch2::DispatchTime::NOW),
            work,
        );
    }

    /// Page `key`'s view and window, while it is opening `generation`.
    fn page_of(key: &str, generation: u64) -> Option<(Retained<AnyObject>, Retained<NSWindow>)> {
        PAGES.with_borrow(|pages| {
            pages
                .get(key)
                .filter(|p| p.generation == generation)
                .map(|p| (p.web.clone(), p.window.clone()))
        })
    }

    impl WebEngine for WebKit {
        fn load(
            &mut self,
            key: &str,
            generation: u64,
            path: &Path,
            pixels: (u32, u32),
            slide: Option<u32>,
        ) {
            let (key, path, tx) = (key.to_string(), path.to_path_buf(), self.tx.clone());
            on_main(move |mtm| match open_page(mtm, &key, generation, &path, pixels, slide) {
                Ok(()) => {
                    // Given up on in time even should its scripts never
                    // answer (a page stuck in a loop).
                    let (late_key, late_tx) = (key.clone(), tx.clone());
                    after(OVERDUE, move || overdue(&late_key, generation, &late_tx));
                    watch(key, generation, tx);
                    keep_telling();
                }
                Err(why) => {
                    let _ = tx.send(WebEvent::Failed {
                        key,
                        generation,
                        attempt: None,
                        why,
                    });
                }
            });
        }

        fn set_playing(&mut self, key: &str, playing: bool) {
            let key = key.to_string();
            on_main(move |_| {
                PAGES.with_borrow(|pages| {
                    if let Some(page) = pages.get(&key) {
                        suspend_media(&page.web, !playing);
                    }
                });
                tell_processes();
            });
        }

        fn set_muted(&mut self, key: &str, muted: bool) {
            let key = key.to_string();
            on_main(move |_| {
                PAGES.with_borrow(|pages| {
                    if let Some(page) = pages.get(&key) {
                        mute(&page.web, muted);
                    }
                });
            });
        }

        fn capture(&mut self, key: &str, attempt: u64) {
            let (key, tx) = (key.to_string(), self.tx.clone());
            on_main(move |_| {
                let found = PAGES.with_borrow(|pages| {
                    pages.get(&key).map(|page| {
                        let frame = page.window.frame();
                        (page.window.windowNumber(), frame.size, page.pixels, page.generation)
                    })
                });
                if let Some((number, size, pixels, generation)) = found {
                    capture(key, generation, attempt, number, size, pixels, tx);
                }
            });
        }

        fn resize(&mut self, key: &str, pixels: (u32, u32)) {
            let key = key.to_string();
            on_main(move |mtm| {
                let scale = screen_scale(mtm);
                PAGES.with_borrow_mut(|pages| {
                    if let Some(page) = pages.get_mut(&key) {
                        page.window.setContentSize(points(pixels, scale));
                        // SAFETY: WKWebView's documented property.
                        unsafe {
                            let _: () = msg_send![&*page.web, setPageZoom: 1.0 / scale];
                        }
                        page.pixels = pixels;
                    }
                });
            });
        }

        fn go(&mut self, key: &str, to: Go) {
            let (key, tx) = (key.to_string(), self.tx.clone());
            on_main(move |_| {
                let generation = PAGES.with_borrow(|pages| {
                    let page = pages.get(&key)?;
                    match to {
                        Go::Slide(n) => go_to_slide(page, n),
                        _ => step(page, to),
                    }
                    Some(page.generation)
                });
                // Where it went: asked once the page has moved.
                if let Some(generation) = generation {
                    after(Duration::from_millis(150), move || {
                        ask_position(&key, generation, &tx);
                        tell_processes();
                    });
                }
            });
        }

        fn unload(&mut self, key: &str) {
            let key = key.to_string();
            on_main(move |_| {
                if let Some(page) = PAGES.with_borrow_mut(|pages| pages.remove(&key)) {
                    end_if_stuck(&page.web);
                    page.window.close();
                }
                tell_processes();
            });
        }

        fn poll(&mut self) -> Vec<WebEvent> {
            self.rx.try_iter().collect()
        }
    }

    /// The main screen's pixels to a point.
    fn screen_scale(mtm: MainThreadMarker) -> f64 {
        NSScreen::mainScreen(mtm).map_or(1.0, |s| s.backingScaleFactor().max(1.0))
    }

    /// A picture `pixels` big, in points at `scale` pixels to a point.
    fn points(pixels: (u32, u32), scale: f64) -> NSSize {
        NSSize::new(f64::from(pixels.0) / scale, f64::from(pixels.1) / scale)
    }

    /// Open the page at `path` in a window of its own off every screen, its
    /// picture `pixels` big, its media paused.
    fn open_page(
        mtm: MainThreadMarker,
        key: &str,
        generation: u64,
        path: &Path,
        pixels: (u32, u32),
        slide: Option<u32>,
    ) -> Result<(), String> {
        if !path.is_file() {
            return Err("its file is not there".into());
        }
        if let Some(old) = PAGES.with_borrow_mut(|pages| pages.remove(key)) {
            end_if_stuck(&old.web);
            old.window.close();
        }
        // The picture's pixels at the screen's pixels to a point: the page
        // laid out as at the picture's size (zoomed by as much), drawn at
        // its pixels.
        let scale = screen_scale(mtm);
        let size = points(pixels, scale);
        let frame = NSRect::new(NSPoint::new(OFF_SCREEN, OFF_SCREEN), size);
        // SAFETY: a borderless window made on the main thread, kept in PAGES
        // until it is closed.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: closed by unload, which drops it from PAGES.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setIgnoresMouseEvents(true);
        window.setExcludedFromWindowsMenu(true);
        // Nida hidden (⌘H) must not hide the page: its picture would stop.
        window.setCanHide(false);
        window.setTitle(&NSString::from_str("Nida — web talk"));
        // SAFETY: WebKit's own classes, linked; each call as documented —
        // `_setWindowOcclusionDetectionEnabled:` and
        // `setInactiveSchedulingPolicy:` only when the class answers them.
        let web: Retained<AnyObject> = unsafe {
            let config: Retained<AnyObject> = msg_send![class!(WKWebViewConfiguration), new];
            // Sound only after a step on (a key, as a browser asks for a
            // click): a page opened ahead is never heard.
            // WKAudiovisualMediaTypeAudio.
            let _: () = msg_send![&*config, setMediaTypesRequiringUserActionForPlayback: 1usize];
            let prefs: Retained<AnyObject> = msg_send![&*config, preferences];
            let responds: bool =
                msg_send![&*prefs, respondsToSelector: sel!(setInactiveSchedulingPolicy:)];
            if responds {
                // WKInactiveSchedulingPolicyNone: timers and animations run
                // on when the page counts itself as not seen.
                let _: () = msg_send![&*prefs, setInactiveSchedulingPolicy: 2isize];
            }
            let alloc: Allocated<AnyObject> = msg_send![class!(WKWebView), alloc];
            let content = NSRect::new(NSPoint::new(0.0, 0.0), size);
            msg_send![alloc, initWithFrame: content, configuration: &*config]
        };
        // SAFETY: as above.
        unsafe {
            let occlusion = sel!(_setWindowOcclusionDetectionEnabled:);
            let responds: bool = msg_send![&*web, respondsToSelector: occlusion];
            if responds {
                let _: () = msg_send![&*web, _setWindowOcclusionDetectionEnabled: Bool::NO];
            } else {
                log::warn!(
                    "Web talk: this macOS's WebKit cannot be kept drawing off screen — the page \
                     may stand still"
                );
            }
            let _: () = msg_send![&*web, setPageZoom: 1.0 / scale];
            let _: () = msg_send![&*window, setContentView: &*web];
            let _: bool = msg_send![&*window, makeFirstResponder: &*web];
        }
        suspend_media(&web, true);
        let url = file_url(path);
        let dir = file_url(path.parent().unwrap_or(path));
        // SAFETY: file URLs; the page may read beside its file (its pictures,
        // its scripts), nothing above that folder.
        unsafe {
            let _: Option<Retained<AnyObject>> =
                msg_send![&*web, loadFileURL: &*url, allowingReadAccessToURL: &*dir];
        }
        window.orderBack(None);
        PAGES.with_borrow_mut(|pages| {
            pages.insert(
                key.to_string(),
                Page {
                    window,
                    web,
                    pixels,
                    generation,
                    slide,
                    opened: Instant::now(),
                    ready: false,
                },
            );
        });
        Ok(())
    }

    fn file_url(path: &Path) -> Retained<AnyObject> {
        let text = NSString::from_str(&path.to_string_lossy());
        // SAFETY: NSURL's class method, any path.
        unsafe { msg_send![class!(NSURL), fileURLWithPath: &*text] }
    }

    /// What a script gave back, when a string.
    fn js_string(result: *mut AnyObject) -> Option<String> {
        if result.is_null() {
            return None;
        }
        // SAFETY: WebKit hands a Foundation object or nil; read as a string
        // only once it says it is one.
        unsafe {
            let is_string: bool = msg_send![&*result, isKindOfClass: class!(NSString)];
            is_string.then(|| (*result.cast::<NSString>()).to_string())
        }
    }

    /// Whether a script gave back `true`.
    fn js_true(result: *mut AnyObject) -> bool {
        if result.is_null() {
            return false;
        }
        // SAFETY: as above; a number's value read only from a number.
        unsafe {
            let is_number: bool = msg_send![&*result, isKindOfClass: class!(NSNumber)];
            is_number && msg_send![&*result, boolValue]
        }
    }

    /// Run `script` on the page, `then` given what it gave back.
    fn run_script(web: &AnyObject, script: &str, then: impl Fn(*mut AnyObject) + 'static) {
        let script = NSString::from_str(script);
        let handler = RcBlock::new(move |result: *mut AnyObject, _error: *mut AnyObject| {
            then(result);
        });
        // SAFETY: WKWebView's evaluateJavaScript, on the main thread; the
        // handler is called on it too.
        unsafe {
            let _: () = msg_send![web, evaluateJavaScript: &*script, completionHandler: &*handler];
        }
    }

    /// See how far page `key` (opening `generation`) has got, until it is
    /// set up — then to its slide, a moment to be drawn, and
    /// [`WebEvent::Ready`] — or has failed, or taken too long.
    fn watch(key: String, generation: u64, tx: mpsc::Sender<WebEvent>) {
        let found = PAGES.with_borrow(|pages| {
            pages
                .get(&key)
                .filter(|p| p.generation == generation)
                .map(|p| (p.web.clone(), p.opened))
        });
        // Closed, or opened again since: nothing more to watch.
        let Some((web, opened)) = found else {
            return;
        };
        // SAFETY: WKWebView's documented property.
        let loading: bool = unsafe { msg_send![&*web, isLoading] };
        run_script(&web, READY_JS, move |result| {
            let (key, tx) = (key.clone(), tx.clone());
            let fail = |why: &str| {
                let _ = tx.send(WebEvent::Failed {
                    key: key.clone(),
                    generation,
                    attempt: None,
                    why: why.to_string(),
                });
            };
            match js_string(result).as_deref() {
                Some("ready") => set_up(key, generation, tx),
                Some("blank") if !loading && opened.elapsed() > Duration::from_secs(2) => {
                    fail("the page could not be opened");
                }
                _ if opened.elapsed() > LOAD_TIMEOUT => fail(
                    "the page did not finish loading in 30 s — what it loads from the internet \
                     may not be reachable",
                ),
                _ => after(WATCH_EVERY, move || watch(key, generation, tx)),
            }
        });
    }

    /// Page `key` has set itself up: to the slide it was left on — there,
    /// and drawn there, before it is ready — or a moment to be drawn.
    fn set_up(key: String, generation: u64, tx: mpsc::Sender<WebEvent>) {
        let slide = PAGES.with_borrow_mut(|pages| {
            pages
                .get_mut(&key)
                .filter(|p| p.generation == generation)
                .map(|p| p.slide.take())
        });
        let Some(slide) = slide else {
            return;
        };
        match slide.filter(|&n| n > 1) {
            Some(n) => restore(key, generation, n, tx),
            None => after(DRAWN, move || ready(&key, generation, &tx)),
        }
    }

    /// Put page `key` at slide `n`: told, for a deck that can be (reveal.js,
    /// at once), and ready once it says it is there; else walked there a key
    /// at a time, and ready a moment after.
    fn restore(key: String, generation: u64, n: u32, tx: mpsc::Sender<WebEvent>) {
        let Some((web, window)) = page_of(&key, generation) else {
            return;
        };
        let walker = (web.clone(), window);
        run_script(&web, &restore_js(n), move |result| {
            let (key, tx) = (key.clone(), tx.clone());
            if js_true(result) {
                arrive(key, generation, n, Instant::now(), tx);
            } else {
                for _ in 0..walk(n) {
                    press(&walker.0, &walker.1, 124, 0xF703);
                }
                after(WALKED, move || ready(&key, generation, &tx));
            }
        });
    }

    /// Until page `key` says it is at slide `n` and has been drawn there,
    /// its transitions let go — or has been given long enough — then a
    /// moment more, and ready.
    fn arrive(key: String, generation: u64, n: u32, since: Instant, tx: mpsc::Sender<WebEvent>) {
        let Some((web, _)) = page_of(&key, generation) else {
            return;
        };
        run_script(&web, SETTLED_JS, move |result| {
            let (key, tx) = (key.clone(), tx.clone());
            let there = settled_of(result) == Some((n, true));
            if there || since.elapsed() > ARRIVE_WITHIN {
                after(DRAWN, move || ready(&key, generation, &tx));
            } else {
                after(ARRIVING_EVERY, move || arrive(key, generation, n, since, tx));
            }
        });
    }

    /// Page `key` is set up, at its slide and drawn: said ready.
    fn ready(key: &str, generation: u64, tx: &mpsc::Sender<WebEvent>) {
        let current = PAGES.with_borrow_mut(|pages| {
            pages
                .get_mut(key)
                .filter(|p| p.generation == generation)
                .map(|p| p.ready = true)
                .is_some()
        });
        if !current {
            return;
        }
        let _ = tx.send(WebEvent::Ready {
            key: key.to_string(),
            generation,
        });
        ask_position(key, generation, tx);
        tell_processes();
    }

    /// Page `key`, opened `OVERDUE` ago, not ready yet: given up on, and
    /// why — its process ended should it no longer answer at all.
    fn overdue(key: &str, generation: u64, tx: &mpsc::Sender<WebEvent>) {
        let late = PAGES.with_borrow(|pages| {
            pages
                .get(key)
                .filter(|p| p.generation == generation && !p.ready)
                .map(|p| p.web.clone())
        });
        if let Some(web) = late {
            end_if_stuck(&web);
            let _ = tx.send(WebEvent::Failed {
                key: key.to_string(),
                generation,
                attempt: None,
                why: "the page did not finish loading in 30 s — what it loads from the internet \
                      may not be reachable, or the page has stopped answering"
                    .into(),
            });
        }
    }

    /// A page that does not answer a script in a second is stuck — a script
    /// of its own that never lets go: its process is ended (`WebKit` keeps
    /// waiting on it, and it keeps a core busy for as long as Nida runs;
    /// `WebKit`'s own `_killWebContentProcess` was measured not to end it).
    /// The process is told apart by its number and when it started, taken
    /// while the page is Nida's and checked again, with its program, just
    /// before: only `WebKit`'s process for this page — never the one every
    /// page plays its media in, never another's given its number since.
    /// Asked of every page closed, and of one given up on.
    fn end_if_stuck(web: &AnyObject) {
        let Some(pid) = page_process(web) else {
            return;
        };
        let Some(born) = crate::audio::app_sound::process_birth(pid) else {
            return;
        };
        let answered = std::rc::Rc::new(Cell::new(false));
        let heard = answered.clone();
        run_script(web, "1", move |_| heard.set(true));
        let token = NEXT_STUCK.get().wrapping_add(1);
        NEXT_STUCK.set(token);
        // Kept here on the main thread: the check a second on goes by the
        // token alone.
        STUCK.with_borrow_mut(|stuck| stuck.push((token, answered)));
        after(Duration::from_secs(1), move || {
            let held = STUCK.with_borrow_mut(|stuck| {
                stuck
                    .iter()
                    .position(|(t, _)| *t == token)
                    .map(|at| stuck.remove(at))
            });
            let Some((_, answered)) = held else {
                return;
            };
            let same = crate::audio::app_sound::process_birth(pid) == Some(born)
                && program_of(pid).is_some_and(|path| is_page_program(&path));
            if !answered.get() && same {
                log::warn!("Web talk: its page's process {pid} no longer answers — ended");
                // SAFETY: a signal to WebKit's process for a page of Nida's,
                // checked to be that very process just now.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        });
    }

    /// A page's own process, as this macOS's `WebKit` says it.
    fn page_process(web: &AnyObject) -> Option<i32> {
        // SAFETY: WKWebView's `_webProcessIdentifier` (a pid_t), only when
        // it answers it.
        unsafe {
            let responds: bool = msg_send![web, respondsToSelector: sel!(_webProcessIdentifier)];
            if !responds {
                return None;
            }
            let pid: i32 = msg_send![web, _webProcessIdentifier];
            (pid > 0).then_some(pid)
        }
    }

    /// The program process `pid` runs.
    fn program_of(pid: i32) -> Option<String> {
        let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buffer` holds the size passed.
        let written = unsafe {
            libc::proc_pidpath(
                pid,
                buffer.as_mut_ptr().cast::<std::ffi::c_void>(),
                u32::try_from(buffer.len()).ok()?,
            )
        };
        let written = usize::try_from(written).ok().filter(|&n| n > 0)?;
        buffer.truncate(written);
        String::from_utf8(buffer).ok()
    }

    /// A page's processes, as this macOS's `WebKit` says them: its own,
    /// and the one `WebKit` plays its video and sound in (started when a
    /// page first needs it).
    fn processes_of(web: &AnyObject) -> Vec<i32> {
        let mut pids = Vec::new();
        // SAFETY: WKWebView's `_webProcessIdentifier` and
        // `_gpuProcessIdentifier` (each a pid_t: "i16@0:8", measured on the
        // mini), only when it answers them.
        unsafe {
            let page: bool = msg_send![web, respondsToSelector: sel!(_webProcessIdentifier)];
            if page {
                let pid: i32 = msg_send![web, _webProcessIdentifier];
                pids.push(pid);
            }
            let media: bool = msg_send![web, respondsToSelector: sel!(_gpuProcessIdentifier)];
            if media {
                let pid: i32 = msg_send![web, _gpuProcessIdentifier];
                pids.push(pid);
            }
        }
        pids.retain(|&pid| pid > 0);
        pids
    }

    /// Tell the sound side which processes are this Nida's web talks':
    /// theirs is web talks' sound, and nothing else's.
    fn tell_processes() {
        let pids: BTreeSet<i32> = PAGES.with_borrow(|pages| {
            pages.values().flat_map(|p| processes_of(&p.web)).collect()
        });
        if TOLD.with_borrow(|told| *told != pids) {
            log::info!("Web talks: their sound is WebKit's processes {pids:?}");
            TOLD.set(pids.clone());
        }
        // Each with when it started: a number reused since is another's.
        let born = pids
            .into_iter()
            .map(|pid| (pid, crate::audio::app_sound::process_birth(pid)))
            .collect();
        crate::audio::app_sound::set_web_talk_pids(born);
    }

    /// While pages are open, the sound side told their processes each
    /// second: `WebKit` starts its media process when a page first needs
    /// it, and again should it end.
    fn keep_telling() {
        if !TELLING.replace(true) {
            tell_on();
        }
    }

    /// The pages' processes told, and again a second later while pages
    /// are open.
    fn tell_on() {
        tell_processes();
        if PAGES.with_borrow(HashMap::is_empty) {
            TELLING.set(false);
        } else {
            after(Duration::from_secs(1), tell_on);
        }
    }

    /// Pause or let play every video and sound of the page.
    fn suspend_media(web: &AnyObject, suspended: bool) {
        // SAFETY: WKWebView's method (macOS 12+), only when it answers it; no
        // completion handler.
        unsafe {
            let sel = sel!(setAllMediaPlaybackSuspended:completionHandler:);
            let responds: bool = msg_send![web, respondsToSelector: sel];
            if responds {
                let _: () = msg_send![
                    web,
                    setAllMediaPlaybackSuspended: Bool::new(suspended),
                    completionHandler: std::ptr::null::<block2::Block<dyn Fn()>>()
                ];
            }
        }
    }

    /// Silence the page's sound, or let it be heard: `WebKit`'s own page
    /// mute (Safari's Mute Tab), when this macOS's has it.
    fn mute(web: &AnyObject, muted: bool) {
        // SAFETY: WKWebView's `_setPageMuted:` (an NSUInteger of
        // _WKMediaMutedState: 1, its audio), only when it answers it.
        unsafe {
            let sel = sel!(_setPageMuted:);
            let responds: bool = msg_send![web, respondsToSelector: sel];
            if responds {
                let _: () = msg_send![web, _setPageMuted: usize::from(muted)];
            } else {
                log::warn!("Web talk: this macOS's WebKit cannot silence a page");
            }
        }
    }

    /// A key pressed on the page and let go.
    fn press(web: &AnyObject, window: &NSWindow, code: u16, ch: u32) {
        let chars = char::from_u32(ch).map(String::from).unwrap_or_default();
        let chars = NSString::from_str(&chars);
        for (kind, method) in [(10usize, "keyDown"), (11usize, "keyUp")] {
            // SAFETY: NSEvent's documented constructor; the event goes to the
            // page's own view, in its own window.
            unsafe {
                let event: Option<Retained<AnyObject>> = msg_send![
                    class!(NSEvent),
                    keyEventWithType: kind,
                    location: NSPoint::new(0.0, 0.0),
                    modifierFlags: 0usize,
                    timestamp: 0.0f64,
                    windowNumber: window.windowNumber(),
                    context: std::ptr::null::<AnyObject>(),
                    characters: &*chars,
                    charactersIgnoringModifiers: &*chars,
                    isARepeat: false,
                    keyCode: code
                ];
                let Some(event) = event else {
                    continue;
                };
                if method == "keyDown" {
                    let _: () = msg_send![web, keyDown: &*event];
                } else {
                    let _: () = msg_send![web, keyUp: &*event];
                }
            }
        }
    }

    /// A step on or back: told, for a deck that can be (reveal.js — on
    /// through its columns); else its key pressed.
    fn step(page: &Page, to: Go) {
        let (Some(script), Some((code, ch))) = (step_js(to), key_of(to)) else {
            return;
        };
        let (web, window) = (page.web.clone(), page.window.clone());
        run_script(&page.web, &script, move |result| {
            if !js_true(result) {
                press(&web, &window, code, ch);
            }
        });
    }

    /// To slide `n`: told, for a deck that can be (reveal.js); else from
    /// the first (Home) on a key at a time, at most a few hundred.
    fn go_to_slide(page: &Page, n: u32) {
        let (web, window) = (page.web.clone(), page.window.clone());
        run_script(&page.web, &go_to_js(n), move |result| {
            if !js_true(result) {
                press(&web, &window, 115, 0xF729);
                for _ in 0..walk(n) {
                    press(&web, &window, 124, 0xF703);
                }
            }
        });
    }

    /// Where a deck said it is: its slide and how many it has, from
    /// [`POSITION_JS`]'s answer.
    fn position_of(result: *mut AnyObject) -> Option<(u32, u32)> {
        if result.is_null() {
            return None;
        }
        // SAFETY: WebKit hands an array of two numbers, or something else
        // when the page is no deck that says: then nothing.
        unsafe {
            let is_array: bool = msg_send![&*result, isKindOfClass: class!(NSArray)];
            if !is_array {
                return None;
            }
            let count: usize = msg_send![&*result, count];
            if count < 2 {
                return None;
            }
            let number = |index: usize| -> Option<u32> {
                let item: *mut AnyObject = msg_send![&*result, objectAtIndex: index];
                if item.is_null() {
                    return None;
                }
                let is_number: bool = msg_send![&*item, isKindOfClass: class!(NSNumber)];
                if !is_number {
                    return None;
                }
                let value: i64 = msg_send![&*item, longLongValue];
                u32::try_from(value).ok()
            };
            Some((number(0)?.max(1), number(1)?))
        }
    }

    /// Where a deck put at its slide is, and whether it has been drawn
    /// there: [`SETTLED_JS`]'s answer.
    fn settled_of(result: *mut AnyObject) -> Option<(u32, bool)> {
        if result.is_null() {
            return None;
        }
        // SAFETY: WebKit hands an array of a number and a boolean, or
        // something else: then nothing.
        unsafe {
            let is_array: bool = msg_send![&*result, isKindOfClass: class!(NSArray)];
            if !is_array {
                return None;
            }
            let count: usize = msg_send![&*result, count];
            if count < 2 {
                return None;
            }
            let at: *mut AnyObject = msg_send![&*result, objectAtIndex: 0usize];
            let drawn: *mut AnyObject = msg_send![&*result, objectAtIndex: 1usize];
            if !is_number(at) {
                return None;
            }
            let at: i64 = msg_send![&*at, longLongValue];
            Some((u32::try_from(at).ok()?, js_true(drawn)))
        }
    }

    /// Whether `item` is a number (a boolean is one too).
    fn is_number(item: *mut AnyObject) -> bool {
        // SAFETY: a Foundation object from WebKit, or nil (checked first).
        !item.is_null() && unsafe { msg_send![&*item, isKindOfClass: class!(NSNumber)] }
    }

    /// Ask page `key` (opening `generation`) where it is; a deck that says
    /// sends a position.
    fn ask_position(key: &str, generation: u64, tx: &mpsc::Sender<WebEvent>) {
        let Some((web, _)) = page_of(key, generation) else {
            return;
        };
        let (key, tx) = (key.to_string(), tx.clone());
        run_script(&web, POSITION_JS, move |result| {
            if let Some((at, of)) = position_of(result) {
                let _ = tx.send(WebEvent::Position {
                    key: key.clone(),
                    generation,
                    at,
                    of,
                });
            }
        });
    }

    /// Capture the page's window — off a screen, found by its number — on
    /// a thread of its own: opening a capture takes a moment.
    fn capture(
        key: String,
        generation: u64,
        attempt: u64,
        number: isize,
        size: NSSize,
        pixels: (u32, u32),
        tx: mpsc::Sender<WebEvent>,
    ) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let target = CaptureTargetInfo {
            kind: CaptureTargetKind::Window,
            platform_id: u64::try_from(number).unwrap_or(0),
            label: "Nida — web talk".into(),
            app: std::env::var("__CFBundleIdentifier")
                .ok()
                .or_else(|| Some("lt.proconf.nida".into())),
            title: Some("Nida — web talk".into()),
            width: size.width.round() as u32,
            height: size.height.round() as u32,
            is_varda: true,
        };
        let spawned = std::thread::Builder::new()
            .name("web-talk-capture".into())
            .spawn(move || {
                let config = crate::presentation::manager::capture_config(&target, pixels, 0);
                let event = match crate::screen_capture::platform::open(&target, &config) {
                    Ok(capture) => WebEvent::Captured {
                        key,
                        generation,
                        attempt,
                        target,
                        capture,
                        config,
                    },
                    Err(e) => WebEvent::Failed {
                        key,
                        generation,
                        attempt: Some(attempt),
                        why: format!("its window was not captured: {e}"),
                    },
                };
                let _ = tx.send(event);
            });
        if let Err(e) = spawned {
            log::warn!("Web talk: could not start capturing its window: {e}");
        }
    }
}

/// A stand-in web engine for tests: pages "open" and set up at once (or when
/// told, with [`FakeWeb::hold_ready`]), each captured as a made-up window
/// with a test pattern; a deck of five slides.
#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Clone, Default)]
pub struct FakeWeb {
    state: std::sync::Arc<std::sync::Mutex<FakeWebState>>,
}

#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Default)]
struct FakeWebState {
    events: Vec<WebEvent>,
    /// The pages open: key → (slide, playing, generation).
    pages: std::collections::BTreeMap<String, (u32, bool, u64)>,
    /// What was done: `load a`, `play a`, `pause a`, `mute a`, `capture a`,
    /// `Next a`, `unload a`.
    log: Vec<String>,
    next_window: u64,
    /// Pages say they are set up only when told.
    hold: bool,
    /// Captures answered only when told: (key, attempt), in order.
    held_captures: Option<Vec<(String, u64)>>,
    /// Each page's window number: one per page, as `WebKit`'s are, however
    /// often it is captured.
    windows: std::collections::BTreeMap<String, u64>,
    /// How many captures from now on open with no picture ever.
    blind: u32,
}

/// A capture that opened and never sends a picture: a stand-in, in tests.
#[cfg(any(test, feature = "test-fixtures"))]
struct Blind(String);

#[cfg(any(test, feature = "test-fixtures"))]
impl ScreenCaptureBackend for Blind {
    fn label(&self) -> &str {
        &self.0
    }

    fn resolution(&self) -> (u32, u32) {
        (1920, 1080)
    }

    fn next_frame(&mut self) -> Option<crate::screen_capture::backend::CaptureFrame> {
        None
    }

    fn set_config(&mut self, _config: &CaptureConfig) -> Result<(), crate::screen_capture::backend::CaptureError> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
impl FakeWeb {
    /// What was done to the pages, in order.
    #[must_use]
    pub fn log(&self) -> Vec<String> {
        self.state.lock().map(|s| s.log.clone()).unwrap_or_default()
    }

    /// The pages open: key, slide, whether playing.
    #[must_use]
    pub fn pages(&self) -> Vec<(String, u32, bool)> {
        self.state
            .lock()
            .map(|s| s.pages.iter().map(|(k, (at, p, _))| (k.clone(), *at, *p)).collect())
            .unwrap_or_default()
    }

    /// Pages opened from now on say they are set up only when
    /// [`Self::set_up`] says so — a page slow to load.
    pub fn hold_ready(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.hold = true;
        }
    }

    /// Page `key` has set itself up.
    pub fn set_up(&self, key: &str) {
        if let Ok(mut s) = self.state.lock() {
            if let Some(&(_, _, generation)) = s.pages.get(key) {
                s.events.push(WebEvent::Ready {
                    key: key.into(),
                    generation,
                });
            }
        }
    }

    /// Which opening of page `key` this is.
    #[must_use]
    pub fn generation_of(&self, key: &str) -> Option<u64> {
        self.state.lock().ok()?.pages.get(key).map(|p| p.2)
    }

    /// Something found, as if the engine had: a late answer, in tests.
    pub fn push(&self, event: WebEvent) {
        if let Ok(mut s) = self.state.lock() {
            s.events.push(event);
        }
    }

    /// Captures asked from now on are answered only by
    /// [`Self::answer_captures`] — a window slow to capture.
    pub fn hold_captures(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.held_captures = Some(Vec::new());
        }
    }

    /// The captures held so far answered, and the rest at once.
    pub fn answer_captures(&self) {
        let held = self
            .state
            .lock()
            .ok()
            .and_then(|mut s| s.held_captures.take())
            .unwrap_or_default();
        for (key, attempt) in held {
            self.clone().answer(&key, attempt);
        }
    }

    /// The next `n` captures open with no picture ever.
    pub fn blind_captures(&self, n: u32) {
        if let Ok(mut s) = self.state.lock() {
            s.blind = n;
        }
    }

    fn answer(&mut self, key: &str, attempt: u64) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        let Some(&(at, _, generation)) = s.pages.get(key) else {
            return;
        };
        let platform_id = if let Some(&window) = s.windows.get(key) {
            window
        } else {
            s.next_window += 1;
            let window = 5000 + s.next_window;
            s.windows.insert(key.to_string(), window);
            window
        };
        let target = CaptureTargetInfo {
            kind: crate::screen_capture::backend::CaptureTargetKind::Window,
            platform_id,
            label: format!("Nida — web talk {key}"),
            app: Some("lt.proconf.nida".into()),
            title: Some("Nida — web talk".into()),
            width: 1920,
            height: 1080,
            is_varda: true,
        };
        let config = CaptureConfig::default();
        let capture: Box<dyn ScreenCaptureBackend> = if s.blind > 0 {
            s.blind -= 1;
            Box::new(Blind(target.label.clone()))
        } else {
            Box::new(crate::screen_capture::backend::MockBackend::new(
                target.label.clone(),
                target.width,
                target.height,
                config.clone(),
            ))
        };
        s.events.push(WebEvent::Captured {
            key: key.into(),
            generation,
            attempt,
            target,
            capture,
            config,
        });
        s.events.push(WebEvent::Position {
            key: key.into(),
            generation,
            at,
            of: FAKE_SLIDES,
        });
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
const FAKE_SLIDES: u32 = 5;

#[cfg(any(test, feature = "test-fixtures"))]
impl WebEngine for FakeWeb {
    fn load(&mut self, key: &str, generation: u64, path: &Path, _pixels: (u32, u32), slide: Option<u32>) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        s.log.push(format!("load {key}"));
        if path.is_file() {
            let at = slide.unwrap_or(1).clamp(1, FAKE_SLIDES);
            s.pages.insert(key.to_string(), (at, false, generation));
            if !s.hold {
                s.events.push(WebEvent::Ready {
                    key: key.into(),
                    generation,
                });
            }
        } else {
            s.events.push(WebEvent::Failed {
                key: key.into(),
                generation,
                attempt: None,
                why: "its file is not there".into(),
            });
        }
    }

    fn set_playing(&mut self, key: &str, playing: bool) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        let Some(page) = s.pages.get_mut(key) else {
            return;
        };
        page.1 = playing;
        s.log
            .push(format!("{} {key}", if playing { "play" } else { "pause" }));
    }

    fn set_muted(&mut self, key: &str, muted: bool) {
        if let Ok(mut s) = self.state.lock() {
            s.log
                .push(format!("{} {key}", if muted { "mute" } else { "unmute" }));
        }
    }

    fn capture(&mut self, key: &str, attempt: u64) {
        {
            let Ok(mut s) = self.state.lock() else {
                return;
            };
            if !s.pages.contains_key(key) {
                return;
            }
            s.log.push(format!("capture {key}"));
            if let Some(held) = s.held_captures.as_mut() {
                held.push((key.to_string(), attempt));
                return;
            }
        }
        self.answer(key, attempt);
    }

    fn resize(&mut self, key: &str, pixels: (u32, u32)) {
        if let Ok(mut s) = self.state.lock() {
            s.log.push(format!("resize {key} {}x{}", pixels.0, pixels.1));
        }
    }

    fn go(&mut self, key: &str, to: Go) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        let Some(page) = s.pages.get_mut(key) else {
            return;
        };
        page.0 = match to {
            Go::Next => (page.0 + 1).min(FAKE_SLIDES),
            Go::Previous => page.0.saturating_sub(1).max(1),
            Go::First => 1,
            Go::Last => FAKE_SLIDES,
            Go::Slide(n) => n.clamp(1, FAKE_SLIDES),
        };
        let (at, generation) = (page.0, page.2);
        s.log.push(format!("{to:?} {key}"));
        s.events.push(WebEvent::Position {
            key: key.into(),
            generation,
            at,
            of: FAKE_SLIDES,
        });
    }

    fn unload(&mut self, key: &str) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        s.pages.remove(key);
        s.log.push(format!("unload {key}"));
    }

    fn poll(&mut self) -> Vec<WebEvent> {
        self.state
            .lock()
            .map(|mut s| std::mem::take(&mut s.events))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clickers_steps_are_the_arrow_keys_an_html_deck_answers_to() {
        assert_eq!(key_of(Go::Next), Some((124, 0xF703)));
        assert_eq!(key_of(Go::Previous), Some((123, 0xF702)));
        assert_eq!(key_of(Go::First), Some((115, 0xF729)));
        assert_eq!(key_of(Go::Last), Some((119, 0xF72B)));
        assert_eq!(key_of(Go::Slide(3)), None, "told by script, or walked to");
    }

    #[test]
    fn going_to_a_slide_tells_reveal_its_number() {
        let js = go_to_js(7);
        assert!(js.ends_with("})(7)"), "{js}");
        assert!(js.contains("Reveal.slide(i.h,i.v)"));
    }

    /// reveal.js stepped by its own next and previous: on down a column of
    /// slides, where the right arrow would skip to the next column. Back to
    /// the first slide, its builds are hidden again — the talk from its
    /// start, not as the rehearsal left it.
    #[test]
    fn reveal_is_stepped_through_its_columns_by_its_own_next() {
        let next = step_js(Go::Next).unwrap();
        assert!(next.contains("Reveal.next();return true;"), "{next}");
        assert!(step_js(Go::Previous).unwrap().contains("Reveal.prev()"));
        assert!(step_js(Go::First).unwrap().contains("Reveal.slide(0,0,-1);"));
        assert!(step_js(Go::Last).unwrap().contains("all[all.length-1]"));
        assert_eq!(step_js(Go::Slide(4)), None);
    }

    /// Only `WebKit`'s process for a page is ever ended — not the one every
    /// page plays its media in.
    #[test]
    fn only_a_pages_own_process_is_one_to_end() {
        let at = "/System/Volumes/Preboot/Cryptexes/OS/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices";
        assert!(is_page_program(&format!(
            "{at}/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent"
        )));
        assert!(!is_page_program(&format!(
            "{at}/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU"
        )));
        assert!(!is_page_program("/Applications/Safari.app/Contents/MacOS/Safari"));
        assert!(!is_page_program("/tmp/com.apple.WebKit.WebContent"));
    }

    /// A walk to a slide, for a deck that cannot be told, is bounded: a
    /// slide number sent by mistake never holds the page for long.
    #[test]
    fn a_walk_to_a_slide_goes_a_few_hundred_steps_at_most() {
        assert_eq!(walk(1), 0);
        assert_eq!(walk(0), 0);
        assert_eq!(walk(12), 11);
        assert_eq!(walk(u32::MAX), MAX_WALK - 1);
    }
}
