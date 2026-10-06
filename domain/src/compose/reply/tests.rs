use super::*;
use crate::compose::clean::QUOTE_STYLE;
use proptest::prelude::*;

fn senders() -> Vec<Identity> {
    vec![
        Identity {
            name: "Ada Lovelace".into(),
            address: "ada@example.test".into(),
            signature: Some("Ada".into()),
        },
        Identity {
            name: "Ada at the board".into(),
            address: "board@example.test".into(),
            signature: None,
        },
    ]
}

fn emails(list: &[Address]) -> Vec<&str> {
    list.iter().map(|a| a.email.as_str()).collect()
}

/// A message from Marius to Ada and Bob, Cc the board alias and Carol, with a Reply-To.
const MESSAGE: &str = "From: Marius Dahl <marius@example.test>\r\n\
Reply-To: Marius at home <marius@home.example.test>\r\n\
To: Ada Lovelace <ADA@example.test>, Bob <bob@example.test>\r\n\
Cc: board@example.test, Carol <carol@example.test>, bob@EXAMPLE.test\r\n\
Subject: SV: =?utf-8?q?M=C3=B8te?= i morgen\r\n\
Date: Sun, 04 Oct 2026 07:50:00 +0200\r\n\
Message-ID: <m3@example.test>\r\n\
In-Reply-To: <m2@example.test>\r\n\
References: <m1@example.test> <m2@example.test>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
\r\n\
--alt\r\n\
Content-Type: text/plain; charset=utf-8; format=flowed\r\n\
\r\n\
Hei Ada, \r\n\
dette er en lang linje.\r\n\
--alt\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body><p onclick=\"x()\">Hei <b>Ada</b></p><script>alert(1)</script>\
<img src=\"cid:logo@example.test\"><img src=\"cid:missing@example.test\">\
<img src=\"https://images.example.test/remote.png\"></body></html>\r\n\
--rel\r\n\
Content-Type: image/png\r\n\
Content-ID: <logo@example.test>\r\n\
Content-Disposition: inline; filename=\"logo.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--rel--\r\n\
--alt--\r\n\
--outer\r\n\
Content-Type: application/pdf; name=\"rapport.pdf\"\r\n\
Content-Disposition: attachment; filename=\"rapport.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--outer\r\n\
Content-Type: application/pgp-signature; name=\"signature.asc\"\r\n\
\r\n\
-----BEGIN PGP SIGNATURE-----\r\n\
--outer--\r\n";

#[test]
fn parses_the_original() {
    let original = Original::parse(MESSAGE.as_bytes());
    assert_eq!(original.headers.subject, "SV: Møte i morgen");
    assert_eq!(
        original.date,
        Some(QuotedDate {
            utc: 1_791_093_000,
            offset_minutes: 120
        })
    );
    assert_eq!(
        original.text.as_deref(),
        Some("Hei Ada, dette er en lang linje.")
    );
    assert!(original.html.as_deref().unwrap().contains("<script>"));
    let types: Vec<&str> = original
        .parts
        .iter()
        .map(|p| p.mime_type.as_str())
        .collect();
    assert_eq!(
        types,
        ["image/png", "application/pdf", "application/pgp-signature"]
    );
    let printed = format!("{original:?}");
    assert!(
        !printed.contains("Ada") && !printed.contains("Marius"),
        "{printed}"
    );
    assert_eq!(Original::parse(b""), Original::default());
}

#[test]
fn reply_goes_to_reply_to_from_the_alias_it_was_sent_to() {
    let original = Original::parse(MESSAGE.as_bytes());
    let reply = plan(&original, ReplyKind::Reply, &senders());
    assert_eq!(emails(&reply.to), ["marius@home.example.test"]);
    assert!(reply.cc.is_empty());
    assert_eq!(reply.sender.address, "ada@example.test", "To before Cc");
    assert_eq!(reply.subject, "Re: Møte i morgen");
    assert_eq!(reply.in_reply_to.as_deref(), Some("m3@example.test"));
    assert_eq!(
        reply.references,
        ["m1@example.test", "m2@example.test", "m3@example.test"]
    );
    let mut to_board = original.clone();
    to_board.headers.to.remove(0);
    let reply = plan(&to_board, ReplyKind::Reply, &senders());
    assert_eq!(reply.sender.address, "board@example.test");
    assert_eq!(reply.sender.name, "Ada at the board");
}

#[test]
fn reply_all_leaves_out_every_own_address() {
    let original = Original::parse(MESSAGE.as_bytes());
    let reply = plan(&original, ReplyKind::ReplyAll, &senders());
    assert_eq!(
        emails(&reply.to),
        ["marius@home.example.test", "bob@example.test"]
    );
    assert_eq!(emails(&reply.cc), ["carol@example.test"]);
    let mut no_reply_to = original.clone();
    no_reply_to.headers.reply_to.clear();
    let reply = plan(&no_reply_to, ReplyKind::ReplyAll, &senders());
    assert_eq!(
        emails(&reply.to),
        ["marius@example.test", "bob@example.test"]
    );
}

#[test]
fn replying_to_own_mail_goes_to_its_recipients() {
    let mut original = Original::parse(MESSAGE.as_bytes());
    original.headers.from = vec![Address::new("Ada", "board@example.test")];
    original.headers.reply_to.clear();
    let reply = plan(&original, ReplyKind::Reply, &senders());
    assert_eq!(emails(&reply.to), ["ADA@example.test", "bob@example.test"]);
    assert_eq!(reply.sender.address, "board@example.test");
    let all = plan(&original, ReplyKind::ReplyAll, &senders());
    assert_eq!(emails(&all.to), ["bob@example.test"]);
    assert_eq!(emails(&all.cc), ["carol@example.test"]);
    original.headers.to = vec![Address::new("", "ada@example.test")];
    original.headers.cc.clear();
    let to_self = plan(&original, ReplyKind::ReplyAll, &senders());
    assert_eq!(emails(&to_self.to), ["ada@example.test"], "a note to self");
}

#[test]
fn the_quote_is_cleaned_and_keeps_its_inline_image() {
    let original = Original::parse(MESSAGE.as_bytes());
    let reply = plan(&original, ReplyKind::Reply, &senders());
    assert!(reply.quote_html.starts_with(&format!(
        "<div>On Sun 4 Oct 2026 at 07:50, Marius Dahl wrote:</div><blockquote type=\"cite\" style=\"{QUOTE_STYLE}\">"
    )));
    assert!(reply.quote_html.contains("<p>Hei <b>Ada</b></p>"));
    assert!(
        reply
            .quote_html
            .contains("<img src=\"cid:logo@example.test\">")
    );
    assert!(
        reply
            .quote_html
            .contains("<img src=\"https://images.example.test/remote.png\">")
    );
    for gone in ["onclick", "script", "alert", "missing@"] {
        assert!(!reply.quote_html.contains(gone), "{gone}");
    }
    assert_eq!(
        reply.quote_text,
        "On Sun 4 Oct 2026 at 07:50, Marius Dahl wrote:\n> Hei Ada, dette er en lang linje."
    );
    assert_eq!(reply.parts.len(), 1);
    assert_eq!(
        reply.parts[0].content_id.as_deref(),
        Some("logo@example.test")
    );
    let part = draft_part(&reply.parts[0], BlobKey([1; 32]));
    assert_eq!(
        (
            part.name.as_str(),
            part.mime_type.as_str(),
            part.content_id.as_deref()
        ),
        ("logo.png", "image/png", Some("logo@example.test"))
    );
}

#[test]
fn forward_carries_the_attachments() {
    let original = Original::parse(MESSAGE.as_bytes());
    let forward = plan(&original, ReplyKind::Forward, &senders());
    assert!(forward.to.is_empty() && forward.cc.is_empty());
    assert_eq!(forward.subject, "Fwd: SV: Møte i morgen");
    assert_eq!(
        forward.in_reply_to, None,
        "a forward starts a conversation of its own"
    );
    assert!(forward.references.is_empty());
    let parts: Vec<(&str, Option<&str>)> = forward
        .parts
        .iter()
        .map(|p| (p.mime_type.as_str(), p.content_id.as_deref()))
        .collect();
    assert_eq!(
        parts,
        [
            ("image/png", Some("logo@example.test")),
            ("application/pdf", None)
        ]
    );
    assert!(forward.quote_html.starts_with(
        "<div>---------- Forwarded message ----------</div><div>From: Marius Dahl &lt;marius@example.test&gt;</div>\
         <div>Date: Sun 4 Oct 2026 at 07:50 +0200</div><div>Subject: SV: Møte i morgen</div>"
    ));
    assert!(!forward.quote_html.contains("<blockquote"));
    assert!(
        forward
            .quote_text
            .contains("\nCc: board@example.test, Carol <carol@example.test>, bob@EXAMPLE.test\n")
    );
}

#[test]
fn plain_text_originals_are_escaped() {
    let raw = "From: bob@example.test\r\nTo: ada@example.test\r\nSubject: x\r\n\r\n<b>not markup</b>\r\n> quoted\r\n";
    let original = Original::parse(raw.as_bytes());
    let reply = plan(&original, ReplyKind::Reply, &senders());
    assert!(
        reply
            .quote_html
            .contains("<div>&lt;b&gt;not markup&lt;/b&gt;</div><div>&gt; quoted</div>")
    );
    assert_eq!(
        reply.quote_text,
        "bob@example.test wrote:\n> <b>not markup</b>\n>> quoted"
    );
    assert_eq!(reply.in_reply_to, None);
    assert!(reply.references.is_empty());
}

#[test]
fn references_are_trimmed_keeping_the_root() {
    let headers = Headers {
        message_id: Some("new@example.test".into()),
        references: (0..40).map(|n| format!("r{n}@example.test")).collect(),
        ..Headers::default()
    };
    let (in_reply_to, references) = threading(&headers);
    assert_eq!(in_reply_to.as_deref(), Some("new@example.test"));
    assert_eq!(references.len(), MAX_REFERENCES);
    assert_eq!(references[0], "r0@example.test");
    assert_eq!(references[1], "r22@example.test");
    assert_eq!(references.last().unwrap(), "new@example.test");
    let only_in_reply_to = Headers {
        message_id: Some("b@example.test".into()),
        in_reply_to: vec!["a@example.test".into()],
        ..Headers::default()
    };
    assert_eq!(
        threading(&only_in_reply_to).1,
        ["a@example.test", "b@example.test"]
    );
}

#[test]
fn subjects_get_one_prefix() {
    for (subject, reply, forward) in [
        ("Møte", "Re: Møte", "Fwd: Møte"),
        ("Re: Møte", "Re: Møte", "Fwd: Re: Møte"),
        (
            "RE: SV: AW: Antw: Re[2]: Møte",
            "Re: Møte",
            "Fwd: RE: SV: AW: Antw: Re[2]: Møte",
        ),
        ("Fwd: Møte", "Re: Fwd: Møte", "Fwd: Møte"),
        (
            "VS: WG: Fw: FWD: Møte",
            "Re: VS: WG: Fw: FWD: Møte",
            "Fwd: Møte",
        ),
        ("Re:Møte", "Re: Møte", "Fwd: Re:Møte"),
        (
            "Regarding: Møte",
            "Re: Regarding: Møte",
            "Fwd: Regarding: Møte",
        ),
        ("", "Re:", "Fwd:"),
        ("Re:", "Re:", "Fwd: Re:"),
        ("Hi\r\nBcc: x", "Re: Hi Bcc: x", "Fwd: Hi Bcc: x"),
    ] {
        assert_eq!(reply_subject(subject), reply, "{subject}");
        assert_eq!(forward_subject(subject), forward, "{subject}");
    }
}

#[test]
fn the_draft_carries_the_plan() {
    let original = Original::parse(MESSAGE.as_bytes());
    let reply = plan(&original, ReplyKind::ReplyAll, &senders());
    let draft = reply.draft(
        AccountId(3),
        MessageId(9),
        "new@example.test".into(),
        "<div>Takk!</div>",
        Vec::new(),
    );
    assert_eq!(draft.from, Address::new("Ada Lovelace", "ada@example.test"));
    assert_eq!(draft.to, reply.to);
    assert_eq!(
        draft.source,
        Some(DraftSource {
            message: MessageId(9),
            kind: ReplyKind::ReplyAll
        })
    );
    assert!(draft.html.starts_with(
        "<div>Takk!</div><div><br></div><div class=\"sw-signature\"><div>--&nbsp;</div><div>Ada</div></div><div><br></div><div>On Sun"
    ));
    assert_eq!(reply.html(""), reply.html("  ").replace("  ", ""));
    let printed = format!("{reply:?}");
    assert!(
        !printed.contains("Ada") && !printed.contains("Møte"),
        "{printed}"
    );
}

proptest! {
    #[test]
    fn prefixes_are_added_once(subject in "((Re|RE|Sv|AW|Fwd|VS|x)(\\[[0-9]\\])?: ?){0,4}[a-zæø ]{0,12}") {
        let once = reply_subject(&subject);
        prop_assert_eq!(reply_subject(&once), once.clone());
        prop_assert!(once.starts_with("Re:"));
        let forwarded = forward_subject(&subject);
        prop_assert_eq!(forward_subject(&forwarded), forwarded.clone());
    }

    #[test]
    fn reply_all_never_includes_the_user(
        to in prop::collection::vec("(ada|board|bob|carol|dan)@example\\.test", 0..5),
        cc in prop::collection::vec("(ADA|board|bob|eve)@Example\\.test", 0..5),
    ) {
        let headers = Headers {
            from: vec![Address::new("", "marius@example.test")],
            to: to.iter().map(|e| Address::new("", e.clone())).collect(),
            cc: cc.iter().map(|e| Address::new("", e.clone())).collect(),
            ..Headers::default()
        };
        let original = Original { headers, ..Original::default() };
        let reply = plan(&original, ReplyKind::ReplyAll, &senders());
        let all: Vec<&Address> = reply.to.iter().chain(&reply.cc).collect();
        for address in &all {
            prop_assert!(!identity::is_own(&senders(), &address.email));
        }
        let keys: Vec<String> = all.iter().map(|a| address::key(&a.email)).collect();
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        prop_assert_eq!(keys.len(), unique.len());
        prop_assert_eq!(reply.to.first().map(|a| a.email.as_str()), Some("marius@example.test"));
    }
}
