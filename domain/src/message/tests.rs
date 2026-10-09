use super::*;
use crate::model::Address;

fn crlf(text: &str) -> Vec<u8> {
    text.replace('\n', "\r\n").into_bytes()
}

const PLAIN: &str = "From: =?UTF-8?B?w4VzZSBCcsOldGVu?= <ase@example.test>
To: Ola Nordmann <ola@example.test>, kari@example.test
Cc: =?ISO-8859-1?Q?=D8ystein_Gr=F8nli?= <oystein@example.test>
Reply-To: Svar <svar@example.test>
Subject: =?UTF-8?Q?Re:_M=C3=B8te_p=C3=A5_fredag?=
Date: Mon, 05 Oct 2026 09:30:00 +0200
Message-ID: <reply-1@example.test>
In-Reply-To: <root-1@example.test>
References: <root-1@example.test> <middle-1@example.test>
List-Id: Turlaget <turlaget.example.test>
Content-Type: text/plain; charset=utf-8

Hei Ola,

det passer fint på fredag.

On Sun, 4 Oct 2026 at 18:00, Ola Nordmann <ola@example.test> wrote:
> Passer det på fredag?
";

const ALTERNATIVE: &str = "From: Nyhetsbrev <nyheter@example.test>
To: ola@example.test
Subject: Ukens nyheter
Date: Tue, 06 Oct 2026 07:00:00 +0000
Message-ID: <news-1@example.test>
MIME-Version: 1.0
Content-Type: multipart/alternative; boundary=\"alt\"

--alt
Content-Type: text/plain; charset=utf-8

Ukens nyheter fra fjellet.
--alt
Content-Type: text/html; charset=utf-8

<html><body><p>Ukens <b>nyheter</b> fra fjellet.</p></body></html>
--alt--
";

const HTML_ONLY: &str = "From: Butikk <ordre@example.test>
To: ola@example.test
Subject: Kvittering
Date: Wed, 07 Oct 2026 12:00:00 +0000
Content-Type: text/html; charset=utf-8

<html><head><style>.x{display:none}</style></head><body>
<div>Takk for handelen&nbsp;&zwnj;&nbsp;&zwnj;</div><table><tr><td>Sum</td><td>199 kr</td></tr></table>
</body></html>
";

const ATTACHMENTS: &str = "From: Regnskap <regnskap@example.test>
To: ola@example.test
Subject: Faktura
Date: Thu, 08 Oct 2026 08:00:00 +0000
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"mixed\"

--mixed
Content-Type: multipart/related; boundary=\"rel\"

--rel
Content-Type: text/html; charset=utf-8

<p>Se vedlagt faktura.</p><img src=\"cid:logo@example.test\">
--rel
Content-Type: image/png
Content-ID: <logo@example.test>
Content-Transfer-Encoding: base64

iVBORw0KGgo=
--rel--
--mixed
Content-Type: application/pdf; name=\"faktura.pdf\"
Content-Disposition: attachment; filename*=UTF-8''faktura%20for%20m%C3%B8te.pdf
Content-Transfer-Encoding: base64

JVBERi0xLjQKJcOkw7zDtsOfCjEgMCBvYmoK
--mixed--
";

const EMBEDDED_ONLY: &str = "From: Butikk <ordre@example.test>
Subject: Logo
Content-Type: multipart/related; boundary=\"rel\"

--rel
Content-Type: text/html; charset=utf-8

<p>Hei</p><img src=\"cid:logo@example.test\">
--rel
Content-Type: image/png
Content-ID: <logo@example.test>
Content-Disposition: inline
Content-Transfer-Encoding: base64

iVBORw0KGgo=
--rel--
";

#[test]
fn decodes_headers() {
    let parsed = parse(&crlf(PLAIN));
    let headers = &parsed.headers;
    assert_eq!(
        headers.from,
        [Address::new("Åse Bråten", "ase@example.test")]
    );
    assert_eq!(
        headers.to,
        [
            Address::new("Ola Nordmann", "ola@example.test"),
            Address::new("", "kari@example.test"),
        ]
    );
    assert_eq!(
        headers.cc,
        [Address::new("Øystein Grønli", "oystein@example.test")]
    );
    assert_eq!(
        headers.reply_to,
        [Address::new("Svar", "svar@example.test")]
    );
    assert_eq!(headers.subject, "Re: Møte på fredag");
    assert_eq!(headers.date, Some(1_791_185_400));
    assert_eq!(headers.message_id.as_deref(), Some("reply-1@example.test"));
    assert_eq!(headers.in_reply_to, ["root-1@example.test"]);
    assert_eq!(
        headers.references,
        ["root-1@example.test", "middle-1@example.test"]
    );
    assert_eq!(headers.list_id.as_deref(), Some("turlaget.example.test"));
    assert_eq!(parsed.body_kind, BodyKind::Plain);
    assert_eq!(parsed.snippet, "Hei Ola, det passer fint på fredag.");
    assert!(parsed.body_text.contains("Passer det på fredag?"));
    assert!(parsed.attachments.is_empty());
    assert_eq!(parsed.size, crlf(PLAIN).len() as u64);
}

#[test]
fn finds_the_body_alternatives() {
    let alternative = parse(&crlf(ALTERNATIVE));
    assert_eq!(alternative.body_kind, BodyKind::Both);
    assert_eq!(alternative.text_parts.len(), 1);
    assert_eq!(alternative.html_parts.len(), 1);
    assert_eq!(alternative.snippet, "Ukens nyheter fra fjellet.");
    let html_part = part(&crlf(ALTERNATIVE), alternative.html_parts[0]).unwrap();
    assert_eq!(html_part.mime_type, "text/html");
    assert!(html_part.text.unwrap().contains("<b>nyheter</b>"));

    let html = parse(&crlf(HTML_ONLY));
    assert_eq!(html.body_kind, BodyKind::Html);
    assert!(html.text_parts.is_empty());
    assert_eq!(html.snippet, "Takk for handelen Sum 199 kr");
    assert_eq!(html.headers.message_id, None);
}

#[test]
fn lists_attachments() {
    let raw = crlf(ATTACHMENTS);
    let parsed = parse(&raw);
    assert_eq!(parsed.body_kind, BodyKind::Html);
    assert_eq!(parsed.snippet, "Se vedlagt faktura.");
    assert_eq!(parsed.attachments.len(), 2, "{:?}", parsed.attachments);
    let logo = &parsed.attachments[0];
    assert_eq!(logo.mime_type, "image/png");
    assert_eq!(logo.content_id.as_deref(), Some("logo@example.test"));
    assert!(logo.inline && logo.is_embedded());
    assert_eq!(logo.size, 8);
    let pdf = &parsed.attachments[1];
    assert_eq!(pdf.name.as_deref(), Some("faktura for møte.pdf"));
    assert_eq!(pdf.mime_type, "application/pdf");
    assert!(!pdf.inline);
    assert_eq!(pdf.size, 27);
    assert!(parsed.has_attachments());

    let image = part_by_content_id(&raw, "logo@example.test").unwrap();
    assert_eq!(image.mime_type, "image/png");
    assert_eq!(image.data, b"\x89PNG\r\n\x1a\n");
    assert!(part_by_content_id(&raw, "other@example.test").is_none());

    let embedded = parse(&crlf(EMBEDDED_ONLY));
    assert_eq!(embedded.attachments.len(), 1);
    assert!(!embedded.has_attachments());
}

#[test]
fn decodes_legacy_and_cjk_charsets() {
    let mut latin = b"From: Kari <kari@example.test>\r\nSubject: Hilsen\r\n\
Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: 8bit\r\n\r\n"
        .to_vec();
    latin.extend_from_slice(b"Hei p\xe5 deg, bl\xe5b\xe6rsyltet\xf8y!\r\n");
    assert_eq!(parse(&latin).snippet, "Hei på deg, blåbærsyltetøy!");

    let japanese = crlf(
        "From: =?ISO-2022-JP?B?GyRCMnE1RCROTT1EahsoQg==?= <sato@example.test>
Subject: =?ISO-2022-JP?B?GyRCMnE1RCROTT1EahsoQg==?=
Content-Type: text/plain; charset=ISO-2022-JP
Content-Transfer-Encoding: base64

GyRCJVclbSU4JSclLyVIJE4/SkQ9JEskRCQkJEZKczlwJDckXiQ5ISMbKEI=
",
    );
    let parsed = parse(&japanese);
    assert_eq!(parsed.headers.subject, "会議の予定");
    assert_eq!(parsed.headers.from[0].name, "会議の予定");
    assert_eq!(parsed.snippet, "プロジェクトの進捗について報告します。");

    let chinese = crlf(
        "From: =?GB2312?B?zfXOsA==?= <wang.wei@example.test>
Subject: =?GB2312?B?zfXOsA==?=
Content-Type: text/plain; charset=gb2312
Content-Transfer-Encoding: base64

udjT2s/C1ty1xLvh0umwssXFo6zH67Lpv7S4vbz+oaM=
",
    );
    let parsed = parse(&chinese);
    assert_eq!(parsed.headers.from[0].name, "王伟");
    assert_eq!(parsed.snippet, "关于下周的会议安排，请查看附件。");
}

#[test]
fn ignores_bad_dates_and_ids() {
    let raw = crlf(
        "From: Kari <kari@example.test>
Date: someday soon
Message-ID: <>
In-Reply-To: <a@example.test> <a@example.test> <b@example.test>
Subject: Test

text
",
    );
    let parsed = parse(&raw);
    assert_eq!(parsed.headers.date, None);
    assert_eq!(parsed.headers.message_id, None);
    assert_eq!(
        parsed.headers.in_reply_to,
        ["a@example.test", "b@example.test"]
    );
}

#[test]
fn keeps_the_root_and_the_newest_references() {
    let references: Vec<String> = (0..80).map(|n| format!("<r{n}@example.test>")).collect();
    let raw = crlf(&format!(
        "From: a@example.test\nReferences: {}\nSubject: x\n\nbody\n",
        references.join(" ")
    ));
    let parsed = parse(&raw);
    assert_eq!(parsed.headers.references.len(), 50);
    assert_eq!(parsed.headers.references[0], "r0@example.test");
    assert_eq!(parsed.headers.references[1], "r31@example.test");
    assert_eq!(parsed.headers.references[49], "r79@example.test");
}

#[test]
fn reads_the_conversation_of_outlooks_thread_index() {
    let conversation = thread_index("Adk8WhAiEBESExQVFhcYGRobHB0eHw==").unwrap();
    assert_eq!(
        conversation.to_hex(),
        "01d93c5a1022101112131415161718191a1b1c1d1e1f"
    );
    for reply in [
        "Adk8WhAiEBESExQVFhcYGRobHB0eHwAAEjRW",
        "Adk8WhAiEBESExQVFhcYGRobHB0eHwAAEjRWAAECAwQ=",
        " Adk8WhAiEBESExQVFhcYGRobHB0eHwAA\r\n EjRW\r\n",
    ] {
        assert_eq!(thread_index(reply), Some(conversation), "{reply}");
    }
    for malformed in [
        "",
        "not base64!",
        "Adk8WhAiEBESExQVFhcYGRobHB0e",
        "Adk8WhAiEBESExQVFhcYGRobHB0eHwEC",
        "Atk8WhAiEBESExQVFhcYGRobHB0eHw==",
        "Adk8WhAiAAAAAAAAAAAAAAAAAAAAAA==",
    ] {
        assert_eq!(thread_index(malformed), None, "{malformed}");
    }
    let folded = crlf(
        "From: a@example.test\nSubject: RE: x\nthread-index: Adk8WhAiEBESExQVFhcYGRobHB0eHwAA\n EjRW\n\
         Thread-Topic: x\n\nbody\n",
    );
    assert_eq!(parse(&folded).headers.thread_index, Some(conversation));
    let header_fields =
        b"Subject: SV: x\r\nThread-Index: Adk8WhAiEBESExQVFhcYGRobHB0eHwAAEjRW\r\n\r\n";
    assert_eq!(
        parse(header_fields).headers.thread_index,
        Some(conversation),
        "a first sync's HEADER.FIELDS"
    );
    let garbage = crlf("From: a@example.test\nThread-Index: Ådk8\n\nbody\n");
    assert_eq!(parse(&garbage).headers.thread_index, None);
    assert_eq!(parse(&crlf(PLAIN)).headers.thread_index, None);
}

#[test]
fn subjects_lose_reply_prefixes() {
    assert_eq!(base_subject("Re: SV: Fwd: Møte"), "Møte");
    assert_eq!(base_subject("RE[2]: budget"), "budget");
    assert_eq!(base_subject("回复：会议安排"), "会议安排");
    assert_eq!(base_subject("Agenda: Tuesday"), "Agenda: Tuesday");
    assert_eq!(base_subject("Re:"), "");
}

/// SplitMix64, for repeatable mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn check_invariants(raw: &[u8]) {
    let parsed = parse(raw);
    assert!(parsed.snippet.chars().count() <= 200);
    assert!(parsed.body_text.len() <= SEARCH_TEXT_LIMIT);
    assert_eq!(parsed.size, raw.len() as u64);
    for index in parsed
        .text_parts
        .iter()
        .chain(&parsed.html_parts)
        .chain(parsed.attachments.iter().map(|attachment| &attachment.part))
    {
        assert!(part(raw, *index).is_some(), "part {index} missing");
    }
}

#[test]
fn never_panics_on_malformed_input() {
    let nested = format!(
        "Content-Type: multipart/mixed; boundary=b0\n\n{}x",
        (1..200)
            .map(|level| format!(
                "--b{}\nContent-Type: multipart/mixed; boundary=b{level}\n\n",
                level - 1
            ))
            .collect::<String>()
    );
    let long_header = format!("Subject: {}\n\nbody", "å".repeat(300_000));
    let samples: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"\r\n\r\n".to_vec(),
        b"no headers at all, just text".to_vec(),
        b"\xff\xfe\x00\x00garbage\x00\x01\x02".to_vec(),
        crlf("Subject: \u{0}\u{7}nul\n\n\u{0}body"),
        crlf("Content-Type: multipart/mixed; boundary=\"never\"\n\nno parts here"),
        crlf("Content-Type: multipart/mixed\n\n--\n--\n"),
        crlf("Content-Type: text/plain; charset=x-unknown-charset\n\nhei"),
        crlf("Content-Transfer-Encoding: base64\n\n!!!not base64!!!"),
        crlf("Content-Transfer-Encoding: quoted-printable\n\n=ZZ=\n=4"),
        crlf("From: <<<@@>>>\nTo: ,,,;;;\nDate: Fri, 99 Zzz 99999 99:99:99 +9999\n\nx"),
        crlf("Subject: =?UTF-8?B?!!!?= =?X?Q?=FF?= =?UTF-8?Q?unterminated\n\nx"),
        b"Subject: \xe5\xff\xfe raw bytes\r\nFrom: \xc3\x28 <x@example.test>\r\n\r\n\xc3\x28"
            .to_vec(),
        crlf(
            "Content-Type: message/rfc822\n\nContent-Type: message/rfc822\n\nSubject: inner\n\ninner",
        ),
        nested.into_bytes(),
        long_header.into_bytes(),
        crlf(PLAIN),
        crlf(ALTERNATIVE),
        crlf(HTML_ONLY),
        crlf(ATTACHMENTS),
    ];
    for sample in &samples {
        check_invariants(sample);
    }
    let mut rng = Rng(0x5EA1_3A11);
    for sample in samples
        .iter()
        .filter(|sample| sample.len() > 16 && sample.len() < 10_000)
    {
        for _ in 0..300 {
            let mut mutated = sample.clone();
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(mutated.len());
                match rng.below(5) {
                    0 => mutated[at] = rng.next() as u8,
                    1 => mutated.truncate(at),
                    2 => {
                        let end = (at + rng.below(64)).min(mutated.len());
                        let copy = mutated[at..end].to_vec();
                        mutated.splice(at..at, copy);
                    }
                    3 => {
                        let end = (at + rng.below(64)).min(mutated.len());
                        mutated.drain(at..end);
                    }
                    _ => mutated.insert(at, *b"<>=?:;\"\r\n-\\".get(rng.below(12)).unwrap_or(&0)),
                }
                if mutated.is_empty() {
                    break;
                }
            }
            check_invariants(&mutated);
        }
    }
}

/// Found by fuzzing (ML-093): this message reaches mail-parser 0.11.9's
/// `debug_assert!(false, "Invalid part ID, could not find multipart.")`, a panic in debug builds
/// (release builds compile it out). A panic beneath `parse` leaves the size only.
#[test]
fn a_panic_beneath_the_parser_is_caught() {
    let raw = include_bytes!("../../../fuzz/known/mail-parser-debug-assert.eml");
    assert_eq!(parse(raw).size, raw.len() as u64);
}

#[test]
fn header_fields_are_unfolded_decoded_and_in_order() {
    let raw = b"From: =?utf-8?q?Kari_Nordmann?= <kari@example.test>\r\n\
To: ola@example.test, \"Per\" <per@example.test>\r\n\
Subject: =?iso-8859-1?q?F=E5ktura?= mars\r\n\
X-Spam-Status: Yes, score=7.1\r\n\x20required=5.0 tests=BAYES_99\r\n\
X-Spam-Flag: YES\r\n\
Date: Thu, 08 Oct 2026 10:00:00 +0200\r\n\
\r\n\
Body: not a header\r\n";
    let fields = header_fields(raw);
    let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "from",
            "to",
            "subject",
            "x-spam-status",
            "x-spam-flag",
            "date"
        ]
    );
    assert_eq!(fields[0].value, "Kari Nordmann <kari@example.test>");
    assert_eq!(fields[0].addresses, ["kari@example.test"]);
    assert_eq!(
        fields[1].addresses,
        ["ola@example.test", "per@example.test"]
    );
    assert_eq!(fields[2].value, "Fåktura mars");
    assert_eq!(
        fields[3].value,
        "Yes, score=7.1 required=5.0 tests=BAYES_99"
    );
    assert_eq!(fields[4].value, "YES");
    assert_eq!(fields[5].value, "Thu, 08 Oct 2026 10:00:00 +0200");
    assert!(header_fields(b"").is_empty());
    assert!(header_fields(b"\r\nno header").is_empty());
}
