use crate::mime::{self, Attachment, Draft, InlineImage, Mailbox, Outgoing};
use crate::roundtrip::{self, Copy, Expected};
use crate::{ids, png};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// 2026-10-04 12:00:00 UTC, so fixture dates don't change between runs.
pub const FIXTURE_DATE: i64 = 1_791_115_200;

pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

pub fn sender() -> Mailbox {
    Mailbox::new("Kari Nordmann", "kari@example.com")
}

pub fn recipients() -> (Vec<Mailbox>, Vec<Mailbox>, Vec<Mailbox>) {
    (
        vec![
            Mailbox::new("Ola Nordmann", "ola@example.org"),
            Mailbox::new("Bjørn Ærø 🙂", "bjorn@example.net"),
        ],
        vec![Mailbox::new("Øystein Åsen", "oystein@example.org")],
        vec![
            Mailbox::new("Blind Kopi", "blind@example.net"),
            Mailbox::new("Hemmelig Mottaker", "hemmelig@example.com"),
        ],
    )
}

pub const NORWEGIAN_SUBJECT: &str = "Blåbærsyltetøy 🫐 på brødskiva — møte i morgen?";

pub fn sample_pdf() -> Vec<u8> {
    let content = "BT /F1 18 Tf 60 760 Td (S7 MIME fixture) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

pub fn sample_attachments() -> Vec<Attachment> {
    vec![
        Attachment {
            filename: "Årsrapport for blåbær 🫐 2026.pdf".into(),
            mime_type: "application/pdf".into(),
            data: sample_pdf(),
        },
        Attachment {
            filename: "notater æøå.txt".into(),
            mime_type: "text/plain".into(),
            data: "Notater: blåbær, multebær og tyttebær.\n"
                .as_bytes()
                .to_vec(),
        },
    ]
}

fn full_draft() -> Draft {
    let (to, cc, bcc) = recipients();
    let content_id = format!("{}@example.com", ids::token());
    let html_fragment = format!(
        "<div>Hei Ola,</div>\n<div><br></div>\n\
         <div>Dette er <b>fet</b>, <i>kursiv</i>, <u>understreket</u> og \
         <strike>gjennomstreket</strike> tekst med æøå, ÆØÅ og 🫐.</div>\n\
         <ul><li>Første punkt</li><li>Andre punkt</li></ul>\n\
         <ol><li>Én</li><li>To</li></ol>\n\
         <div>Lenke: <a href=\"https://example.org/bl%C3%A5b%C3%A6r?a=1&amp;b=2\">example.org</a></div>\n\
         <blockquote type=\"cite\" style=\"margin:0 0 0 0.8ex;border-left:2px solid #8a8a8a;padding-left:1ex\">\
         <div>Sitert tekst fra forrige melding.</div></blockquote>\n\
         <blockquote style=\"margin: 0 0 0 40px; border: none; padding: 0px;\">\
         <div>Innrykket avsnitt.</div></blockquote>\n\
         <div><img src=\"cid:{content_id}\" alt=\"Blåbær\" width=\"120\" height=\"75\"></div>\n\
         <div>Hilsen Kari</div>"
    );
    Draft {
        from: sender(),
        to,
        cc,
        bcc,
        subject: NORWEGIAN_SUBJECT.into(),
        html_fragment,
        inline_images: vec![InlineImage {
            content_id,
            mime_type: "image/png".into(),
            filename: Some("blåbær.png".into()),
            data: png::sample_image(120, 75),
        }],
        attachments: sample_attachments(),
    }
}

fn plain_draft() -> Draft {
    Draft {
        from: sender(),
        to: vec![Mailbox::new("Ola Nordmann", "ola@example.org")],
        cc: Vec::new(),
        bcc: vec![Mailbox::new("Blind Kopi", "blind@example.net")],
        subject: "Plain alternative test".into(),
        html_fragment: "<div>Hello <b>world</b>, no images and no attachments.</div>".into(),
        inline_images: Vec::new(),
        attachments: Vec::new(),
    }
}

fn bcc_only_draft() -> Draft {
    Draft {
        from: sender(),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: recipients().2,
        subject: "Kun blindkopi".into(),
        html_fragment: "<div>Only Bcc recipients: the transmitted copy says \
                        undisclosed-recipients.</div>"
            .into(),
        inline_images: Vec::new(),
        attachments: Vec::new(),
    }
}

fn long_draft() -> Draft {
    let long_url = format!("https://example.org/{}", "a1b2c3d4e5".repeat(150));
    Draft {
        from: sender(),
        to: vec![Mailbox::new("Ola Nordmann", "ola@example.org")],
        cc: Vec::new(),
        bcc: vec![Mailbox::new("Blind Kopi", "blind@example.net")],
        subject: format!("{NORWEGIAN_SUBJECT} {NORWEGIAN_SUBJECT} {NORWEGIAN_SUBJECT}"),
        html_fragment: format!(
            "<div>A line longer than 998 octets without spaces: <a href=\"{long_url}\">{long_url}</a></div>"
        ),
        inline_images: Vec::new(),
        attachments: Vec::new(),
    }
}

pub fn samples() -> Vec<(&'static str, Draft)> {
    vec![
        ("full", full_draft()),
        ("plain", plain_draft()),
        ("bcc-only", bcc_only_draft()),
        ("long", long_draft()),
    ]
}

pub fn write_pair(name: &str, outgoing: &Outgoing) -> std::io::Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(dir())?;
    let transmitted = dir().join(format!("{name}.transmitted.eml"));
    let sent = dir().join(format!("{name}.sent-copy.eml"));
    std::fs::write(&transmitted, &outgoing.transmitted)?;
    std::fs::write(&sent, &outgoing.sent_copy)?;
    Ok((transmitted, sent))
}

pub fn check_pair(name: &str, draft: &Draft, outgoing: &Outgoing) -> bool {
    let expected = Expected { draft, outgoing };
    let transmitted = roundtrip::check(&outgoing.transmitted, Copy::Transmitted, Some(&expected));
    let sent = roundtrip::check(&outgoing.sent_copy, Copy::SentCopy, Some(&expected));
    let first = roundtrip::print(&format!("{name}.transmitted.eml"), &transmitted);
    let second = roundtrip::print(&format!("{name}.sent-copy.eml"), &sent);
    first && second
}

pub fn print_header_difference(outgoing: &Outgoing) {
    let transmitted = mime::header_block(&outgoing.transmitted);
    let sent = mime::header_block(&outgoing.sent_copy);
    let lines: Vec<&str> = transmitted.split("\r\n").collect();
    println!("Header lines only in the Sent copy:");
    for line in sent.split("\r\n").filter(|line| !lines.contains(line)) {
        println!("  + {line}");
    }
    println!(
        "Bytes after the header block identical: {}",
        outgoing.transmitted[transmitted.len()..] == outgoing.sent_copy[sent.len()..]
    );
}

pub fn run() -> ExitCode {
    let mut ok = true;
    for (name, draft) in samples() {
        let outgoing = match mime::build(&draft, FIXTURE_DATE) {
            Ok(outgoing) => outgoing,
            Err(error) => {
                eprintln!("{name}: build failed: {error}");
                return ExitCode::FAILURE;
            }
        };
        match write_pair(name, &outgoing) {
            Ok((transmitted, sent)) => println!(
                "wrote {} ({} bytes) and {} ({} bytes)",
                transmitted.display(),
                outgoing.transmitted.len(),
                sent.display(),
                outgoing.sent_copy.len()
            ),
            Err(error) => {
                eprintln!("{name}: writing fixtures failed: {error}");
                return ExitCode::FAILURE;
            }
        }
        ok &= check_pair(name, &draft, &outgoing);
        if name == "full" {
            print_header_difference(&outgoing);
            println!(
                "Envelope recipients (RCPT TO): {}",
                outgoing.envelope_recipients.len()
            );
        }
    }
    ok &= check_files_on_disk();
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Generic checks (structure, linkage, Bcc, hostname, encoding) over every .eml in fixtures/.
pub fn check_files_on_disk() -> bool {
    let Ok(entries) = std::fs::read_dir(dir()) else {
        return true;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "eml"))
        .collect();
    paths.sort();
    let mut ok = true;
    for path in paths {
        let Ok(raw) = std::fs::read(&path) else {
            ok = false;
            continue;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let copy = if name.contains(".sent-copy.") {
            Copy::SentCopy
        } else {
            Copy::Transmitted
        };
        ok &= roundtrip::print(
            &format!("{name} (generic checks)"),
            &roundtrip::check(&raw, copy, None),
        );
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_round_trip_through_mail_parser() {
        for (name, draft) in samples() {
            let outgoing = mime::build(&draft, FIXTURE_DATE).unwrap();
            assert!(check_pair(name, &draft, &outgoing), "{name}");
        }
    }

    #[test]
    fn fixtures_on_disk_pass_generic_checks() {
        assert!(check_files_on_disk());
    }
}
