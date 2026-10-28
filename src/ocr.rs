//! Reading a receipt file into text (docs/SPEC.md, "Receipts"): plain text
//! as is, PDFs through poppler's `pdftotext` (else rendered with `pdftoppm`
//! and OCRed), images through the `tesseract` CLI. Local tools only, never
//! a network service. The tools sit behind [`Ocr`], so tests feed text.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{AppError, Result};

/// What a receipt file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Pdf,
    Image,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Pdf => "pdf",
            Self::Image => "image",
        }
    }
}

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff", "bmp", "gif", "webp", "heic", "heif", "pnm", "jp2"];

/// The kind of a file, by its first bytes, else its extension; UTF-8
/// without a known signature is text.
pub fn kind_of(path: &Path, bytes: &[u8]) -> Kind {
    let magic: &[(&[u8], Kind)] = &[
        (b"%PDF", Kind::Pdf),
        (b"\x89PNG", Kind::Image),
        (b"\xff\xd8\xff", Kind::Image),
        (b"GIF8", Kind::Image),
        (b"II*\0", Kind::Image),
        (b"MM\0*", Kind::Image),
        (b"BM", Kind::Image),
    ];
    if let Some((_, k)) = magic.iter().find(|(m, _)| bytes.starts_with(m)) {
        return *k;
    }
    // RIFF....WEBP, ....ftypheic
    if (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        || (bytes.len() >= 12
            && &bytes[4..8] == b"ftyp"
            && matches!(&bytes[8..12], b"heic" | b"heix" | b"mif1" | b"heim"))
    {
        return Kind::Image;
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if ext == "pdf" {
        Kind::Pdf
    } else if IMAGE_EXT.contains(&ext.as_str()) || std::str::from_utf8(bytes).is_err() {
        Kind::Image
    } else {
        Kind::Text
    }
}

/// Text from images and PDFs.
pub trait Ocr {
    /// The text of an image.
    fn image(&self, path: &Path) -> Result<String>;
    /// The text of a PDF (its text layer, else its pages OCRed).
    fn pdf(&self, path: &Path) -> Result<String>;
}

/// The text of a receipt file, its kind and its bytes.
pub fn read(path: &Path, ocr: &dyn Ocr) -> Result<(String, Kind, Vec<u8>)> {
    let bytes = std::fs::read(path).map_err(|e| AppError::io(format!("{}: {e}", path.display())))?;
    let kind = kind_of(path, &bytes);
    let text = match kind {
        Kind::Text => String::from_utf8_lossy(&bytes).replace('\r', ""),
        Kind::Pdf => ocr.pdf(path)?,
        Kind::Image => ocr.image(path)?,
    };
    if text.trim().is_empty() {
        return Err(AppError::invalid(format!("{}: no text found ({} file)", path.display(), kind.as_str())));
    }
    Ok((text, kind, bytes))
}

/// The local FOSS tools: `tesseract`, and poppler's `pdftotext` and
/// `pdftoppm`.
pub struct System;

const TESSERACT_HINT: &str = "install Tesseract OCR: `brew install tesseract` (macOS) or \
    `apt install tesseract-ocr` (Debian/Ubuntu)";
const POPPLER_HINT: &str =
    "install poppler: `brew install poppler` (macOS) or `apt install poppler-utils` (Debian/Ubuntu)";

/// Run a tool and return its stdout; a missing tool names what to install.
fn run(tool: &str, args: &[&std::ffi::OsStr], hint: &str) -> Result<String> {
    let out = Command::new(tool).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::io(format!("`{tool}` not found on PATH: {hint}"))
        } else {
            AppError::io(format!("running {tool}: {e}"))
        }
    })?;
    if !out.status.success() {
        return Err(AppError::io(format!(
            "{tool} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A fresh temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let p = std::env::temp_dir().join(format!("wherebox-ocr-{}", crate::ulid::new()?));
        std::fs::create_dir_all(&p).map_err(|e| AppError::io(format!("{}: {e}", p.display())))?;
        Ok(Self(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Ocr for System {
    fn image(&self, path: &Path) -> Result<String> {
        // --psm 4: one column of text of variable sizes, as receipts are
        run("tesseract", &[path.as_os_str(), "stdout".as_ref(), "--psm".as_ref(), "4".as_ref()], TESSERACT_HINT)
            .map_err(|e| {
                let heic = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("heic"));
                if heic && !e.message.contains("not found") {
                    e.context("this tesseract cannot read HEIC; convert it to PNG or JPEG first (e.g. `sips -s format png` or `heif-convert`)")
                } else {
                    e
                }
            })
    }

    fn pdf(&self, path: &Path) -> Result<String> {
        let text = run("pdftotext", &["-layout".as_ref(), path.as_os_str(), "-".as_ref()], POPPLER_HINT)?;
        if text.chars().filter(|c| !c.is_whitespace()).count() >= 10 {
            return Ok(text);
        }
        // a scanned PDF: render the pages, OCR each
        let dir = TempDir::new()?;
        let prefix = dir.0.join("page");
        run(
            "pdftoppm",
            &["-r".as_ref(), "300".as_ref(), "-png".as_ref(), path.as_os_str(), prefix.as_os_str()],
            POPPLER_HINT,
        )?;
        let mut pages: Vec<PathBuf> = std::fs::read_dir(&dir.0)
            .map_err(|e| AppError::io(e.to_string()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "png"))
            .collect();
        pages.sort();
        pages.iter().map(|p| self.image(p)).collect::<Result<Vec<_>>>().map(|t| t.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Ocr for Fake {
        fn image(&self, _: &Path) -> Result<String> {
            Ok("IMAGE TEXT".into())
        }
        fn pdf(&self, _: &Path) -> Result<String> {
            Ok("PDF TEXT".into())
        }
    }

    #[test]
    fn kinds_by_magic_then_extension() {
        assert_eq!(kind_of(Path::new("r.bin"), b"%PDF-1.7"), Kind::Pdf);
        assert_eq!(kind_of(Path::new("r.txt"), b"\x89PNG\r\n"), Kind::Image);
        assert_eq!(kind_of(Path::new("r"), b"\xff\xd8\xff\xe0"), Kind::Image);
        assert_eq!(kind_of(Path::new("r.heic"), b"\0\0\0\x18ftypheic"), Kind::Image);
        assert_eq!(kind_of(Path::new("r.HEIC"), b"whatever"), Kind::Image);
        assert_eq!(kind_of(Path::new("r.pdf"), b"broken"), Kind::Pdf);
        assert_eq!(kind_of(Path::new("r.eml"), b"ALDI\nMILK 3.15\n"), Kind::Text);
        assert_eq!(kind_of(Path::new("r"), b"\xff\xfe\x00bad"), Kind::Image, "not UTF-8: try OCR");
    }

    #[test]
    fn read_dispatches_to_the_ocr() {
        let t = std::env::temp_dir().join(format!("wherebox-ocr-test-{}", crate::ulid::new().unwrap()));
        std::fs::create_dir_all(&t).unwrap();
        let (png, txt, empty) = (t.join("r.png"), t.join("r.txt"), t.join("e.txt"));
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::write(&txt, "MILK 3.00\r\n").unwrap();
        std::fs::write(&empty, "  \n").unwrap();
        assert_eq!(read(&png, &Fake).unwrap().0, "IMAGE TEXT");
        let (text, kind, _) = read(&txt, &Fake).unwrap();
        assert_eq!((text.as_str(), kind), ("MILK 3.00\n", Kind::Text));
        assert!(read(&empty, &Fake).is_err());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn missing_tools_name_the_package() {
        let e = run("wherebox-no-such-tool", &[], TESSERACT_HINT).unwrap_err();
        assert!(e.message.contains("brew install tesseract") && e.message.contains("apt install tesseract-ocr"));
        assert_eq!(e.exit_code(), 6);
    }
}
