//! Keeping the computer and its displays awake while Varda runs.
//!
//! macOS puts idle displays to sleep (30 minutes by default), and a sleeping
//! display is an HDMI port with no signal: the LED wall and the TV went black
//! mid-talk whenever nobody had touched the machine for that long. The
//! screensaver and the lock that follows it are just as fatal on air. So, like
//! vMix or a slideshow presenter in full screen, Varda holds the machine awake for as
//! long as it runs:
//!
//! - **macOS** — a user-initiated, latency-critical activity with idle display
//!   sleep disabled: no system sleep, no display sleep, no screensaver, and no
//!   App Nap throttling the render loop when its windows are covered. Listed
//!   in `pmset -g assertions`.
//! - **Linux** — a `systemd-inhibit` idle + sleep lock, which hypridle (the
//!   idle daemon on Omarchy) honours: no screensaver, lock or screen-off. It
//!   lives in a child process tied to Varda's pid, so a crash cannot leave it
//!   behind. Listed in `systemd-inhibit --list`.
//! - **Windows** — a process-owned display + system power request, listed in
//!   `powercfg /requests`. Closing the handle (including on process exit)
//!   releases it. Explicit sleep and lock remain under the user's control.

#[cfg(target_os = "windows")]
#[path = "keep_awake/windows.rs"]
mod windows;

/// Why the machine is held awake, as the OS lists it.
pub(crate) const REASON: &str = "Nida: live outputs";

/// Holds the machine awake while wanted, and lets go on drop.
#[derive(Default)]
pub(crate) struct KeepAwake {
    #[cfg(target_os = "macos")]
    activity: Option<Activity>,
    #[cfg(target_os = "linux")]
    inhibitor: Option<std::process::Child>,
    #[cfg(target_os = "windows")]
    request: Option<windows::PowerRequest>,
    /// Set once holding has failed, so a missing tool is reported once rather
    /// than retried every frame.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    unavailable: bool,
}

/// The token `beginActivityWithOptions:reason:` hands back, kept for
/// `endActivity:`.
#[cfg(target_os = "macos")]
struct Activity(
    objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>,
);

// SAFETY: NSProcessInfo's activity calls may be made from any thread, and the
// token is only ever handed back to `endActivity:` — never messaged itself.
// It moves with the engine between the main thread and the engine thread.
#[cfg(target_os = "macos")]
unsafe impl Send for Activity {}

impl KeepAwake {
    /// Hold or let go. Cheap to call every frame: it only reaches the OS when
    /// the answer changes.
    pub(crate) fn set(&mut self, wanted: bool) {
        if wanted == self.is_held() {
            return;
        }
        if wanted {
            self.hold();
        } else {
            self.release();
            log::info!("Letting the computer and displays sleep again");
        }
    }

    fn is_held(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.activity.is_some();
        #[cfg(target_os = "linux")]
        return self.inhibitor.is_some() || self.unavailable;
        #[cfg(target_os = "windows")]
        return self.request.is_some() || self.unavailable;
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        return true;
    }

    #[cfg(target_os = "macos")]
    fn hold(&mut self) {
        use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
        // `UserInitiated` already includes idle *system* sleep.
        let options = NSActivityOptions::UserInitiated
            | NSActivityOptions::IdleDisplaySleepDisabled
            | NSActivityOptions::LatencyCritical;
        let activity = NSProcessInfo::processInfo()
            .beginActivityWithOptions_reason(options, &NSString::from_str(REASON));
        self.activity = Some(Activity(activity));
        log::info!("Keeping the computer and displays awake while Nida runs");
    }

    #[cfg(target_os = "linux")]
    fn hold(&mut self) {
        use std::process::{Command, Stdio};
        // `tail --pid` ends when Varda does, releasing the lock even if Varda
        // is killed without running its destructors.
        let spawned = Command::new("systemd-inhibit")
            .args([
                "--what=idle:sleep",
                "--who=Nida",
                &format!("--why={REASON}"),
                "--mode=block",
                "tail",
                &format!("--pid={}", std::process::id()),
                "-f",
                "/dev/null",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(child) => {
                self.inhibitor = Some(child);
                log::info!("Keeping the computer and displays awake while Nida runs");
            }
            Err(e) => {
                self.unavailable = true;
                log::warn!(
                    "Could not hold the machine awake (systemd-inhibit: {e}); \
                     disable the screensaver and sleep by hand for the show"
                );
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn hold(&mut self) {
        match windows::PowerRequest::new(REASON) {
            Ok(request) => {
                self.request = Some(request);
                log::info!("Keeping the computer and displays awake while Nida runs");
            }
            Err(e) => {
                self.unavailable = true;
                log::warn!(
                    "Could not hold the machine awake (Windows power request: {e}); \
                     disable display and system sleep by hand for the show"
                );
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    fn hold(&mut self) {}

    fn release(&mut self) {
        #[cfg(target_os = "windows")]
        {
            self.request = None;
            self.unavailable = false;
        }
        #[cfg(target_os = "macos")]
        if let Some(activity) = self.activity.take() {
            // Safety: `activity` is the object `beginActivityWithOptions:reason:`
            // returned, which is what `endActivity:` expects.
            unsafe { objc2_foundation::NSProcessInfo::processInfo().endActivity(&activity.0) };
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(mut child) = self.inhibitor.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            self.unavailable = false;
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    #[test]
    fn windows_hold_is_idempotent_and_can_move_between_threads() {
        let mut keep = KeepAwake::default();
        keep.set(false);
        assert!(!keep.is_held());
        keep.set(true);
        assert!(
            keep.request.is_some(),
            "Windows must accept the power request"
        );
        let request = std::ptr::from_ref(keep.request.as_ref().unwrap());
        keep.set(true);
        assert_eq!(request, std::ptr::from_ref(keep.request.as_ref().unwrap()));
        std::thread::spawn(move || {
            keep.set(false);
            assert!(!keep.is_held());
            assert!(keep.request.is_none());
            keep.set(true);
            assert!(keep.request.is_some());
            // Drop on this thread must release the newly acquired request too.
        })
        .join()
        .unwrap();
    }

    #[test]
    fn windows_failed_hold_is_retried_only_after_disabling() {
        let mut keep = KeepAwake {
            request: None,
            unavailable: true,
        };
        keep.set(true);
        assert!(keep.request.is_none());
        keep.set(false);
        assert!(!keep.unavailable);
        keep.set(true);
        assert!(keep.request.is_some());
    }

    /// Run elevated and without another Nida instance holding a request.
    #[test]
    #[ignore = "powercfg /requests requires an elevated Windows terminal"]
    fn windows_os_lists_and_releases_power_requests() {
        fn count() -> usize {
            let output = std::process::Command::new("powercfg")
                .arg("/requests")
                .output()
                .unwrap();
            assert!(output.status.success(), "powercfg must run elevated");
            String::from_utf8_lossy(&output.stdout)
                .matches(REASON)
                .count()
        }
        let before = count();
        let mut keep = KeepAwake::default();
        keep.set(true);
        assert_eq!(count(), before + 2, "display and system requests");
        keep.set(true);
        keep.set(false);
        assert_eq!(count(), before);
        keep.set(true);
        drop(keep);
        assert_eq!(count(), before, "drop must release both requests");
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    /// What the OS lists as holding the machine awake for `pid` under our
    /// reason, or `None` where the listing tool is missing.
    fn listed(pid: u32) -> Option<Vec<String>> {
        #[cfg(target_os = "macos")]
        let (program, args, marker) = (
            "pmset",
            ["-g", "assertions"].as_slice(),
            format!("pid {pid}("),
        );
        #[cfg(target_os = "linux")]
        let (program, args, marker) =
            ("systemd-inhibit", ["--list"].as_slice(), format!(" {pid} "));
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .ok()?;
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|line| line.contains(&marker) && line.contains(REASON))
                .map(str::to_string)
                .collect(),
        )
    }

    /// The pid the OS files the hold under: Varda itself on macOS, the
    /// `systemd-inhibit` child on Linux.
    fn holder(keep: &KeepAwake) -> u32 {
        #[cfg(target_os = "linux")]
        return keep.inhibitor.as_ref().map_or(0, std::process::Child::id);
        #[cfg(not(target_os = "linux"))]
        {
            let _ = keep;
            std::process::id()
        }
    }

    fn settle() {
        // systemd-inhibit registers from its own process.
        #[cfg(target_os = "linux")]
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    #[test]
    fn the_os_sees_the_machine_held_awake_and_then_released() {
        let mut keep = KeepAwake::default();
        let Some(before) = listed(holder(&keep)) else {
            return;
        };
        assert!(
            before.is_empty(),
            "precondition: nothing held yet: {before:?}"
        );

        keep.set(true);
        settle();
        let pid = holder(&keep);
        let held = listed(pid).unwrap_or_default();
        #[cfg(target_os = "macos")]
        for kind in ["PreventUserIdleDisplaySleep", "PreventUserIdleSystemSleep"] {
            assert!(
                held.iter().any(|l| l.contains(kind)),
                "macOS must list {kind} for Nida, got {held:?}"
            );
        }
        #[cfg(target_os = "linux")]
        assert!(!held.is_empty(), "systemd must list Nida's idle/sleep lock");

        keep.set(true);
        keep.set(false);
        settle();
        assert_eq!(listed(pid), Some(vec![]), "letting go must drop the hold");

        keep.set(true);
        settle();
        let pid = holder(&keep);
        drop(keep);
        settle();
        assert_eq!(listed(pid), Some(vec![]), "dropping must drop the hold");
    }
}
