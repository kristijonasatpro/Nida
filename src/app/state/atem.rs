//! A warning for a show taking an ATEM's webcam: ATEM Setup left open holds
//! the ATEM's USB control interface, and the webcam's pictures stall every
//! few seconds — each stall a frozen picture until the camera is started
//! again (an M5 Max, 2026-10-28: 21 stalls in 8.5 minutes while ATEM Setup
//! ran, none once it was quit). ATEM Software Control does not do it.

use std::time::{Duration, Instant};

use super::super::VardaApp;
use crate::notifications::NotificationLevel;

/// ATEM Setup's program. Looked for by name: its bundle ID changes with
/// its version (com.blackmagic-design.Switchers.Setup, a capital S twice,
/// on an M5 Max; com.blackmagic-design.atem.utility on a Mac mini), and the
/// first one, asked for in lower case, was never found.
const ATEM_SETUP: &str = "ATEM Setup";
/// How often it is looked for, while a camera is open.
const CHECK_EVERY: Duration = Duration::from_secs(5);
/// A camera whose pictures stopped this lately, while ATEM Setup runs, is
/// named in the warning.
const STOPPED_LATELY: Duration = Duration::from_secs(60);
/// The warning's key: said once while ATEM Setup stays open, again if it is
/// opened again.
const WARNED: &str = "atem-setup-open";

impl VardaApp {
    /// Every few seconds while a camera is open: say so while ATEM Setup
    /// runs and a camera keeps stopping.
    pub(crate) fn check_atem_setup(&mut self) {
        let now = Instant::now();
        if self.atem_checked.is_some_and(|at| now - at < CHECK_EVERY) {
            return;
        }
        self.atem_checked = Some(now);
        if self.camera_manager.open_names().is_empty() {
            return;
        }
        let stopped = self.camera_manager.stopped_names(STOPPED_LATELY);
        let running = crate::audio::app_sound::process_named(ATEM_SETUP);
        self.warn_atem_setup(&stopped, running);
    }

    /// Warn, once, that ATEM Setup stalls an ATEM's camera — while it runs
    /// and a camera's pictures keep stopping (`stopped`, by name): the
    /// camera named is the one stopping, not one guessed by its name (both
    /// an ATEM and an UltraStudio are "Blackmagic"). Forget the warning
    /// once it is quit.
    fn warn_atem_setup(&mut self, stopped: &[String], setup_running: bool) {
        if !setup_running {
            self.session.notifications.clear_once_key_prefix(WARNED);
            return;
        }
        if let Some(camera) = stopped.first() {
            self.session.notifications.notify_once(
                WARNED,
                NotificationLevel::Warning,
                format!(
                    "Camera '{camera}' keeps stopping while ATEM Setup is open: quit ATEM \
                     Setup during the show — it stalls an ATEM's USB camera. ATEM Software \
                     Control is fine."
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless_app() -> Option<VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        VardaApp::new(gpu, &crate::testing::headless_config()).ok()
    }

    fn warnings(app: &VardaApp) -> Vec<String> {
        app.session
            .notifications
            .visible()
            .iter()
            .filter(|n| n.message.contains("ATEM Setup"))
            .map(|n| n.message.clone())
            .collect()
    }

    /// ATEM Setup running while a camera keeps stopping is said once,
    /// naming that camera, not every look; quit and opened again, said
    /// again; with no camera stopping, or ATEM Setup not open, never.
    #[test]
    fn atem_setup_is_warned_of_once_while_it_runs() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let atem = vec!["Blackmagic Design".to_string()];
        app.warn_atem_setup(&[], true);
        assert!(warnings(&app).is_empty(), "no camera stopping");
        app.warn_atem_setup(&atem, false);
        assert!(warnings(&app).is_empty(), "ATEM Setup not open");
        app.warn_atem_setup(&atem, true);
        app.warn_atem_setup(&atem, true);
        assert_eq!(
            warnings(&app).len(),
            1,
            "once while it runs: {:?}",
            warnings(&app)
        );
        assert!(warnings(&app)[0].contains("Camera 'Blackmagic Design' keeps stopping"));
        app.warn_atem_setup(&atem, false);
        app.warn_atem_setup(&atem, true);
        assert_eq!(warnings(&app).len(), 2, "opened again: said again");
    }
}
