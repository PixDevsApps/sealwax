//! The list's preview line: the start of the message's own text, with quoted replies, reply
//! attributions and signatures left out where they are easy to spot, whitespace collapsed and
//! invisible formatting characters dropped.

/// The snippet length the store keeps, in characters.
pub const SNIPPET_CHARS: usize = 200;

/// At most `max_chars` characters of `text` for a list row.
pub fn snippet(text: &str, max_chars: usize) -> String {
    let own = own_text(text, max_chars);
    if own.is_empty() {
        let mut all = String::new();
        push_collapsed(&mut all, text, max_chars + 1);
        cut(all, max_chars)
    } else {
        cut(own, max_chars)
    }
}

/// Collapses whitespace runs to one space and drops control and invisible format characters,
/// stopping once `limit` characters are kept.
pub fn collapse(text: &str, limit: usize) -> String {
    let mut out = String::new();
    push_collapsed(&mut out, text, limit);
    out.trim_end().to_owned()
}

fn own_text(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with('>') {
            continue;
        }
        if ends_quote(trimmed)
            || opens_attribution(trimmed)
                && lines
                    .peek()
                    .is_some_and(|next| is_attribution(&format!("{trimmed} {}", next.trim())))
        {
            break;
        }
        push_collapsed(&mut out, line, max_chars + 1);
        if out.chars().count() > max_chars {
            break;
        }
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim_end().to_owned()
}

/// A line after which only quoted or signature text follows.
fn ends_quote(line: &str) -> bool {
    if line == "--" || is_attribution(line) || (line.len() >= 10 && line.chars().all(|c| c == '_'))
    {
        return true;
    }
    line.starts_with("--") && {
        let lower = line.to_lowercase();
        [
            "original message",
            "forwarded message",
            "opprinnelig melding",
            "videresendt melding",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
    }
}

/// The first line of a reply attribution that a client wrapped over two lines.
fn opens_attribution(line: &str) -> bool {
    ["On ", "Den ", "Am ", "Le ", "På "]
        .iter()
        .any(|start| line.starts_with(start))
}

/// "On …, Kari <kari@example.test> wrote:", "Den … skrev Kari <…>:" and the like.
fn is_attribution(line: &str) -> bool {
    line.ends_with(':')
        && [" wrote", " skrev", " schrieb", " a écrit", " kirjoitti"]
            .iter()
            .any(|verb| line.contains(verb))
}

/// Characters that render as nothing: zero-width spaces and joiners, bidi marks, the soft hyphen,
/// the byte order mark, and the combining grapheme joiner newsletters pad their preheaders with.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FEFF}'
            | '\u{FFA0}'
    )
}

fn push_collapsed(out: &mut String, text: &str, limit: usize) {
    let mut count = out.chars().count();
    for c in text.chars() {
        if count >= limit {
            return;
        }
        if c.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
                count += 1;
            }
        } else if !c.is_control() && !invisible(c) {
            out.push(c);
            count += 1;
        }
    }
}

/// Cuts at `max_chars`, at the last space if one is near the end.
fn cut(text: String, max_chars: usize) -> String {
    let Some((end, _)) = text.char_indices().nth(max_chars) else {
        return text.trim_end().to_owned();
    };
    let head = &text[..end];
    let tail_start = head
        .char_indices()
        .nth(max_chars.saturating_sub(25))
        .map_or(0, |(index, _)| index);
    let at = match head[tail_start..].rfind(' ') {
        Some(space) => tail_start + space,
        None => end,
    };
    head[..at].trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_whitespace_and_drops_invisible_characters() {
        let text = "Hei\u{200C}\u{00A0}\u{200C} alle\r\n\r\n\tsammen\u{FEFF}!\u{0007}";
        assert_eq!(snippet(text, 200), "Hei alle sammen!");
        assert_eq!(collapse("  a \n\n b  ", 10), "a b");
    }

    #[test]
    fn skips_quotes_attributions_and_signatures() {
        let text = "Takk, det passer fint.\n\nOn Mon, 3 Oct 2026 at 10:00, Kari <kari@example.test> wrote:\n> Passer det på fredag?\n> Hilsen Kari";
        assert_eq!(snippet(text, 200), "Takk, det passer fint.");

        let wrapped =
            "Ja!\n\nOn Mon, 3 Oct 2026 at 10:00 Kari Nordmann <\nkari@example.test> wrote:\n> hei";
        assert_eq!(snippet(wrapped, 200), "Ja!");

        let norwegian = "Svar under.\nDen 3. okt. 2026 kl. 10:00 skrev Ola Nordmann <ola@example.test>:\n> spørsmål";
        assert_eq!(snippet(norwegian, 200), "Svar under.");

        let norwegian_wrapped = "Fint.\nDen 3. okt. 2026 kl. 10:00 skrev Ola Nordmann <\nola@example.test>:\n> spørsmål";
        assert_eq!(snippet(norwegian_wrapped, 200), "Fint.");

        let signature = "Kort svar.\n-- \nOla Nordmann\nTelefon 000";
        assert_eq!(snippet(signature, 200), "Kort svar.");

        let outlook = "Sees i morgen.\n________________________________\nFra: Kari\nSendt: mandag";
        assert_eq!(snippet(outlook, 200), "Sees i morgen.");

        let interleaved = "> spørsmål 1\nsvar 1\n> spørsmål 2\nsvar 2";
        assert_eq!(snippet(interleaved, 200), "svar 1 svar 2");
    }

    #[test]
    fn falls_back_to_the_quote_when_nothing_else_is_there() {
        let forward = "---------- Forwarded message ---------\nFrom: Kari\nSubject: Møte\n\nAgenda";
        assert_eq!(
            snippet(forward, 200),
            "---------- Forwarded message --------- From: Kari Subject: Møte Agenda"
        );
        assert_eq!(snippet("> bare sitat", 200), "> bare sitat");
        assert_eq!(snippet("", 200), "");
    }

    #[test]
    fn cuts_at_a_character_boundary_near_a_space() {
        let words = "blåbærsyltetøy ".repeat(30);
        let cut = snippet(&words, 200);
        assert!(cut.chars().count() <= 200);
        assert!(cut.ends_with("blåbærsyltetøy"));

        let cjk = "会议安排请查看附件".repeat(40);
        let cut = snippet(&cjk, 200);
        assert_eq!(cut.chars().count(), 200);

        let long_word = "x".repeat(500);
        assert_eq!(snippet(&long_word, 200).len(), 200);
    }
}
