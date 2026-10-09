//! The grammar (RFC 5228 section 8), the subset `check` accepts, errors with their positions,
//! and the limits.

use super::super::lexer::{Lexer, Token};
use super::super::{MAX_NESTING, MAX_SCRIPT_BYTES, compile, parse};
use super::{Message, error, filed, run, script};

fn tokens(text: &str) -> Vec<Token> {
    let mut lexer = Lexer::new(text);
    let mut tokens = Vec::new();
    loop {
        let lexed = lexer.next().unwrap_or_else(|error| panic!("{error}"));
        if lexed.token == Token::End {
            return tokens;
        }
        tokens.push(lexed.token);
    }
}

fn string(text: &str) -> Token {
    Token::String(text.to_owned())
}

#[test]
fn comments_are_white_space() {
    assert_eq!(
        tokens("keep # to the end\n/* across\nlines */ ; /**/stop;"),
        [
            Token::Identifier("keep".into()),
            Token::Semicolon,
            Token::Identifier("stop".into()),
            Token::Semicolon,
        ]
    );
    let tree = parse("# one\nkeep; /* two\n */ # three\r\n").expect("parses");
    let texts: Vec<_> = tree
        .comments
        .iter()
        .map(|comment| (comment.hash, comment.line, comment.text.as_str()))
        .collect();
    assert_eq!(
        texts,
        [
            (true, 1, " one"),
            (false, 2, " two\n "),
            (true, 3, " three")
        ]
    );
}

#[test]
fn quoted_strings_unescape() {
    assert_eq!(
        tokens(
            r#""a\"b\\c\d" "two
lines""#
        ),
        [string("a\"b\\cd"), string("two\nlines")]
    );
    let outcome = run(
        "require \"fileinto\";\nif header :is \"subject\" \"say \\\"hi\\\" \\\\o/\" { fileinto \"hit\"; }",
        &Message::new().header("Subject", "say \"hi\" \\o/"),
    );
    assert_eq!(filed(&outcome), ["hit"]);
}

#[test]
fn multi_line_strings_unstuff_dots() {
    assert_eq!(
        tokens("text:\nline one\n..dotted\n.not the end\n.\n;"),
        [
            string("line one\n.dotted\n.not the end\n"),
            Token::Semicolon
        ]
    );
    assert_eq!(
        tokens("TEXT: # a comment\r\nfirst\r\n..\r\n.\r\n"),
        [string("first\r\n.\r\n")]
    );
    assert_eq!(tokens("text:\n.\n"), [string("")]);
    let comments = parse("keep; # x\nif header :contains \"subject\" text: # why\n.\n{ }")
        .expect("parses")
        .comments;
    assert_eq!(comments.len(), 2);
}

#[test]
fn numbers_take_quantifiers() {
    assert_eq!(
        tokens("10 10k 2M 1g 007"),
        [
            Token::Number(10),
            Token::Number(10 << 10),
            Token::Number(2 << 20),
            Token::Number(1 << 30),
            Token::Number(7),
        ]
    );
    assert_eq!(error("if size :over 99999999999999999999 { }").0, 1);
    let (line, column, message) = error("if size :over 20000000000G { }");
    assert_eq!((line, column), (1, 15));
    assert!(message.contains("too large"), "{message}");
}

#[test]
fn identifiers_and_tags_ignore_case() {
    let outcome = run(
        "REQUIRE \"fileinto\"; IF ANYOF (HEADER :CONTAINS \"SUBJECT\" \"x\", FALSE) { FILEINTO \"hit\"; }",
        &Message::new().header("Subject", "an x"),
    );
    assert_eq!(filed(&outcome), ["hit"]);
}

#[test]
fn tagged_arguments_may_come_anywhere() {
    let outcome = run(
        "require \"fileinto\";\nif header \"subject\" :comparator \"i;octet\" \"x\" :contains { \
         fileinto \"hit\"; }",
        &Message::new().header("Subject", "an x"),
    );
    assert_eq!(filed(&outcome), ["hit"]);
}

#[test]
fn string_lists_tests_and_blocks() {
    let text = r#"
require ["fileinto", "copy"];
if allof (header :contains ["subject", "x-topic"] ["a", "b"], not exists "x-spam") {
    if size :under 10K { fileinto :copy "small"; }
    fileinto "both";
} elsif true { stop; } else { keep; }
"#;
    let outcome = run(text, &Message::new().header("X-Topic", "b").size(500));
    assert_eq!(filed(&outcome), ["small", "both"]);
    assert!(!outcome.keep);
}

/// RFC 5228 section 9's example, with fictional addresses: comments in every place, upper-case
/// tags and identifiers, `elsif` chains, a test list across lines.
pub(super) const RFC_EXAMPLE: &str = r#"#
# Example Sieve Filter
# Declare any optional features or extension used by the script
#
require ["fileinto"];

#
# Handle messages from known mailing lists
# Move messages from IETF filter discussion list to filter mailbox
#
if header :is "Sender" "owner-ietf-mta-filters@lists.example.test"
        {
        fileinto "filter";  # move to "filter" mailbox
        }
#
# Keep all messages to or from people in my company
#
elsif address :DOMAIN :is ["From", "To"] "example.test"
        {
        keep;               # keep in "In" mailbox
        }

#
# Try and catch unsolicited email.  If a message is not to me,
# or it contains a subject known to be spam, file it away.
#
elsif anyof (NOT address :all :contains
               ["To", "Cc", "Bcc"] "me@home.example.test",
             header :matches "subject"
               ["*make*money*fast*", "*university*dipl*mas*"])
        {
        fileinto "spam";   # move to "spam" mailbox
        }
else
        {
        # Move all other (non-company) mail to "personal"
        # mailbox.
        fileinto "personal";
        }
"#;

#[test]
fn rfc_5228_examples_check() {
    let checked = script(RFC_EXAMPLE);
    assert_eq!(checked.requires(), ["fileinto"]);
    assert_eq!(
        checked.commands.len(),
        2,
        "require, then one if with its elsifs"
    );
    for text in [
        // Section 3.1, with fileinto for redirect.
        "require \"fileinto\";\nif header :contains [\"From\"] [\"coyote\"] {\n   fileinto \"acme\";\n} \
         elsif header :contains \"Subject\" \"$$$\" {\n   fileinto \"money\";\n} else {\n   \
         fileinto \"field\";\n}",
        // Section 3.3.
        "if header :contains \"subject\" \"test\" { stop; }",
        // Section 4.1.
        "require \"fileinto\";\nif header :contains [\"from\"] \"coyote\" {\n   fileinto \
         \"INBOX.harassment\";\n}",
        // Section 5.1.
        "if address :is :all \"from\" \"tim@example.test\" {\n   discard;\n}",
        // Section 5.2.
        "if allof (header :contains \"From\" \"Cream\",\n          header :contains \"Subject\" \
         \"Cheese\") { keep; }",
        // Section 5.5.
        "if exists [\"From\",\"Date\"] { keep; }",
        // Section 5.7.
        "if header :contains [\"To\", \"Cc\"] [\"me@example.test\", \"me00@landru.example.test\"] \
         { keep; }",
        // Section 5.9.
        "if size :over 500K { discard; }",
        // RFC 5232's example.
        "require [\"imap4flags\", \"fileinto\"];\nif header :contains \"from\" \
         \"boss@frobnitzm.example.test\" {\n    setflag \"\\\\Flagged\";\n    fileinto \"From \
         Boss\";\n}",
        // RFC 3894's.
        "require [\"copy\", \"fileinto\"];\nfileinto :copy \"incoming\";",
        // RFC 5183 with RFC 6785's item.
        "require [\"environment\", \"imapsieve\"];\nif environment :is \"imap.mailbox\" \"Junk\" { \
         stop; }",
        // The comparators may be required.
        "require [\"comparator-i;octet\", \"comparator-i;ascii-casemap\"];\nif header \
         :comparator \"i;octet\" :is \"subject\" \"X\" { keep; }",
    ] {
        script(text);
    }
}

#[test]
fn grammar_errors_have_positions() {
    let cases: &[(&str, (u32, u32), &str)] = &[
        (
            "require \"fileinto\";\nfileinto \"a\"\nkeep;",
            (3, 1),
            "a `;` may be missing",
        ),
        ("if header :is \"subject\" \"abc", (1, 25), "never ends"),
        ("keep;\n  /* never", (2, 3), "never ends"),
        ("keep; }", (1, 7), "a command is expected"),
        ("keep :", (1, 6), "tag name"),
        ("keep; @", (1, 7), "doesn't allow"),
        (
            "if true {\n keep;\n",
            (3, 1),
            "closing the block from line 1",
        ),
        ("require [];", (1, 10), "a string is expected"),
        ("if anyof () { }", (1, 11), "a test is expected"),
        ("if header \"a\" \"b\" ", (1, 19), "`;` or a block"),
        ("if text:\nx\n", (1, 4), "closing `.` line"),
        ("if text: x\n.\n", (1, 4), "must end its line"),
    ];
    for (text, at, words) in cases {
        let (line, column, message) = error(text);
        assert_eq!((line, column), *at, "{text:?}: {message}");
        assert!(message.contains(words), "{text:?}: {message}");
    }
}

#[test]
fn the_subset_names_what_it_refuses() {
    let cases: &[(&str, (u32, u32), &str)] = &[
        ("frobnicate;", (1, 1), "unknown command `frobnicate`"),
        ("if frob { }", (1, 4), "unknown test `frob`"),
        (
            "require [\"fileinto\", \"vacation\"];",
            (1, 9),
            "\"vacation\": its rules never send",
        ),
        (
            "redirect \"someone@example.test\";",
            (1, 1),
            "never send mail",
        ),
        ("set \"a\" \"b\";", (1, 1), "the variables extension"),
        (
            "if envelope :is \"from\" \"x\" { }",
            (1, 4),
            "`envelope` test",
        ),
        ("if body :contains \"x\" { }", (1, 4), "`body` test"),
        ("if header :regex \"subject\" \"x\" { }", (1, 11), "regex"),
        (
            "if header :count \"ge\" \"subject\" \"1\" { }",
            (1, 11),
            "relational",
        ),
        ("if address :user \"from\" \"x\" { }", (1, 12), "subaddress"),
        (
            "if header :is :contains \"subject\" \"x\" { }",
            (1, 15),
            "twice",
        ),
        (
            "if header :comparator \"i;unicode-casemap\" \"subject\" \"x\" { }",
            (1, 23),
            "not \"i;unicode-casemap\"",
        ),
        (
            "if address \"subject\" \"x\" { }",
            (1, 12),
            "only headers that hold addresses",
        ),
        ("if header \"sub ject\" \"x\" { }", (1, 11), "a header name"),
        ("if exists \"a:b\" { }", (1, 11), "a header name"),
        ("elsif true { }", (1, 1), "without an `if`"),
        ("else { }", (1, 1), "without an `if`"),
        ("if false { } else if true { }", (1, 19), "use `elsif`"),
        (
            "keep;\nrequire \"fileinto\";",
            (2, 1),
            "before every other command",
        ),
        (
            "if true { require \"fileinto\"; }",
            (1, 11),
            "before every other command",
        ),
        ("fileinto \"a\";", (1, 1), "`require \"fileinto\";`"),
        (
            "require \"fileinto\";\nfileinto :copy \"a\";",
            (2, 10),
            "`require \"copy\";`",
        ),
        ("addflag \"\\\\Seen\";", (1, 1), "`require \"imap4flags\";`"),
        (
            "if environment :is \"imap.mailbox\" \"x\" { }",
            (1, 4),
            "`require \"environment\";`",
        ),
        (
            "require \"fileinto\";\nfileinto [\"a\"];",
            (2, 10),
            "not a list",
        ),
        (
            "require \"fileinto\";\nfileinto \"\";",
            (2, 10),
            "can't be empty",
        ),
        (
            "require \"fileinto\";\nfileinto \"a\" \"b\";",
            (2, 1),
            "one folder name",
        ),
        (
            "require \"imap4flags\";\naddflag \"(bad\";",
            (2, 9),
            "IMAP keyword",
        ),
        (
            "require \"imap4flags\";\naddflag \"\\\\Deleted\";",
            (2, 9),
            "use `discard`",
        ),
        (
            "require \"imap4flags\";\naddflag \"\\\\Recent\";",
            (2, 9),
            "a system flag other",
        ),
        (
            "require \"imap4flags\";\naddflag \"name\" \"\\\\Seen\";",
            (2, 1),
            "variables",
        ),
        (
            "require \"imap4flags\";\nif hasflag \"v\" \"\\\\Seen\" { }",
            (2, 4),
            "variables",
        ),
        (
            "require [\"fileinto\", \"imap4flags\"];\nfileinto :flags \"\\\\Seen\" \"a\";",
            (2, 10),
            "`:flags`",
        ),
        (
            "require [\"fileinto\"];\nfileinto :create \"a\";",
            (2, 10),
            "mailbox",
        ),
        ("if (true) { }", (1, 4), "takes one test"),
        ("if not (true, false) { }", (1, 8), "not a list"),
        ("if allof true { }", (1, 4), "list of tests"),
        ("if size 100 { }", (1, 4), "`:over` or `:under`"),
        ("if size :over \"1\" { }", (1, 15), "a number"),
        ("if size :over :under 1 { }", (1, 15), "twice"),
        ("if true \"x\" { }", (1, 9), "takes no arguments"),
        ("if true { keep \"x\"; }", (1, 16), "takes no arguments"),
        ("if true { keep { } }", (1, 11), "not a block"),
        ("if true;", (1, 1), "needs a block"),
        ("if { }", (1, 1), "needs a test"),
        ("if header :is \"subject\" { }", (1, 4), "two arguments"),
        (
            "if header :is \"subject\" \"x\" true { }",
            (1, 29),
            "takes no test",
        ),
    ];
    for (text, at, words) in cases {
        let (line, column, message) = error(text);
        assert_eq!((line, column), *at, "{text:?}: {message}");
        assert!(message.contains(words), "{text:?}: {message}");
    }
}

#[test]
fn every_refused_extension_is_named() {
    for name in [
        "envelope",
        "body",
        "variables",
        "vacation",
        "reject",
        "redirect",
        "regex",
        "relational",
        "spamtest",
        "date",
        "editheader",
        "include",
        "mailbox",
        "subaddress",
        "comparator-i;unicode-casemap",
    ] {
        let (line, column, message) = error(&format!("require \"{name}\";"));
        assert_eq!((line, column), (1, 9));
        assert!(message.contains(&format!("\"{name}\"")), "{message}");
    }
    let (_, _, message) = error("require \"kari@example.test with spaces\";");
    assert_eq!(message, "an extension Sealwax doesn't know");
}

#[test]
fn errors_show_where_but_debug_never_what() {
    let error = compile("keep;\nfrobnicate;").expect_err("an unknown command");
    assert_eq!(
        error.to_string(),
        "line 2, column 1: unknown command `frobnicate`"
    );
    let debug = format!("{error:?}");
    assert!(
        debug.contains("line: 2") && !debug.contains("frobnicate"),
        "{debug}"
    );
}

#[test]
fn scripts_over_a_mebibyte_are_refused() {
    let largest = format!("#{}", "a".repeat(MAX_SCRIPT_BYTES - 1));
    assert!(script(&largest).is_empty());
    let (line, column, message) = error(&format!("{largest}a"));
    assert_eq!((line, column), (1, 1));
    assert!(message.contains("1 MiB"), "{message}");
}

#[test]
fn blocks_nest_at_most_32_deep() {
    let nested = |depth: usize| format!("{}{}", "if true {\n".repeat(depth), "}".repeat(depth));
    script(&nested(MAX_NESTING));
    let (line, column, message) = error(&nested(MAX_NESTING + 1));
    assert_eq!((line, column), (33, 9), "{message}");
    assert!(message.contains("32 levels"), "{message}");
}

#[test]
fn tests_nest_at_most_32_deep() {
    let nested = |nots: usize| format!("if {}true {{ }}", "not ".repeat(nots));
    script(&nested(MAX_NESTING - 1));
    let (line, column, message) = error(&nested(MAX_NESTING));
    assert_eq!((line, column), (1, 4 + 4 * MAX_NESTING as u32), "{message}");
    assert!(message.contains("32 levels"), "{message}");
    let lists = |depth: usize| {
        format!(
            "if {}true{} {{ }}",
            "anyof(".repeat(depth),
            ")".repeat(depth)
        )
    };
    script(&lists(MAX_NESTING - 1));
    assert!(error(&lists(MAX_NESTING)).2.contains("32 levels"));
}

#[test]
fn a_script_lists_the_headers_it_reads() {
    let checked = script(
        "if anyof (header :contains \"X-Spam\" \"yes\", not address :domain \"From\" \"example.test\") \
         { if exists [\"Date\", \"x-spam\"] { keep; } } else { if size :over 1M { keep; } }",
    );
    assert_eq!(checked.headers(), ["date", "from", "x-spam"]);
    assert!(!checked.is_empty());
    assert!(script("require \"fileinto\";").is_empty());
}
