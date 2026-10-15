//! Monitor names an output can be addressed by.
//!
//! winit on macOS names a display by its EDID model number, so two identical
//! displays — a pair of Blackmagic HDMI converters, say — both come back as
//! `Monitor #0` and the second could never be chosen. Where names collide, each
//! gets its desktop position appended (`Monitor #0 @ 1920,0`). Unique names are
//! left alone, so Linux connector names (`HDMI-A-1`) and saved stages keep
//! working unchanged.

use winit::monitor::MonitorHandle;

/// Every how many refreshes of a `refresh_hz` screen a frame is drawn for
/// `target_fps` — `None` when no whole number of refreshes makes that rate
/// to within 1 %: frames there cannot all stay on screen equally long.
pub(crate) fn refreshes_per_frame(refresh_hz: f64, target_fps: u32) -> Option<u32> {
    if refresh_hz <= 0.0 || target_fps == 0 {
        return None;
    }
    let target = f64::from(target_fps);
    let every = (refresh_hz / target).round().max(1.0);
    let rate = refresh_hz / every;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    ((rate - target).abs() / target <= 0.01).then_some(every as u32)
}

/// What to tell someone whose show runs at `fps` while output `output` is on
/// a screen refreshing at `hz` — `None` when the rate fits. A 50 fps show on
/// a laptop's 120 Hz screen judders whatever paces it: some frames stay two
/// refreshes, some three.
pub(crate) fn uneven_rate_warning(output: &str, hz: f64, fps: u32) -> Option<String> {
    if hz <= 0.0 || fps == 0 || refreshes_per_frame(hz, fps).is_some() {
        return None;
    }
    let show_rate = fitting_choice(hz, fps).map_or_else(String::new, |fits| {
        format!(
            ", or — where the screen's rate is fixed, as an LED processor's often is — \
             run the show at {fits} fps (Target FPS, in the top bar)"
        )
    });
    Some(format!(
        "Output '{output}' is on a {hz:.0} Hz screen: at {fps} fps its frames cannot all \
         stay up equally long, so motion judders. Set that screen to {fps} Hz or a \
         multiple (System Settings → Displays → Refresh Rate){show_rate}."
    ))
}

/// The show rate the Target FPS menu offers that fits a `hz` screen —
/// whole refreshes a frame — nearest `fps`, the higher of two as near;
/// `None` when none does (a 165 Hz screen).
fn fitting_choice(hz: f64, fps: u32) -> Option<u32> {
    crate::engine::value::render::TARGET_FPS_CHOICES
        .into_iter()
        .filter(|&choice| refreshes_per_frame(hz, choice).is_some())
        .min_by_key(|&choice| (choice.abs_diff(fps), std::cmp::Reverse(choice)))
}

/// The platform's name for a monitor.
fn base_name(monitor: &MonitorHandle) -> String {
    monitor.name().unwrap_or_else(|| "Unknown".to_string())
}

/// The position-qualified form of a name.
fn placed_name(base: &str, (x, y): (i32, i32)) -> String {
    format!("{base} @ {x},{y}")
}

fn position(monitor: &MonitorHandle) -> (i32, i32) {
    let p = monitor.position();
    (p.x, p.y)
}

/// Names for `monitors`, in order: the platform's name, or the
/// position-qualified form where two or more share it.
pub(crate) fn names(monitors: &[MonitorHandle]) -> Vec<String> {
    let described: Vec<(String, (i32, i32))> = monitors
        .iter()
        .map(|m| (base_name(m), position(m)))
        .collect();
    disambiguate(&described)
}

fn disambiguate(monitors: &[(String, (i32, i32))]) -> Vec<String> {
    monitors
        .iter()
        .map(|(base, pos)| {
            let shared = monitors.iter().filter(|(other, _)| other == base).count() > 1;
            if shared {
                placed_name(base, *pos)
            } else {
                base.clone()
            }
        })
        .collect()
}

/// The monitor an output configured for `wanted` should go to.
///
/// An exact name wins. Failing that, a position-qualified name still finds its
/// monitor when the twin that forced the qualification is unplugged. A bare
/// name that has since become ambiguous matches nothing — waiting is better
/// than guessing which of two identical screens was meant.
/// Whether the screen `monitor` is still connected: among the active screens
/// listed, or — a hardware mirror, which macOS lists only by its primary —
/// still online.
pub(crate) fn still_connected(monitor: &MonitorHandle, listed: &[(String, MonitorHandle)]) -> bool {
    listed.iter().any(|(_, h)| h == monitor) || online(monitor)
}

#[cfg(target_os = "macos")]
fn online(monitor: &MonitorHandle) -> bool {
    use winit::platform::macos::MonitorHandleExtMacOS as _;
    let id = monitor.native_id();
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    // SAFETY: room for 32 display ids, and where their number is written.
    let listed = unsafe {
        objc2_core_graphics::CGGetOnlineDisplayList(32, ids.as_mut_ptr(), &raw mut count)
    };
    listed.0 == 0 && ids[..(count as usize).min(ids.len())].contains(&id)
}

#[cfg(not(target_os = "macos"))]
fn online(_monitor: &MonitorHandle) -> bool {
    false
}

pub(crate) fn find<'a>(
    monitors: &'a [(String, MonitorHandle)],
    wanted: &str,
) -> Option<(usize, &'a MonitorHandle)> {
    let names: Vec<&str> = monitors.iter().map(|(n, _)| n.as_str()).collect();
    if let Some(i) = find_index(&names, &[], wanted) {
        return Some((i, &monitors[i].1));
    }
    // A screen's position is asked of macOS on the main thread only (winit
    // waits for the main thread otherwise): off it, only the names the main
    // thread listed are matched — the ones the screen pickers offer.
    if !crate::renderer::context::on_main_thread() {
        return None;
    }
    let placed: Vec<String> = monitors
        .iter()
        .map(|(_, m)| placed_name(&base_name(m), position(m)))
        .collect();
    find_index(&names, &placed, wanted).map(|i| (i, &monitors[i].1))
}

fn find_index(names: &[&str], placed: &[String], wanted: &str) -> Option<usize> {
    names
        .iter()
        .position(|n| *n == wanted)
        .or_else(|| placed.iter().position(|p| p == wanted))
}

#[cfg(test)]
mod tests {
    use super::{disambiguate, find_index, refreshes_per_frame, uneven_rate_warning};

    #[test]
    fn a_frame_every_refresh_or_every_other_as_the_rates_fit() {
        assert_eq!(refreshes_per_frame(60.0, 60), Some(1));
        assert_eq!(refreshes_per_frame(59.94, 60), Some(1), "within 1 %");
        assert_eq!(refreshes_per_frame(120.0, 60), Some(2));
        assert_eq!(refreshes_per_frame(60.0, 30), Some(2));
        assert_eq!(refreshes_per_frame(60.0, 50), None, "50 does not fit 60");
        assert_eq!(refreshes_per_frame(0.0, 60), None);
        assert_eq!(refreshes_per_frame(60.0, 0), None);
    }

    /// Measured on a laptop: 50 fps on its 120 Hz panel judders in every
    /// pacing mode. A rate that fits says nothing.
    #[test]
    fn a_show_rate_that_does_not_fit_the_screen_is_named_with_the_fix() {
        let text = uneven_rate_warning("TV", 120.0, 50).expect("50 on 120 judders");
        assert!(text.contains("120 Hz") && text.contains("50 fps"), "{text}");
        assert!(text.contains("50 Hz or a multiple"), "{text}");
        assert!(text.contains("run the show at 60 fps"), "{text}");
        let tv = uneven_rate_warning("TV", 50.0, 60).expect("60 on a 50 Hz TV");
        assert!(tv.contains("run the show at 50 fps"), "{tv}");
        assert!(tv.contains("Target FPS, in the top bar"), "where to: {tv}");
        // Only a rate the menu offers: 24 on 60 Hz is 30, not 20.
        let film = uneven_rate_warning("TV", 60.0, 24).expect("24 on 60 judders");
        assert!(film.contains("run the show at 30 fps"), "{film}");
        // And none where no rate on offer fits.
        let odd = uneven_rate_warning("TV", 165.0, 24).expect("24 on 165 judders");
        assert!(!odd.contains("run the show"), "{odd}");
        assert!(odd.contains("Set that screen to 24 Hz"), "{odd}");
        assert_eq!(uneven_rate_warning("TV", 120.0, 60), None);
        assert_eq!(uneven_rate_warning("TV", 59.94, 60), None);
        assert_eq!(uneven_rate_warning("TV", 0.0, 60), None, "rate unknown");
    }

    fn described(list: &[(&str, (i32, i32))]) -> Vec<(String, (i32, i32))> {
        list.iter().map(|(n, p)| ((*n).to_string(), *p)).collect()
    }

    #[test]
    fn unique_names_are_left_alone() {
        let names = disambiguate(&described(&[("eDP-1", (0, 0)), ("HDMI-A-1", (1920, 0))]));
        assert_eq!(names, ["eDP-1", "HDMI-A-1"]);
    }

    #[test]
    fn identical_displays_get_their_position() {
        // Two Blackmagic converters on a Mac both report model number 0.
        let names = disambiguate(&described(&[
            ("Monitor #0", (0, 0)),
            ("Monitor #0", (1920, 0)),
            ("Monitor #7", (3840, 0)),
        ]));
        assert_eq!(
            names,
            ["Monitor #0 @ 0,0", "Monitor #0 @ 1920,0", "Monitor #7"]
        );
    }

    #[test]
    fn an_exact_name_is_found() {
        let placed = [
            "Monitor #0 @ 0,0".to_string(),
            "Monitor #0 @ 1920,0".to_string(),
        ];
        let names = ["Monitor #0 @ 0,0", "Monitor #0 @ 1920,0"];
        assert_eq!(find_index(&names, &placed, "Monitor #0 @ 1920,0"), Some(1));
    }

    #[test]
    fn a_placed_name_survives_its_twin_being_unplugged() {
        // Only the right-hand display is left, so it is plain `Monitor #0` again.
        let names = ["Monitor #0"];
        let placed = ["Monitor #0 @ 1920,0".to_string()];
        assert_eq!(find_index(&names, &placed, "Monitor #0 @ 1920,0"), Some(0));
    }

    #[test]
    fn a_bare_name_that_became_ambiguous_matches_nothing() {
        let names = ["Monitor #0 @ 0,0", "Monitor #0 @ 1920,0"];
        let placed = [
            "Monitor #0 @ 0,0".to_string(),
            "Monitor #0 @ 1920,0".to_string(),
        ];
        assert_eq!(find_index(&names, &placed, "Monitor #0"), None);
    }
}
