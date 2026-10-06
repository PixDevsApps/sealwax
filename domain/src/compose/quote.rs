//! The quoted original under a reply and the forwarded block, in the approved mock's style:
//! "On Sun 4 Oct 2026 at 07:50, Marius Dahl wrote:" above a cite quote. Times are shown in the
//! original's own time zone, from its Date header, never the user's: the quote tells the
//! recipients nothing the original didn't show. English in 1.0, like the rest of the app.

use super::clean::QUOTE_STYLE;
use super::text;
use crate::model::{Address, Headers};

/// When the original was sent, as its Date header says: the instant and the header's offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotedDate {
    /// Unix seconds.
    pub utc: i64,
    /// The Date header's offset from UTC in minutes (`+0200` is 120); 0 when unknown.
    pub offset_minutes: i32,
}

impl QuotedDate {
    /// A date whose offset isn't known, shown in UTC.
    pub fn utc(seconds: i64) -> Self {
        Self {
            utc: seconds,
            offset_minutes: 0,
        }
    }
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The calendar date and time at the date's own offset: (year, month 1–12, day, weekday 0 =
/// Monday, hour, minute).
fn civil(date: QuotedDate) -> (i64, usize, i64, usize, i64, i64) {
    let local = date.utc + i64::from(date.offset_minutes) * 60;
    let days = local.div_euclid(86_400);
    let seconds = local.rem_euclid(86_400);
    let weekday = (days + 3).rem_euclid(7) as usize;
    // Howard Hinnant's days_from_civil, inverted.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        month as usize,
        day,
        weekday,
        seconds / 3_600,
        seconds % 3_600 / 60,
    )
}

/// "Sun 4 Oct 2026 at 07:50", at the original's offset.
pub fn date_text(date: QuotedDate) -> String {
    let (year, month, day, weekday, hour, minute) = civil(date);
    format!(
        "{} {day} {} {year} at {hour:02}:{minute:02}",
        WEEKDAYS[weekday],
        MONTHS[month - 1]
    )
}

/// "+0200", the offset as the original's Date header wrote it.
pub fn offset_text(date: QuotedDate) -> String {
    let sign = if date.offset_minutes < 0 { '-' } else { '+' };
    let minutes = date.offset_minutes.unsigned_abs();
    format!("{sign}{:02}{:02}", minutes / 60, minutes % 60)
}

/// "On Sun 4 Oct 2026 at 07:50, Marius Dahl wrote:"; without a date "Marius Dahl wrote:". The
/// name is the sender's display name, or the address when it has none.
pub fn attribution(date: Option<QuotedDate>, from: Option<&Address>) -> String {
    let who = from
        .map(|from| text::header_text(from.display_name()))
        .filter(|who| !who.is_empty())
        .unwrap_or_else(|| "The sender".to_owned());
    match date {
        Some(date) => format!("On {}, {who} wrote:", date_text(date)),
        None => format!("{who} wrote:"),
    }
}

/// The attribution above the original's (cleaned) HTML in a cite quote.
pub fn reply_html(attribution: &str, original_html: &str) -> String {
    format!(
        "<div>{}</div><blockquote type=\"cite\" style=\"{QUOTE_STYLE}\">{original_html}</blockquote>",
        text::escape(attribution)
    )
}

/// The attribution above the original's text, every line after `> ` (`>` before a line that
/// is already quoted, and alone for an empty line).
pub fn reply_text(attribution: &str, original_text: &str) -> String {
    let mut out = String::with_capacity(original_text.len() + original_text.len() / 16 + 64);
    out.push_str(attribution);
    let text = original_text.replace("\r\n", "\n");
    for line in text.trim_end_matches('\n').split('\n') {
        out.push('\n');
        if line.is_empty() {
            out.push('>');
        } else if line.starts_with('>') {
            out.push('>');
            out.push_str(line);
        } else {
            out.push_str("> ");
            out.push_str(line);
        }
    }
    out
}

/// The heading line of a forwarded message.
pub const FORWARDED: &str = "---------- Forwarded message ----------";

/// The forwarded block's header lines: From, Date (at the original's offset, with it), Subject,
/// To and Cc, leaving out what the original didn't have.
pub fn forward_fields(headers: &Headers, date: Option<QuotedDate>) -> Vec<(&'static str, String)> {
    let list = |addresses: &[Address]| {
        addresses
            .iter()
            .map(|address| {
                let name = text::header_text(&address.name);
                if name.is_empty() {
                    address.email.clone()
                } else {
                    format!("{name} <{}>", address.email)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut fields = Vec::new();
    if !headers.from.is_empty() {
        fields.push(("From", list(&headers.from)));
    }
    if let Some(date) = date {
        fields.push(("Date", format!("{} {}", date_text(date), offset_text(date))));
    }
    fields.push(("Subject", text::header_text(&headers.subject)));
    if !headers.to.is_empty() {
        fields.push(("To", list(&headers.to)));
    }
    if !headers.cc.is_empty() {
        fields.push(("Cc", list(&headers.cc)));
    }
    fields
}

/// The forwarded block: the heading, the header lines, an empty line, then the original's
/// (cleaned) HTML as it was, not quoted.
pub fn forward_html(fields: &[(&str, String)], original_html: &str) -> String {
    let mut out = format!("<div>{FORWARDED}</div>");
    for (label, value) in fields {
        out.push_str(&format!("<div>{label}: {}</div>", text::escape(value)));
    }
    out.push_str("<div><br></div>");
    out.push_str(original_html);
    out
}

/// The forwarded block as text.
pub fn forward_text(fields: &[(&str, String)], original_text: &str) -> String {
    let mut out = String::from(FORWARDED);
    for (label, value) in fields {
        out.push_str(&format!("\n{label}: {value}"));
    }
    out.push_str("\n\n");
    out.push_str(original_text.replace("\r\n", "\n").trim_end_matches('\n'));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sun 4 Oct 2026 07:50 +0200 (the mock writes "Sat"; 4 Oct 2026 is a Sunday).
    const MOCK: QuotedDate = QuotedDate {
        utc: 1_791_093_000,
        offset_minutes: 120,
    };

    #[test]
    fn the_mocks_attribution() {
        let marius = Address::new("Marius Dahl", "marius@example.test");
        assert_eq!(
            attribution(Some(MOCK), Some(&marius)),
            "On Sun 4 Oct 2026 at 07:50, Marius Dahl wrote:"
        );
        let bare = Address::new("", "marius@example.test");
        assert_eq!(attribution(None, Some(&bare)), "marius@example.test wrote:");
        assert_eq!(attribution(None, None), "The sender wrote:");
    }

    #[test]
    fn times_are_the_originals_own() {
        assert_eq!(date_text(MOCK), "Sun 4 Oct 2026 at 07:50");
        assert_eq!(
            date_text(QuotedDate::utc(MOCK.utc)),
            "Sun 4 Oct 2026 at 05:50"
        );
        let west = QuotedDate {
            utc: MOCK.utc,
            offset_minutes: -(7 * 60),
        };
        assert_eq!(date_text(west), "Sat 3 Oct 2026 at 22:50");
        assert_eq!(offset_text(west), "-0700");
        let india = QuotedDate {
            utc: 0,
            offset_minutes: 330,
        };
        assert_eq!(date_text(india), "Thu 1 Jan 1970 at 05:30");
        assert_eq!(offset_text(india), "+0530");
        assert_eq!(
            date_text(QuotedDate::utc(951_782_400)),
            "Tue 29 Feb 2000 at 00:00"
        );
        assert_eq!(date_text(QuotedDate::utc(-1)), "Wed 31 Dec 1969 at 23:59");
    }

    #[test]
    fn replies_quote_html_and_text() {
        let html = reply_html("On x, A & B wrote:", "<p>hi</p>");
        assert_eq!(
            html,
            format!(
                "<div>On x, A &amp; B wrote:</div><blockquote type=\"cite\" style=\"{QUOTE_STYLE}\"><p>hi</p></blockquote>"
            )
        );
        assert_eq!(
            reply_text("A wrote:", "Hei\r\n\r\n> older\nslutt\n"),
            "A wrote:\n> Hei\n>\n>> older\n> slutt"
        );
    }

    #[test]
    fn forwards_list_the_header() {
        let headers = Headers {
            from: vec![Address::new("Marius Dahl", "marius@example.test")],
            to: vec![
                Address::new("", "ada@example.test"),
                Address::new("Bob <x>", "bob@example.test"),
            ],
            subject: "Kvartals\r\nrapport".into(),
            ..Headers::default()
        };
        let fields = forward_fields(&headers, Some(MOCK));
        assert_eq!(
            fields,
            [
                ("From", "Marius Dahl <marius@example.test>".to_owned()),
                ("Date", "Sun 4 Oct 2026 at 07:50 +0200".to_owned()),
                ("Subject", "Kvartals rapport".to_owned()),
                (
                    "To",
                    "ada@example.test, Bob <x> <bob@example.test>".to_owned()
                ),
            ]
        );
        let html = forward_html(&fields, "<p>body</p>");
        assert!(html.starts_with("<div>---------- Forwarded message ----------</div><div>From: Marius Dahl &lt;marius@example.test&gt;</div>"));
        assert!(html.ends_with("<div><br></div><p>body</p>"));
        let text = forward_text(&fields, "body\n");
        assert!(text.starts_with(FORWARDED));
        assert!(text.ends_with("To: ada@example.test, Bob <x> <bob@example.test>\n\nbody"));
    }
}
