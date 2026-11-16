//! Frames: filters that hold a layer's picture (`shaders/frame_*.fs`).
//!
//! A frame's window is what shows of the layer's picture — what its Crop
//! keeps, all of a picture that fills the layer — which the frame's shader
//! finds itself, from the picture, each time it is drawn. The picture
//! fills the window and the frame — a picture of one, or lines of light,
//! with a light going round — is drawn over its edge; or, Picture inside
//! the frame, the picture is fitted into the frame. Outside the window all
//! is clear. A layer has one from Input Settings → Position → Frame
//! (`EngineCommand::SetLayerFrame`), where it runs before the layer's
//! Transform — so the layer is moved and scaled frame and all — and is
//! told from the layer's other effects by its shader's name.

/// What every frame's shader name starts with.
pub const FRAME_PREFIX: &str = "frame_";

/// Whether the filter `shader_name` is a frame.
#[must_use]
pub fn is_frame(shader_name: &str) -> bool {
    shader_name.starts_with(FRAME_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::is_frame;

    #[test]
    fn a_frame_is_told_by_its_name() {
        assert!(is_frame("frame_hud"));
        assert!(!is_frame("crop"));
        assert!(!is_frame("broadcast_hex_glow"));
        assert!(!is_frame("freeze_frame"));
    }
}
