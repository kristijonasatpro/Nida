//! A PDF talk's pages, drawn by macOS's own PDF engine (Core Graphics) at
//! the picture's size — sharp however big — on a thread of their own, the
//! page on screen first and the ones either side of it after, so a step
//! never waits on a page (a page with large photos takes 50–90 ms to draw,
//! measured on an M4 mini).

use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// What the drawing thread found.
#[derive(Debug)]
pub enum PdfEvent {
    /// The file opened: its pages, and the first page's width over height.
    Opened { pages: u32, aspect: f64 },
    /// It would not open, and why.
    Failed(String),
    /// Page `page` (from 1), drawn to fit `size` with its shape kept.
    Drawn {
        page: u32,
        size: (u32, u32),
        image: image::RgbaImage,
    },
    /// Page `page` would not draw.
    DrawFailed { page: u32, message: String },
}

/// The pages a talk wants drawn, first wanted first, each fitted to `size`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Want {
    pages: Vec<u32>,
    size: (u32, u32),
}

/// A PDF's pages drawn on a thread: asked for by [`Self::want`], taken by
/// [`Self::poll`]. Dropped, its thread ends.
pub struct PdfDrawer {
    wants: mpsc::Sender<Want>,
    events: mpsc::Receiver<PdfEvent>,
}

impl PdfDrawer {
    /// Open the PDF at `path` on a thread of its own.
    #[must_use]
    pub fn open(path: PathBuf) -> Self {
        let (wants_tx, wants_rx) = mpsc::channel::<Want>();
        let (events_tx, events_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("pdf-pages".into())
            .spawn(move || draw_pages(&path, &wants_rx, &events_tx));
        let (wants, events) = match spawned {
            Ok(_) => (wants_tx, events_rx),
            Err(e) => {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(PdfEvent::Failed(format!("could not start drawing it: {e}")));
                (wants_tx, rx)
            }
        };
        Self { wants, events }
    }

    /// Draw `pages` (from 1), in that order, to fit `size`: what was asked
    /// before and not yet drawn is not drawn.
    pub fn want(&self, pages: Vec<u32>, size: (u32, u32)) {
        let _ = self.wants.send(Want { pages, size });
    }

    /// What the thread has found since last asked.
    #[must_use]
    pub fn poll(&self) -> Vec<PdfEvent> {
        self.events.try_iter().collect()
    }
}

/// The drawing thread: open the file, then draw what is wanted — the newest
/// want each time, looked for again between pages.
fn draw_pages(path: &Path, wants: &mpsc::Receiver<Want>, events: &mpsc::Sender<PdfEvent>) {
    let pages = match PdfPages::open(path) {
        Ok(pages) => pages,
        Err(e) => {
            let _ = events.send(PdfEvent::Failed(format!("{e:#}")));
            return;
        }
    };
    let _ = events.send(PdfEvent::Opened {
        pages: pages.pages(),
        aspect: pages.aspect(1).unwrap_or(16.0 / 9.0),
    });
    let Ok(mut want) = wants.recv() else {
        return;
    };
    loop {
        let mut newer = None;
        for &page in &want.pages {
            let event = match pages.draw(page, want.size) {
                Ok(image) => PdfEvent::Drawn {
                    page,
                    size: want.size,
                    image,
                },
                Err(e) => PdfEvent::DrawFailed {
                    page,
                    message: format!("{e:#}"),
                },
            };
            if events.send(event).is_err() {
                return;
            }
            if let Some(w) = wants.try_iter().last() {
                newer = Some(w);
                break;
            }
        }
        want = match newer {
            Some(w) => w,
            None => match wants.recv() {
                Ok(w) => w,
                Err(_) => return,
            },
        };
    }
}

/// A PDF opened for drawing. Core Graphics documents are not sent between
/// threads: each is opened and drawn on one.
pub struct PdfPages {
    #[cfg(target_os = "macos")]
    doc: objc2_core_foundation::CFRetained<objc2_core_graphics::CGPDFDocument>,
    pages: u32,
}

impl PdfPages {
    /// Open the PDF at `path`.
    ///
    /// # Errors
    ///
    /// Why it cannot be drawn: not there, not a PDF, locked with a
    /// password, or no pages.
    #[cfg(target_os = "macos")]
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        use objc2_core_foundation::CFURL;
        use objc2_core_graphics::CGPDFDocument;
        if !path.is_file() {
            anyhow::bail!("its file is not there");
        }
        let url = CFURL::from_file_path(path).ok_or_else(|| anyhow::anyhow!("its path"))?;
        let doc = CGPDFDocument::with_url(Some(&url))
            .ok_or_else(|| anyhow::anyhow!("it is not a PDF macOS can read"))?;
        if CGPDFDocument::is_encrypted(Some(&doc)) && !CGPDFDocument::is_unlocked(Some(&doc)) {
            anyhow::bail!("it is locked with a password: save it unlocked");
        }
        let pages = u32::try_from(CGPDFDocument::number_of_pages(Some(&doc))).unwrap_or(u32::MAX);
        if pages == 0 {
            anyhow::bail!("it has no pages");
        }
        Ok(Self { doc, pages })
    }

    /// PDF pages are drawn on macOS only.
    ///
    /// # Errors
    ///
    /// Always, off macOS.
    #[cfg(not(target_os = "macos"))]
    pub fn open(_path: &Path) -> anyhow::Result<Self> {
        anyhow::bail!("PDF talks play on macOS")
    }

    /// How many pages it has.
    #[must_use]
    pub fn pages(&self) -> u32 {
        self.pages
    }

    /// Page `n`'s (from 1) width over height, as shown (turned as the file
    /// says).
    #[must_use]
    pub fn aspect(&self, n: u32) -> Option<f64> {
        let (w, h, _) = self.page_box(n)?;
        (h > 0.0).then(|| w / h)
    }

    /// Page `n`'s crop box as shown — width, height, points — and how far
    /// it is turned, degrees clockwise.
    #[cfg(target_os = "macos")]
    fn page_box(&self, n: u32) -> Option<(f64, f64, i32)> {
        use objc2_core_graphics::{CGPDFBox, CGPDFDocument, CGPDFPage};
        let page = CGPDFDocument::page(Some(&self.doc), n as usize)?;
        let rect = CGPDFPage::box_rect(Some(&page), CGPDFBox::CropBox);
        let turn = CGPDFPage::rotation_angle(Some(&page)).rem_euclid(360);
        let (w, h) = (rect.size.width.abs(), rect.size.height.abs());
        Some(if turn == 90 || turn == 270 {
            (h, w, turn)
        } else {
            (w, h, turn)
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn page_box(&self, _n: u32) -> Option<(f64, f64, i32)> {
        None
    }

    /// Page `n` (from 1) on white paper, as big as fits in `size` with its
    /// shape kept: the picture is the page's size there, not `size` — what
    /// shows it fits it into the frame.
    ///
    /// # Errors
    ///
    /// No such page, or Core Graphics would not draw it.
    #[cfg(target_os = "macos")]
    pub fn draw(&self, n: u32, size: (u32, u32)) -> anyhow::Result<image::RgbaImage> {
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        use objc2_core_graphics::{
            kCGColorSpaceSRGB, CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo,
            CGInterpolationQuality, CGPDFBox, CGPDFDocument, CGPDFPage,
        };
        let page = CGPDFDocument::page(Some(&self.doc), n as usize)
            .ok_or_else(|| anyhow::anyhow!("it has no page {n}"))?;
        let (pw, ph, turn) = self
            .page_box(n)
            .filter(|(w, h, _)| *w > 0.0 && *h > 0.0)
            .ok_or_else(|| anyhow::anyhow!("page {n} has no size"))?;
        let (out_w, out_h) = fitted(pw, ph, size);
        let mut pixels = vec![0u8; out_w as usize * out_h as usize * 4];
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .ok_or_else(|| anyhow::anyhow!("no sRGB colour space"))?;
        // Red, green, blue, alpha in that order: the default byte order.
        let info = CGImageAlphaInfo::PremultipliedLast.0;
        // SAFETY: `pixels` holds `out_h` rows of `out_w * 4` bytes and
        // outlives the context, dropped before it is read.
        let ctx = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                out_w as usize,
                out_h as usize,
                8,
                out_w as usize * 4,
                Some(&space),
                info,
            )
        }
        .ok_or_else(|| anyhow::anyhow!("no {out_w} × {out_h} picture to draw it in"))?;
        let c = Some(&*ctx);
        let (w, h) = (f64::from(out_w), f64::from(out_h));
        CGContext::set_rgb_fill_color(c, 1.0, 1.0, 1.0, 1.0);
        CGContext::fill_rect(c, CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h)));
        CGContext::set_interpolation_quality(c, CGInterpolationQuality::High);
        let rect = CGPDFPage::box_rect(Some(&page), CGPDFBox::CropBox);
        let scale = (w / pw).min(h / ph);
        // The page's middle to the picture's, turned as the file says
        // (clockwise; Core Graphics turns anticlockwise), scaled to fit.
        CGContext::translate_ctm(c, w / 2.0, h / 2.0);
        CGContext::rotate_ctm(c, -f64::from(turn).to_radians());
        CGContext::scale_ctm(c, scale, scale);
        CGContext::translate_ctm(
            c,
            -(rect.origin.x + rect.size.width / 2.0),
            -(rect.origin.y + rect.size.height / 2.0),
        );
        CGContext::clip_to_rect(c, rect);
        CGContext::draw_pdf_page(c, Some(&page));
        drop(ctx);
        // The paper is opaque, so premultiplied is the straight colour.
        image::RgbaImage::from_raw(out_w, out_h, pixels)
            .ok_or_else(|| anyhow::anyhow!("page {n} came out the wrong size"))
    }

    /// PDF pages are drawn on macOS only.
    ///
    /// # Errors
    ///
    /// Always, off macOS.
    #[cfg(not(target_os = "macos"))]
    pub fn draw(&self, _n: u32, _size: (u32, u32)) -> anyhow::Result<image::RgbaImage> {
        anyhow::bail!("PDF talks play on macOS")
    }
}

/// A PDF of `pages` pages, 400 × 300 points, each all one colour of its
/// own (page `n`'s red is `n * 40`, its green `255 - n * 40`): for tests.
#[cfg(any(test, feature = "test-fixtures"))]
#[must_use]
pub fn sample_pdf(pages: u32) -> Vec<u8> {
    let pages = pages.max(1);
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            (0..pages)
                .map(|i| format!("{} 0 R", 3 + i * 2))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ];
    for n in 1..=pages {
        let shade = (n * 40) % 256;
        let content = format!(
            "{:.3} {:.3} 0 rg 0 0 400 300 re f",
            f64::from(shade) / 255.0,
            f64::from(255 - shade) / 255.0
        );
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Contents {} 0 R >>",
            4 + (n - 1) * 2
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A `pw × ph` page fitted into `size`, whole pixels, never under one.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn fitted(pw: f64, ph: f64, size: (u32, u32)) -> (u32, u32) {
    let (w, h) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let scale = (w / pw).min(h / ph);
    (
        ((pw * scale).round() as u32).clamp(1, size.0.max(1)),
        ((ph * scale).round() as u32).clamp(1, size.1.max(1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_keeps_its_shape_inside_the_picture() {
        // A 16:9 page fills a 16:9 picture; a 4:3 one stands in its middle
        // the picture's height; an A4 portrait page likewise.
        assert_eq!(fitted(960.0, 540.0, (1920, 1080)), (1920, 1080));
        assert_eq!(fitted(1024.0, 768.0, (1920, 1080)), (1440, 1080));
        assert_eq!(fitted(595.0, 842.0, (1920, 1080)), (763, 1080));
        assert_eq!(fitted(100.0, 1.0, (1920, 1080)), (1920, 19));
        assert_eq!(fitted(1.0, 1.0, (0, 0)), (1, 1));
    }

    /// A one-page PDF, 400 × 300 points, its left half red: drawn, and its
    /// pixels read back the right way up and round.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_page_is_drawn_whole_and_the_right_way_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.pdf");
        std::fs::write(&path, tiny_pdf()).unwrap();
        let pages = PdfPages::open(&path).expect("opens");
        assert_eq!(pages.pages(), 1);
        assert!((pages.aspect(1).unwrap() - 4.0 / 3.0).abs() < 1e-6);
        let image = pages.draw(1, (1920, 1080)).expect("draws");
        assert_eq!(image.dimensions(), (1440, 1080));
        let at = |x: u32, y: u32| image.get_pixel(x, y).0;
        // Left half red, right half the white paper, top and bottom alike.
        assert_eq!(at(100, 100), [255, 0, 0, 255]);
        assert_eq!(at(100, 1000), [255, 0, 0, 255]);
        assert_eq!(at(1300, 150), [255, 255, 255, 255]);
        // A green band along its top, 30 points (108 pixels): the right way
        // up.
        assert_eq!(at(1300, 20), [0, 255, 0, 255]);
        assert_eq!(at(1300, 1060), [255, 255, 255, 255]);
        assert!(pages.draw(2, (1920, 1080)).is_err(), "no second page");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_drawer_opens_and_draws_on_its_thread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.pdf");
        std::fs::write(&path, tiny_pdf()).unwrap();
        let drawer = PdfDrawer::open(path);
        drawer.want(vec![1], (800, 600));
        let mut events = Vec::new();
        let start = std::time::Instant::now();
        while events.len() < 2 && start.elapsed() < std::time::Duration::from_secs(10) {
            events.extend(drawer.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            matches!(events[0], PdfEvent::Opened { pages: 1, .. }),
            "{events:?}"
        );
        assert!(
            matches!(&events[1], PdfEvent::Drawn { page: 1, size: (800, 600), image } if image.dimensions() == (800, 600)),
            "{events:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_sample_pdf_has_its_pages_each_its_own_colour() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("three.pdf");
        std::fs::write(&path, sample_pdf(3)).unwrap();
        let pages = PdfPages::open(&path).expect("opens");
        assert_eq!(pages.pages(), 3);
        let p2 = pages.draw(2, (400, 300)).unwrap();
        assert_eq!(p2.get_pixel(200, 150).0, [80, 175, 0, 255]);
    }

    #[test]
    fn a_missing_or_wrong_file_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let gone = PdfPages::open(&dir.path().join("gone.pdf"));
        assert!(gone.is_err());
        #[cfg(target_os = "macos")]
        {
            assert!(format!("{:#}", gone.err().unwrap()).contains("not there"));
            let text = dir.path().join("text.pdf");
            std::fs::write(&text, b"hello").unwrap();
            let wrong = PdfPages::open(&text).err().unwrap();
            assert!(format!("{wrong:#}").contains("not a PDF"), "{wrong:#}");
        }
    }

    /// A 400 × 300 PDF: its left half red, a green band 30 points high along
    /// its top right.
    #[cfg(target_os = "macos")]
    pub(crate) fn tiny_pdf() -> Vec<u8> {
        let content = "1 0 0 rg 0 0 200 300 re f 0 1 0 rg 200 270 200 30 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Contents 4 0 R >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}
