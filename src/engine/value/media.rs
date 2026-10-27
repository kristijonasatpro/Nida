//! Which files are pictures, clips and sound files, by extension: one list
//! for the layers files make (`crate::deck::media`) and the talks of a Slides
//! input (`super::slides`).

/// Stills: PNG, JPEG, TIFF, WebP, SVG…
pub const IMAGES: [&str; 11] = [
    "png", "jpg", "jpeg", "bmp", "tif", "tiff", "tga", "webp", "svg", "svgz", "heic",
];
/// Clips: MOV (HAP included), MP4, MKV, `WebM`, animated GIF…
pub const VIDEOS: [&str; 12] = [
    "mov", "mp4", "m4v", "avi", "mkv", "webm", "gif", "mxf", "mpg", "mpeg", "ts", "mts",
];
/// Sound files: MP3, WAV, AAC…
pub const SOUNDS: [&str; 10] = [
    "mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "aif", "aiff", "wma",
];

/// `path`'s extension, lower case.
#[must_use]
pub fn extension(path: &std::path::Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}
