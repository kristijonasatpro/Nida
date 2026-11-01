//! A PowerPoint deck's slides as pictures, for the presenter view's next
//! slide: PowerPoint saves the deck as a PDF — every slide a page — into
//! Nida's folder for them, named after the deck's file: its path, size and
//! time, so a deck changed since is saved again, under another name. Nida
//! draws the pages as it draws a PDF talk's.
//!
//! PowerPoint, sandboxed, asks once for the folder (Grant Access), and holds
//! everything else it does — a show on air too — till the user answers: the
//! pictures are made when asked, with no PowerPoint show running.

use std::path::{Path, PathBuf};

/// A test's folder for them, in this process.
#[cfg(any(test, feature = "test-fixtures"))]
static TEST_FOLDER: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Tests: the slide pictures go in `folder` from now on, in this process —
/// never the user's own.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn use_folder(folder: &Path) {
    *TEST_FOLDER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(folder.to_path_buf());
}

/// The folder PowerPoint saves the decks' PDFs in: `VARDA_SLIDE_PICTURES`
/// if set, else "Slide pictures" in Nida's folder of the user's application
/// data.
#[must_use]
pub fn folder() -> Option<PathBuf> {
    #[cfg(any(test, feature = "test-fixtures"))]
    if let Some(dir) = TEST_FOLDER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return Some(dir);
    }
    if let Some(dir) = std::env::var_os("VARDA_SLIDE_PICTURES") {
        return Some(PathBuf::from(dir));
    }
    dirs::data_dir().map(|dir| dir.join("Varda").join("Slide pictures"))
}

/// Where the PDF of the deck at `deck` goes, as the file is now; none when
/// there is no such file (or no folder for them).
#[must_use]
pub fn pdf_for(deck: &Path) -> Option<PathBuf> {
    pdf_in(&folder()?, deck)
}

/// [`pdf_for`], in `folder`.
#[must_use]
pub fn pdf_in(folder: &Path, deck: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(deck).ok()?;
    if !meta.is_file() {
        return None;
    }
    let path = std::fs::canonicalize(deck).unwrap_or_else(|_| deck.to_path_buf());
    let time = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let name = format!(
        "{:016x}",
        fnv1a(&[
            path.to_string_lossy().as_bytes(),
            &meta.len().to_le_bytes(),
            &time.to_le_bytes(),
        ])
    );
    Some(folder.join(format!("{name}.pdf")))
}

/// The deck's PDF, when PowerPoint has saved it for the file as it is now.
#[must_use]
pub fn saved_for(deck: &Path) -> Option<PathBuf> {
    pdf_for(deck).filter(|pdf| pdf.is_file())
}

/// Where PowerPoint writes the PDF for `out` first: put in place once whole
/// ([`put_in_place`]), so a save cut short leaves nothing that looks made.
#[must_use]
pub fn part_of(out: &Path) -> PathBuf {
    out.with_extension("saving.pdf")
}

/// The PDF PowerPoint wrote at `part` put in its place at `out`, once it
/// opens as a PDF of a page at least.
///
/// # Errors
///
/// In words: no PDF at `part`, one that does not open, or no moving it.
pub fn put_in_place(part: &Path, out: &Path) -> Result<(), String> {
    if !part.is_file() {
        return Err(format!(
            "PowerPoint said it saved it, but there is no PDF at {}",
            part.display()
        ));
    }
    super::pdf::PdfPages::open(part)
        .map_err(|e| format!("the PDF PowerPoint saved does not open: {e}"))?;
    std::fs::rename(part, out)
        .map_err(|e| format!("the PDF could not be put at {}: {e}", out.display()))
}

/// The page of a deck's PDF of `pages` pages that shows its slide coming
/// next: PowerPoint's own number for it (`next`, of `slides`) when the PDF
/// has every slide; its place among those the show plays (`position` + 1,
/// of `shown`) when the PDF leaves the hidden ones out; none when nothing
/// comes next, or the PDF is of neither count (a deck changed since).
#[must_use]
pub fn page_of_next(
    pages: u32,
    next: Option<u32>,
    slides: Option<u32>,
    position: Option<u32>,
    shown: Option<u32>,
) -> Option<u32> {
    let next = next?;
    if slides == Some(pages) {
        return Some(next).filter(|n| *n <= pages);
    }
    if shown == Some(pages) {
        return position.map(|p| p + 1).filter(|n| *n <= pages);
    }
    None
}

/// A stable 64-bit FNV-1a over `parts`: the same name for the same file,
/// whatever build of Nida made it.
fn fnv1a(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in *part {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deck's PDF is named after its file as it is: the same name again
    /// for the same file, another once the file changes, none for no file.
    #[test]
    fn a_decks_pdf_is_named_after_its_file_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let deck = dir.path().join("talk.pptx");
        std::fs::write(&deck, b"one").unwrap();
        let folder = dir.path().join("pictures");
        let first = pdf_in(&folder, &deck).expect("named");
        assert_eq!(first.parent(), Some(folder.as_path()));
        assert_eq!(first.extension().and_then(|e| e.to_str()), Some("pdf"));
        assert_eq!(pdf_in(&folder, &deck), Some(first.clone()), "the same file: the same name");
        std::fs::write(&deck, b"changed, and longer").unwrap();
        assert_ne!(pdf_in(&folder, &deck), Some(first), "changed: another");
        assert_eq!(pdf_in(&folder, &dir.path().join("none.pptx")), None);
    }

    /// The page coming next: PowerPoint's number when the PDF has every
    /// slide, the place among those shown when it leaves hidden ones out,
    /// none when the PDF is of neither count or nothing comes next.
    #[test]
    fn the_next_slides_page_follows_what_the_pdf_has() {
        // 24 slides, 2 hidden: slide 5 is the 4th shown, 6 comes next.
        assert_eq!(page_of_next(24, Some(6), Some(24), Some(4), Some(22)), Some(6));
        assert_eq!(page_of_next(22, Some(6), Some(24), Some(4), Some(22)), Some(5));
        assert_eq!(page_of_next(20, Some(6), Some(24), Some(4), Some(22)), None, "changed");
        assert_eq!(page_of_next(24, None, Some(24), Some(24), Some(24)), None, "the last");
    }
}
