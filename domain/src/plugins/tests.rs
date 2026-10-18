use super::*;

const SIGNET: &str = r#"
name = "Signet"
description = "Create calendar events from email"
command = ["signet", "--from-sealwax"]

[[action]]
id = "create-event"
title = "Create Event"
receives = ["text"]

[[hook]]
event = "new-mail"
"#;

#[test]
fn reads_a_manifest() {
    let manifest = parse_manifest("signet", SIGNET).unwrap();
    assert_eq!(manifest.name, "Signet");
    assert_eq!(manifest.command, ["signet", "--from-sealwax"]);
    assert!(manifest.enabled);
    let action = manifest.action("create-event").unwrap();
    assert_eq!(action.title, "Create Event");
    assert_eq!(action.receives, BTreeSet::from([Part::Text]));
    assert_eq!(
        manifest.hooks,
        vec![Hook {
            event: HookEvent::NewMail,
            receives: BTreeSet::new()
        }]
    );
}

#[test]
fn reports_every_mistake_with_its_place() {
    let text = "name = 3\ncomand = [\"x\"]\n[[action]]\nid = \"Bad Id\"\ntitle = \"T\"\nreceives = [\"txt\"]\n[[hook]]\nevent = \"new-mial\"\n";
    let errors = parse_manifest("demo", text).unwrap_err();
    let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
    let all = messages.join("\n");
    assert!(
        all.contains("line 1, column 8: name must be a string, not an integer"),
        "{all}"
    );
    assert!(
        all.contains("line 2, column 1: plugin.toml has no setting comand; did you mean command?"),
        "{all}"
    );
    assert!(all.contains("id takes a–z, 0–9, - and _ only"), "{all}");
    assert!(all.contains("did you mean text?"), "{all}");
    assert!(
        all.contains("event takes \"new-mail\"; did you mean new-mail?"),
        "{all}"
    );
    assert!(all.contains("needs command"), "{all}");
}

#[test]
fn a_plugin_must_do_something_and_ids_are_unique() {
    let errors = parse_manifest("x", "command = [\"x\"]\n").unwrap_err();
    assert!(errors[0].message.contains("does nothing"));
    let twice = "command = [\"x\"]\n[[action]]\nid = \"a\"\ntitle = \"A\"\n[[action]]\nid = \"a\"\ntitle = \"B\"\n";
    assert!(
        parse_manifest("x", twice).unwrap_err()[0]
            .message
            .contains("two actions have the id a")
    );
    assert!(parse_manifest("Bad Folder", SIGNET).is_err());
    let off = format!("enabled = false\n{SIGNET}");
    assert!(!parse_manifest("signet", &off).unwrap().enabled);
}

#[test]
fn ids() {
    assert!(is_valid_id("create-event_2"));
    assert!(!is_valid_id(""));
    assert!(!is_valid_id("Create"));
    assert!(!is_valid_id("a.b"));
    assert!(!is_valid_id(&"a".repeat(65)));
}

const RAW: &[u8] = b"From: Grace Hopper <grace@example.com>\r\nTo: ada@example.com, Bob <bob@example.com>\r\n\
Subject: Planning \"call\"\r\nDate: Sun, 18 Oct 2026 09:12:00 +0000\r\nMessage-ID: <m1@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\nLet's talk Thursday.\r\nJoin: https://meet.google.com/abc\r\n";

#[test]
fn the_payload_has_only_what_was_asked_for() {
    let account = AccountInfo {
        key: "work".into(),
        address: "ada@example.com".into(),
        label: "Work".into(),
    };
    let headers_only = MessageInfo::from_raw("INBOX", RAW, &BTreeSet::new());
    assert_eq!(headers_only.text, None);
    assert_eq!(headers_only.raw, None);
    let json = payload(
        "signet",
        &Trigger::Action("create-event".into()),
        &account,
        &[headers_only],
    );
    assert!(json.starts_with("{\"version\": 1, \"event\": \"action\", \"plugin\": \"signet\", \"action\": \"create-event\""), "{json}");
    assert!(
        json.contains("\"subject\": \"Planning \\\"call\\\"\""),
        "{json}"
    );
    assert!(
        json.contains(
            "\"from\": [{\"name\": \"Grace Hopper\", \"address\": \"grace@example.com\"}]"
        ),
        "{json}"
    );
    assert!(
        json.contains("\"message_id\": \"m1@example.com\""),
        "{json}"
    );
    assert!(json.contains("\"date\": 1792314720"), "{json}");
    assert!(json.contains("\"text\": null, \"raw\": null"), "{json}");

    let everything = MessageInfo::from_raw("INBOX", RAW, &BTreeSet::from([Part::Text, Part::Raw]));
    let json = payload(
        "signet",
        &Trigger::Hook(HookEvent::NewMail),
        &account,
        &[everything],
    );
    assert!(json.contains("\"event\": \"new-mail\""), "{json}");
    assert!(json.contains("\"action\": null"), "{json}");
    assert!(json.contains("Let's talk Thursday.\\n"), "{json}");
    assert!(
        json.contains(&format!("\"raw\": \"{}\"", base64(RAW))),
        "{json}"
    );
}

#[test]
fn json_text_is_escaped() {
    assert_eq!(
        json_string("a\"b\\c\nd\u{1}\u{2028}"),
        "\"a\\\"b\\\\c\\nd\\u0001\\u2028\""
    );
}

#[test]
fn base64_matches_rfc4648() {
    for (input, output) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(input.as_bytes()), output);
    }
}

#[test]
fn replies() {
    assert_eq!(parse_reply("").unwrap(), Reply::default());
    assert_eq!(
        parse_reply("{\"notice\": \"Opened in Signet\"}\n")
            .unwrap()
            .notice
            .as_deref(),
        Some("Opened in Signet")
    );
    assert_eq!(
        parse_reply("{\"notice\": \"a\\nb \\u00e9\"}")
            .unwrap()
            .notice
            .as_deref(),
        Some("a b é")
    );
    assert_eq!(parse_reply("{\"other\": 1}").unwrap().notice, None);
    assert!(parse_reply("done!").is_err());
    assert_eq!(
        first_line("\n  couldn't start\nmore").as_deref(),
        Some("couldn't start")
    );
}
