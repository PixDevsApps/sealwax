//! The reader's words for dates, recipients and attachments (DESIGN.md "Reader": "to me" or the
//! recipients, the date with relative time, attachment cards with a type badge, name and size).

use gtk4::glib;
use sealwax_domain::model::Address;

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;

/// The header's date: `09:30 (2 h ago)` today, `Yesterday 18:44`, `Mon 21:10` within the week,
/// `12 Sep, 09:30` this year, `12 Sep 2025` before; a date in the future gets the full form.
pub fn date_line(date: i64, now: i64, zone: &glib::TimeZone) -> String {
    let (Some(then), Some(today)) = (local(date, zone), local(now, zone)) else {
        return String::new();
    };
    let format = |pattern: &str| {
        then.format(pattern)
            .map(|text| text.to_string())
            .unwrap_or_default()
    };
    if date > now + MINUTE {
        return format("%-d %b %Y, %H:%M");
    }
    let days = day_number(&today) - day_number(&then);
    match days {
        0 => format!("{} ({})", format("%H:%M"), ago(now - date)),
        1 => format("Yesterday %H:%M"),
        2..=6 => format("%a %H:%M"),
        _ if then.year() == today.year() => format("%-d %b, %H:%M"),
        _ => format("%-d %b %Y"),
    }
}

/// A collapsed message's time (the approved mock's rows): `09:30` today, `Yesterday`, `Mon`
/// within the week, `4 Sep` this year, `30 Aug 2025` before; a date in the future gets the
/// last form.
pub fn short_date(date: i64, now: i64, zone: &glib::TimeZone) -> String {
    let (Some(then), Some(today)) = (local(date, zone), local(now, zone)) else {
        return String::new();
    };
    let format = |pattern: &str| {
        then.format(pattern)
            .map(|text| text.to_string())
            .unwrap_or_default()
    };
    if date > now + MINUTE {
        return format("%-d %b %Y");
    }
    match day_number(&today) - day_number(&then) {
        0 => format("%H:%M"),
        1 => "Yesterday".to_owned(),
        2..=6 => format("%a"),
        _ if then.year() == today.year() => format("%-d %b"),
        _ => format("%-d %b %Y"),
    }
}

/// The date in the expanded details: `Sunday 4 October 2026, 09:30 (+02:00)`.
pub fn full_date(date: i64, zone: &glib::TimeZone) -> String {
    local(date, zone)
        .and_then(|then| then.format("%A %-d %B %Y, %H:%M (%:z)").ok())
        .map(|text| text.to_string())
        .unwrap_or_default()
}

fn local(time: i64, zone: &glib::TimeZone) -> Option<glib::DateTime> {
    glib::DateTime::from_unix_utc(time)
        .and_then(|utc| utc.to_timezone(zone))
        .ok()
}

/// Days since an epoch in the date's own calendar, so two dates compare by calendar day.
fn day_number(date: &glib::DateTime) -> i64 {
    let (year, month, day) = date.ymd();
    let (year, month) = (i64::from(year), i64::from(month));
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let of_era = shifted - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + i64::from(day) - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_cycle
}

fn ago(seconds: i64) -> String {
    if seconds < MINUTE {
        "just now".to_owned()
    } else if seconds < HOUR {
        format!("{} min ago", seconds / MINUTE)
    } else {
        format!("{} h ago", seconds / HOUR)
    }
}

/// `to me` when one of the user's addresses received the message, else the recipients:
/// `to Leah Kim, Marius Dahl and 2 others`.
pub fn recipients_line(to: &[Address], cc: &[Address], mine: &[String]) -> String {
    let recipients: Vec<&Address> = to.iter().chain(cc).collect();
    if recipients.is_empty() {
        return "to undisclosed recipients".to_owned();
    }
    if recipients
        .iter()
        .any(|address| is_mine(&address.email, mine))
    {
        return "to me".to_owned();
    }
    const SHOWN: usize = 3;
    let names: Vec<&str> = recipients
        .iter()
        .take(SHOWN)
        .map(|address| address.display_name())
        .collect();
    let rest = recipients.len().saturating_sub(SHOWN);
    match rest {
        0 => format!("to {}", names.join(", ")),
        1 => format!("to {} and 1 other", names.join(", ")),
        _ => format!("to {} and {rest} others", names.join(", ")),
    }
}

/// Whether `email` is one of the user's own addresses (case doesn't matter).
pub fn is_mine(email: &str, mine: &[String]) -> bool {
    let email = email.trim();
    !email.is_empty()
        && mine
            .iter()
            .any(|own| own.trim().eq_ignore_ascii_case(email))
}

/// A list of addresses as the details show it: `Leah Kim <leah@example.test>, …`.
pub fn address_list(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(
            |address| match (address.name.is_empty(), address.email.is_empty()) {
                (false, false) => format!("{} <{}>", address.name, address.email),
                (true, _) => address.email.clone(),
                (false, true) => address.name.clone(),
            },
        )
        .collect::<Vec<_>>()
        .join(", ")
}

/// The attachment card's type badge: the file name's extension, else the MIME subtype, in at
/// most four capitals; `FILE` when neither says anything short.
pub fn type_label(name: Option<&str>, mime_type: &str) -> String {
    let short = |text: &str| {
        let text = text.trim();
        (!text.is_empty() && text.len() <= 4 && text.chars().all(|c| c.is_ascii_alphanumeric()))
            .then(|| text.to_ascii_uppercase())
    };
    let extension = name
        .and_then(|name| name.rsplit_once('.'))
        .and_then(|(stem, extension)| (!stem.is_empty()).then_some(extension))
        .and_then(short);
    let subtype = mime_type.split_once('/').and_then(|(_, subtype)| {
        let subtype = subtype.trim_start_matches("x-");
        match subtype {
            "plain" => Some("TXT".to_owned()),
            "jpeg" => Some("JPG".to_owned()),
            "calendar" => Some("ICS".to_owned()),
            "rfc822" => Some("EML".to_owned()),
            _ => short(subtype),
        }
    });
    extension.or(subtype).unwrap_or_else(|| "FILE".to_owned())
}

/// `1.4 MB`, `212 kB`, `84 bytes` (GLib's SI sizes, as Files shows them).
pub fn size(bytes: u64) -> String {
    glib::format_size(bytes).to_string()
}

/// `2 attachments`.
pub fn attachment_count(count: usize) -> String {
    match count {
        1 => "1 attachment".to_owned(),
        _ => format!("{count} attachments"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-04 12:00 UTC, a Sunday.
    const NOW: i64 = 1_791_115_200;

    fn utc() -> glib::TimeZone {
        glib::TimeZone::utc()
    }

    #[test]
    fn dates_read_relative_to_now() {
        let line = |date: i64| date_line(date, NOW, &utc());
        assert_eq!(line(NOW - 30), "11:59 (just now)");
        assert_eq!(line(NOW - 25 * MINUTE), "11:35 (25 min ago)");
        assert_eq!(line(NOW - 2 * HOUR - 5 * MINUTE), "09:55 (2 h ago)");
        assert_eq!(line(NOW - 13 * HOUR), "Yesterday 23:00");
        assert_eq!(line(NOW - 3 * 24 * HOUR), "Thu 12:00");
        assert_eq!(line(NOW - 30 * 24 * HOUR), "4 Sep, 12:00");
        assert_eq!(line(NOW - 400 * 24 * HOUR), "30 Aug 2025");
        assert_eq!(line(NOW + 2 * 24 * HOUR), "6 Oct 2026, 12:00");
    }

    #[test]
    fn collapsed_rows_take_a_short_time() {
        let short = |date: i64| short_date(date, NOW, &utc());
        assert_eq!(short(NOW - 25 * MINUTE), "11:35");
        assert_eq!(short(NOW - 13 * HOUR), "Yesterday");
        assert_eq!(short(NOW - 3 * 24 * HOUR), "Thu");
        assert_eq!(short(NOW - 30 * 24 * HOUR), "4 Sep");
        assert_eq!(short(NOW - 400 * 24 * HOUR), "30 Aug 2025");
        assert_eq!(short(NOW + 2 * 24 * HOUR), "6 Oct 2026");
    }

    #[test]
    fn days_follow_the_time_zone() {
        let oslo = glib::TimeZone::from_identifier(Some("Europe/Oslo")).unwrap_or_else(utc);
        if oslo.identifier() == "UTC" {
            return;
        }
        // 22:30 UTC on Saturday is 00:30 on Sunday in Oslo: today there, yesterday in UTC.
        let date = NOW - 13 * HOUR - 30 * MINUTE;
        assert_eq!(date_line(date, NOW, &oslo), "00:30 (13 h ago)");
        assert_eq!(date_line(date, NOW, &utc()), "Yesterday 22:30");
        assert_eq!(
            full_date(NOW, &oslo),
            "Sunday 4 October 2026, 14:00 (+02:00)"
        );
    }

    #[test]
    fn day_numbers_are_consecutive() {
        let day = |time: i64| day_number(&local(time, &utc()).unwrap());
        assert_eq!(day(NOW) - day(NOW - 24 * HOUR), 1);
        assert_eq!(day(0), 719_468);
        assert_eq!(day(951_782_400) - day(951_696_000), 1, "29 Feb 2000");
    }

    #[test]
    fn recipients_say_me_or_name_them() {
        let mine = vec!["Alex.Berg@example.test".to_owned()];
        let to = [
            Address::new("Leah Kim", "leah@example.test"),
            Address::new("", "alex.berg@example.test"),
        ];
        assert_eq!(recipients_line(&to, &[], &mine), "to me");
        assert_eq!(recipients_line(&[], &to, &mine), "to me");
        let others = [
            Address::new("Leah Kim", "leah@example.test"),
            Address::new("", "marius@example.test"),
            Address::new("Ingrid", "ingrid@example.test"),
            Address::new("Tomas", "tomas@example.test"),
            Address::new("Sana", "sana@example.test"),
        ];
        assert_eq!(
            recipients_line(&others[..2], &[], &mine),
            "to Leah Kim, marius@example.test"
        );
        assert_eq!(
            recipients_line(&others[..4], &[], &mine),
            "to Leah Kim, marius@example.test, Ingrid and 1 other"
        );
        assert_eq!(
            recipients_line(&others, &[], &mine),
            "to Leah Kim, marius@example.test, Ingrid and 2 others"
        );
        assert_eq!(
            recipients_line(&[], &[], &mine),
            "to undisclosed recipients"
        );
        assert!(!is_mine("", &[String::new()]));
    }

    #[test]
    fn address_lists_show_names_and_addresses() {
        let list = [
            Address::new("Leah Kim", "leah@example.test"),
            Address::new("", "marius@example.test"),
        ];
        assert_eq!(
            address_list(&list),
            "Leah Kim <leah@example.test>, marius@example.test"
        );
    }

    #[test]
    fn type_badges() {
        assert_eq!(
            type_label(Some("Invoice-2041.pdf"), "application/pdf"),
            "PDF"
        );
        assert_eq!(type_label(Some("photo.JPEG"), "image/jpeg"), "JPEG");
        assert_eq!(type_label(None, "image/jpeg"), "JPG");
        assert_eq!(type_label(Some("notes"), "text/plain"), "TXT");
        assert_eq!(type_label(Some(".bashrc"), "text/x-shellscript"), "FILE");
        assert_eq!(type_label(None, "text/calendar"), "ICS");
        assert_eq!(type_label(None, "application/x-zip"), "ZIP");
        assert_eq!(
            type_label(
                Some("report.final"),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            ),
            "FILE"
        );
        assert_eq!(
            type_label(Some("Q3 report.docx"), "application/octet-stream"),
            "DOCX"
        );
    }

    #[test]
    fn sizes_and_counts() {
        assert_eq!(size(84), "84 bytes");
        assert_eq!(size(212_000), "212.0\u{a0}kB");
        assert_eq!(attachment_count(1), "1 attachment");
        assert_eq!(attachment_count(3), "3 attachments");
    }
}
