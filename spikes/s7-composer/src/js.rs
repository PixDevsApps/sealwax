use webkit6::javascriptcore::Value;

pub fn get(value: &Value, name: &str) -> Option<Value> {
    value
        .is_object()
        .then(|| value.object_get_property(name))
        .flatten()
        .filter(|property| !property.is_undefined() && !property.is_null())
}

pub fn string(value: &Value, name: &str) -> String {
    get(value, name)
        .filter(Value::is_string)
        .map(|property| property.to_str().to_string())
        .unwrap_or_default()
}

pub fn boolean(value: &Value, name: &str) -> bool {
    get(value, name).is_some_and(|property| property.to_boolean())
}

pub fn number(value: &Value, name: &str) -> f64 {
    get(value, name).map_or(0.0, |property| property.to_double())
}

pub fn array(value: &Value, name: &str) -> Vec<Value> {
    let Some(list) = get(value, name).filter(Value::is_array) else {
        return Vec::new();
    };
    let length = get(&list, "length").map_or(0, |length| length.to_int32().max(0) as u32);
    (0..length)
        .filter_map(|index| list.object_get_property_at_index(index))
        .collect()
}

pub fn keys(value: &Value) -> Vec<String> {
    if !value.is_object() {
        return Vec::new();
    }
    value
        .object_enumerate_properties()
        .into_iter()
        .map(|key| key.to_string())
        .collect()
}

/// A JavaScript string literal for `text`.
pub fn literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_escapes_quotes_and_line_breaks() {
        assert_eq!(literal("a\"b\\c\nd\u{2028}"), "\"a\\\"b\\\\c\\nd\\u2028\"");
        assert_eq!(literal("æøå 🫐"), "\"æøå 🫐\"");
    }
}
