//! Grouping the message list by sender or by domain (issue #24, ADR-023): the keys a message is
//! grouped under.

use crate::links;

/// What the message list groups by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GroupBy {
    #[default]
    None,
    Sender,
    Domain,
}

impl GroupBy {
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sender => "sender",
            Self::Domain => "domain",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::None, Self::Sender, Self::Domain]
            .into_iter()
            .find(|group| group.name() == name)
    }
}

/// The key a sender's address is grouped under: the address, lower-cased.
pub fn sender_key(address: &str) -> String {
    address.trim().to_lowercase()
}

/// The key a sender's domain is grouped under: the registrable domain of the address's domain,
/// in ASCII and lower case (`news.shop.example.co.uk` → `example.co.uk`); empty when the address
/// has none.
pub fn domain_key(address: &str) -> String {
    let Some((_, domain)) = address.trim().rsplit_once('@') else {
        return String::new();
    };
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        return String::new();
    }
    let ascii = idna::domain_to_ascii(domain).unwrap_or_else(|_| domain.to_lowercase());
    links::registrable(&ascii)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(sender_key(" News@Shop.Example "), "news@shop.example");
        assert_eq!(domain_key("news@mail.shop.example.co.uk"), "example.co.uk");
        assert_eq!(domain_key("a@Shop.COM."), "shop.com");
        assert_eq!(
            domain_key("a@xn--bcher-kva.example"),
            "xn--bcher-kva.example"
        );
        assert_eq!(domain_key("a@bücher.example"), "xn--bcher-kva.example");
        assert_eq!(domain_key("a@localhost"), "localhost");
        assert_eq!(domain_key("no-at-sign"), "");
        assert_eq!(domain_key("a@"), "");
    }

    #[test]
    fn names() {
        for group in [GroupBy::None, GroupBy::Sender, GroupBy::Domain] {
            assert_eq!(GroupBy::from_name(group.name()), Some(group));
        }
        assert_eq!(GroupBy::from_name("x"), None);
    }
}
