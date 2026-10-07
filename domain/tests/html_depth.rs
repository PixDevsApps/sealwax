//! Deeply nested HTML (M9: N1 and N2 in docs/m9/hardening.md, the depth limit in
//! docs/m9/depth.md) through everything that builds a tree of a message's or a draft's HTML, each
//! on a 2 MiB thread like the engine's blocking threads and the ingest workers: a message's
//! search text at sync, the cleaner, a reply planned and built for sending, and a draft read back
//! from the Drafts folder. Before the limit html2text overflowed such a thread on the report's
//! inputs, which aborts the process. Synthetic messages only.
//!
//! The release timings in docs/m9/depth.md come from
//! `cargo test --release -p sealwax-domain --test html_depth -- --nocapture --test-threads=1`.

use sealwax_domain::account::Identity;
use sealwax_domain::compose::reply::{self, Original};
use sealwax_domain::compose::{ReplyKind, clean, ids, mime, stored};
use sealwax_domain::html_depth;
use sealwax_domain::message;
use sealwax_domain::model::{AccountId, MessageId};
use std::time::{Duration, Instant};

const STACK: usize = 2 << 20;

/// Far above what a debug build takes for the slowest step (the old path took minutes for some).
const BOUND: Duration = Duration::from_secs(60);

fn nested(open: &str, close: &str, depth: usize) -> String {
    format!("{}dypt{}", open.repeat(depth), close.repeat(depth))
}

/// The report's inputs (N1: lists and open `<ul>` under the 256 KiB cap and above it; N2: open
/// `<s>`), and shapes that a count of tags alone would get wrong.
fn inputs() -> Vec<(&'static str, String)> {
    vec![
        (
            "14,563 nested <ul><li>",
            nested("<ul><li>", "</li></ul>", 14_563),
        ),
        ("65,536 open <ul>", nested("<ul>", "", 65_536)),
        ("87,381 open <s>", nested("<s>", "", 87_381)),
        (
            "55,000 nested <ul><li>",
            nested("<ul><li>", "</li></ul>", 55_000),
        ),
        (
            "formatting opened again in 20,000 closed divs",
            "<div><b></div>x".repeat(20_000),
        ),
        (
            "10,000 list end tags out of scope",
            "<ul><table><td></ul>".repeat(10_000),
        ),
        ("20,000 lists in formatting", "<b><ul></b>".repeat(20_000)),
        (
            "45 lists, then 20,000 nested <b>",
            format!("{}{}", "<ul><li>".repeat(45), nested("<b>", "", 20_000)),
        ),
    ]
}

fn raw(content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Avsender <avsender@example.test>\r\nTo: Ada <ada@example.test>\r\n\
         Subject: Dypt\r\nDate: Wed, 07 Oct 2026 12:00:00 +0200\r\n\
         Message-ID: <dypt@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: {content_type}; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// Every step on one input, each on its own 2 MiB thread; the time each took.
fn steps(html: &str) -> Vec<(&'static str, Duration)> {
    let on_thread = |name: &'static str, html: String, work: fn(String)| {
        let started = Instant::now();
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(move || work(html))
            .unwrap()
            .join()
            .unwrap_or_else(|_| panic!("{name} panicked"));
        (name, started.elapsed())
    };
    vec![
        on_thread("html_to_text", html.to_owned(), |html| {
            let _ = message::html_to_text(&html);
        }),
        on_thread("plain_text", html.to_owned(), |html| {
            let _ = mime::plain_text(&html);
        }),
        on_thread("clean", html.to_owned(), |html| {
            let cleaned = clean::clean(&html);
            assert!(!html_depth::too_deep(&cleaned.html), "the cleaner's output");
        }),
        on_thread("message::parse", html.to_owned(), |html| {
            let _ = message::parse(&raw("text/html", &html));
        }),
        on_thread("reply planned and built", html.to_owned(), |html| {
            let original = Original::parse(&raw("text/html", &html));
            let senders = [Identity {
                name: "Ada".into(),
                address: "ada@example.test".into(),
                signature: Some("Ada".into()),
            }];
            let planned = reply::plan(&original, ReplyKind::Reply, &senders);
            let message_id = ids::message_id("ada@example.test", [9; 16]).unwrap();
            let draft = planned.draft(
                AccountId(1),
                MessageId(1),
                message_id,
                "<div>Svar</div>",
                Vec::new(),
            );
            let built = mime::build(&draft, 1_791_360_000, &[]).unwrap();
            let _ = mime::drafts_copy(&draft, 1_791_360_000, &[]).unwrap();
            assert!(!built.transmitted.is_empty());
        }),
        on_thread("draft read back", html.to_owned(), |html| {
            let _ = stored::parse(&raw("text/html", &html));
        }),
    ]
}

#[test]
fn deep_html_takes_no_more_than_a_2_mib_thread() {
    for (name, html) in inputs() {
        for (step, took) in steps(&html) {
            println!("{name} ({} bytes): {step} {took:.2?}", html.len());
            assert!(took < BOUND, "{name}: {step} took {took:?}");
        }
    }
}

/// Too deep for a tree, the text still comes through: from mail-parser's converter for the
/// search text and the plain part, and from the elements within the limit for the quote.
#[test]
fn deep_html_keeps_its_text() {
    let html = format!(
        "<p>Over</p>{}<p>Under</p>",
        nested("<ul><li>", "</li></ul>", 2_000)
    );
    assert!(html_depth::too_deep(&html));
    let text = message::html_to_text(&html);
    let plain = mime::plain_text(&html);
    let quote = clean::clean(&html).html;
    for (step, out) in [
        ("html_to_text", &text),
        ("plain_text", &plain),
        ("clean", &quote),
    ] {
        for word in ["Over", "dypt", "Under"] {
            assert!(out.contains(word), "{step}: {word}");
        }
    }
    assert_eq!(quote.matches("<ul>").count(), html_depth::MAX_DEPTH / 2);
}

/// The backstop alone: html2text, handed a tree deeper than the count allows (the count is
/// bypassed here), renders it flattened, and the part of the render tree a narrow width leaves
/// undone is dropped without overflowing.
#[test]
fn a_tree_too_deep_is_flattened_before_html2text_renders_it() {
    let html = format!("{}{}", "<ul><li>".repeat(45), nested("<b>", "", 20_000));
    let text = std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            (
                html_depth::render(html2text::config::plain(), &html, mime::TEXT_WIDTH),
                html_depth::render(
                    html2text::config::plain_no_decorate().allow_width_overflow(),
                    &html,
                    mime::TEXT_WIDTH,
                ),
            )
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(text.0.is_err(), "45 nested lists don't fit 78 columns");
    let kept = text.1.unwrap().replace([' ', '\n', '*'], "");
    assert_eq!(kept, "dypt", "the text below is kept (wrapped)");
}
