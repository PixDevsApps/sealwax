//! HTML signatures (issue #6): an account's or identity's `signature_html` names a file under
//! `~/.config/sealwax/signatures/` ([`SIGNATURES_DIR`]). [`load`] reads it, keeps what the
//! composer's allowlist keeps (`domain::compose::clean::clean_signature`: bold, italic, links,
//! lists, tables, line breaks; no styles, scripts or remote content), and turns each image that
//! names a PNG, JPEG or GIF file in the signature's own folder into an inline part with a
//! Content-ID on the sender's domain. Paths are checked after symlinks are resolved, so nothing
//! outside that folder is read. A file that can't be used is an error that says why; the composer
//! then keeps the plain-text signature and says so. Nothing here is logged.

use sealwax_domain::accounts_file::is_signature_path;
use sealwax_domain::compose::clean::clean_signature;
use sealwax_domain::compose::identity::{SignatureBlock, html_signature};
use sealwax_domain::compose::ids;
use std::cell::RefCell;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The signatures folder's name in Sealwax's configuration folder.
pub const SIGNATURES_DIR: &str = "signatures";
/// The largest signature file.
pub const MAX_HTML_BYTES: u64 = 256 * 1024;
/// The largest image a signature can show.
pub const MAX_IMAGE_BYTES: u64 = 1024 * 1024;
/// The most images a signature can show.
pub const MAX_IMAGES: usize = 10;

/// An image the signature shows, as an inline part.
#[derive(Clone, PartialEq, Eq)]
pub struct SignatureImage {
    pub content_id: String,
    /// The file's name, as the part's.
    pub name: String,
    pub mime_type: &'static str,
    pub data: Vec<u8>,
}

/// A signature ready for the composer: its block and the images it shows.
#[derive(Clone, PartialEq, Eq)]
pub struct LoadedSignature {
    pub block: SignatureBlock,
    pub images: Vec<SignatureImage>,
}

impl fmt::Debug for LoadedSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedSignature")
            .field("block", &self.block)
            .field("images", &self.images.len())
            .finish()
    }
}

/// Why an image can't be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFault {
    Missing,
    Outside,
    Kind,
    TooLarge,
}

/// Why a signature file can't be used; its text is for the user and names only their own
/// file's or image's name.
#[derive(Clone, PartialEq, Eq)]
pub enum SignatureError {
    /// The file isn't there.
    Missing,
    /// The path leads outside the signatures folder.
    Outside,
    TooLarge,
    /// Not UTF-8 text.
    NotText,
    /// It shows nothing once cleaned.
    Empty,
    TooManyImages,
    Image(String, ImageFault),
    Unreadable,
}

impl fmt::Debug for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Image(_, fault) => f.debug_tuple("Image").field(fault).finish(),
            Self::Missing => f.write_str("Missing"),
            Self::Outside => f.write_str("Outside"),
            Self::TooLarge => f.write_str("TooLarge"),
            Self::NotText => f.write_str("NotText"),
            Self::Empty => f.write_str("Empty"),
            Self::TooManyImages => f.write_str("TooManyImages"),
            Self::Unreadable => f.write_str("Unreadable"),
        }
    }
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("the file isn't in the signatures folder"),
            Self::Outside => f.write_str("the file is outside the signatures folder"),
            Self::TooLarge => write!(f, "the file is larger than {} KB", MAX_HTML_BYTES / 1024),
            Self::NotText => f.write_str("the file isn't UTF-8 text"),
            Self::Empty => f.write_str("the file shows nothing Sealwax can send"),
            Self::TooManyImages => write!(f, "it shows more than {MAX_IMAGES} images"),
            Self::Unreadable => f.write_str("the file can't be read"),
            Self::Image(name, fault) => match fault {
                ImageFault::Missing => write!(f, "its image “{name}” isn't there"),
                ImageFault::Outside => {
                    write!(f, "its image “{name}” is outside the signature's folder")
                }
                ImageFault::Kind => write!(f, "its image “{name}” isn't a PNG, JPEG or GIF"),
                ImageFault::TooLarge => write!(
                    f,
                    "its image “{name}” is larger than {} KB",
                    MAX_IMAGE_BYTES / 1024
                ),
            },
        }
    }
}

impl std::error::Error for SignatureError {}

/// The signatures folder in `config`, Sealwax's configuration folder.
pub fn dir(config: &Path) -> PathBuf {
    config.join(SIGNATURES_DIR)
}

/// The signature `relative` names in the signatures folder `dir`, its images given Content-IDs on
/// `domain` (the sender's, in ASCII).
pub fn load(dir: &Path, relative: &str, domain: &str) -> Result<LoadedSignature, SignatureError> {
    if !is_signature_path(relative) {
        return Err(SignatureError::Outside);
    }
    let root = canonical(dir).map_err(|_| SignatureError::Missing)?;
    let file = canonical(&root.join(relative)).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => SignatureError::Missing,
        _ => SignatureError::Unreadable,
    })?;
    if !file.starts_with(&root) {
        return Err(SignatureError::Outside);
    }
    let size = fs::metadata(&file)
        .map_err(|_| SignatureError::Unreadable)?
        .len();
    if size > MAX_HTML_BYTES {
        return Err(SignatureError::TooLarge);
    }
    let bytes = fs::read(&file).map_err(|_| SignatureError::Unreadable)?;
    let html = String::from_utf8(bytes).map_err(|_| SignatureError::NotText)?;
    let folder = file.parent().unwrap_or(&root).to_owned();
    let images: RefCell<Vec<(String, SignatureImage)>> = RefCell::default();
    let fault: RefCell<Option<SignatureError>> = RefCell::default();
    let cleaned = clean_signature(&html, |source| {
        if fault.borrow().is_some() {
            return None;
        }
        if let Some((_, known)) = images.borrow().iter().find(|(name, _)| name == source) {
            return Some(known.content_id.clone());
        }
        match image(&folder, source, domain) {
            Ok(None) => None,
            Ok(Some(image)) => {
                if images.borrow().len() >= MAX_IMAGES {
                    fault.replace(Some(SignatureError::TooManyImages));
                    return None;
                }
                let id = image.content_id.clone();
                images.borrow_mut().push((source.to_owned(), image));
                Some(id)
            }
            Err(error) => {
                fault.replace(Some(error));
                None
            }
        }
    });
    if let Some(error) = fault.into_inner() {
        return Err(error);
    }
    let block = html_signature(&cleaned.html).ok_or(SignatureError::Empty)?;
    let images = images
        .into_inner()
        .into_iter()
        .map(|(_, image)| image)
        .filter(|image| cleaned.content_ids.contains(&image.content_id))
        .collect();
    Ok(LoadedSignature { block, images })
}

fn canonical(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

/// The image `source` names in `folder`; `None` for a source that names no file there (a web
/// address, `data:` or `cid:`), which the signature leaves out.
fn image(
    folder: &Path,
    source: &str,
    domain: &str,
) -> Result<Option<SignatureImage>, SignatureError> {
    let source = source.trim();
    let local = !source.is_empty()
        && !source.contains(':')
        && !source.starts_with('/')
        && !source.contains(['\\', '?', '#']);
    if !local {
        return Ok(None);
    }
    let name = source.rsplit('/').next().unwrap_or(source).to_owned();
    let fault = |fault| SignatureError::Image(name.clone(), fault);
    let steps_ok = source
        .split('/')
        .all(|step| !step.is_empty() && step != "..");
    if !steps_ok {
        return Err(fault(ImageFault::Outside));
    }
    let path = canonical(&folder.join(source)).map_err(|_| fault(ImageFault::Missing))?;
    if !path.starts_with(folder) {
        return Err(fault(ImageFault::Outside));
    }
    let size = fs::metadata(&path)
        .map_err(|_| fault(ImageFault::Missing))?
        .len();
    if size > MAX_IMAGE_BYTES {
        return Err(fault(ImageFault::TooLarge));
    }
    let data = fs::read(&path).map_err(|_| fault(ImageFault::Missing))?;
    let mime_type = image_type(&data).ok_or_else(|| fault(ImageFault::Kind))?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| SignatureError::Unreadable)?;
    let content_id = format!("{}@{domain}", ids::token(random));
    if !ids::is_header_safe(&content_id) {
        return Err(SignatureError::Unreadable);
    }
    Ok(Some(SignatureImage {
        content_id,
        name,
        mime_type,
        data,
    }))
}

/// The image type `data` is, by its first bytes.
fn image_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000";

    fn folder() -> (tempfile::TempDir, PathBuf) {
        let root = crate::store::unit_tests::scratch();
        let dir = dir(root.path());
        fs::create_dir_all(dir.join("work")).unwrap();
        (root, dir)
    }

    #[test]
    fn a_signature_with_a_logo_becomes_cleaned_html_and_an_inline_part() {
        let (_root, dir) = folder();
        fs::write(dir.join("work/logo.png"), PNG).unwrap();
        fs::write(
            dir.join("work/signature.html"),
            "<html><body><table><tr><td><img src=\"logo.png\" alt=\"Acme\"></td>\
             <td style=\"color:red\"><b>Ada Lovelace</b><br><a href=\"https://acme.example.test\">\
             acme.example.test</a><img src=\"https://tracker.example.test/p.gif\">\
             <script>alert(1)</script><img src=\"logo.png\"></td></tr></table></body></html>",
        )
        .unwrap();
        let loaded = load(&dir, "work/signature.html", "example.test").unwrap();
        assert_eq!(loaded.images.len(), 1, "one part for the logo shown twice");
        let image = &loaded.images[0];
        assert_eq!(
            (image.name.as_str(), image.mime_type),
            ("logo.png", "image/png")
        );
        assert!(image.content_id.ends_with("@example.test"));
        let html = &loaded.block.html;
        assert_eq!(
            html.matches(&format!("cid:{}", image.content_id)).count(),
            2
        );
        assert!(html.contains("<b>Ada Lovelace</b>"));
        assert!(html.contains("href=\"https://acme.example.test\""));
        for gone in [
            "tracker",
            "script",
            "alert",
            "style",
            "color:red",
            "logo.png\"",
        ] {
            assert!(!html.contains(gone), "{gone} in {html}");
        }
        assert!(loaded.block.text.starts_with("-- \n"));
        assert!(!format!("{loaded:?}").contains("Ada"));
    }

    #[test]
    fn what_cant_be_used_says_why() {
        let (root, dir) = folder();
        let write = |name: &str, text: &str| fs::write(dir.join(name), text).unwrap();
        assert_eq!(
            load(&dir, "work/none.html", "example.test").unwrap_err(),
            SignatureError::Missing
        );
        assert_eq!(
            load(&dir, "../outside.html", "example.test").unwrap_err(),
            SignatureError::Outside
        );
        fs::write(root.path().join("outside.html"), "<b>x</b>").unwrap();
        symlink(root.path().join("outside.html"), dir.join("work/link.html")).unwrap();
        assert_eq!(
            load(&dir, "work/link.html", "example.test").unwrap_err(),
            SignatureError::Outside,
            "a symlink out of the folder"
        );
        write("work/missing-image.html", "<img src=\"logo.png\"> Ada");
        let error = load(&dir, "work/missing-image.html", "example.test").unwrap_err();
        assert_eq!(
            error,
            SignatureError::Image("logo.png".into(), ImageFault::Missing)
        );
        assert_eq!(error.to_string(), "its image “logo.png” isn't there");
        fs::write(root.path().join("secret.png"), PNG).unwrap();
        write("work/escape.html", "<img src=\"../../secret.png\"> Ada");
        assert!(matches!(
            load(&dir, "work/escape.html", "example.test").unwrap_err(),
            SignatureError::Image(_, ImageFault::Outside)
        ));
        fs::write(dir.join("work/fake.png"), b"<svg onload=alert(1)>").unwrap();
        write("work/fake.html", "<img src=\"fake.png\"> Ada");
        assert!(matches!(
            load(&dir, "work/fake.html", "example.test").unwrap_err(),
            SignatureError::Image(_, ImageFault::Kind)
        ));
        fs::write(
            dir.join("work/big.html"),
            vec![b'a'; MAX_HTML_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(
            load(&dir, "work/big.html", "example.test").unwrap_err(),
            SignatureError::TooLarge
        );
        fs::write(dir.join("work/latin1.html"), [0x41, 0xe6, 0x42]).unwrap();
        assert_eq!(
            load(&dir, "work/latin1.html", "example.test").unwrap_err(),
            SignatureError::NotText
        );
        write("work/empty.html", "<script>only this</script>");
        assert_eq!(
            load(&dir, "work/empty.html", "example.test").unwrap_err(),
            SignatureError::Empty
        );
    }
}
