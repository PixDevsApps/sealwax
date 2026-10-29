//! List-Unsubscribe (RFC 2369) and its one-click form (RFC 8058), for the Unsubscribe command
//! and `is:subscription` (issue #14, ADR-023). A message's header is kept as the URIs Sealwax can
//! use, each in angle brackets; [`methods`] turns them into what Unsubscribe does, best first.
//! The URIs are the sender's: never log them.

use crate::mailto::{self, Mailto};

/// The most URIs kept from one header.
const MAX_URIS: usize = 4;
/// The longest URI kept.
const MAX_URI_CHARS: usize = 2048;
/// RFC 8058's one value of List-Unsubscribe-Post.
const ONE_CLICK: &str = "List-Unsubscribe=One-Click";

/// The `https:`, `http:` and `mailto:` URIs of a List-Unsubscribe header, normalized: each in
/// angle brackets, whitespace inside dropped (RFC 2369 lets a URI fold), the first four, joined
/// with ", ". `None` when none is usable.
pub fn normalize(header: &str) -> Option<String> {
    let mut uris: Vec<String> = Vec::new();
    let mut rest = header;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let uri: String = rest[open + 1..open + close]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        rest = &rest[open + close + 1..];
        if usable(&uri) && !uris.contains(&uri) {
            uris.push(uri);
            if uris.len() == MAX_URIS {
                break;
            }
        }
    }
    (!uris.is_empty()).then(|| {
        uris.iter()
            .map(|uri| format!("<{uri}>"))
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// Whether a List-Unsubscribe-Post header asks for RFC 8058's one-click POST.
pub fn is_one_click(header: &str) -> bool {
    header.trim().eq_ignore_ascii_case(ONE_CLICK)
}

fn usable(uri: &str) -> bool {
    uri.len() <= MAX_URI_CHARS
        && !uri.chars().any(char::is_control)
        && (scheme(uri, "https:") || scheme(uri, "http:") || mailto::is_mailto(uri))
}

fn scheme(uri: &str, scheme: &str) -> bool {
    uri.get(..scheme.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
}

/// How a message can be unsubscribed from, from its normalized header and whether it asked for
/// one-click.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Methods {
    /// The HTTPS URI to POST to (RFC 8058), when the message asked for one-click.
    pub one_click: Option<String>,
    /// The first `mailto:` URI with an address, parsed.
    pub mailto: Option<Mailto>,
    /// The first web page, to open in the browser.
    pub web: Option<String>,
}

impl std::fmt::Debug for Methods {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Methods")
            .field("one_click", &self.one_click.is_some())
            .field("mailto", &self.mailto)
            .field("web", &self.web.is_some())
            .finish()
    }
}

/// What Unsubscribe does with a message, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    OneClick,
    Mail,
    Web,
}

impl Methods {
    pub fn best(&self) -> Option<Method> {
        if self.one_click.is_some() {
            Some(Method::OneClick)
        } else if self.mailto.is_some() {
            Some(Method::Mail)
        } else if self.web.is_some() {
            Some(Method::Web)
        } else {
            None
        }
    }
}

pub fn methods(normalized: &str, one_click: bool) -> Methods {
    let mut out = Methods::default();
    for uri in normalized
        .split(", ")
        .filter_map(|uri| uri.strip_prefix('<')?.strip_suffix('>'))
    {
        if mailto::is_mailto(uri) {
            if out.mailto.is_none()
                && let Ok(parsed) = mailto::parse(uri)
                && !parsed.to.is_empty()
            {
                out.mailto = Some(parsed);
            }
        } else if scheme(uri, "https:") {
            if one_click && out.one_click.is_none() {
                out.one_click = Some(uri.to_owned());
            }
            if out.web.is_none() {
                out.web = Some(uri.to_owned());
            }
        } else if out.web.is_none() {
            out.web = Some(uri.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_usable_uris() {
        assert_eq!(
            normalize(
                "<mailto:leave@list.example?subject=unsubscribe>, <https://list.example/u/1>"
            )
            .as_deref(),
            Some("<mailto:leave@list.example?subject=unsubscribe>, <https://list.example/u/1>")
        );
        assert_eq!(
            normalize("<https://list.example/u/\r\n 1>").as_deref(),
            Some("<https://list.example/u/1>"),
            "a folded URI"
        );
        assert_eq!(
            normalize("<ftp://x.example/>, <javascript:alert(1)>, <HTTP://a.example/>").as_deref(),
            Some("<HTTP://a.example/>")
        );
        assert_eq!(normalize("https://no-brackets.example/"), None);
        assert_eq!(
            normalize("<https://a.example/>, <https://a.example/>").as_deref(),
            Some("<https://a.example/>")
        );
        let many = (0..10)
            .map(|n| format!("<https://{n}.example/>"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(normalize(&many).unwrap().split(", ").count(), MAX_URIS);
        let long = format!("<https://a.example/{}>", "x".repeat(MAX_URI_CHARS));
        assert_eq!(normalize(&long), None);
        assert_eq!(normalize("<https://a.example/\u{7}>"), None);
        assert_eq!(normalize("<https://a.example/"), None, "unclosed");
    }

    #[test]
    fn one_click() {
        assert!(is_one_click("List-Unsubscribe=One-Click"));
        assert!(is_one_click(" list-unsubscribe=one-click\r\n"));
        assert!(!is_one_click("List-Unsubscribe=One-Click; x=y"));
        assert!(!is_one_click(""));
    }

    #[test]
    fn best_method_first() {
        let header = "<mailto:leave@list.example?subject=stop>, <https://list.example/u/1>";
        let found = methods(header, true);
        assert_eq!(found.one_click.as_deref(), Some("https://list.example/u/1"));
        assert_eq!(found.best(), Some(Method::OneClick));
        assert_eq!(found.mailto.as_ref().unwrap().to, ["leave@list.example"]);
        assert_eq!(
            found.mailto.as_ref().unwrap().subject.as_deref(),
            Some("stop")
        );

        let found = methods(header, false);
        assert_eq!(found.one_click, None);
        assert_eq!(found.best(), Some(Method::Mail));
        assert_eq!(found.web.as_deref(), Some("https://list.example/u/1"));

        let found = methods("<http://list.example/u>", true);
        assert_eq!(found.one_click, None, "one-click is HTTPS only");
        assert_eq!(found.best(), Some(Method::Web));

        assert_eq!(
            methods("<mailto:?subject=x>", false).best(),
            None,
            "no address"
        );
        assert_eq!(methods("", true).best(), None);
    }

    #[test]
    fn debug_prints_no_uri() {
        let found = methods(
            "<mailto:leave@list.example>, <https://list.example/u/1>",
            true,
        );
        let printed = format!("{found:?}");
        assert!(!printed.contains("list.example"), "{printed}");
    }
}
