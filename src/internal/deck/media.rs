//! Which files can become decks, decided by extension — for files dropped in
//! from Finder, where anything at all can arrive.

use std::path::{Path, PathBuf};

/// The kind of deck a file makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// A still: PNG, JPEG, TIFF, WebP, SVG…
    Image,
    /// A clip: MOV (HAP included), MP4, MKV, `WebM`, animated GIF…
    Video,
    /// An ISF shader (`.fs`), as a generator.
    Shader,
    /// A sound file: MP3, WAV, AAC… — played like a clip, with nothing to see.
    Audio,
}

use crate::engine::value::media::{IMAGES, SOUNDS, VIDEOS};

impl MediaKind {
    /// What `path` would make, by its extension; `None` for anything else.
    pub fn of(path: &Path) -> Option<Self> {
        let ext = crate::engine::value::media::extension(path)?;
        if IMAGES.contains(&ext.as_str()) {
            Some(Self::Image)
        } else if VIDEOS.contains(&ext.as_str()) {
            Some(Self::Video)
        } else if SOUNDS.contains(&ext.as_str()) {
            Some(Self::Audio)
        } else if ext == "fs" {
            Some(Self::Shader)
        } else {
            None
        }
    }
}

/// Files as dropped, with each folder replaced by the files directly inside
/// it (not its subfolders), sorted by name — so a folder of numbered speaker
/// slides arrives in order. Hidden files are skipped; files keep the order
/// they were dropped in.
pub fn expand_folders(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut inside: Vec<PathBuf> = std::fs::read_dir(path)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|e| e.path())
                        .filter(|p| p.is_file() && !is_hidden(p))
                        .collect()
                })
                .unwrap_or_default();
            inside.sort_by_key(|p| name_key(p));
            out.extend(inside);
        } else {
            out.push(path.clone());
        }
    }
    out
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// Sort key that puts "slide 2" before "slide 10", as Finder does.
fn name_key(path: &Path) -> Vec<NameChunk> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut chunks = Vec::new();
    let mut digits = String::new();
    let mut text = String::new();
    for c in name.chars() {
        if c.is_ascii_digit() {
            if !text.is_empty() {
                chunks.push(NameChunk::Text(std::mem::take(&mut text)));
            }
            digits.push(c);
        } else {
            if !digits.is_empty() {
                chunks.push(NameChunk::Number(digits.parse().unwrap_or(u64::MAX)));
                digits.clear();
            }
            text.push(c);
        }
    }
    if !digits.is_empty() {
        chunks.push(NameChunk::Number(digits.parse().unwrap_or(u64::MAX)));
    }
    if !text.is_empty() {
        chunks.push(NameChunk::Text(text));
    }
    chunks
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NameChunk {
    Number(u64),
    Text(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_recognised_by_extension_whatever_the_case() {
        assert_eq!(
            MediaKind::of(Path::new("a/Slide.PNG")),
            Some(MediaKind::Image)
        );
        assert_eq!(MediaKind::of(Path::new("logo.svg")), Some(MediaKind::Image));
        assert_eq!(
            MediaKind::of(Path::new("IMG_2501.MOV")),
            Some(MediaKind::Video)
        );
        assert_eq!(
            MediaKind::of(Path::new("intro.mp4")),
            Some(MediaKind::Video)
        );
        assert_eq!(
            MediaKind::of(Path::new("plasma.fs")),
            Some(MediaKind::Shader)
        );
        assert_eq!(
            MediaKind::of(Path::new("Jingle.MP3")),
            Some(MediaKind::Audio)
        );
        assert_eq!(
            MediaKind::of(Path::new("walk-in.wav")),
            Some(MediaKind::Audio)
        );
        assert_eq!(MediaKind::of(Path::new("notes.pdf")), None);
        assert_eq!(MediaKind::of(Path::new("README")), None);
    }

    #[test]
    fn a_folder_becomes_its_files_in_finder_order() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["slide 10.png", "slide 2.png", "Slide 1.png", ".DS_Store"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("deep.png"), b"x").unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let loose = elsewhere.path().join("loose.mp4");
        std::fs::write(&loose, b"x").unwrap();

        let got = expand_folders(&[loose.clone(), dir.path().to_path_buf()]);
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["loose.mp4", "Slide 1.png", "slide 2.png", "slide 10.png"],
            "the loose file first, then the folder's files by number; no hidden files or subfolders"
        );
    }
}
