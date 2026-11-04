//! The app reports what happened as one JSON object per stdout line, prefixed so the bench can
//! pick its lines out of GTK's own output. Every event carries `t`, `CLOCK_MONOTONIC` ns.

use serde_json::{Map, Value};
use std::io::Write;

pub const PREFIX: &str = "s6-event ";

pub fn line(event: &str, fields: Value) -> String {
    let mut object = match fields {
        Value::Object(object) => object,
        Value::Null => Map::new(),
        other => {
            let mut object = Map::new();
            object.insert("value".into(), other);
            object
        }
    };
    object.insert("event".into(), Value::from(event));
    object
        .entry("t")
        .or_insert_with(|| Value::from(crate::clock::mono_ns()));
    object.insert("pid".into(), Value::from(std::process::id()));
    format!("{PREFIX}{}", Value::Object(object))
}

pub fn emit(event: &str, fields: Value) {
    let text = line(event, fields);
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{text}");
    let _ = stdout.flush();
}

/// The event name and its fields, if `text` is an event line.
pub fn parse(text: &str) -> Option<(String, Value)> {
    let json = text.trim_end().strip_prefix(PREFIX)?;
    let value: Value = serde_json::from_str(json).ok()?;
    let name = value.get("event")?.as_str()?.to_owned();
    Some((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip() {
        let text = line("first-frame", json!({ "rows": 12, "t": 5 }));
        let (name, value) = parse(&text).unwrap();
        assert_eq!(name, "first-frame");
        assert_eq!(value["rows"], 12);
        assert_eq!(value["t"], 5);
        assert!(parse("Gtk-WARNING something").is_none());
    }
}
