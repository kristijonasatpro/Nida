//! Stable window identities for Linux compositors.
//!
//! Wayland compositors match window rules on the `app_id` (Hyprland shows it as
//! `class`); X11 window managers match `WM_CLASS`. Without one every Nida
//! window is anonymous, so an operator cannot write a rule that pins output
//! windows fullscreen to a given monitor. Output windows share one id and are
//! told apart by title (`Nida - <output name>`).

use winit::window::WindowAttributes;

/// The operator console.
pub const MAIN: &str = "nida";
/// Every output window.
pub const OUTPUT: &str = "nida-output";
/// A panel in a window of its own.
pub const PANEL: &str = "nida-panel";
/// The interactive HTML window.
#[cfg(feature = "html")]
pub const INTERACTIVE: &str = "nida-interactive";

/// Tag `attrs` with `app_id` (Wayland) and `WM_CLASS` (X11). A no-op elsewhere.
pub fn tagged(attrs: WindowAttributes, app_id: &str) -> WindowAttributes {
    #[cfg(target_os = "linux")]
    {
        // winit's Wayland and X11 `with_name` write the same field, so one call
        // covers whichever session the binary lands in.
        use winit::platform::wayland::WindowAttributesExtWayland;
        attrs.with_name(app_id, app_id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app_id;
        attrs
    }
}
