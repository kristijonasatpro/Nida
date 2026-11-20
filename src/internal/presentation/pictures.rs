//! A PowerPoint deck's slides as pictures, for the presenter view's next
//! slide: PowerPoint saves the deck as a PDF — every slide a page — into
//! Nida's folder for them, named after the deck's file: its path, size and
//! time, so a deck changed since is saved again, under another name. Nida
//! draws the pages as it draws a PDF talk's.
//!
//! A copy of each PDF made is put beside its deck too, for the user to find
//! and to carry along with the decks: "Slide pictures" in the deck's folder,
//! under the deck's file name ([`put_beside`]). Nida draws from its own
//! folder alone: PowerPoint is given that one, and would ask again for each
//! other folder.
//!
//! PowerPoint, sandboxed, asks once for the folder (Grant Access), and holds
//! everything else it does — a show on air too — till the user answers. So
//! the first time they are made when asked (Make), with no PowerPoint show
//! running. Once PowerPoint has saved into the folder ([`given`]) they are
//! made by themselves as talks are added, beside a running show too: saving
//! leaves a show as it is (tried, PowerPoint 16.114).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The folder beside a deck its slide pictures are copied into.
pub const BESIDE: &str = "Slide pictures";

/// The folders beside decks said not to take the copy, in this process.
static UNWRITABLE: std::sync::Mutex<Option<HashSet<PathBuf>>> = std::sync::Mutex::new(None);

/// Held while a copy is put beside a deck: one at a time.
static PUTTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// [`pdf_for`], and the deck's file as it is now — its size, and when it
/// last changed: what the PDF is named for.
#[must_use]
pub fn pdf_and_file_for(deck: &Path) -> Option<(PathBuf, (u64, Option<std::time::SystemTime>))> {
    pdf_and_file_in(&folder()?, deck)
}

/// [`pdf_for`], in `folder`.
#[must_use]
pub fn pdf_in(folder: &Path, deck: &Path) -> Option<PathBuf> {
    pdf_and_file_in(folder, deck).map(|(pdf, _)| pdf)
}

/// [`pdf_and_file_for`], in `folder`.
fn pdf_and_file_in(
    folder: &Path,
    deck: &Path,
) -> Option<(PathBuf, (u64, Option<std::time::SystemTime>))> {
    let meta = std::fs::metadata(deck).ok()?;
    if !meta.is_file() {
        return None;
    }
    let path = std::fs::canonicalize(deck).unwrap_or_else(|_| deck.to_path_buf());
    let (len, modified) = (meta.len(), meta.modified().ok());
    Some((pdf_named(folder, &path, len, modified), (len, modified)))
}

/// The PDF in `folder` of the deck really at `path`, of `len` bytes, last
/// changed at `modified`.
fn pdf_named(
    folder: &Path,
    path: &Path,
    len: u64,
    modified: Option<std::time::SystemTime>,
) -> PathBuf {
    let time = modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let name = format!(
        "{:016x}",
        fnv1a(&[
            path.to_string_lossy().as_bytes(),
            &len.to_le_bytes(),
            &time.to_le_bytes(),
        ])
    );
    folder.join(format!("{name}.pdf"))
}

/// Whether `name` is a deck's PDF made, not one being written (see
/// [`part_of`]).
// The PDFs here are named by Nida itself (".pdf", lower case): no other case
// can occur in the folder.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn made(name: &str) -> bool {
    name.ends_with(".pdf") && !name.ends_with(".saving.pdf")
}

/// Whether PowerPoint has saved into `folder` before — a PDF made is there
/// — and so was given it: it asks no more (Grant Access) to save there
/// again.
#[must_use]
pub fn given(folder: &Path) -> bool {
    std::fs::read_dir(folder).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| made(&entry.file_name().to_string_lossy()))
    })
}

/// [`given`] of Nida's folder as last found, for a show thread: from what
/// [`crate::file_facts`] knows, never asking the disk. None while not yet
/// looked at.
#[must_use]
pub fn given_known() -> Option<bool> {
    let names = crate::file_facts::folder(&folder()?)?;
    Some(names.is_some_and(|names| names.iter().any(|name| made(name))))
}

/// The deck's file as last found, and where its PDF goes, when that PDF is
/// known not to be there: from what [`crate::file_facts`] knows, never
/// asking the disk. None too while either is not yet looked at.
#[must_use]
pub fn missing_known(deck: &Path) -> Option<(PathBuf, crate::file_facts::Found)> {
    let found = crate::file_facts::file(deck)??;
    let pdf = pdf_named(&folder()?, &found.real, found.len, found.modified);
    match crate::file_facts::file(&pdf)? {
        None => Some((pdf, found)),
        Some(_) => None,
    }
}

/// [`saved_for`] as last found, for a show thread: from what
/// [`crate::file_facts`] knows of the deck and its PDF, never asking the
/// disk. None too while either is not yet looked at.
#[must_use]
pub fn saved_known(deck: &Path) -> Option<(PathBuf, crate::file_facts::Found)> {
    let found = crate::file_facts::file(deck)??;
    let pdf = pdf_named(&folder()?, &found.real, found.len, found.modified);
    let saved = crate::file_facts::file(&pdf)??;
    Some((pdf, saved))
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

/// Where the copy of the slide pictures of the deck at `deck` goes beside
/// it: "Slide pictures" in the deck's folder, under the deck's whole file
/// name ("talk.pptx.pdf": a talk.ppt beside it has its own).
#[must_use]
pub fn beside(deck: &Path) -> Option<PathBuf> {
    let mut name = deck.file_name()?.to_os_string();
    name.push(".pdf");
    Some(deck.parent()?.join(BESIDE).join(name))
}

/// A copy of the slide pictures made at `pdf` put beside the deck at `deck`
/// ([`beside`]), in place of one put there before; where it is now. None,
/// and nothing copied, when that folder is the one `pdf` is in: a deck kept
/// beside Nida's own folder; and when the deck is not there any more (no
/// folder is made where no deck is).
///
/// One at a time: two saves of one deck close together never write the
/// same file at once. Called in the order the pictures were saved (a deck
/// changed and saved again has another `pdf`), the newest are left there.
///
/// Written under another name first and put in place whole. It may be held
/// a long while: macOS asks the user the first time Nida writes into their
/// Downloads, Desktop or Documents, and holds whoever writes till answered
/// — never call it from a thread a show waits on.
///
/// # Errors
///
/// In words: no folder to be made there, or no writing into it. `pdf` stays
/// as it is.
pub fn put_beside(pdf: &Path, deck: &Path) -> Result<Option<PathBuf>, String> {
    let Some(to) = beside(deck) else {
        return Err(format!("{} has no folder to put them beside", deck.display()));
    };
    let Some(dir) = to.parent() else {
        return Ok(None);
    };
    let _one_at_a_time = PUTTING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !deck.is_file() {
        return Ok(None);
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("no folder for them at {}: {e}", dir.display()))?;
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if pdf.parent().is_some_and(|own| real(own) == real(dir)) {
        return Ok(None);
    }
    let mut part = to.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let put = std::fs::copy(pdf, &part)
        .and_then(|_| std::fs::rename(&part, &to))
        .map_err(|e| format!("no copy of them could be put at {}: {e}", to.display()));
    if put.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    put.map(|()| Some(to))
}

/// Whether `folder` is said for the first time, in this process, not to
/// take the copies put beside its decks: said once a folder, not once a
/// deck.
#[must_use]
pub fn said_first(folder: &Path) -> bool {
    UNWRITABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(HashSet::new)
        .insert(folder.to_path_buf())
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

    /// A folder is given once a PDF made is in it: not while it is not
    /// there, empty, or holds only what a save cut short left.
    #[test]
    fn a_folder_is_given_once_a_pdf_was_made_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("pictures");
        assert!(!given(&folder), "not there");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(!given(&folder), "empty");
        let out = folder.join("0123456789abcdef.pdf");
        std::fs::write(part_of(&out), b"part").unwrap();
        std::fs::write(folder.join("notes.txt"), b"other").unwrap();
        assert!(!given(&folder), "a save cut short");
        std::fs::rename(part_of(&out), &out).unwrap();
        assert!(given(&folder), "made");
    }

    /// The slide pictures made are copied beside their deck, under its
    /// file name, whole: nothing half written left, the one made staying
    /// where it is; made again, the copy is the new one.
    #[test]
    fn slide_pictures_made_are_copied_beside_their_deck_under_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let decks = dir.path().join("decks");
        let own = dir.path().join("own");
        std::fs::create_dir_all(&decks).unwrap();
        std::fs::create_dir_all(&own).unwrap();
        let deck = decks.join("talk.pptx");
        std::fs::write(&deck, b"deck").unwrap();
        let pdf = own.join("0123456789abcdef.pdf");
        std::fs::write(&pdf, b"first").unwrap();
        let to = decks.join("Slide pictures").join("talk.pptx.pdf");
        assert_eq!(beside(&deck), Some(to.clone()));
        assert_eq!(put_beside(&pdf, &deck), Ok(Some(to.clone())));
        assert_eq!(std::fs::read(&to).unwrap(), b"first");
        assert_eq!(std::fs::read(&pdf).unwrap(), b"first", "the one made stays");
        std::fs::write(&pdf, b"made again").unwrap();
        assert_eq!(put_beside(&pdf, &deck), Ok(Some(to.clone())));
        assert_eq!(std::fs::read(&to).unwrap(), b"made again");
        let left: Vec<_> = std::fs::read_dir(decks.join("Slide pictures"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("talk.pptx.pdf")], "nothing else");
    }

    /// A deck that is not there (taken away since, or never a file): no
    /// folder is made where it was, nothing copied.
    #[test]
    fn no_folder_is_made_beside_a_deck_that_is_not_there() {
        let dir = tempfile::tempdir().unwrap();
        let decks = dir.path().join("decks");
        let own = dir.path().join("own");
        std::fs::create_dir_all(&decks).unwrap();
        std::fs::create_dir_all(&own).unwrap();
        let pdf = own.join("0123456789abcdef.pdf");
        std::fs::write(&pdf, b"made").unwrap();
        assert_eq!(put_beside(&pdf, &decks.join("talk.pptx")), Ok(None));
        assert_eq!(std::fs::read_dir(&decks).unwrap().count(), 0, "nothing made there");
    }

    /// A deck kept beside Nida's own folder: nothing is copied into that
    /// folder — a copy there would look like a PDF PowerPoint made.
    #[test]
    fn nothing_is_copied_into_the_folder_the_pictures_are_made_in() {
        let dir = tempfile::tempdir().unwrap();
        let deck = dir.path().join("talk.pptx");
        std::fs::write(&deck, b"deck").unwrap();
        let own = dir.path().join("Slide pictures");
        std::fs::create_dir_all(&own).unwrap();
        let pdf = own.join("0123456789abcdef.pdf");
        std::fs::write(&pdf, b"made").unwrap();
        assert_eq!(put_beside(&pdf, &deck), Ok(None));
        assert!(!own.join("talk.pptx.pdf").exists());
        assert_eq!(std::fs::read_dir(&own).unwrap().count(), 1, "the one made alone");
    }

    /// A folder that takes no copy — here a file is where the folder would
    /// be — leaves the pictures made as they are, and is said once.
    #[test]
    fn a_folder_that_takes_no_copy_leaves_the_pictures_made_and_is_said_once() {
        let dir = tempfile::tempdir().unwrap();
        let decks = dir.path().join("decks");
        let own = dir.path().join("own");
        std::fs::create_dir_all(&decks).unwrap();
        std::fs::create_dir_all(&own).unwrap();
        let deck = decks.join("talk.pptx");
        std::fs::write(&deck, b"deck").unwrap();
        let pdf = own.join("0123456789abcdef.pdf");
        std::fs::write(&pdf, b"made").unwrap();
        std::fs::write(decks.join("Slide pictures"), b"a file in the way").unwrap();
        let put = put_beside(&pdf, &deck);
        assert!(matches!(&put, Err(why) if why.contains("no folder for them")), "{put:?}");
        assert_eq!(std::fs::read(&pdf).unwrap(), b"made");
        assert!(said_first(&decks), "the first time");
        assert!(!said_first(&decks), "not again");
        assert!(said_first(&own), "another folder: its own first time");
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
