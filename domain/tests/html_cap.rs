//! Timings of the HTML parse limit (M9, F14; `message::HTML_PARSE_LIMIT`). html5ever's tree
//! builder takes time quadratic in the nesting depth, so Sealwax parses at most 256 KiB of a
//! message's HTML for its search text and snippet, and quotes a larger original as text. Ignored
//! in CI (a debug build is far slower); run in release:
//!
//! `cargo test --release -p sealwax-domain --test html_cap -- --ignored --nocapture --test-threads=1`
//!
//! The figures are one machine's (docs/m9/hardening.md has them, with the old path's); the
//! bounds only catch a return to the old path's minutes. Synthetic messages only.

use sealwax_domain::account::Identity;
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::compose::reply::{self, Original};
use sealwax_domain::{html_scan, message};
use std::time::{Duration, Instant};

/// Far below the old path's (on the first run's machine `message::parse` took 19.8 s for the
/// first case and 175 s for 990,000 bytes of open `<ul>`) and above a normal run of the new one.
const BOUND: Duration = Duration::from_secs(30);

fn nested(open: &str, close: &str, depth: usize) -> String {
    format!("{}dypt{}", open.repeat(depth), close.repeat(depth))
}

/// The security review's cases, 55,000 nested lists (990,000 bytes of markup), 14,563 of them
/// (262,134 bytes, just under the limit) and 55,000 nested `div`s, and about the deepest list
/// the limit lets through whole: 65,534 `<ul>` left open (the body, its line end included, is
/// 262,142 bytes).
fn cases() -> [(&'static str, String); 4] {
    [
        (
            "55,000 nested <ul><li>",
            nested("<ul><li>", "</li></ul>", 55_000),
        ),
        (
            "14,563 nested <ul><li>",
            nested("<ul><li>", "</li></ul>", 14_563),
        ),
        ("55,000 nested <div>", nested("<div>", "</div>", 55_000)),
        (
            "65,534 open <ul>",
            nested("<ul>", "", (message::HTML_PARSE_LIMIT - 6) / 4),
        ),
    ]
}

fn html_only(html: &str) -> Vec<u8> {
    format!(
        "From: Avsender <avsender@example.test>\r\nTo: Ada <ada@example.test>\r\n\
         Subject: Dypt\r\nDate: Wed, 07 Oct 2026 12:00:00 +0200\r\n\
         Message-ID: <dypt@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n"
    )
    .into_bytes()
}

fn timed<T>(work: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let done = work();
    (done, started.elapsed())
}

fn senders() -> [Identity; 1] {
    [Identity {
        name: "Ada".into(),
        address: "ada@example.test".into(),
        signature: None,
        signature_html: None,
    }]
}

/// At sync (`message::parse`: the search text and the snippet), when a reply is planned
/// (`Original::parse` and `plan`: the quote), and in the reader's scan.
#[test]
#[ignore = "timing: run in release (the module's comment)"]
fn nested_html_takes_seconds_not_minutes() {
    for (name, html) in cases() {
        let raw = html_only(&html);
        let (_, parse) = timed(|| message::parse(&raw));
        let (planned, reply) = timed(|| {
            let original = Original::parse(&raw);
            reply::plan(&original, ReplyKind::Reply, &senders())
        });
        let (_, scan) = timed(|| html_scan::scan(&html));
        println!(
            "{name} ({} bytes): message::parse {parse:.2?}; a reply planned \
             (Original::parse and plan) {reply:.2?}, quoted as {}; the reader's scan {scan:.2?}",
            html.len(),
            if planned.quote_html.contains("<ul>") || planned.quote_html.contains("<div><div>") {
                "HTML"
            } else {
                "text"
            },
        );
        for (step, took) in [("parse", parse), ("reply", reply)] {
            assert!(took < BOUND, "{name}: {step} took {took:?}");
        }
    }
}

/// Not html5ever's: html2text renders nested `<s>` in time quadratic in their depth (36 s here
/// under the size limit alone); the depth limit (docs/m9/depth.md) takes such HTML's text from
/// mail-parser's converter.
#[test]
#[ignore = "timing: run in release (the module's comment)"]
fn nested_strikeouts_within_the_limit() {
    let html = nested("<s>", "", message::HTML_PARSE_LIMIT / 3);
    let raw = html_only(&html);
    let (_, parse) = timed(|| message::parse(&raw));
    println!(
        "{} open <s> ({} bytes): message::parse {parse:.2?}",
        message::HTML_PARSE_LIMIT / 3,
        html.len()
    );
    assert!(parse < BOUND, "parse took {parse:?}");
}

/// The reader's scan (`domain::html_scan`) reads the whole HTML without a tree; braces nested in
/// a `<style>` block were the slowest input tried.
#[test]
#[ignore = "timing: run in release (the module's comment)"]
fn the_readers_scan_of_nested_css() {
    for depth in [50_000, 100_000, 200_000] {
        let html = format!(
            "<style>{}{}</style><p>x</p>",
            "a{".repeat(depth),
            "}".repeat(depth)
        );
        let (scan, took) = timed(|| html_scan::scan(&html));
        println!(
            "<style> with {depth} nested blocks ({} bytes): the scan {took:.2?}",
            html.len()
        );
        assert!(!scan.own_colours);
    }
}
