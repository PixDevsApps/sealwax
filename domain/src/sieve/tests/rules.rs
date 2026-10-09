//! The rules model: the canonical script, the round trip through Roundcube's form, the spam
//! sections, hand-written Sieve, and what can't be written.

use super::super::{
    Action, Condition, Field, Folders, Invalid, Item, Match, Operator, Reason, Rule, RuleSet,
    Senders, compile, evaluate,
};
use super::{Message, filed, run};
use proptest::prelude::*;

fn folders() -> Folders {
    Folders::default()
}

fn field(field: Field, operator: Operator, value: &str) -> Condition {
    Condition::Field {
        field,
        operator,
        value: value.to_owned(),
    }
}

fn rule(name: &str, matching: Match, conditions: Vec<Condition>, actions: Vec<Action>) -> Rule {
    Rule {
        name: name.to_owned(),
        enabled: true,
        matching,
        conditions,
        actions,
    }
}

fn render(set: &RuleSet) -> String {
    set.render(&folders())
        .unwrap_or_else(|invalid| panic!("{invalid:?}"))
}

fn read(text: &str) -> RuleSet {
    RuleSet::read(text, &folders()).unwrap_or_else(|error| panic!("{error}\n{text}"))
}

/// A set with both spam lists and two rules, its actions in the order they are written.
fn sample() -> RuleSet {
    let mut set = RuleSet::default();
    assert!(set.never_junk("ola@example.test"));
    assert!(set.never_junk_domain("@Partner.Example.test"));
    assert!(set.always_junk("spam@example.test"));
    set.items.push(Item::Rule(rule(
        "Kunder",
        Match::Any,
        vec![
            field(Field::From, Operator::Contains, "kari@example.test"),
            field(Field::Subject, Operator::Contains, "faktura"),
        ],
        vec![Action::MarkRead, Action::MoveTo("INBOX.Kunder".into())],
    )));
    set.items.push(Item::Rule(Rule {
        enabled: false,
        ..rule(
            "Store vedlegg",
            Match::All,
            vec![Condition::SizeOver(5 << 20)],
            vec![Action::Star, Action::CopyTo("Arkiv".into()), Action::Stop],
        )
    }));
    set
}

const SAMPLE: &str = r#"require ["fileinto","copy","imap4flags","environment","imapsieve"];
# Sealwax: never junk
if environment :is "imap.mailbox" "Junk"
{
	if anyof (address :is "from" "ola@example.test", address :domain :is "from" "partner.example.test")
	{
		removeflag "$Junk";
		addflag "$NotJunk";
		fileinto "INBOX";
		stop;
	}
	stop;
}
# Sealwax: always junk
if address :is "from" "spam@example.test"
{
	removeflag "$NotJunk";
	addflag "$Junk";
	fileinto "Junk";
	stop;
}
# rule:[Kunder]
if anyof (header :contains "from" "kari@example.test", header :contains "subject" "faktura")
{
	addflag "\\Seen";
	fileinto "INBOX.Kunder";
}
# rule:[Store vedlegg]
if false # allof (size :over 5M)
{
	addflag "\\Flagged";
	fileinto :copy "Arkiv";
	stop;
}
"#;

#[test]
fn the_canonical_script() {
    assert_eq!(render(&sample()), SAMPLE);
    assert_eq!(read(SAMPLE), sample());
    assert!(!read(SAMPLE).is_hand_written());
}

#[test]
fn an_empty_set_still_keeps_junk_out_of_the_rules() {
    let text = render(&RuleSet::default());
    assert_eq!(
        text,
        "require [\"environment\",\"imapsieve\"];\n# Sealwax: never junk\nif environment :is \
         \"imap.mailbox\" \"Junk\"\n{\n\tstop;\n}\n"
    );
    assert_eq!(read(&text), RuleSet::default());
    assert!(run(&text, &Message::new().mailbox("Junk")).stopped);
}

#[test]
fn actions_are_written_in_a_fixed_order() {
    let mut set = sample();
    let Item::Rule(kunder) = &mut set.items[0] else {
        unreachable!()
    };
    kunder.actions.reverse();
    assert_eq!(render(&set), SAMPLE, "the flag before the fileinto");
}

#[test]
fn the_spam_sections_work() {
    let text = render(&sample());
    let blocked = run(&text, &Message::new().from("Spam <SPAM@example.test>"));
    assert!(!blocked.keep && blocked.stopped);
    assert_eq!(filed(&blocked), ["Junk"]);
    assert_eq!(blocked.filed[0].flags.add, ["$Junk"]);

    let junk = Message::new().mailbox("Junk").flags(&["$Junk"]);
    for sender in ["ola@example.test", "Partner <news@partner.example.test>"] {
        let trusted = run(&text, &junk.clone().from(sender));
        assert!(!trusted.keep && trusted.stopped, "{sender}");
        assert_eq!(filed(&trusted), ["INBOX"]);
        assert_eq!(trusted.filed[0].flags.add, ["$NotJunk"]);
        assert_eq!(trusted.filed[0].flags.remove, ["$Junk"]);
    }
    let spam_from_a_customer = run(&text, &junk.clone().from("kari@example.test"));
    assert!(spam_from_a_customer.keep && spam_from_a_customer.stopped);
    assert!(
        spam_from_a_customer.filed.is_empty(),
        "rules never pull spam out of Junk"
    );
    let subdomain = run(&text, &junk.from("news@mail.partner.example.test"));
    assert!(subdomain.keep, "a domain is that domain only");

    let customer = run(&text, &Message::new().from("Kari <kari@example.test>"));
    assert_eq!(filed(&customer), ["INBOX.Kunder"]);
    assert_eq!(customer.filed[0].flags.add, ["\\Seen"]);
    let large = run(&text, &Message::new().size(6 << 20));
    assert!(
        large.keep && large.filed.is_empty(),
        "a disabled rule doesn't run"
    );
}

#[test]
fn sender_lists() {
    let mut set = RuleSet::default();
    assert!(set.always_junk("  spam@example.test "));
    assert!(!set.always_junk("SPAM@example.test"), "listed already");
    assert!(set.always_junk("<offers@example.test>"));
    assert!(
        !set.always_junk("Kari <kari@example.test>"),
        "not an address alone"
    );
    assert!(!set.always_junk("no-at-sign"));
    assert_eq!(
        set.blocked.addresses,
        ["spam@example.test", "offers@example.test"]
    );
    assert!(
        set.never_junk("spam@example.test"),
        "moves to the trusted list"
    );
    assert_eq!(set.blocked.addresses, ["offers@example.test"]);
    assert_eq!(set.trusted.addresses, ["spam@example.test"]);
    assert!(set.always_junk_domain("@Example.TEST"));
    assert_eq!(set.blocked.domains, ["example.test"]);
    assert!(set.never_junk_domain("example.test"));
    assert!(set.blocked.domains.is_empty());
    assert!(!set.blocked.remove_domain("example.test"));
    assert!(set.trusted.remove_domain("EXAMPLE.test"));
    assert!(!set.trusted.add_domain("two words.test"));
    assert!(!set.trusted.add_domain("@"));
    let mut senders = Senders::default();
    assert!(senders.is_empty());
    assert!(senders.add_address("a@example.test") && senders.remove_address("A@EXAMPLE.TEST"));
    assert!(!senders.remove_address("a@example.test"));
}

#[test]
fn every_condition_and_action_round_trips() {
    let fields = [
        Field::From,
        Field::To,
        Field::Cc,
        Field::ToOrCc,
        Field::Subject,
        Field::ListId,
        Field::ReplyTo,
        Field::Header("x-mailer".into()),
    ];
    let operators = [
        (Operator::Contains, "kari@"),
        (Operator::NotContains, "æøå \"quoted\" back\\slash"),
        (Operator::Is, "kari@example.test"),
        (Operator::IsNot, ""),
        (Operator::BeginsWith, "Re: [x]*?"),
        (Operator::EndsWith, "@example.test"),
        (Operator::Matches, "*@*.example.test"),
    ];
    let mut set = RuleSet::default();
    for field_ in &fields {
        for (operator, value) in operators {
            set.items.push(Item::Rule(rule(
                "",
                Match::Any,
                vec![field(field_.clone(), operator, value)],
                vec![Action::Stop],
            )));
        }
    }
    set.items.push(Item::Rule(rule(
        "Sizes",
        Match::All,
        vec![
            Condition::SizeOver(1),
            Condition::SizeUnder(3 << 30),
            Condition::SizeOver(1536),
            Condition::SizeUnder(0),
        ],
        vec![
            Action::MoveToJunk,
            Action::NotJunk,
            Action::MarkRead,
            Action::Star,
            Action::CopyTo("Kopi".into()),
            Action::MoveTo("INBOX/Kunder/2026".into()),
            Action::Delete,
            Action::Stop,
        ],
    )));
    set.items.push(Item::Rule(rule(
        "Every message",
        Match::All,
        Vec::new(),
        vec![Action::Delete],
    )));
    let text = render(&set);
    assert!(text.contains("size :over 1536"), "{text}");
    assert!(text.contains("size :under 3G"), "{text}");
    assert!(
        text.contains("not address :is [\"to\",\"cc\"] \"\""),
        "{text}"
    );
    assert!(
        text.contains("address :matches \"from\" \"Re: [x]\\\\*\\\\?*\""),
        "{text}"
    );
    assert!(
        text.contains("header :matches \"list-id\" \"*@example.test\""),
        "{text}"
    );
    assert_eq!(read(&text), set);
    compile(&text).expect("checks");
}

#[test]
fn matches_reads_back_as_begins_or_ends_with_when_it_is_one() {
    let mut set = RuleSet::default();
    for value in ["faktura*", "*.pdf", "*", "a\\*", "a*b"] {
        set.items.push(Item::Rule(rule(
            "",
            Match::Any,
            vec![field(Field::Subject, Operator::Matches, value)],
            vec![Action::Stop],
        )));
    }
    let read_back: Vec<(Operator, String)> = read(&render(&set))
        .rules()
        .map(|rule| match &rule.conditions[0] {
            Condition::Field {
                operator, value, ..
            } => (*operator, value.clone()),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        read_back,
        [
            (Operator::BeginsWith, "faktura".to_owned()),
            (Operator::EndsWith, ".pdf".to_owned()),
            (Operator::Matches, "*".to_owned()),
            (Operator::Matches, "a\\*".to_owned()),
            (Operator::Matches, "a*b".to_owned()),
        ]
    );
}

#[test]
fn names_and_values_lose_control_characters() {
    let mut set = RuleSet::default();
    set.items.push(Item::Rule(rule(
        "Tab\there]",
        Match::Any,
        vec![field(
            Field::Header("X-Topic".into()),
            Operator::Is,
            "two\nlines",
        )],
        vec![Action::Stop],
    )));
    let text = render(&set);
    assert!(text.contains("# rule:[Tab here]]\n"), "{text}");
    assert!(
        text.contains("header :is \"X-Topic\" \"two lines\""),
        "{text}"
    );
    let read_back = read(&text);
    let rule = read_back.rules().next().expect("a rule");
    assert_eq!(rule.name, "Tab here]");
    assert!(
        rule.conditions[0] == field(Field::Header("x-topic".into()), Operator::Is, "two lines")
    );
}

/// Rules as Roundcube's managesieve plugin writes them.
const ROUNDCUBE: &str = r#"require ["fileinto","copy","imap4flags"];
# rule:[Kunder]
if anyof (header :contains "from" "kari@example.test", header :contains "subject" "faktura")
{
	fileinto "INBOX.Kunder";
}
# rule:[Nyhetsbrev]
if allof (header :contains "list-id" "nyheter.example.test")
{
	addflag "\\Seen";
	fileinto "INBOX.Nyheter";
	stop;
}
# rule:[Av]
if false # header :contains "subject" "test"
{
	discard;
	stop;
}
# rule:[Alle]
if true
{
	fileinto :copy "Arkiv";
}
# rule:[Store]
if not size :under 10M
{
	stop;
}
"#;

#[test]
fn roundcube_rules_are_recognised() {
    let set = read(ROUNDCUBE);
    let expected = [
        rule(
            "Kunder",
            Match::Any,
            vec![
                field(Field::From, Operator::Contains, "kari@example.test"),
                field(Field::Subject, Operator::Contains, "faktura"),
            ],
            vec![Action::MoveTo("INBOX.Kunder".into())],
        ),
        rule(
            "Nyhetsbrev",
            Match::All,
            vec![field(
                Field::ListId,
                Operator::Contains,
                "nyheter.example.test",
            )],
            vec![
                Action::MarkRead,
                Action::MoveTo("INBOX.Nyheter".into()),
                Action::Stop,
            ],
        ),
        Rule {
            enabled: false,
            ..rule(
                "Av",
                Match::Any,
                vec![field(Field::Subject, Operator::Contains, "test")],
                vec![Action::Delete, Action::Stop],
            )
        },
        rule(
            "Alle",
            Match::All,
            Vec::new(),
            vec![Action::CopyTo("Arkiv".into())],
        ),
    ];
    let rules: Vec<Rule> = set.rules().cloned().collect();
    assert!(rules[..4] == expected, "{rules:?}");
    assert_eq!(
        rules.len(),
        4,
        "`not size` isn't a condition the dialog has"
    );
}

#[test]
fn not_size_is_hand_written_but_kept() {
    let set = read(ROUNDCUBE);
    let Some(Item::HandWritten(text)) = set.items.last() else {
        panic!("{set:?}");
    };
    assert_eq!(
        text,
        "# rule:[Store]\nif not size :under 10M\n{\n\tstop;\n}"
    );
    assert!(set.requires.is_empty(), "it needs no extension");
}

/// Hand-written Sieve between rules the model holds.
const HAND_WRITTEN: &str = r#"# Rules for the Kunder account
require ["fileinto", "imap4flags"];
# Written by hand
if header :contains "subject" "[lists]" { fileinto "Lists"; } elsif exists "list-id" { fileinto "Lists"; }

# A note about the next rule
# rule:[Kunder]
if header :contains "from" "kari@example.test"
{
	fileinto "INBOX.Kunder";
}
# rule:[Odd order]
if true
{
	fileinto "A";
	addflag "\\Seen";
}
if header :is "from" "ola@example.test" { fileinto "Ola"; }
# rule:[Note inside]
if header :contains "subject" "x"
{
	# why
	stop;
}
if false { stop; }
if false # header :contains "subject"
{
	stop;
}
# trailing note
"#;

#[test]
fn hand_written_sieve_keeps_its_place_and_text() {
    let set = read(HAND_WRITTEN);
    assert!(set.is_hand_written());
    let items: Vec<String> = set
        .items
        .iter()
        .map(|item| match item {
            Item::Rule(rule) => format!("rule {}", rule.name),
            Item::HandWritten(text) => text.clone(),
        })
        .collect();
    let odd_order = HAND_WRITTEN
        .find("# rule:[Odd order]")
        .expect("in the text");
    assert_eq!(
        items,
        [
            "# Rules for the Kunder account\n# Written by hand\nif header :contains \"subject\" \
             \"[lists]\" { fileinto \"Lists\"; } elsif exists \"list-id\" { fileinto \"Lists\"; \
             }\n# A note about the next rule",
            "rule Kunder",
            HAND_WRITTEN[odd_order..].trim_end(),
        ],
        "hand-written text between two rules is one item"
    );
    let text = render(&set);
    assert!(
        text.starts_with("require [\"fileinto\",\"environment\",\"imapsieve\",\"imap4flags\"];\n"),
        "{text}"
    );
    assert_eq!(read(&text), set);
}

#[test]
fn reading_and_writing_again_never_changes_what_a_script_does() {
    let messages = [
        Message::new()
            .from("kari@example.test")
            .header("Subject", "[lists] x"),
        Message::new()
            .from("Ola <ola@example.test>")
            .header("List-Id", "<a.example.test>"),
        Message::new().from("spam@example.test").flags(&["\\Seen"]),
        Message::new().header("Subject", "x").size(20 << 20),
    ];
    for text in [
        SAMPLE,
        ROUNDCUBE,
        HAND_WRITTEN,
        super::language::RFC_EXAMPLE,
    ] {
        let first = read(text);
        let written = render(&first);
        assert_eq!(read(&written), first, "{written}");
        for message in &messages {
            assert_eq!(
                evaluate(&compile(text).expect("checks"), message),
                evaluate(&compile(&written).expect("checks"), message),
                "{written}"
            );
        }
    }
}

#[test]
fn sections_need_their_marker_and_their_place() {
    let unmarked = read(&SAMPLE.replace("# Sealwax: never junk\n", ""));
    assert!(unmarked.trusted.is_empty() && unmarked.blocked.is_empty());
    assert!(unmarked.is_hand_written());
    let always_junk_first = format!(
        "require [\"fileinto\", \"copy\", \"imap4flags\"];\n{}",
        &SAMPLE[SAMPLE.find("# Sealwax: always junk").expect("a marker")..]
    );
    let set = read(&always_junk_first);
    assert!(
        set.blocked.addresses == ["spam@example.test"],
        "the always-junk rule may come first"
    );
    assert!(!set.is_hand_written());

    let late = format!(
        "require [\"fileinto\", \"copy\", \"imap4flags\"];\n# rule:[First]\nif true\n{{\n\tstop;\n}}\n{}",
        SAMPLE.split_once("# Sealwax: always junk\n").expect("a marker").1
    )
    .replacen("if address", "# Sealwax: always junk\nif address", 1);
    let set = read(&late);
    assert!(
        set.blocked.is_empty(),
        "after a rule, it is a rule like any other"
    );
    assert_eq!(set.rules().count(), 4);

    let renamed = read(&SAMPLE.replace("\"Junk\"", "\"Spam\""));
    assert!(renamed.trusted.is_empty() && renamed.blocked.is_empty());
    assert!(
        renamed.is_hand_written(),
        "another Junk folder than the account's"
    );
}

#[test]
fn what_cannot_be_written() {
    let mut set = RuleSet::default();
    set.items.push(Item::HandWritten("keep;".into()));
    set.items.push(Item::Rule(rule(
        "",
        Match::Any,
        vec![field(Field::Header("X Bad".into()), Operator::Is, "x")],
        vec![Action::Stop],
    )));
    assert_eq!(
        set.render(&folders()),
        Err(Invalid {
            item: Some(1),
            reason: Reason::HeaderName,
        })
    );
    let bad_folder = rule(
        "",
        Match::Any,
        Vec::new(),
        vec![Action::MoveTo(String::new())],
    );
    assert_eq!(bad_folder.validate(), Err(Reason::Folder));
    set.items[1] = Item::Rule(bad_folder);
    assert_eq!(
        set.render(&folders()).map_err(|invalid| invalid.reason),
        Err(Reason::Folder)
    );
    set.items.truncate(1);
    let no_junk = Folders {
        junk: String::new(),
        ..folders()
    };
    assert_eq!(
        set.render(&no_junk),
        Err(Invalid {
            item: None,
            reason: Reason::Folder,
        })
    );
    set.blocked.addresses.push("not an address".into());
    assert_eq!(
        set.render(&folders()).map_err(|invalid| invalid.reason),
        Err(Reason::Sender)
    );
}

#[test]
fn custom_header_names_read_back_in_lower_case() {
    let mut set = RuleSet::default();
    set.items.push(Item::Rule(rule(
        "",
        Match::Any,
        vec![field(
            Field::Header("X-Spam-Flag".into()),
            Operator::Is,
            "YES",
        )],
        vec![Action::MoveToJunk],
    )));
    let read_back = read(&render(&set));
    let rule = read_back.rules().next().expect("a rule");
    assert!(rule.conditions[0] == field(Field::Header("x-spam-flag".into()), Operator::Is, "YES"));
    assert!(rule.actions == [Action::MoveToJunk]);
}

#[test]
fn a_script_that_does_not_check_has_no_rules() {
    let error = RuleSet::read("require \"vacation\";", &folders()).expect_err("refused");
    assert_eq!((error.line, error.column), (1, 9));
}

#[test]
fn rule_sets_never_show_names_in_debug() {
    let debug = format!("{:?}", read(HAND_WRITTEN));
    let sample = format!("{:?}", sample());
    for text in [debug, sample] {
        for secret in [
            "kari", "ola", "Kunder", "faktura", "spam@", "Arkiv", "lists",
        ] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
}

fn arbitrary_field() -> impl Strategy<Value = Field> {
    prop_oneof![
        Just(Field::From),
        Just(Field::To),
        Just(Field::Cc),
        Just(Field::ToOrCc),
        Just(Field::Subject),
        Just(Field::ListId),
        Just(Field::ReplyTo),
        "x-[a-z]{1,6}".prop_map(Field::Header),
    ]
}

fn arbitrary_condition() -> impl Strategy<Value = Condition> {
    let operator = prop_oneof![
        Just(Operator::Contains),
        Just(Operator::NotContains),
        Just(Operator::Is),
        Just(Operator::IsNot),
        Just(Operator::BeginsWith),
        Just(Operator::EndsWith),
        Just(Operator::Matches),
    ];
    prop_oneof![
        (
            arbitrary_field(),
            operator,
            "[a-zA-Z0-9 æøå*?\\\\\"@.:#\\]-]{0,10}"
        )
            .prop_map(|(field, operator, value)| Condition::Field {
                field,
                operator,
                value,
            }),
        any::<u64>().prop_map(Condition::SizeOver),
        (0_u64..1 << 40).prop_map(Condition::SizeUnder),
    ]
}

fn arbitrary_action() -> impl Strategy<Value = Action> {
    let folder = "[A-Za-z][A-Za-z0-9 ./æ]{0,10}";
    prop_oneof![
        folder.prop_map(Action::MoveTo),
        folder.prop_map(Action::CopyTo),
        Just(Action::MarkRead),
        Just(Action::Star),
        Just(Action::MoveToJunk),
        Just(Action::NotJunk),
        Just(Action::Delete),
        Just(Action::Stop),
        Just(Action::MoveTo("Junk".into())),
        Just(Action::MoveTo("INBOX".into())),
    ]
}

fn arbitrary_rule() -> impl Strategy<Value = Rule> {
    (
        "[A-Za-zæøå0-9 \\[\\]#:]{0,10}",
        any::<bool>(),
        prop_oneof![Just(Match::All), Just(Match::Any)],
        prop::collection::vec(arbitrary_condition(), 0..4),
        prop::collection::vec(arbitrary_action(), 0..5),
    )
        .prop_map(|(name, enabled, matching, conditions, actions)| Rule {
            name,
            enabled,
            matching,
            conditions,
            actions,
        })
}

fn arbitrary_senders() -> impl Strategy<Value = Senders> {
    (
        prop::collection::vec("[a-z]{1,6}@[a-z]{1,6}\\.test", 0..3),
        prop::collection::vec("[a-z]{1,6}\\.test", 0..3),
    )
        .prop_map(|(addresses, domains)| Senders { addresses, domains })
}

proptest! {
    #[test]
    fn rule_sets_read_back_as_written(
        trusted in arbitrary_senders(),
        blocked in arbitrary_senders(),
        rules in prop::collection::vec(arbitrary_rule(), 0..6),
    ) {
        let set = RuleSet {
            trusted,
            blocked,
            items: rules.into_iter().map(Item::Rule).collect(),
            requires: Vec::new(),
        };
        let text = render(&set);
        let read_back = read(&text);
        prop_assert!(!read_back.is_hand_written(), "{}", text);
        prop_assert!(read_back.trusted == set.trusted && read_back.blocked == set.blocked);
        prop_assert_eq!(read_back.items.len(), set.items.len());
        for (written, read) in set.rules().zip(read_back.rules()) {
            prop_assert_eq!(&written.name, &read.name);
            prop_assert_eq!(written.enabled, read.enabled);
            prop_assert_eq!(written.conditions.len(), read.conditions.len());
            prop_assert_eq!(written.actions.len(), read.actions.len());
        }
        prop_assert_eq!(render(&read_back), text.clone());
        prop_assert!(read(&text) == read_back);
    }
}
