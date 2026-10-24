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
pub fn read_from<R: Read + Seek>(reader: R, name: &str) -> Result<Document, PptxError> {
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
