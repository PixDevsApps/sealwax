use email_spike_s2::corpus::{self, Assets, Kind};
use email_spike_s2::heuristic;
use std::collections::BTreeSet;

const ORIGIN: &str = "http://127.0.0.1:9";

fn cid_references(html: &str) -> BTreeSet<String> {
    html.match_indices("cid:")
        .map(|(index, _)| {
            html[index + 4..]
                .chars()
                .take_while(|c| !matches!(c, '"' | '\'' | ')' | ' ' | '>'))
                .collect()
        })
        .collect()
}

#[test]
fn at_least_fifty_newsletters_with_the_required_features() {
    let assets = Assets::generate(Some(b"fake font".to_vec()));
    let newsletters = corpus::newsletters(ORIGIN, &assets);
    assert!(newsletters.len() >= 50, "{} newsletters", newsletters.len());
    let all: String = newsletters.iter().map(|m| m.body.as_str()).collect();
    for feature in [
        "@media",
        "@font-face",
        "@import",
        "background-image:url(",
        "background=\"cid:",
        "<table",
        "<!--[if",
        "<font ",
        "height: 100%",
        "100vh",
    ] {
        assert!(all.contains(feature), "no newsletter uses {feature}");
    }
    for message in &newsletters {
        assert_eq!(message.kind, Kind::Newsletter);
        let nesting = message.body.matches("<table").count();
        assert!(
            nesting >= 1 || message.family == "responsive",
            "{} has no table",
            message.id
        );
        assert!(
            message.body.contains(ORIGIN),
            "{} has no remote reference",
            message.id
        );
        assert!(!message.parts.is_empty(), "{} has no cid part", message.id);
    }
}

#[test]
fn every_cid_reference_has_its_part_and_parts_are_unique() {
    let assets = Assets::generate(Some(b"fake font".to_vec()));
    for message in corpus::newsletters(ORIGIN, &assets) {
        let referenced = cid_references(&message.body);
        let provided: BTreeSet<String> = message.parts.iter().map(|p| p.cid.clone()).collect();
        assert_eq!(
            provided.len(),
            message.parts.len(),
            "{} repeats a cid",
            message.id
        );
        assert_eq!(referenced, provided, "{}", message.id);
    }
}

#[test]
fn heuristic_puts_newsletters_on_paper_and_simple_html_in_the_theme() {
    let assets = Assets::generate(None);
    for message in corpus::newsletters(ORIGIN, &assets) {
        assert!(
            heuristic::analyse(&message.body).own_colours(),
            "{}",
            message.id
        );
    }
    for message in corpus::simple_html() {
        let verdict = heuristic::analyse(&message.body);
        assert!(!verdict.own_colours(), "{}: {:?}", message.id, verdict);
    }
    assert!(!heuristic::analyse(&corpus::tall_message(10).body).own_colours());
}

#[test]
fn generation_is_deterministic() {
    let assets = Assets::generate(None);
    let first = corpus::newsletters(ORIGIN, &assets);
    let second = corpus::newsletters(ORIGIN, &assets);
    assert!(first.iter().zip(&second).all(|(a, b)| a.body == b.body));
    assert_eq!(corpus::plain_text().len(), 6);
    assert_eq!(corpus::simple_html().len(), 8);
}
