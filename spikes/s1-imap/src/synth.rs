//! Synthetic test messages. Content is generated; no real data.

use crate::rng::Rng;

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "kilo",
    "lima", "mike", "oscar", "papa", "romeo", "sierra", "tango", "fjord", "bre", "skog", "elv",
    "hytte", "ski", "vann",
];

/// A message with the given Message-ID (without angle brackets).
pub fn message(message_id: &str, rng: &mut Rng) -> Vec<u8> {
    let sender = rng.below(40);
    let words = 20 + rng.below(200);
    let mut body = String::new();
    for i in 0..words {
        body.push_str(rng.pick(WORDS));
        body.push_str(if i % 12 == 11 { "\r\n" } else { " " });
    }
    let reply = if rng.chance(25) {
        format!(
            "In-Reply-To: <parent.{}@example.test>\r\nReferences: <parent.{}@example.test>\r\n",
            sender, sender
        )
    } else {
        String::new()
    };
    format!(
        "From: \"Synthetic Sender {sender}\" <sender{sender}@example.test>\r\n\
         To: <rand@example.test>\r\n\
         Subject: Synthetic {message_id}\r\n\
         Date: Sun, 4 Oct 2026 12:00:00 +0000\r\n\
         Message-ID: <{message_id}>\r\n\
         {reply}MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_the_message_id_and_crlf_line_ends() {
        let bytes = message("x.1@example.test", &mut Rng::new(1));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("Message-ID: <x.1@example.test>\r\n"));
        assert!(!text.replace("\r\n", "").contains('\n'));
    }
}
