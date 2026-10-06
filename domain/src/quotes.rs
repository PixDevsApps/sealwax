//! The quoted history at the end of a plain-text reply (DESIGN.md "HTML mail inside the
//! theme": quotes that repeat earlier messages collapse behind "…"): a trailing block of `>`
//! lines with the attribution line before it ("On … wrote:", "Den … skrev …:"), or Outlook's
//! header block ("From: … Sent: …") and everything after it. Quotes between the reply's own
//! paragraphs stay: the reply answers them.

/// Verbs that end an attribution line, as mail programs write it in English, the Nordic
/// languages, German, Dutch, French, Spanish, Italian, Portuguese, Finnish and Polish.
const WROTE: [&str; 12] = [
    "wrote",
    "skrev",
    "schrieb",
    "schreef",
    "a écrit",
    "escribió",
    "ha scritto",
    "escreveu",
    "kirjoitti",
    "napisał",
    "napisała",
    "написал",
];

/// How an attribution line may start when a long one is wrapped onto two lines.
const OPENERS: [&str; 11] = [
    "on ", "den ", "på ", "am ", "le ", "el ", "op ", "il ", "em ", "w dniu ", "the ",
];

/// Header names that start Outlook's block, and those that must follow within a few lines.
const FROM: [&str; 9] = [
    "from:", "fra:", "från:", "von:", "de :", "de:", "van:", "da:", "od:",
];
const SENT: [&str; 12] = [
    "sent:",
    "sendt:",
    "skickat:",
    "gesendet:",
    "envoyé :",
    "envoyé:",
    "verzonden:",
    "inviato:",
    "enviado:",
    "date:",
    "dato:",
    "datum:",
];

/// Lines Outlook and others put above the original message.
const SEPARATORS: [&str; 7] = [
    "-----original message-----",
    "-----opprinnelig melding-----",
    "-----ursprungligt meddelande-----",
    "-----oprindelig meddelelse-----",
    "-----ursprüngliche nachricht-----",
    "-----message d'origine-----",
    "-----mensaje original-----",
];

/// The collapsible end of a plain-text body: from the byte `start` (the beginning of a line)
/// to the end of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote {
    pub start: usize,
    /// Lines from `start` to the last line with text.
    pub lines: usize,
}

/// The trailing quoted history of `text`, if there is one and some text of the reply's own
/// comes before it.
pub fn trailing_quote(text: &str) -> Option<Quote> {
    let lines = lines(text);
    let last = lines
        .iter()
        .rposition(|line| !line.text.trim().is_empty())?;
    let quoted = quoted_block(&lines, last);
    let header = header_block(&lines, last);
    let first = match (quoted, header) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    let own_text = lines[..first]
        .iter()
        .any(|line| !line.text.trim().is_empty());
    own_text.then(|| Quote {
        start: lines[first].start,
        lines: last - first + 1,
    })
}

struct Line<'a> {
    start: usize,
    text: &'a str,
}

fn lines(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for piece in text.split_inclusive('\n') {
        let line = piece.trim_end_matches(['\n', '\r']);
        lines.push(Line { start, text: line });
        start += piece.len();
    }
    lines
}

fn is_quoted(line: &str) -> bool {
    line.trim_start().starts_with('>')
}

/// The first line of the trailing run of `>` lines that ends at `last`, with the attribution
/// before it.
fn quoted_block(lines: &[Line<'_>], last: usize) -> Option<usize> {
    if !is_quoted(lines[last].text) {
        return None;
    }
    let mut first = last;
    let mut at = last;
    while at > 0 {
        at -= 1;
        let line = lines[at].text;
        if is_quoted(line) {
            first = at;
        } else if !line.trim().is_empty() {
            break;
        }
    }
    Some(attribution(lines, first).unwrap_or(first))
}

/// The attribution line just above `first` (one blank line may sit between), and the line
/// before it when a long attribution was wrapped.
fn attribution(lines: &[Line<'_>], first: usize) -> Option<usize> {
    let mut at = first.checked_sub(1)?;
    if lines[at].text.trim().is_empty() {
        at = at.checked_sub(1)?;
    }
    if !is_attribution(lines[at].text) {
        return None;
    }
    let opens = |line: &str| {
        let line = line.trim_start().to_lowercase();
        OPENERS.iter().any(|opener| line.starts_with(opener))
    };
    if !opens(lines[at].text)
        && let Some(previous) = at.checked_sub(1)
        && opens(lines[previous].text)
        && !is_quoted(lines[previous].text)
    {
        return Some(previous);
    }
    Some(at)
}

fn is_attribution(line: &str) -> bool {
    let line = line.trim();
    if !line.ends_with(':') || line.chars().count() > 300 || is_quoted(line) {
        return false;
    }
    let lower = line.to_lowercase();
    WROTE.iter().any(|verb| {
        lower.match_indices(verb).any(|(index, _)| {
            let before = lower[..index].chars().next_back();
            let after = lower[index + verb.len()..].chars().next();
            before.is_none_or(|c| !c.is_alphanumeric())
                && after.is_none_or(|c| !c.is_alphanumeric() && c != '-')
        })
    })
}

/// The first line of Outlook's header block for the original message ("From: …" with "Sent:"
/// or "Date:" within the next five lines), or of the separator line above it (blank lines may
/// sit between).
fn header_block(lines: &[Line<'_>], last: usize) -> Option<usize> {
    let starts = |line: &str, names: &[&str]| {
        let line = line.trim_start().to_lowercase();
        let line = line.trim_start_matches('*');
        names.iter().any(|name| line.starts_with(name))
    };
    let from = (0..=last).find(|&at| {
        starts(lines[at].text, &FROM)
            && lines[at + 1..=last.min(at + 5)]
                .iter()
                .any(|line| starts(line.text, &SENT))
    })?;
    let separator = |line: &str| {
        let line = line.trim().to_lowercase();
        SEPARATORS.contains(&line.as_str())
            || (line.len() >= 10 && line.chars().all(|c| c == '_' || c == '-'))
    };
    let above = lines[..from]
        .iter()
        .rposition(|line| !line.text.trim().is_empty());
    Some(
        above
            .filter(|&at| separator(lines[at].text))
            .unwrap_or(from),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collapsed(text: &str) -> Option<(&str, &str)> {
        trailing_quote(text).map(|quote| (&text[..quote.start], &text[quote.start..]))
    }

    #[test]
    fn a_gmail_reply_collapses_its_attribution_and_quote() {
        let text = "Sounds good, see you at noon.\n\nTomas\n\nOn Mon, 5 Oct 2026 at 10:00, Ada Lovelace <ada@example.com> wrote:\n\n> Lunch Thursday?\n> By the fountain.\n";
        let (kept, quote) = collapsed(text).unwrap();
        assert_eq!(kept, "Sounds good, see you at noon.\n\nTomas\n\n");
        assert!(quote.starts_with("On Mon, 5 Oct 2026"));
        assert_eq!(trailing_quote(text).unwrap().lines, 4);
    }

    #[test]
    fn nordic_and_german_attributions() {
        for attribution in [
            "Den man. 5. okt. 2026 kl. 10:00 skrev Ada Lovelace <ada@example.com>:",
            "Den mån 5 okt. 2026 kl 10:00 skrev Ada <ada@example.com>:",
            "Am Mo., 5. Okt. 2026 um 10:00 Uhr schrieb Ada <ada@example.com>:",
            "Le lun. 5 oct. 2026 à 10:00, Ada <ada@example.com> a écrit :",
            "Ada Lovelace skrev 05.10.2026 10:00:",
        ] {
            let text = format!("Takk!\n\n{attribution}\n> Hei\n");
            let (kept, quote) = collapsed(&text).unwrap();
            assert_eq!(kept, "Takk!\n\n", "{attribution}");
            assert!(quote.starts_with(attribution));
        }
    }

    #[test]
    fn a_wrapped_attribution_collapses_both_lines() {
        let text = "Yes.\n\nOn Mon, 5 Oct 2026 at 10:00, Ada Lovelace\n<ada@example.com> wrote:\n> Coming?\n";
        let (kept, quote) = collapsed(text).unwrap();
        assert_eq!(kept, "Yes.\n\n");
        assert!(quote.starts_with("On Mon"));
    }

    #[test]
    fn without_an_attribution_the_quote_alone_collapses() {
        let text = "Agreed.\n\n> The report is ready.\n>\n> Marius\n\n";
        let (kept, quote) = collapsed(text).unwrap();
        assert_eq!(kept, "Agreed.\n\n");
        assert_eq!(quote, "> The report is ready.\n>\n> Marius\n\n");
    }

    #[test]
    fn interleaved_quotes_stay() {
        let text = "Hi,\n\n> Do we need to bring bedding?\n\nNo, it's all there.\n\nIngrid\n";
        assert_eq!(trailing_quote(text), None);
    }

    #[test]
    fn a_message_that_is_only_a_quote_stays() {
        assert_eq!(trailing_quote("> just a quote\n> nothing else\n"), None);
        assert_eq!(
            trailing_quote("On Mon, Ada wrote:\n> only quoted\n"),
            None,
            "an attribution isn't the reply's own text"
        );
        assert_eq!(trailing_quote(""), None);
        assert_eq!(trailing_quote("\n\n"), None);
        assert_eq!(trailing_quote("No quote here.\n"), None);
    }

    #[test]
    fn a_signature_after_the_quote_keeps_it() {
        let text = "Reply.\n> quoted\n\n-- \nAda\n";
        assert_eq!(trailing_quote(text), None);
    }

    #[test]
    fn apple_mail_quotes_its_attribution() {
        let text = "Great.\n\n> On 5 Oct 2026, at 10:00, Ada <ada@example.com> wrote:\n> \n> Draft attached.\n";
        let (kept, quote) = collapsed(text).unwrap();
        assert_eq!(kept, "Great.\n\n");
        assert!(quote.starts_with("> On 5 Oct"));
    }

    #[test]
    fn outlook_header_blocks_collapse_to_the_end() {
        let text = "Thanks, I'll look at it today.\r\n\r\nLeah\r\n\r\nFrom: Marius Dahl <marius@example.com>\r\nSent: Monday, October 5, 2026 10:00 AM\r\nTo: Leah Kim <leah@example.com>\r\nSubject: Quarterly report\r\n\r\nHere's the draft.\r\n";
        let (kept, quote) = collapsed(text).unwrap();
        assert_eq!(kept, "Thanks, I'll look at it today.\r\n\r\nLeah\r\n\r\n");
        assert!(quote.starts_with("From: Marius"));
        assert!(quote.ends_with("Here's the draft.\r\n"));
    }

    #[test]
    fn norwegian_outlook_with_its_separator() {
        let text = "Ok!\n\n________________________________\nFra: Åse Nygård <ase@example.com>\nSendt: mandag 5. oktober 2026 10:00\nTil: Kåre <kare@example.com>\nEmne: Vafler\n\nHar du jern?\n";
        let (kept, quote) = collapsed(text).unwrap();
        assert_eq!(kept, "Ok!\n\n");
        assert!(quote.starts_with("_____"));
    }

    #[test]
    fn original_message_separators_with_blank_lines() {
        let text = "See below.\n\n-----Original Message-----\n\nFrom: Ada\nDate: Mon, 5 Oct 2026\nSubject: x\n\nbody\n";
        let (kept, quote) = collapsed(text).unwrap();
        assert_eq!(kept, "See below.\n\n");
        assert!(quote.starts_with("-----Original Message-----"));
    }

    #[test]
    fn a_from_line_without_sent_is_text() {
        let text = "From: tomorrow on, I'm in Bergen.\nSee you there.\n";
        assert_eq!(trailing_quote(text), None);
        let text = "Note.\n\nFrom: the team\n\nThanks\n";
        assert_eq!(trailing_quote(text), None);
    }

    #[test]
    fn verbs_must_be_whole_words() {
        assert!(!is_attribution("He rewrote the draft:"));
        assert!(!is_attribution("Things I wrote down"));
        assert!(is_attribution("Ada wrote:"));
        assert!(is_attribution(
            "On Monday, Ada Lovelace <ada@example.com> wrote:"
        ));
    }

    #[test]
    fn the_earliest_block_wins() {
        let text = "Top reply.\n\nFrom: B <b@example.com>\nSent: today\n\nMiddle.\n\nOn Mon, C wrote:\n> bottom\n";
        let (kept, quote) = collapsed(text).unwrap();
        assert_eq!(kept, "Top reply.\n\n");
        assert!(quote.starts_with("From: B"));
    }
}
