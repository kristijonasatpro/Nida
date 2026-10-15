//! Where the pointer is while files are dragged in from Finder.
//!
//! winit 0.30 reports that files are hovering and when they drop, but not
//! where: while another app runs the drag, `AppKit` sends this window no mouse
//! events, so egui's pointer freezes wherever it last was. Asking the window
//! for the mouse location directly gives the real position, which the runner
//! feeds to egui so the drop lands on the channel under the pointer.

/// The pointer in the window's content, in logical points from its top-left
/// corner, or `None` where the platform cannot say.
#[cfg(target_os = "macos")]
pub(super) fn pointer_in_window(window: &winit::window::Window) -> Option<(f64, f64)> {
    use objc2::msg_send;
    use objc2::runtime::{AnyObject, Bool};
    use objc2_foundation::{NSPoint, NSRect};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // Safety: winit's live content view, used on the main thread (the event
    // loop), and messages that take and return plain values.
    unsafe {
        let view: &AnyObject = appkit.ns_view.cast().as_ref();
        let ns_window: *mut AnyObject = msg_send![view, window];
        let ns_window = ns_window.as_ref()?;
        let in_window: NSPoint = msg_send![ns_window, mouseLocationOutsideOfEventStream];
        let nil: Option<&AnyObject> = None;
        let in_view: NSPoint = msg_send![view, convertPoint: in_window, fromView: nil];
        let flipped: Bool = msg_send![view, isFlipped];
        let bounds: NSRect = msg_send![view, bounds];
        let y = if flipped.as_bool() {
            in_view.y
        } else {
            bounds.size.height - in_view.y
        };
        Some((in_view.x, y))
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn pointer_in_window(_window: &winit::window::Window) -> Option<(f64, f64)> {
    None
}
