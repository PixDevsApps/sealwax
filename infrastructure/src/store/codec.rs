//! Compact text columns: address lists as `name US email RS name US email`, id and keyword
//! lists space-separated (ids hold no whitespace once normalized).

use sealwax_domain::model::Address;

const UNIT: char = '\u{1f}';
const RECORD: char = '\u{1e}';

fn field(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().filter(|c| *c != UNIT && *c != RECORD)
}

pub fn encode_addresses(list: &[Address]) -> String {
    let mut out = String::new();
    for (n, address) in list.iter().enumerate() {
        if n > 0 {
            out.push(RECORD);
        }
        out.extend(field(&address.name));
        out.push(UNIT);
        out.extend(field(&address.email));
    }
    out
}

pub fn decode_addresses(text: &str) -> Vec<Address> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split(RECORD)
        .map(|record| match record.split_once(UNIT) {
            Some((name, email)) => Address::new(name, email),
            None => Address::new("", record),
        })
        .collect()
}

pub fn encode_words(words: &[String]) -> String {
    words.join(" ")
}

pub fn decode_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_round_trip() {
        let list = vec![
            Address::new("Åse Bråten", "ase@example.test"),
            Address::new("", "kari@example.test"),
            Address::new("王伟", "wang.wei@example.test"),
        ];
        assert_eq!(decode_addresses(&encode_addresses(&list)), list);
        assert!(decode_addresses(&encode_addresses(&[])).is_empty());
        let hostile = [Address::new("a\u{1e}b\u{1f}c", "d@example.test")];
        assert_eq!(
            decode_addresses(&encode_addresses(&hostile)),
            [Address::new("abc", "d@example.test")]
        );
    }

    #[test]
    fn words_round_trip() {
        let words = vec!["a@example.test".to_owned(), "b@example.test".to_owned()];
        assert_eq!(decode_words(&encode_words(&words)), words);
        assert!(decode_words("").is_empty());
    }
}
