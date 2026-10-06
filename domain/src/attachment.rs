//! Attachment rules (ML-061; SECURITY.md "Attachments"): which attachments are programs,
//! launchers, scripts or installers and so are never opened directly, and the file names an
//! attachment is opened and saved under. Names are hostile: path separators, `..`, control and
//! invisible characters, direction overrides and look-alike dots never reach the file system
//! or the card, and a name that hides its real type counts as dangerous.

/// Why an attachment is never opened directly; Save is still offered, with a warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Danger {
    /// Its name or type says it is a program, a script, a launcher or an installer.
    Program,
    /// Its name hides its real type: a direction override, or a look-alike dot in front of
    /// what reads as its extension.
    Disguised,
}

/// Extensions that run code when the desktop opens them (SECURITY.md's list first): Linux
/// launchers, scripts and self-extracting installers; Linux, Android and Flatpak packages,
/// whose installers run their scripts; Java archives and Web Start files, which the Java
/// runtime runs; and Windows programs, scripts, shortcuts and installers, which Wine runs
/// where it is installed. Lower case, without the dot.
pub const DANGEROUS_EXTENSIONS: &[&str] = &[
    "desktop",
    "sh",
    "appimage",
    "jar",
    "exe",
    "bat",
    "ps1",
    "run",
    "bash",
    "zsh",
    "csh",
    "tcsh",
    "ksh",
    "fish",
    "bin",
    "deb",
    "rpm",
    "apk",
    "snap",
    "flatpak",
    "flatpakref",
    "flatpakrepo",
    "jnlp",
    "msi",
    "msp",
    "cmd",
    "com",
    "scr",
    "pif",
    "cpl",
    "lnk",
    "reg",
    "hta",
    "vbs",
    "vbe",
    "jse",
    "wsf",
    "wsh",
    "psm1",
];

/// MIME types of the same kinds (shared-mime-info's names and the common aliases senders
/// use), lower case.
pub const DANGEROUS_TYPES: &[&str] = &[
    "application/x-desktop",
    "application/x-executable",
    "application/x-pie-executable",
    "application/x-elf",
    "application/x-shellscript",
    "application/x-sh",
    "application/x-csh",
    "text/x-sh",
    "text/x-shellscript",
    "application/java-archive",
    "application/x-java-archive",
    "application/x-java-jnlp-file",
    "application/vnd.appimage",
    "application/x-appimage",
    "application/x-iso9660-appimage",
    "application/x-msdownload",
    "application/x-msdos-program",
    "application/x-ms-dos-executable",
    "application/x-dosexec",
    "application/vnd.microsoft.portable-executable",
    "application/x-msi",
    "application/x-ms-installer",
    "application/x-ms-shortcut",
    "application/x-bat",
    "text/x-msdos-batch",
    "application/hta",
    "application/vnd.debian.binary-package",
    "application/x-deb",
    "application/x-debian-package",
    "application/x-rpm",
    "application/x-redhat-package-manager",
    "application/vnd.android.package-archive",
    "application/vnd.flatpak",
    "application/vnd.flatpak.ref",
    "application/vnd.flatpak.repo",
    "application/vnd.snap",
];

/// The longest file name Sealwax writes, in bytes; Linux allows 255, and the rest leaves room
/// for " (12)" when a name is taken.
pub const MAX_NAME_BYTES: usize = 200;

/// The longest extension kept whole when a long name is shortened, in bytes.
const MAX_EXTENSION_BYTES: usize = 16;

/// Whether the desktop must never open this attachment, and why.
pub fn danger(name: Option<&str>, mime_type: &str) -> Option<Danger> {
    let base = last_component(name.unwrap_or_default());
    if base.chars().any(is_direction_control) || lookalike_extension(base) {
        return Some(Danger::Disguised);
    }
    let normalized = normalize(base);
    let program = extension(&normalized).is_some_and(|extension| {
        DANGEROUS_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
    }) || dangerous_type(mime_type);
    program.then_some(Danger::Program)
}

/// Whether a MIME type (parameters allowed) names a program, launcher, script or installer.
pub fn dangerous_type(mime_type: &str) -> bool {
    let essence = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    DANGEROUS_TYPES.contains(&essence.as_str())
}

/// The name an attachment is opened and saved under: its last path component, without control
/// or invisible characters, look-alike dots made real, no leading dot, dash or space, no
/// trailing dot or space, no `..`, at most [`MAX_NAME_BYTES`]; `attachment` with an extension
/// from its type when nothing is left.
pub fn safe_file_name(name: Option<&str>, mime_type: &str) -> String {
    let cleaned = clean(last_component(name.unwrap_or_default()));
    if cleaned.is_empty() {
        fallback_name(mime_type)
    } else {
        shorten(&cleaned, MAX_NAME_BYTES)
    }
}

/// The name a message is saved under as `.eml`: its subject, with path separators made dashes.
pub fn eml_file_name(subject: &str) -> String {
    let subject: String = subject
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect();
    let stem = clean(&subject);
    let stem = if stem.is_empty() {
        "message".to_owned()
    } else {
        stem
    };
    let stem = cut(&stem, MAX_NAME_BYTES - ".eml".len());
    let stem = stem.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    format!("{stem}.eml")
}

/// `name`, or the first of `name (2)`, `name (3)`… that `taken` doesn't hold, its extension
/// kept (`.tar.gz` too), within [`MAX_NAME_BYTES`].
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let (stem, extension) = split_for_numbering(name);
    let mut candidate = name.to_owned();
    for number in 2..=100_000u32 {
        let suffix = format!(" ({number})");
        let room = MAX_NAME_BYTES.saturating_sub(suffix.len() + extension.len());
        candidate = format!("{}{suffix}{extension}", cut(stem, room));
        if !taken(&candidate) {
            break;
        }
    }
    candidate
}

/// The extension a name ends in, without the dot: `pdf` of `a.pdf`, `desktop` of `.desktop`.
pub fn extension(name: &str) -> Option<&str> {
    name.rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| !extension.is_empty())
}

/// What follows the last `/` or `\`: a name that tries to climb out of its folder keeps only
/// its last part.
fn last_component(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or_default()
}

/// The name without control or invisible characters, look-alike dots made `.`, runs of dots
/// made one, and no trailing dot or space (which Windows drops, revealing the extension
/// before them).
fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control() || is_invisible(c) {
            continue;
        }
        let c = if is_lookalike_dot(c) { '.' } else { c };
        if c == '.' && out.ends_with('.') {
            continue;
        }
        out.push(c);
    }
    out.trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .to_owned()
}

/// [`normalize`]d, with no leading dot (a hidden file), dash (an option) or space either.
fn clean(name: &str) -> String {
    normalize(name)
        .trim_start_matches(|c: char| c == '.' || c == '-' || c.is_whitespace())
        .to_owned()
}

/// Bidirectional embeddings, overrides and isolates: they reorder what a name shows
/// (`invoice`, U+202E, `fdp.exe` reads as `invoiceexe.pdf`).
fn is_direction_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Characters that show nothing: direction marks and controls, zero-width characters, the
/// soft hyphen, variation selectors, the byte-order mark, interlinear annotation marks and
/// tags.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// Characters that read as a full stop in a file name.
fn is_lookalike_dot(c: char) -> bool {
    matches!(
        c,
        '\u{00B7}'
            | '\u{0387}'
            | '\u{06D4}'
            | '\u{0701}'
            | '\u{0702}'
            | '\u{2024}'
            | '\u{2027}'
            | '\u{2219}'
            | '\u{22C5}'
            | '\u{2E31}'
            | '\u{2E33}'
            | '\u{3002}'
            | '\u{30FB}'
            | '\u{FE52}'
            | '\u{FF0E}'
            | '\u{FF61}'
    )
}

/// A look-alike dot after the last real one, followed by one to eight ASCII letters or digits:
/// the name shows an extension the file system doesn't see (`photo․jpg`).
fn lookalike_extension(name: &str) -> bool {
    let visible: String = name.chars().filter(|c| !is_invisible(*c)).collect();
    let Some((at, dot)) = visible.char_indices().rfind(|(_, c)| is_lookalike_dot(*c)) else {
        return false;
    };
    if visible[at..].contains('.') {
        return false;
    }
    let after = &visible[at + dot.len_utf8()..];
    !after.is_empty() && after.len() <= 8 && after.chars().all(|c| c.is_ascii_alphanumeric())
}

/// `attachment`, with the extension of a common type.
fn fallback_name(mime_type: &str) -> String {
    let essence = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let extension = match essence.as_str() {
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/heic" => "heic",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/csv" => "csv",
        "text/calendar" => "ics",
        "text/vcard" | "text/x-vcard" => "vcf",
        "message/rfc822" => "eml",
        "application/zip" => "zip",
        "application/gzip" => "gz",
        "application/json" => "json",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "video/mp4" => "mp4",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/vnd.oasis.opendocument.text" => "odt",
        "application/vnd.oasis.opendocument.spreadsheet" => "ods",
        "application/vnd.oasis.opendocument.presentation" => "odp",
        _ => return "attachment".to_owned(),
    };
    format!("attachment.{extension}")
}

/// `name` within `max` bytes, its extension kept whole when it is short.
fn shorten(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_owned();
    }
    if let Some((stem, extension)) = name.rsplit_once('.')
        && !stem.is_empty()
        && extension.len() <= MAX_EXTENSION_BYTES
    {
        let stem = cut(stem, max - extension.len() - 1);
        let stem = stem.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
        return format!("{stem}.{extension}");
    }
    cut(name, max)
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .to_owned()
}

/// At most `max` bytes of `text`, cut at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The part a number goes before: `archive` and `.tar.gz`, `report` and `.pdf`, `notes` and
/// nothing.
fn split_for_numbering(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for compound in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(compound) && name.len() > compound.len() {
            let at = name.len() - compound.len();
            return (&name[..at], &name[at..]);
        }
    }
    match name.rfind('.') {
        Some(at) if at > 0 && name.len() - at <= MAX_EXTENSION_BYTES + 1 => {
            (&name[..at], &name[at..])
        }
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn danger_of(name: &str) -> Option<Danger> {
        danger(Some(name), "application/octet-stream")
    }

    #[test]
    fn security_md_s_list_is_dangerous() {
        for name in [
            "start.desktop",
            "install.sh",
            "Tool.AppImage",
            "app.jar",
            "setup.exe",
            "run.bat",
            "script.ps1",
            "installer.run",
        ] {
            assert_eq!(danger_of(name), Some(Danger::Program), "{name}");
        }
    }

    #[test]
    fn other_launchers_scripts_and_installers_are_dangerous() {
        for name in [
            "a.bash",
            "a.zsh",
            "a.csh",
            "a.tcsh",
            "a.ksh",
            "a.fish",
            "a.bin",
            "a.deb",
            "a.rpm",
            "a.apk",
            "a.snap",
            "a.flatpak",
            "a.flatpakref",
            "a.flatpakrepo",
            "a.jnlp",
            "a.msi",
            "a.msp",
            "a.cmd",
            "a.com",
            "a.scr",
            "a.pif",
            "a.cpl",
            "a.lnk",
            "a.reg",
            "a.hta",
            "a.vbs",
            "a.vbe",
            "a.jse",
            "a.wsf",
            "a.wsh",
            "a.psm1",
        ] {
            assert_eq!(danger_of(name), Some(Danger::Program), "{name}");
        }
    }

    #[test]
    fn documents_images_and_archives_are_not() {
        for name in [
            "Invoice-2041.pdf",
            "photo.JPG",
            "hours.csv",
            "notes.txt",
            "archive.tar.gz",
            "report.docx",
            "slides.odp",
            "message.eml",
            "invite.ics",
            "invoice.desktop.pdf",
            "shell.png",
            "sh",
            "desktop",
            "README",
            "",
        ] {
            assert_eq!(danger_of(name), None, "{name}");
        }
        assert_eq!(danger(None, "application/pdf"), None);
    }

    #[test]
    fn extensions_match_whatever_their_case_and_trailing_dots_or_spaces() {
        for name in [
            "SETUP.EXE",
            "Setup.Exe",
            "launcher.DeskTop",
            "run.sh.",
            "run.sh...",
            "run.sh ",
            "run.sh . .",
            "tool.appimage",
            ".desktop",
            "..sh",
        ] {
            assert_eq!(danger_of(name), Some(Danger::Program), "{name:?}");
        }
    }

    #[test]
    fn the_last_extension_decides_a_double_extension() {
        assert_eq!(danger_of("invoice.pdf.desktop"), Some(Danger::Program));
        assert_eq!(danger_of("photo.jpg.exe"), Some(Danger::Program));
        assert_eq!(danger_of("report.pdf.sh"), Some(Danger::Program));
        assert_eq!(danger_of("backup.sh.gz"), None);
    }

    #[test]
    fn a_path_in_the_name_is_ignored_for_the_check() {
        assert_eq!(danger_of("../../bin/evil.sh"), Some(Danger::Program));
        assert_eq!(danger_of("C:\\Users\\x\\setup.exe"), Some(Danger::Program));
        assert_eq!(danger_of("dir.sh/readme.txt"), None);
    }

    #[test]
    fn dangerous_types_count_whatever_the_name() {
        for mime in [
            "application/x-desktop",
            "application/x-executable",
            "application/x-shellscript",
            "application/java-archive",
            "application/vnd.appimage",
            "application/x-msdownload",
            "Application/X-MSDownload; name=\"a\"",
            "application/x-sh",
            "application/vnd.debian.binary-package",
            "application/vnd.android.package-archive",
            "application/vnd.flatpak.ref",
            "application/x-ms-shortcut",
        ] {
            assert_eq!(
                danger(Some("harmless.pdf"), mime),
                Some(Danger::Program),
                "{mime}"
            );
            assert_eq!(danger(None, mime), Some(Danger::Program), "{mime}");
            assert!(dangerous_type(mime), "{mime}");
        }
        for mime in ["application/pdf", "text/plain", "image/png", "", "garbage"] {
            assert!(!dangerous_type(mime), "{mime}");
        }
    }

    #[test]
    fn a_direction_override_disguises_a_name() {
        // Shows as "invoiceexe.pdf"; the file system sees ".pdf" reversed into "fdp.exe".
        assert_eq!(danger_of("invoice\u{202E}fdp.exe"), Some(Danger::Disguised));
        // Even when what it hides is harmless, the name lies about its type.
        assert_eq!(danger_of("summary\u{202E}fdp.txt"), Some(Danger::Disguised));
        for control in [
            '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{2066}', '\u{2069}',
        ] {
            assert_eq!(
                danger_of(&format!("a{control}b.pdf")),
                Some(Danger::Disguised),
                "U+{:04X}",
                control as u32
            );
        }
    }

    #[test]
    fn invisible_characters_do_not_hide_an_extension() {
        assert_eq!(danger_of("setup.e\u{200B}xe"), Some(Danger::Program));
        assert_eq!(danger_of("setup.\u{FEFF}exe"), Some(Danger::Program));
        assert_eq!(danger_of("setup.ex\u{00AD}e"), Some(Danger::Program));
        assert_eq!(danger_of("run\u{200D}.sh"), Some(Danger::Program));
        assert_eq!(danger_of("run.s\u{E0041}h"), Some(Danger::Program));
        // Marks reorder nothing visible in a plain name: no disguise.
        assert_eq!(danger_of("rapport\u{200F}.pdf"), None);
    }

    #[test]
    fn look_alike_dots_disguise_an_extension() {
        assert_eq!(danger_of("photo\u{2024}jpg"), Some(Danger::Disguised));
        assert_eq!(danger_of("invoice\u{FF0E}pdf"), Some(Danger::Disguised));
        assert_eq!(danger_of("setup\u{2024}exe"), Some(Danger::Disguised));
        assert_eq!(danger_of("invoice.pdf\u{2024}exe"), Some(Danger::Disguised));
        // Before a real extension, or not followed by one, they hide nothing.
        assert_eq!(danger_of("Q3\u{2024}final.pdf"), None);
        assert_eq!(danger_of("報告\u{3002}"), None);
        assert_eq!(danger_of("旅行\u{3002}写真"), None);
        assert_eq!(danger_of("Kari\u{00B7}Nordmann CV.pdf"), None);
    }

    #[test]
    fn safe_names_drop_paths_and_traversal() {
        assert_eq!(safe_file_name(Some("../../etc/passwd"), ""), "passwd");
        assert_eq!(safe_file_name(Some("..\\..\\boot.ini"), ""), "boot.ini");
        assert_eq!(safe_file_name(Some("/abs/path/a.pdf"), ""), "a.pdf");
        assert_eq!(
            safe_file_name(Some(".."), "application/pdf"),
            "attachment.pdf"
        );
        assert_eq!(safe_file_name(Some("dir/"), "text/plain"), "attachment.txt");
        assert_eq!(safe_file_name(Some("report..pdf"), ""), "report.pdf");
        assert_eq!(safe_file_name(Some("a....b"), ""), "a.b");
        let name = safe_file_name(Some("x/..\\../y/../../.."), "");
        assert!(!name.contains("..") && !name.contains('/'), "{name}");
    }

    #[test]
    fn safe_names_have_no_leading_dot_dash_or_space_and_no_trailing_dot() {
        assert_eq!(safe_file_name(Some(".bashrc"), ""), "bashrc");
        assert_eq!(safe_file_name(Some("-rf"), ""), "rf");
        assert_eq!(safe_file_name(Some("--help.txt"), ""), "help.txt");
        assert_eq!(safe_file_name(Some("  . -x.pdf"), ""), "x.pdf");
        assert_eq!(safe_file_name(Some("invoice.pdf. . "), ""), "invoice.pdf");
        assert_eq!(safe_file_name(Some("a b.pdf"), ""), "a b.pdf");
    }

    #[test]
    fn safe_names_drop_control_and_invisible_characters() {
        assert_eq!(safe_file_name(Some("a\nb\r\tc\0.txt"), ""), "abc.txt");
        assert_eq!(
            safe_file_name(Some("in\u{1B}[31mvoice.pdf"), ""),
            "in[31mvoice.pdf"
        );
        assert_eq!(
            safe_file_name(Some("invoice\u{202E}fdp.exe"), ""),
            "invoicefdp.exe"
        );
        assert_eq!(
            safe_file_name(Some("set\u{200B}up.e\u{FEFF}xe"), ""),
            "setup.exe"
        );
        assert_eq!(safe_file_name(Some("photo\u{2024}jpg"), ""), "photo.jpg");
        assert_eq!(
            safe_file_name(Some("Åse Nygård — plan.pdf"), ""),
            "Åse Nygård — plan.pdf"
        );
        assert_eq!(safe_file_name(Some("報告書.pdf"), ""), "報告書.pdf");
    }

    #[test]
    fn a_missing_or_empty_name_takes_one_from_the_type() {
        assert_eq!(safe_file_name(None, "application/pdf"), "attachment.pdf");
        assert_eq!(safe_file_name(Some(""), "image/jpeg"), "attachment.jpg");
        assert_eq!(
            safe_file_name(Some("\u{200B}"), "text/calendar"),
            "attachment.ics"
        );
        assert_eq!(
            safe_file_name(None, "Message/RFC822; charset=utf-8"),
            "attachment.eml"
        );
        assert_eq!(safe_file_name(None, "application/x-desktop"), "attachment");
        assert_eq!(
            safe_file_name(None, "application/octet-stream"),
            "attachment"
        );
        assert_eq!(safe_file_name(None, ""), "attachment");
    }

    #[test]
    fn long_names_are_cut_keeping_the_extension() {
        let long = format!("{}.pdf", "a".repeat(400));
        let name = safe_file_name(Some(&long), "");
        assert_eq!(name.len(), MAX_NAME_BYTES);
        assert!(name.ends_with(".pdf"));
        let wide = format!("{}.pdf", "æ".repeat(300));
        let name = safe_file_name(Some(&wide), "");
        assert!(
            name.len() <= MAX_NAME_BYTES && name.ends_with(".pdf"),
            "{}",
            name.len()
        );
        let odd = format!("{}.{}", "b".repeat(300), "x".repeat(40));
        assert_eq!(safe_file_name(Some(&odd), "").len(), MAX_NAME_BYTES);
        let none = "c".repeat(300);
        assert_eq!(safe_file_name(Some(&none), "").len(), MAX_NAME_BYTES);
    }

    #[test]
    fn unique_names_number_before_the_extension() {
        let taken: HashSet<&str> = ["report.pdf", "report (2).pdf", "notes", "data.tar.gz"]
            .into_iter()
            .collect();
        let free = |name: &str| taken.contains(name);
        assert_eq!(unique_name("other.pdf", free), "other.pdf");
        assert_eq!(unique_name("report.pdf", free), "report (3).pdf");
        assert_eq!(unique_name("notes", free), "notes (2)");
        assert_eq!(unique_name("data.tar.gz", free), "data (2).tar.gz");
        let long = format!("{}.pdf", "a".repeat(MAX_NAME_BYTES - 4));
        let numbered = unique_name(&long, |name| name == long);
        assert!(numbered.len() <= MAX_NAME_BYTES, "{}", numbered.len());
        assert!(numbered.ends_with(" (2).pdf"), "{numbered}");
    }

    #[test]
    fn unique_names_never_repeat_in_one_folder() {
        let mut folder: HashSet<String> = HashSet::new();
        for _ in 0..25 {
            let name = unique_name("scan.png", |name| folder.contains(name));
            assert!(folder.insert(name));
        }
        assert!(folder.contains("scan (25).png"));
    }

    #[test]
    fn eml_names_come_from_the_subject() {
        assert_eq!(eml_file_name("Invoice #2041"), "Invoice #2041.eml");
        assert_eq!(eml_file_name("Re: Q3/Q4 plan"), "Re: Q3-Q4 plan.eml");
        assert_eq!(eml_file_name("..\\..\\evil"), "evil.eml");
        assert_eq!(eml_file_name(""), "message.eml");
        assert_eq!(eml_file_name("  \u{202E}  "), "message.eml");
        assert_eq!(eml_file_name(". hidden"), "hidden.eml");
        let long = eml_file_name(&"x".repeat(500));
        assert_eq!(long.len(), MAX_NAME_BYTES);
        assert!(long.ends_with(".eml"));
    }

    proptest::proptest! {
        #[test]
        fn any_name_becomes_a_safe_one(name in "\\PC{0,300}", mime in "[a-z/.+-]{0,30}") {
            for safe in [safe_file_name(Some(&name), &mime), eml_file_name(&name)] {
                proptest::prop_assert!(!safe.is_empty());
                proptest::prop_assert!(safe.len() <= MAX_NAME_BYTES, "{}", safe.len());
                proptest::prop_assert!(!safe.contains(['/', '\\']), "{safe:?}");
                proptest::prop_assert!(!safe.contains(".."), "{safe:?}");
                proptest::prop_assert!(!safe.starts_with(['.', '-']), "{safe:?}");
                proptest::prop_assert!(!safe.chars().any(|c| c.is_control() || is_invisible(c)));
                proptest::prop_assert!(!safe.ends_with(|c: char| c.is_whitespace()));
            }
        }
    }

    #[test]
    fn extensions_are_what_follows_the_last_dot() {
        assert_eq!(extension("a.pdf"), Some("pdf"));
        assert_eq!(extension(".desktop"), Some("desktop"));
        assert_eq!(extension("a.tar.gz"), Some("gz"));
        assert_eq!(extension("README"), None);
        assert_eq!(extension("trailing."), None);
    }
}
