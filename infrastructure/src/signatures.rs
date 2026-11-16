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
    /// A designed signature's HTML with its styles, made by Sealwax from the form's fields, its
    /// images by the same Content-IDs; `None` for a file written or edited by hand.
    pub styled: Option<String>,
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
    let images = images.into_inner();
    // A designed signature keeps its styles only while the file is exactly what the form made
    // of its fields: then the styled HTML is Sealwax's own, never the file's.
    let styled = read_template(&root, relative)
        .filter(|template| template_html(template) == html)
        .map(|template| {
            designed_html(&template, &|name| {
                images
                    .iter()
                    .find(|(source, _)| source == name)
                    .map(|(_, image)| format!("cid:{}", image.content_id))
                    .unwrap_or_default()
            })
        });
    let images = images
        .into_iter()
        .map(|(_, image)| image)
        .filter(|image| cleaned.content_ids.contains(&image.content_id))
        .collect();
    Ok(LoadedSignature {
        block,
        images,
        styled,
    })
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

/// A signature made with Sealwax's **Signature…** form (1.5.0): what the form asks for. It is
/// kept beside the HTML file as `<name>.json`, so the form opens with it again, and the HTML
/// file is made from it ([`template_html`]): a designed signature in Sealwax's style, whose
/// styles reach the recipient only while the file is exactly what the form wrote ([`load`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    pub title: String,
    pub company: String,
    pub address: String,
    pub phone: String,
    pub email: String,
    pub website: String,
    /// The social links, by [`SOCIAL`]'s keys, in its order.
    pub social: Vec<(String, String)>,
    /// The picture is a photo, shown round; else a logo, shown as it is.
    pub round: bool,
    /// The picture's file name beside the HTML file.
    pub logo: Option<String>,
}

/// The longest field the form keeps, in characters.
pub const FIELD_MAX: usize = 200;

/// The social links the form offers: key and name; the icon is `<key>.png` in [`ICONS`].
pub const SOCIAL: [(&str, &str); 6] = [
    ("linkedin", "LinkedIn"),
    ("x", "X"),
    ("github", "GitHub"),
    ("instagram", "Instagram"),
    ("facebook", "Facebook"),
    ("youtube", "YouTube"),
];

/// The signature's icons, in one grey ([`MUTED`]) on transparent, readable on light and dark
/// mail: the contact rows' and the social links' (`assets/signature/SOURCES.md`).
pub const ICONS: [(&str, &[u8]); 10] = [
    ("place", include_bytes!("../assets/signature/place.png")),
    ("phone", include_bytes!("../assets/signature/phone.png")),
    ("mail", include_bytes!("../assets/signature/mail.png")),
    ("web", include_bytes!("../assets/signature/web.png")),
    (
        "linkedin",
        include_bytes!("../assets/signature/linkedin.png"),
    ),
    ("x", include_bytes!("../assets/signature/x.png")),
    ("github", include_bytes!("../assets/signature/github.png")),
    (
        "instagram",
        include_bytes!("../assets/signature/instagram.png"),
    ),
    (
        "facebook",
        include_bytes!("../assets/signature/facebook.png"),
    ),
    ("youtube", include_bytes!("../assets/signature/youtube.png")),
];

/// The one colour the signature sets: the title, the company, the line and the icons. A grey
/// that reads on white and on near-black; every other text takes the reader's own colour, so
/// the signature follows the mail client's light or dark look.
pub const MUTED: &str = "#8a8a8a";

/// An icon's file name beside the signature.
pub fn icon_file(icon: &str) -> String {
    format!("sealwax-{icon}.png")
}

/// What Save does with the picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogoChange {
    Keep,
    /// A new picture's bytes, checked with [`check_logo`].
    Replace(Vec<u8>),
    Remove,
}

/// Whether `data` can be a signature's picture: its type, or why not.
pub fn check_logo(data: &[u8]) -> Result<&'static str, ImageFault> {
    if data.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ImageFault::TooLarge);
    }
    image_type(data).ok_or(ImageFault::Kind)
}

/// The form's file name for `address`: lower case, letters, digits and `@ . _ -` kept, anything
/// else `_`, at most 100 characters.
pub fn file_stem(address: &str) -> String {
    let stem: String = address
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "@._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let stem = stem.trim_start_matches('.');
    if stem.is_empty() {
        "signature".to_owned()
    } else {
        stem.to_owned()
    }
}

/// The path, in the signatures folder, of the form's signature for `address` of the account
/// `key`: `<key>/<address>.html`.
pub fn template_path(key: &sealwax_domain::account::AccountKey, address: &str) -> String {
    format!("{}/{}.html", key.as_str(), file_stem(address))
}

/// A website as a link: `https://` added when it names no scheme; `None` for one that isn't
/// a web address.
pub fn website_link(website: &str) -> Option<String> {
    let website = website.trim();
    if website.is_empty()
        || website.contains(char::is_whitespace)
        || website.contains(['"', '<', '>'])
    {
        return None;
    }
    let lower = website.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Some(website.to_owned())
    } else if website.contains("://") || website.contains(':') {
        None
    } else {
        Some(format!("https://{website}"))
    }
}

fn escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

fn field(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(FIELD_MAX)
        .collect()
}

const FONT: &str = "font-family:'Helvetica Neue',Helvetica,Arial,sans-serif";

/// The designed signature's HTML: the picture at the left (round for a photo); the name in
/// bold, the title and the company in grey, the social links as their icons; a thin grey line;
/// then the address, phone, email and website, each after its icon. No text colour but the grey,
/// and no background, so it reads in light and dark mail. Tables and inline styles, as mail
/// clients need. `source` gives each image's `src` from its file name: the file name itself in
/// the HTML file, a `cid:` when it is sent, a `data:` address in the form's preview.
pub fn designed_html(template: &Template, source: &dyn Fn(&str) -> String) -> String {
    let name = field(&template.name);
    let mut middle = String::new();
    if !name.is_empty() {
        middle.push_str(&format!(
            "<div style=\"{FONT};font-size:20px;line-height:26px;font-weight:bold\">{}</div>",
            escape(&name)
        ));
    }
    for text in [field(&template.title), field(&template.company)] {
        if !text.is_empty() {
            middle.push_str(&format!(
                "<div style=\"{FONT};font-size:13px;line-height:20px;color:{MUTED}\">{}</div>",
                escape(&text)
            ));
        }
    }
    let social: Vec<String> = SOCIAL
        .iter()
        .filter_map(|(key, label)| {
            let (_, url) = template.social.iter().find(|(known, _)| known == key)?;
            let link = website_link(&field(url))?;
            Some(format!(
                "<td style=\"padding:0 10px 0 0\"><a href=\"{}\" title=\"{label}\" \
                 style=\"text-decoration:none\"><img src=\"{}\" width=\"18\" height=\"18\" \
                 alt=\"{label}\" style=\"display:block;border:0;width:18px;height:18px\"></a></td>",
                escape(&link),
                escape(&source(&icon_file(key)))
            ))
        })
        .collect();
    if !social.is_empty() {
        middle.push_str(&format!(
            "<table cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin-top:10px\">\
             <tr>{}</tr></table>",
            social.concat()
        ));
    }
    let row = |icon: &str, content: String| {
        format!(
            "<tr><td width=\"16\" style=\"width:16px;padding:3px 0;vertical-align:middle\">\
             <img src=\"{}\" width=\"16\" height=\"16\" alt=\"\" style=\"display:block;\
             border:0;width:16px;height:16px\"></td><td style=\"padding:3px 0 3px 10px;{FONT};\
             font-size:13px;line-height:20px;vertical-align:middle\">{content}</td></tr>",
            escape(&source(&icon_file(icon)))
        )
    };
    let mut contact = String::new();
    let address = field(&template.address);
    if !address.is_empty() {
        contact.push_str(&row("place", escape(&address)));
    }
    let phone = field(&template.phone);
    if !phone.is_empty() {
        contact.push_str(&row("phone", escape(&phone)));
    }
    let email = field(&template.email);
    if sealwax_domain::address::looks_like_address(&email) && !email.contains(['"', '<', '>', '?'])
    {
        contact.push_str(&row(
            "mail",
            format!(
                "<a href=\"mailto:{0}\" style=\"text-decoration:none\">{0}</a>",
                escape(&email)
            ),
        ));
    }
    let website = field(&template.website);
    if let Some(link) = website_link(&website) {
        let shown = website
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        contact.push_str(&row(
            "web",
            format!(
                "<a href=\"{}\" style=\"text-decoration:none\">{}</a>",
                escape(&link),
                escape(shown)
            ),
        ));
    }
    let mut cells = String::new();
    if let Some(logo) = &template.logo {
        let company = field(&template.company);
        let picture = if template.round {
            format!(
                "<img src=\"{}\" width=\"88\" height=\"88\" alt=\"{}\" \
                 style=\"display:block;border:0;width:88px;height:88px;border-radius:44px\">",
                escape(&source(logo)),
                escape(&name)
            )
        } else {
            format!(
                "<img src=\"{}\" height=\"64\" alt=\"{}\" \
                 style=\"display:block;border:0;height:64px\">",
                escape(&source(logo)),
                escape(if company.is_empty() { &name } else { &company })
            )
        };
        cells.push_str(&format!(
            "<td style=\"padding:0 20px 0 0;vertical-align:middle\">{picture}</td>"
        ));
    }
    if !middle.is_empty() {
        cells.push_str(&format!(
            "<td style=\"padding:0 20px 0 0;vertical-align:middle\">{middle}</td>"
        ));
    }
    if !contact.is_empty() {
        let line = if middle.is_empty() && template.logo.is_none() {
            String::new()
        } else {
            format!("border-left:1px solid {MUTED};padding:0 0 0 20px;")
        };
        cells.push_str(&format!(
            "<td style=\"{line}vertical-align:middle\"><table cellpadding=\"0\" cellspacing=\"0\" \
             border=\"0\">{contact}</table></td>"
        ));
    }
    format!(
        "<table cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"border-collapse:collapse\">\
         <tr>{cells}</tr></table>"
    )
}

/// The HTML file the form writes: a note, then [`designed_html`] with each image by its file
/// name beside it.
pub fn template_html(template: &Template) -> String {
    format!(
        "<!-- Made with Sealwax's Signature… form (right-click the account in the sidebar). \
         Sealwax sends it with its styles while it is as the form wrote it; edited by hand, it \
         is sent as plain formatting. -->\n{}\n",
        designed_html(template, &|name| name.to_owned())
    )
}

/// The form's fields of the signature `relative` names, when the form made it (its `.json`
/// beside it); `None` for a file written by hand or one that isn't there.
pub fn read_template(dir: &Path, relative: &str) -> Option<Template> {
    let html = relative.strip_suffix(".html")?;
    if !is_signature_path(relative) {
        return None;
    }
    let root = canonical(dir).ok()?;
    let sidecar = canonical(&root.join(format!("{html}.json"))).ok()?;
    if !sidecar.starts_with(&root) {
        return None;
    }
    let text = fs::read_to_string(&sidecar).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    if value.get("version")?.as_u64()? != 1 {
        return None;
    }
    let text = |name: &str| {
        field(
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        )
    };
    let logo = value
        .get("logo")
        .and_then(serde_json::Value::as_str)
        .filter(|logo| !logo.is_empty() && !logo.contains(['/', '\\', '"', '<', '>']))
        .map(str::to_owned);
    let social = SOCIAL
        .iter()
        .filter_map(|(key, _)| {
            let url = value.get("social")?.get(*key)?.as_str().map(field)?;
            (!url.is_empty()).then(|| ((*key).to_owned(), url))
        })
        .collect();
    Some(Template {
        name: text("name"),
        title: text("title"),
        company: text("company"),
        address: text("address"),
        phone: text("phone"),
        email: text("email"),
        website: text("website"),
        social,
        round: value
            .get("round")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        logo,
    })
}

/// Writes the form's signature for `address` of the account `key` into the signatures folder
/// `dir`: `<key>/<address>.html`, its fields as `<address>.json`, its picture as
/// `<address>-logo.<png|jpg|gif>` (a replaced or removed picture's file goes) and the contact
/// icons as `sealwax-<icon>.png`. Gives the path for `signature_html`. The folders are 0700,
/// the files 0600.
pub fn save_template(
    dir: &Path,
    key: &sealwax_domain::account::AccountKey,
    address: &str,
    template: &Template,
    logo: LogoChange,
) -> io::Result<String> {
    let folder = dir.join(key.as_str());
    crate::xdg::ensure_private_dir(dir)?;
    crate::xdg::ensure_private_dir(&folder)?;
    let stem = file_stem(address);
    let logos: Vec<String> = ["png", "jpg", "gif"]
        .iter()
        .map(|extension| format!("{stem}-logo.{extension}"))
        .collect();
    let current = logos
        .iter()
        .find(|name| folder.join(name).is_file())
        .cloned();
    let logo = match logo {
        LogoChange::Keep => current,
        LogoChange::Remove | LogoChange::Replace(_) => {
            for name in &logos {
                match fs::remove_file(folder.join(name)) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            }
            match logo {
                LogoChange::Replace(data) => {
                    let extension = match check_logo(&data) {
                        Ok("image/png") => "png",
                        Ok("image/jpeg") => "jpg",
                        Ok(_) => "gif",
                        Err(_) => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "the picture isn't a PNG, JPEG or GIF of at most 1 MB",
                            ));
                        }
                    };
                    let name = format!("{stem}-logo.{extension}");
                    write_private(&folder.join(&name), &data)?;
                    Some(name)
                }
                _ => None,
            }
        }
    };
    for (icon, data) in ICONS {
        let path = folder.join(icon_file(icon));
        if fs::read(&path).ok().as_deref() != Some(data) {
            write_private(&path, data)?;
        }
    }
    let template = Template {
        logo,
        ..template.clone()
    };
    let social: serde_json::Map<String, serde_json::Value> = template
        .social
        .iter()
        .filter(|(key, url)| SOCIAL.iter().any(|(known, _)| known == key) && !field(url).is_empty())
        .map(|(key, url)| (key.clone(), serde_json::Value::from(field(url))))
        .collect();
    let json = serde_json::json!({
        "version": 1,
        "name": field(&template.name),
        "title": field(&template.title),
        "company": field(&template.company),
        "address": field(&template.address),
        "phone": field(&template.phone),
        "email": field(&template.email),
        "website": field(&template.website),
        "social": social,
        "round": template.round,
        "logo": template.logo.clone().unwrap_or_default(),
    });
    crate::accounts::replace(&folder.join(format!("{stem}.json")), &format!("{json}\n"))?;
    crate::accounts::replace(
        &folder.join(format!("{stem}.html")),
        &template_html(&template),
    )?;
    Ok(template_path(key, address))
}

/// `data` in a new file at `path`, 0600, replacing what was there.
fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension("part");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
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

    #[test]
    fn the_form_writes_a_designed_signature_that_loads_and_opens_again() {
        let (_root, dir) = folder();
        let key = sealwax_domain::account::AccountKey::parse("work").unwrap();
        let template = Template {
            name: "Ada <Lovelace>".into(),
            title: "Analyst".into(),
            company: "Analytical Engines".into(),
            address: "Storgata 1, Oslo".into(),
            phone: "+47 22 00 00 00".into(),
            email: "ada@example.test".into(),
            website: "example.test/ada".into(),
            social: vec![
                ("linkedin".into(), "linkedin.com/in/ada".into()),
                ("github".into(), "javascript:alert(1)".into()),
            ],
            round: true,
            logo: None,
        };
        let path = save_template(
            &dir,
            &key,
            "Ada@Example.test",
            &template,
            LogoChange::Replace(PNG.to_vec()),
        )
        .unwrap();
        assert_eq!(path, "work/ada@example.test.html");
        assert!(dir.join("work/ada@example.test-logo.png").is_file());
        assert!(dir.join("work/sealwax-phone.png").is_file());
        let loaded = load(&dir, &path, "example.test").unwrap();
        assert_eq!(
            loaded.images.len(),
            6,
            "the photo, four contact icons and LinkedIn's"
        );
        let styled = loaded
            .styled
            .as_deref()
            .expect("the form's own file keeps its styles");
        assert!(styled.contains("border-radius:44px"), "a round photo");
        assert!(
            styled.contains("font-weight:bold\">Ada &lt;Lovelace&gt;</div>"),
            "{styled}"
        );
        let colours: Vec<&str> = styled
            .match_indices("color:")
            .map(|(at, _)| &styled[at..at + 13])
            .collect();
        assert!(
            colours.iter().all(|colour| *colour == "color:#8a8a8a"),
            "{colours:?}"
        );
        assert!(!styled.contains("background"), "nothing on a background");
        assert!(styled.contains("href=\"https://linkedin.com/in/ada\""));
        assert!(!styled.contains("javascript"), "only web links");
        assert!(styled.contains("mailto:ada@example.test"));
        for image in &loaded.images {
            assert!(styled.contains(&format!("cid:{}", image.content_id)));
        }
        assert!(!styled.contains(".png"), "no file name is sent");
        let read = read_template(&dir, &path).unwrap();
        assert_eq!(read.name, "Ada <Lovelace>");
        assert!(read.round);
        assert_eq!(read.social.len(), 2);
        assert_eq!(read.logo.as_deref(), Some("ada@example.test-logo.png"));
        // Edited by hand, the file is plain formatting again.
        let file = dir.join(&path);
        let edited = fs::read_to_string(&file)
            .unwrap()
            .replace("Analyst", "Engineer");
        fs::write(&file, edited).unwrap();
        let loaded = load(&dir, &path, "example.test").unwrap();
        assert_eq!(loaded.styled, None);
        assert!(loaded.block.html.contains("Engineer"));
        // Saved again without a picture: its file goes.
        save_template(&dir, &key, "ada@example.test", &read, LogoChange::Remove).unwrap();
        assert!(!dir.join("work/ada@example.test-logo.png").exists());
        let loaded = load(&dir, &path, "example.test").unwrap();
        assert_eq!(loaded.images.len(), 5);
        assert!(loaded.styled.is_some());
        assert_eq!(read_template(&dir, &path).unwrap().logo, None);
        assert_eq!(read_template(&dir, "work/none.html"), None);
        assert!(
            save_template(
                &dir,
                &key,
                "a@example.test",
                &read,
                LogoChange::Replace(b"no".to_vec())
            )
            .is_err()
        );
        assert_eq!(file_stem("../Ola Nordmann@x"), "_ola_nordmann@x");
        assert_eq!(website_link("javascript:alert(1)"), None);
        assert_eq!(
            website_link("http://a.test").as_deref(),
            Some("http://a.test")
        );
        assert_eq!(website_link("a.test\"onclick=x"), None);
    }
}
