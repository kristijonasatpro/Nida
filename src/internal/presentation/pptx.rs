//! What Nida reads from a PowerPoint file itself, without PowerPoint: the
//! slides in the order the show plays them, each one's title, speaker notes
//! and whether it is hidden, and the slide size.
//!
//! A .pptx is a zip of XML parts (ECMA-376, PresentationML):
//! `ppt/presentation.xml` lists the slides in order by relationship id, its
//! relationships name each slide's part, each slide's relationships name its
//! notes page, and the notes page's body placeholder holds the notes. Names
//! are matched by their local part, so the Strict dialect reads as well.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::Path;

/// A part bigger than this is refused rather than read: a .pptx's XML is
/// small, and a zip may claim anything.
const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

/// The slide size PowerPoint writes for 4:3 when a file names none.
const DEFAULT_SIZE_EMU: (u64, u64) = (9_144_000, 6_858_000);

/// A zip claiming more parts than this, or a longer list of them, is no
/// deck to open: opening one reserves room for every part it claims.
const MAX_PARTS: u64 = 100_000;
const MAX_LIST_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PptxError {
    #[error("cannot open {0}: {1}")]
    Open(String, std::io::Error),
    /// Not a zip at all: an old .ppt, a password-protected file, or not
    /// PowerPoint's.
    #[error("{0} is not a .pptx file (an old .ppt, or protected by a password?)")]
    NotPptx(String),
    #[error("{part} in {file} cannot be read: {reason}")]
    Broken {
        file: String,
        part: String,
        reason: String,
    },
}

/// One slide, as the file has it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Slide {
    /// The title placeholder's text, when the slide has one.
    pub title: Option<String>,
    /// The speaker notes, a line for each paragraph; empty for none.
    pub notes: String,
    /// Left out of the slide show (Hide Slide).
    pub hidden: bool,
}

/// What a .pptx says about its slides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// In the order the show plays them, hidden ones included: slide `n` in
    /// PowerPoint's numbering is `slides[n - 1]`.
    pub slides: Vec<Slide>,
    /// The slide size, EMU (914,400 to the inch): 16:9 is 12,192,000 ×
    /// 6,858,000, 4:3 is 9,144,000 × 6,858,000.
    pub width_emu: u64,
    pub height_emu: u64,
}

impl Document {
    /// Width over height: 1.78 for 16:9.
    #[must_use]
    pub fn aspect(&self) -> f64 {
        self.width_emu as f64 / self.height_emu.max(1) as f64
    }
}

/// Read `path`.
///
/// # Errors
///
/// [`PptxError`]: the file will not open, is not a .pptx, or a part of it
/// cannot be read.
pub fn read(path: &Path) -> Result<Document, PptxError> {
    let name = path.display().to_string();
    let file = std::fs::File::open(path).map_err(|e| PptxError::Open(name.clone(), e))?;
    read_from(file, &name)
}

/// Read a .pptx from `reader`; `name` is for messages.
///
/// # Errors
///
/// As [`read`].
pub fn read_from<R: Read + Seek>(mut reader: R, name: &str) -> Result<Document, PptxError> {
    if !zip_fits(&mut reader, MAX_PARTS, MAX_LIST_BYTES) {
        return Err(PptxError::NotPptx(name.to_string()));
    }
    let zip = zip::ZipArchive::new(reader).map_err(|_| PptxError::NotPptx(name.to_string()))?;
    let mut package = Package::new(zip, name);
    let presentation_part = "ppt/presentation.xml";
    let text = package
        .part(presentation_part)?
        .ok_or_else(|| PptxError::NotPptx(name.to_string()))?;
    let xml = package.parse(presentation_part, &text)?;
    let root = xml.root_element();
    let (width_emu, height_emu) = child(root, "sldSz")
        .and_then(|size| {
            let cx = size.attribute("cx")?.parse().ok()?;
            let cy = size.attribute("cy")?.parse().ok()?;
            Some((cx, cy))
        })
        .unwrap_or(DEFAULT_SIZE_EMU);
    let order: Vec<String> = child(root, "sldIdLst")
        .map(|list| {
            list.children()
                .filter(|n| n.tag_name().name() == "sldId")
                .filter_map(|n| relationship_id(n).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let rels = package.relationships(presentation_part)?;
    let mut slides = Vec::with_capacity(order.len());
    for id in order {
        // A slide the list names but the package lacks is skipped, as
        // PowerPoint repairs it, rather than failing the file.
        let Some(target) = rels.get(&id).filter(|r| r.kind == "slide") else {
            continue;
        };
        let target = target.target.clone();
        if let Some(slide) = package.slide(&target)? {
            slides.push(slide);
        }
    }
    Ok(Document {
        slides,
        width_emu,
        height_emu,
    })
}

/// A relationship: what kind (the last part of its type, as `slide` or
/// `notesSlide`) and the part it names.
struct Relationship {
    kind: String,
    target: String,
}

struct Package<R: Read + Seek> {
    zip: zip::ZipArchive<R>,
    name: String,
    /// Each part's name lowercased, to the name the zip has: part names are
    /// not case-sensitive.
    names: HashMap<String, String>,
}

impl<R: Read + Seek> Package<R> {
    fn new(zip: zip::ZipArchive<R>, name: &str) -> Self {
        let names = zip
            .file_names()
            .map(|n| (n.to_ascii_lowercase(), n.to_string()))
            .collect();
        Self {
            zip,
            name: name.to_string(),
            names,
        }
    }

    fn broken(&self, part: &str, reason: &dyn std::fmt::Display) -> PptxError {
        broken(&self.name, part, reason)
    }

    /// The text of `part`, or `None` when the package has no such part.
    fn part(&mut self, part: &str) -> Result<Option<String>, PptxError> {
        let Some(actual) = self.names.get(&part.to_ascii_lowercase()).cloned() else {
            return Ok(None);
        };
        let name = &self.name;
        let mut file = self
            .zip
            .by_name(&actual)
            .map_err(|e| broken(name, part, &e))?;
        if file.size() > MAX_PART_BYTES {
            return Err(broken(name, part, &"too big"));
        }
        let mut text = String::new();
        file.by_ref()
            .take(MAX_PART_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|e| broken(name, part, &e))?;
        if text.len() as u64 > MAX_PART_BYTES {
            return Err(broken(name, part, &"too big"));
        }
        Ok(Some(text))
    }

    fn parse<'a>(&self, part: &str, text: &'a str) -> Result<roxmltree::Document<'a>, PptxError> {
        roxmltree::Document::parse(text).map_err(|e| self.broken(part, &e))
    }

    /// `part`'s relationships by id, each target resolved to a part name.
    fn relationships(&mut self, part: &str) -> Result<HashMap<String, Relationship>, PptxError> {
        let rels_part = rels_name(part);
        let Some(text) = self.part(&rels_part)? else {
            return Ok(HashMap::new());
        };
        let xml = self.parse(&rels_part, &text)?;
        let base = part.rsplit_once('/').map_or("", |(dir, _)| dir);
        Ok(xml
            .root_element()
            .children()
            .filter(|n| n.tag_name().name() == "Relationship")
            .filter(|n| n.attribute("TargetMode") != Some("External"))
            .filter_map(|n| {
                let id = n.attribute("Id")?;
                let kind = n.attribute("Type")?.rsplit('/').next()?.to_string();
                let target = resolve(base, n.attribute("Target")?);
                Some((id.to_string(), Relationship { kind, target }))
            })
            .collect())
    }

    /// The slide at `part`, with its notes; `None` when the part is missing.
    fn slide(&mut self, part: &str) -> Result<Option<Slide>, PptxError> {
        let Some(text) = self.part(part)? else {
            return Ok(None);
        };
        let xml = self.parse(part, &text)?;
        let root = xml.root_element();
        // `show="0"` (or "false") hides it; the default is shown.
        let hidden = matches!(root.attribute("show"), Some("0" | "false"));
        let title = placeholders(root, &["title", "ctrTitle"])
            .map(shape_text)
            .find(|t| !t.is_empty());
        let rels = self.relationships(part)?;
        let mut notes = String::new();
        if let Some(rel) = rels.values().find(|r| r.kind == "notesSlide") {
            let notes_part = rel.target.clone();
            if let Some(text) = self.part(&notes_part)? {
                let xml = self.parse(&notes_part, &text)?;
                notes = placeholders(xml.root_element(), &["body"])
                    .map(shape_text)
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
        Ok(Some(Slide {
            title,
            notes,
            hidden,
        }))
    }
}

/// Whether the zip in `file` claims at most `max_parts` parts and a list of
/// them at most `max_list` bytes long — read from its end records before
/// the zip is opened: opening one reserves room for every part it claims,
/// and a zip may claim millions. Every end record in its last 64 KB that a
/// zip reader could take — its comment within the file; bytes that only
/// look like one, their "comment" running past the end, are not — is
/// counted, each with the ZIP64 record a locator before it points to, whose
/// extra data must be small. Not a zip at all: false. `file` is left at its
/// start.
///
/// A file made to mislead a zip reader can still lead it to an end record
/// further back than this looks, or to a ZIP64 record other than the one
/// its locator points to; a speaker's deck, or a damaged one, is not that.
pub fn zip_fits(file: &mut (impl Read + Seek), max_parts: u64, max_list: u64) -> bool {
    let fits = claims(file).is_some_and(|claims| {
        !claims.is_empty()
            && claims
                .iter()
                .all(|&(parts, list)| parts <= max_parts && list <= max_list)
    });
    let _ = file.seek(std::io::SeekFrom::Start(0));
    fits
}

/// What a zip's end records claim: for each, how many parts and how long a
/// list of them — its ZIP64 record's too, when a locator is before it.
/// None when one cannot be read through (a ZIP64 locator pointing nowhere).
fn claims(file: &mut (impl Read + Seek)) -> Option<Vec<(u64, u64)>> {
    use std::io::SeekFrom;
    let le16 = |b: &[u8], at: usize| Some(u64::from(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?)));
    let le32 = |b: &[u8], at: usize| Some(u64::from(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?)));
    let le64 = |b: &[u8], at: usize| Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?));
    let len = file.seek(SeekFrom::End(0)).ok()?;
    // An end record (22 bytes) and a comment of up to 65,535 after it.
    let window = len.min(22 + 65_535);
    let start = len - window;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = vec![0_u8; usize::try_from(window).ok()?];
    file.read_exact(&mut tail).ok()?;
    let mut out = Vec::new();
    let Some(latest) = tail.len().checked_sub(22) else {
        return Some(out);
    };
    for at in (0..=latest).filter(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]) {
        // Its comment must end within the file, or no reader takes it.
        let comment = usize::try_from(le16(&tail, at + 20)?).ok()?;
        if at + 22 + comment > tail.len() {
            continue;
        }
        let (parts, list) = (le16(&tail, at + 10)?, le32(&tail, at + 12)?);
        // "See the ZIP64 record" is what all-ones means here, not a count.
        let to_zip64 = parts == 0xFFFF || list == 0xFFFF_FFFF;
        // A ZIP64 locator just before it — read where it is in the file,
        // whether or not this record says it needs one.
        let record_at = start + u64::try_from(at).ok()?;
        let mut locator = [0_u8; 20];
        let located = record_at.checked_sub(20).is_some_and(|locator_at| {
            file.seek(SeekFrom::Start(locator_at)).is_ok()
                && file.read_exact(&mut locator).is_ok()
                && locator[..4] == [0x50, 0x4b, 0x06, 0x07]
        });
        if !located {
            out.push((parts, list));
            continue;
        }
        if !to_zip64 {
            out.push((parts, list));
        }
        file.seek(SeekFrom::Start(le64(&locator, 8)?)).ok()?;
        let mut record = [0_u8; 56];
        file.read_exact(&mut record).ok()?;
        // No ZIP64 record where it points: the end record of a zip inside
        // this one (a workbook a deck carries, stored near its end), whose
        // locator counts from where that zip starts — not this deck's.
        if record[..4] != [0x50, 0x4b, 0x06, 0x06] {
            continue;
        }
        // Its extra data (a reader holds all of it) small: the record is 44
        // bytes after its size, and what follows that is the extra.
        if le64(&record, 4)? > 44 + 64 * 1024 {
            return None;
        }
        out.push((le64(&record, 32)?, le64(&record, 40)?));
    }
    Some(out)
}

fn broken(file: &str, part: &str, reason: &dyn std::fmt::Display) -> PptxError {
    PptxError::Broken {
        file: file.to_string(),
        part: part.to_string(),
        reason: reason.to_string(),
    }
}

/// The relationships part for `part`: `a/b.xml` → `a/_rels/b.xml.rels`.
fn rels_name(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// A relationship's target as a part name: from the package root when it
/// starts with `/`, else from `base`, the directory of the part it is in.
fn resolve(base: &str, target: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let joined = match target.strip_prefix('/') {
        Some(absolute) => absolute.to_string(),
        None if base.is_empty() => target.to_string(),
        None => format!("{base}/{target}"),
    };
    for piece in joined.split('/') {
        match piece {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(piece),
        }
    }
    parts.join("/")
}

fn child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|n| n.tag_name().name() == name)
}

/// The `r:id` of an element: an attribute named `id` in a namespace (the
/// element's own unprefixed `id` is something else).
fn relationship_id<'a>(node: roxmltree::Node<'a, '_>) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == "id" && a.namespace().is_some())
        .map(|a| a.value())
}

/// The shapes (`sp`) on a slide or notes page that are placeholders of one
/// of `kinds`.
fn placeholders<'a, 'input: 'a>(
    root: roxmltree::Node<'a, 'input>,
    kinds: &'a [&'a str],
) -> impl Iterator<Item = roxmltree::Node<'a, 'input>> + 'a {
    root.descendants()
        .filter(|n| n.tag_name().name() == "sp")
        .filter(move |shape| {
            shape
                .descendants()
                .find(|n| n.tag_name().name() == "ph")
                .and_then(|ph| ph.attribute("type"))
                .is_some_and(|kind| kinds.contains(&kind))
        })
}

/// A shape's text: its paragraphs a line each, a line break within one a
/// line too, surrounding blank lines dropped.
fn shape_text(shape: roxmltree::Node<'_, '_>) -> String {
    let Some(body) = shape.children().find(|n| n.tag_name().name() == "txBody") else {
        return String::new();
    };
    let mut lines: Vec<String> = Vec::new();
    for paragraph in body.children().filter(|n| n.tag_name().name() == "p") {
        let mut line = String::new();
        for piece in paragraph.descendants() {
            match piece.tag_name().name() {
                "t" => line.push_str(piece.text().unwrap_or("")),
                "br" => line.push('\n'),
                _ => {}
            }
        }
        lines.push(line);
    }
    lines.join("\n").trim_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::io::{Cursor, Write};

    const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
    const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    const TYPES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

    fn zip_of(parts: &[(&str, String)]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, text) in parts {
            zip.start_file(*name, options).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    fn rels(entries: &[(&str, &str, &str)]) -> String {
        let mut s =
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{RELS}">"#);
        for (id, kind, target) in entries {
            let _ = write!(
                s,
                r#"<Relationship Id="{id}" Type="{TYPES}/{kind}" Target="{target}"/>"#
            );
        }
        s + "</Relationships>"
    }

    fn shape(kind: &str, paragraphs: &[&str]) -> String {
        let mut body = String::new();
        for p in paragraphs {
            let runs: Vec<String> = p
                .split('\n')
                .map(|line| format!("<a:r><a:t>{line}</a:t></a:r>"))
                .collect();
            let _ = write!(body, "<a:p>{}</a:p>", runs.join("<a:br/>"));
        }
        format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="x"/><p:cNvSpPr/><p:nvPr><p:ph type="{kind}"/></p:nvPr></p:nvSpPr><p:txBody><a:bodyPr/>{body}</p:txBody></p:sp>"#
        )
    }

    fn slide(show: Option<&str>, shapes: &[String]) -> String {
        let show = show.map(|s| format!(r#" show="{s}""#)).unwrap_or_default();
        format!(
            r#"<?xml version="1.0"?><p:sld xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"{show}><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:sld>"#,
            shapes.concat()
        )
    }

    fn notes(shapes: &[String]) -> String {
        format!(
            r#"<?xml version="1.0"?><p:notes xmlns:p="{P}" xmlns:a="{A}"><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:notes>"#,
            shapes.concat()
        )
    }

    /// A zip's parts are counted from its end record before it is opened:
    /// one claiming more than a deck has — in its own record or its ZIP64
    /// one — is no deck, as is no zip at all; a real deck fits.
    #[test]
    fn a_zip_claiming_more_parts_than_a_deck_has_is_not_opened() {
        let deck = sample();
        assert!(zip_fits(&mut Cursor::new(&deck), 100, 1 << 20));
        assert!(!zip_fits(&mut Cursor::new(&deck), 3, 1 << 20), "more parts than allowed");
        assert!(!zip_fits(&mut Cursor::new(b"not a zip at all".to_vec()), 100, 1 << 20));
        // An end record claiming 60,000 parts.
        let mut claims = deck.clone();
        let at = claims.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).unwrap();
        claims[at + 8..at + 12].copy_from_slice(&[0x60, 0xEA, 0x60, 0xEA]);
        assert!(!zip_fits(&mut Cursor::new(&claims), 20_000, 1 << 30));
        // A ZIP64 end record claiming a billion parts.
        let mut zip64 = deck.clone();
        let at = zip64.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).unwrap();
        let record_at = u64::try_from(at).unwrap();
        let mut record = vec![0x50, 0x4b, 0x06, 0x06];
        record.extend_from_slice(&44_u64.to_le_bytes());
        record.extend_from_slice(&[0; 20]);
        record.extend_from_slice(&1_000_000_000_u64.to_le_bytes());
        record.extend_from_slice(&1_000_000_000_u64.to_le_bytes());
        record.extend_from_slice(&64_u64.to_le_bytes());
        let mut locator = vec![0x50, 0x4b, 0x06, 0x07, 0, 0, 0, 0];
        locator.extend_from_slice(&record_at.to_le_bytes());
        locator.extend_from_slice(&1_u32.to_le_bytes());
        let mut end = zip64.split_off(at);
        end[8..12].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        zip64.extend_from_slice(&record);
        zip64.extend_from_slice(&locator);
        zip64.extend_from_slice(&end);
        assert!(!zip_fits(&mut Cursor::new(&zip64), MAX_PARTS, MAX_LIST_BYTES));
        assert!(matches!(read_from(Cursor::new(zip64.clone()), "huge.pptx"), Err(PptxError::NotPptx(_))));
        // The same, its own record saying it needs none: still counted.
        let mut quiet = zip64.clone();
        let at = quiet.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).unwrap();
        quiet[at + 12..at + 16].copy_from_slice(&64_u32.to_le_bytes());
        assert!(!zip_fits(&mut Cursor::new(&quiet), MAX_PARTS, MAX_LIST_BYTES));
        // A record earlier in the comment claiming a million: counted too.
        let mut two = deck.clone();
        let at = two.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).unwrap();
        let mut fake = two[at..].to_vec();
        fake[10..12].copy_from_slice(&[0xFF, 0xFE]);
        let comment = u16::try_from(fake.len()).unwrap();
        two[at + 20..at + 22].copy_from_slice(&comment.to_le_bytes());
        two.extend_from_slice(&fake);
        assert!(!zip_fits(&mut Cursor::new(&two), 20_000, 1 << 30));
        // Bytes that only look like an end record, in a comment: their own
        // "comment" runs past the end of the file. Not counted: the deck fits.
        let mut commented = deck.clone();
        let at = commented.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).unwrap();
        let mut junk = vec![0x50, 0x4b, 0x05, 0x06];
        junk.extend_from_slice(&[b'A'; 32]);
        let length = u16::try_from(junk.len()).unwrap();
        commented[at + 20..at + 22].copy_from_slice(&length.to_le_bytes());
        commented.extend_from_slice(&junk);
        assert!(zip_fits(&mut Cursor::new(&commented), 100, 1 << 20));
        assert!(read_from(Cursor::new(commented), "commented.pptx").is_ok());
        // A zip carried inside, stored near the end, its ZIP64 locator
        // counting from its own start: not this deck's, and the deck fits.
        let mut carried = deck.clone();
        let at = carried.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).unwrap();
        let mut inner = vec![0x50, 0x4b, 0x06, 0x07, 0, 0, 0, 0];
        inner.extend_from_slice(&3_u64.to_le_bytes());
        inner.extend_from_slice(&1_u32.to_le_bytes());
        inner.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
        inner.extend_from_slice(&[0xFF; 8]);
        inner.extend_from_slice(&[0, 0]);
        let length = u16::try_from(inner.len()).unwrap();
        carried[at + 20..at + 22].copy_from_slice(&length.to_le_bytes());
        carried.extend_from_slice(&inner);
        assert!(zip_fits(&mut Cursor::new(&carried), 100, 1 << 20));
        assert!(read_from(Cursor::new(carried), "carried.pptx").is_ok());
        // A ZIP64 record declaring 4 GB of extra data: not opened.
        let mut extra = zip64.clone();
        let record = extra.windows(4).rposition(|w| w == [0x50, 0x4b, 0x06, 0x06]).unwrap();
        extra[record + 4..record + 12].copy_from_slice(&(4_u64 << 30).to_le_bytes());
        assert!(!zip_fits(&mut Cursor::new(&extra), u64::MAX, u64::MAX));
    }

    /// A real ZIP64 deck with a long comment fits: its locator is found
    /// where it is in the file, though before the last 64 KB.
    #[test]
    fn a_zip64_deck_with_a_long_comment_fits() {
        let deck = sample();
        let at = deck.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).unwrap();
        let mut zip64 = deck[..at].to_vec();
        let record_at = u64::try_from(zip64.len()).unwrap();
        let mut record = vec![0x50, 0x4b, 0x06, 0x06];
        record.extend_from_slice(&44_u64.to_le_bytes());
        record.extend_from_slice(&[0; 20]);
        record.extend_from_slice(&5_u64.to_le_bytes());
        record.extend_from_slice(&400_u64.to_le_bytes());
        record.extend_from_slice(&0_u64.to_le_bytes());
        let mut locator = vec![0x50, 0x4b, 0x06, 0x07, 0, 0, 0, 0];
        locator.extend_from_slice(&record_at.to_le_bytes());
        locator.extend_from_slice(&1_u32.to_le_bytes());
        let mut end = deck[at..at + 22].to_vec();
        end[8..12].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        end[20..22].copy_from_slice(&65_530_u16.to_le_bytes());
        zip64.extend_from_slice(&record);
        zip64.extend_from_slice(&locator);
        zip64.extend_from_slice(&end);
        zip64.extend(std::iter::repeat_n(b'c', 65_530));
        assert!(zip_fits(&mut Cursor::new(&zip64), 100, 1 << 20));
    }

    /// Three slides, played in an order unlike their part names: the first
    /// played is slide2.xml (a centred title, notes of two paragraphs, one
    /// with a line break), then slide1.xml (hidden, no notes), then
    /// slide3.xml (a title, notes by an absolute target, the notes page's
    /// slide number and picture placeholders left out).
    fn sample() -> Vec<u8> {
        let presentation = format!(
            r#"<?xml version="1.0"?><p:presentation xmlns:p="{P}" xmlns:r="{R}"><p:sldIdLst><p:sldId id="256" r:id="rId7"/><p:sldId id="257" r:id="rId3"/><p:sldId id="258" r:id="rId4"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
        );
        zip_of(&[
            ("[Content_Types].xml", "<Types/>".to_string()),
            ("ppt/presentation.xml", presentation),
            (
                "ppt/_rels/presentation.xml.rels",
                rels(&[
                    ("rId1", "slideMaster", "slideMasters/slideMaster1.xml"),
                    ("rId3", "slide", "slides/slide1.xml"),
                    ("rId4", "slide", "slides/slide3.xml"),
                    ("rId7", "slide", "slides/slide2.xml"),
                ]),
            ),
            (
                "ppt/slides/slide2.xml",
                slide(
                    None,
                    &[shape("ctrTitle", &["Welcome"]), shape("subTitle", &["x"])],
                ),
            ),
            (
                "ppt/slides/_rels/slide2.xml.rels",
                rels(&[
                    ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                    ("rId2", "notesSlide", "../notesSlides/notesSlide1.xml"),
                ]),
            ),
            (
                "ppt/notesSlides/notesSlide1.xml",
                notes(&[
                    shape("sldImg", &[]),
                    shape(
                        "body",
                        &["Thank the organisers.", "Then the agenda:\nthree parts."],
                    ),
                    shape("sldNum", &["1"]),
                ]),
            ),
            (
                "ppt/slides/slide1.xml",
                slide(Some("0"), &[shape("body", &["hidden"])]),
            ),
            (
                "ppt/slides/slide3.xml",
                slide(None, &[shape("title", &["Results"])]),
            ),
            (
                "ppt/slides/_rels/slide3.xml.rels",
                rels(&[("rId9", "notesSlide", "/ppt/notesSlides/notesSlide7.xml")]),
            ),
            (
                "ppt/notesSlides/notesSlide7.xml",
                notes(&[shape("body", &["", "Pause for questions.", ""])]),
            ),
        ])
    }

    #[test]
    fn slides_come_in_show_order_with_titles_notes_and_hidden() {
        let doc = read_from(Cursor::new(sample()), "sample.pptx").unwrap();
        assert_eq!(
            doc.slides,
            vec![
                Slide {
                    title: Some("Welcome".into()),
                    notes: "Thank the organisers.\nThen the agenda:\nthree parts.".into(),
                    hidden: false,
                },
                Slide {
                    title: None,
                    notes: String::new(),
                    hidden: true,
                },
                Slide {
                    title: Some("Results".into()),
                    notes: "Pause for questions.".into(),
                    hidden: false,
                },
            ]
        );
        assert_eq!((doc.width_emu, doc.height_emu), (12_192_000, 6_858_000));
        assert!((doc.aspect() - 16.0 / 9.0).abs() < 1e-9);
    }

    #[test]
    fn a_slide_the_package_lacks_is_skipped_and_no_size_means_four_by_three() {
        let presentation = format!(
            r#"<p:presentation xmlns:p="{P}" xmlns:r="{R}"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>"#
        );
        let bytes = zip_of(&[
            ("ppt/presentation.xml", presentation),
            (
                "ppt/_rels/presentation.xml.rels",
                rels(&[
                    ("rId1", "slide", "slides/slide1.xml"),
                    ("rId2", "slide", "slides/gone.xml"),
                ]),
            ),
            ("ppt/slides/slide1.xml", slide(Some("1"), &[])),
        ]);
        let doc = read_from(Cursor::new(bytes), "x.pptx").unwrap();
        assert_eq!(doc.slides.len(), 1);
        assert!(!doc.slides[0].hidden);
        assert_eq!((doc.width_emu, doc.height_emu), DEFAULT_SIZE_EMU);
    }

    #[test]
    fn an_old_ppt_or_anything_else_is_not_a_pptx() {
        let err = read_from(
            Cursor::new(b"\xD0\xCF\x11\xE0 not a zip".to_vec()),
            "old.ppt",
        )
        .unwrap_err();
        assert!(matches!(err, PptxError::NotPptx(_)), "{err}");
        let no_presentation = zip_of(&[("word/document.xml", "<w/>".into())]);
        let err = read_from(Cursor::new(no_presentation), "letter.docx").unwrap_err();
        assert!(matches!(err, PptxError::NotPptx(_)), "{err}");
    }

    #[test]
    fn targets_resolve_from_their_parts_directory_or_the_root() {
        assert_eq!(resolve("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(
            resolve("ppt/slides", "../notesSlides/n.xml"),
            "ppt/notesSlides/n.xml"
        );
        assert_eq!(resolve("ppt/slides", "/ppt/media/a.mp4"), "ppt/media/a.mp4");
        assert_eq!(resolve("", "ppt/presentation.xml"), "ppt/presentation.xml");
        assert_eq!(
            rels_name("ppt/slides/slide1.xml"),
            "ppt/slides/_rels/slide1.xml.rels"
        );
        assert_eq!(rels_name("x.xml"), "_rels/x.xml.rels");
    }
}
