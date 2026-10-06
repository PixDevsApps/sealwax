//! Link safety (SECURITY.md "Phishing"; ML-062): where a link really goes, whether its visible
//! text names another domain than its target, how a host is written when its letters could
//! pass for another's, and the links in plain text. Hosts are compared by registrable domain
//! (the public suffix list), in ASCII (UTS 46 IDNA), so `www.`, subdomains, letter case,
//! trailing dots, `http` against `https` and an IDN against its punycode never differ.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Top-level names reserved for documentation, tests and local use (RFC 2606, RFC 6761):
/// in link text they count as domains, so fictional addresses behave like real ones.
const SPECIAL_USE: [&str; 4] = ["test", "example", "invalid", "localhost"];

/// Country-code domains that are also common file extensions: a bare `README.md` or
/// `main.rs` in link text is a file name, not a domain (a `www.` or a scheme still makes it
/// one).
const FILE_LIKE: [&str; 9] = ["md", "py", "rs", "sh", "zip", "mov", "cc", "so", "ps"];

/// What a link's text and target say together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to warn about: the text names no domain, or the target's own.
    Fine,
    /// The text names `text` (its host, or an address's domain, as the reader should see it)
    /// and the link opens `target` in another registrable domain.
    Mismatch { text: String, target: String },
    /// The target puts something before an `@` ahead of its host, so it reads as `shown`
    /// but opens `target` (`https://bank.example@evil.example/`).
    Disguised { shown: String, target: String },
}

/// Where an http(s) or mailto link goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// `http`, `https` or `mailto`.
    pub scheme: String,
    /// The host in ASCII and lower case, without brackets' contents changed or a trailing dot;
    /// for mailto, the first address's domain.
    pub host: String,
    /// What an http(s) URL puts before an `@` in its authority, percent-decoded.
    pub userinfo: Option<String>,
    /// After the host: the port, path, query and fragment as written; for mailto, the first
    /// address, percent-decoded.
    pub rest: String,
}

/// Parses an http(s) or mailto URL the way a browser finds its host: any run of slashes or
/// backslashes after the scheme, the last `@` of the authority, percent-escapes, IDNA and
/// the numeric forms of IPv4 addresses. Anything else is `None`.
pub fn target(url: &str) -> Option<Target> {
    let url = url.trim_matches(|c: char| c.is_whitespace() || c.is_control());
    let (scheme, rest) = url.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" => {
            let rest = rest.trim_start_matches(['/', '\\']);
            let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
            let authority = &rest[..end];
            let (userinfo, hostport) = match authority.rfind('@') {
                Some(at) => (Some(percent_decode(&authority[..at])), &authority[at + 1..]),
                None => (None, authority),
            };
            let host_end = if hostport.starts_with('[') {
                hostport.find(']').map_or(hostport.len(), |close| close + 1)
            } else {
                hostport.find(':').unwrap_or(hostport.len())
            };
            let host = normalize_host(&hostport[..host_end])?;
            let after = authority.len() - hostport.len() + host_end;
            Some(Target {
                scheme,
                host,
                userinfo: userinfo.filter(|info| !info.is_empty()),
                rest: rest[after..].to_owned(),
            })
        }
        "mailto" => {
            let addresses = rest.split(['?', '#']).next().unwrap_or_default();
            let address = percent_decode(addresses);
            let address = address.split(',').next().unwrap_or_default().trim();
            let (_, domain) = address.rsplit_once('@')?;
            Some(Target {
                scheme,
                host: normalize_host(domain)?,
                userinfo: None,
                rest: address.to_owned(),
            })
        }
        _ => None,
    }
}

/// The host or address domain a link's visible text names, in ASCII and lower case: an
/// http(s) or mailto URL, an address, or a lone name with a known public suffix (or `www.`,
/// or a special-use name). Text with spaces or without a dot names nothing.
pub fn text_domain(text: &str) -> Option<String> {
    let token = text
        .trim()
        .trim_matches(|c: char| "<>()[]{}\"'«»“”‘’".contains(c))
        .trim_end_matches(['.', ',', ';', ':', '!', '?']);
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return None;
    }
    if let Some(found) = target(token) {
        return Some(found.host);
    }
    if let Some((scheme, after)) = token.split_once(':')
        && after.starts_with("//")
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return None;
    }
    if let Some((local, domain)) = token.rsplit_once('@') {
        if local.is_empty() || domain.contains(['/', '?', '#']) {
            return None;
        }
        return named_host(domain, false);
    }
    let host = token.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    named_host(host, token.to_ascii_lowercase().starts_with("www."))
}

/// `name` as a host when it looks like one in text: an IP address, or labels with a known
/// suffix.
fn named_host(name: &str, www: bool) -> Option<String> {
    let name = name.trim_end_matches('.');
    if !name.contains('.') && !name.starts_with('[') {
        return None;
    }
    let valid = |label: &str| {
        !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    };
    let numeric = name.starts_with('[') || name.parse::<Ipv4Addr>().is_ok();
    if !numeric && !name.split('.').all(valid) {
        return None;
    }
    let host = normalize_host(name)?;
    if numeric {
        return Some(host);
    }
    let labels = host.split('.').count();
    let suffix = host.rsplit('.').next().unwrap_or_default();
    let known = psl::suffix(host.as_bytes()).is_some_and(|suffix| suffix.is_known());
    let file_like = labels == 2 && FILE_LIKE.contains(&suffix);
    (www || SPECIAL_USE.contains(&suffix) || (known && !file_like)).then_some(host)
}

/// The registrable domain of an ASCII host (`www.bank.example.co.uk` → `example.co.uk`); an IP
/// address, a single label or a public suffix stands for itself.
pub fn registrable(host: &str) -> String {
    if host.starts_with('[') || host.parse::<Ipv4Addr>().is_ok() {
        return host.to_owned();
    }
    psl::domain_str(host).unwrap_or(host).to_owned()
}

/// Whether following `url` deserves a warning, given the link's visible `text`.
pub fn check(text: &str, url: &str) -> Verdict {
    let Some(target) = target(url) else {
        return Verdict::Fine;
    };
    if let Some(shown) = &target.userinfo {
        return Verdict::Disguised {
            shown: shown.clone(),
            target: display_host(&target.host),
        };
    }
    let Some(named) = text_domain(text) else {
        return Verdict::Fine;
    };
    if registrable(&named) == registrable(&target.host) {
        Verdict::Fine
    } else {
        Verdict::Mismatch {
            text: display_host(&named),
            target: display_host(&target.host),
        }
    }
}

/// A host written so that letters from another alphabet can't pass for Latin ones: in
/// Unicode, unless a label mixes Latin, Greek and Cyrillic letters, or is made only of Greek
/// or Cyrillic letters that look Latin under an ASCII top-level domain; then in punycode.
pub fn display_host(host: &str) -> String {
    if confusable(host) {
        return host.to_owned();
    }
    let (unicode, result) = idna::domain_to_unicode(host);
    if result.is_ok() {
        unicode
    } else {
        host.to_owned()
    }
}

/// Whether a host's letters could pass for another host's (see [`display_host`]).
pub fn confusable(host: &str) -> bool {
    let (unicode, _) = idna::domain_to_unicode(host);
    let tld_ascii = unicode.rsplit('.').next().is_some_and(|tld| tld.is_ascii());
    unicode
        .split('.')
        .any(|label| mixed_scripts(label) || (tld_ascii && lookalike(label)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Greek,
    Cyrillic,
    Other,
}

fn script(c: char) -> Option<Script> {
    match c {
        '0'..='9' | '-' | '_' => None,
        'a'..='z' | 'A'..='Z' => Some(Script::Latin),
        '\u{00d7}' | '\u{00f7}' => Some(Script::Other),
        '\u{00c0}'..='\u{024f}' | '\u{1e00}'..='\u{1eff}' => Some(Script::Latin),
        '\u{0370}'..='\u{03ff}' | '\u{1f00}'..='\u{1fff}' => Some(Script::Greek),
        '\u{0400}'..='\u{052f}'
        | '\u{1c80}'..='\u{1c8f}'
        | '\u{2de0}'..='\u{2dff}'
        | '\u{a640}'..='\u{a69f}' => Some(Script::Cyrillic),
        _ => Some(Script::Other),
    }
}

fn mixed_scripts(label: &str) -> bool {
    let mut seen = [false; 3];
    for c in label.chars() {
        match script(c) {
            Some(Script::Latin) => seen[0] = true,
            Some(Script::Greek) => seen[1] = true,
            Some(Script::Cyrillic) => seen[2] = true,
            _ => {}
        }
    }
    seen.iter().filter(|seen| **seen).count() > 1
}

/// Cyrillic and Greek letters that look like Latin ones.
const LOOKALIKES: &str = "авсԁеһіјкӏмнорԛѕтԝхуүαειικνορτυχ";

fn lookalike(label: &str) -> bool {
    !label.is_ascii()
        && label
            .chars()
            .all(|c| script(c).is_none() || LOOKALIKES.contains(c))
}

/// What the status strip shows for a link: a mailto link's address, else the URL with its
/// host as [`display_host`] writes it. A URL that doesn't parse is shown as it is.
pub fn status_text(url: &str) -> String {
    match target(url) {
        Some(target) if target.scheme == "mailto" => target.rest,
        Some(target) => {
            let userinfo = target
                .userinfo
                .map(|info| format!("{info}@"))
                .unwrap_or_default();
            format!(
                "{}://{userinfo}{}{}",
                target.scheme,
                display_host(&target.host),
                target.rest
            )
        }
        None => url.trim().to_owned(),
    }
}

/// A link found in plain text: bytes `start..end` of the text, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    /// An `http:`, `https:` or `mailto:` URL; `www.` names get `https://`.
    pub target: String,
}

/// The http(s), `www.` and mailto links and the bare addresses in plain text. Nothing else
/// becomes a link: `javascript:`, `file:`, `data:` and relative targets are text.
pub fn find(text: &str) -> Vec<Found> {
    let lower = text.to_ascii_lowercase();
    let mut found: Vec<Found> = Vec::new();
    for prefix in ["https://", "http://", "mailto:", "www."] {
        for (start, _) in lower.match_indices(prefix) {
            if !at_word_start(text, start)
                || found.iter().any(|f| f.start <= start && start < f.end)
            {
                continue;
            }
            let end = url_end(text, start);
            let url = &text[start..end];
            let rest = url.get(prefix.len()..).unwrap_or_default();
            let usable = match prefix {
                "mailto:" => rest.contains('@'),
                "www." => rest.contains('.'),
                _ => !rest.is_empty(),
            };
            if !usable {
                continue;
            }
            let target = if prefix == "www." {
                format!("https://{url}")
            } else {
                url.to_owned()
            };
            found.push(Found { start, end, target });
        }
    }
    for (at, _) in text.match_indices('@') {
        if found.iter().any(|f| f.start <= at && at < f.end) {
            continue;
        }
        if let Some(address) = address_around(text, at)
            && !found
                .iter()
                .any(|f| f.start < address.end && address.start < f.end)
        {
            found.push(address);
        }
    }
    found.sort_by_key(|found| found.start);
    found
}

fn at_word_start(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric() && !"/@.-_".contains(c))
}

/// Where a URL starting at `start` ends: before whitespace or `<>"`, without trailing
/// punctuation or an unbalanced closing bracket.
fn url_end(text: &str, start: usize) -> usize {
    let mut end = text[start..]
        .find(|c: char| c.is_whitespace() || c.is_control() || "<>\"`".contains(c))
        .map_or(text.len(), |offset| start + offset);
    loop {
        let url = &text[start..end];
        let Some(last) = url.chars().next_back() else {
            return end;
        };
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(close).count() > url.matches(open).count()
        };
        if ".,;:!?'*".contains(last)
            || unbalanced('(', ')')
            || unbalanced('[', ']')
            || unbalanced('{', '}')
        {
            end -= last.len_utf8();
        } else {
            return end;
        }
    }
}

/// A bare address around the `@` at `at` (`ada@example.com`), as a mailto link.
fn address_around(text: &str, at: usize) -> Option<Found> {
    let local_char = |c: char| c.is_alphanumeric() || "._%+-".contains(c);
    let domain_char = |c: char| c.is_alphanumeric() || ".-".contains(c);
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| local_char(*c))
        .last()
        .map_or(at, |(index, _)| index);
    let after = &text[at + 1..];
    let length = after.find(|c: char| !domain_char(c)).unwrap_or(after.len());
    let domain = after[..length].trim_end_matches(['.', '-']);
    let local = text[start..at].trim_start_matches('.');
    let start = at - local.len();
    let tld = domain.rsplit('.').next().unwrap_or_default();
    let valid = !local.is_empty()
        && domain.contains('.')
        && tld.chars().count() >= 2
        && tld.chars().all(char::is_alphabetic);
    valid.then(|| Found {
        start,
        end: at + 1 + domain.len(),
        target: format!("mailto:{}", &text[start..at + 1 + domain.len()]),
    })
}

/// A host in ASCII and lower case, or `None` when it is empty or not a host: percent-escapes
/// decoded, the trailing dot dropped, IPv4 in any of its numeric forms written as four
/// numbers, IPv6 in brackets, names through UTS 46.
fn normalize_host(host: &str) -> Option<String> {
    let host = percent_decode(host);
    let host = host.trim().trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    if let Some(inner) = host.strip_prefix('[') {
        let address: Ipv6Addr = inner.strip_suffix(']')?.parse().ok()?;
        return Some(format!("[{address}]"));
    }
    if let Some(address) = ipv4(host) {
        return Some(address.to_string());
    }
    match idna::domain_to_ascii(host) {
        Ok(ascii) if !ascii.is_empty() => Some(ascii),
        _ => host
            .is_ascii()
            .then(|| host.to_ascii_lowercase())
            .filter(|host| !host.contains(['/', '\\', '@', ' ', '%'])),
    }
}

/// IPv4 as browsers read it (WHATWG URL): one to four parts, each decimal, `0x` hex or
/// leading-zero octal, the last filling the remaining bytes.
fn ipv4(host: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() > 4 {
        return None;
    }
    let numbers = parts
        .iter()
        .map(|part| {
            let (digits, radix) = match part.strip_prefix("0x").or(part.strip_prefix("0X")) {
                Some(hex) => (hex, 16),
                None if part.len() > 1 && part.starts_with('0') => (&part[1..], 8),
                None => (*part, 10),
            };
            if part.is_empty() {
                None
            } else if digits.is_empty() {
                Some(0)
            } else {
                u64::from_str_radix(digits, radix).ok()
            }
        })
        .collect::<Option<Vec<u64>>>()?;
    let (last, init) = numbers.split_last()?;
    let room = 256u64.pow(5 - u32::try_from(numbers.len()).ok()?);
    if init.iter().any(|number| *number > 255) || *last >= room {
        return None;
    }
    let value = init
        .iter()
        .enumerate()
        .fold(*last, |value, (index, number)| {
            value + number * 256u64.pow(3 - index as u32)
        });
    Some(Ipv4Addr::from(u32::try_from(value).ok()?))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |byte: u8| char::from(byte).to_digit(16);
        match (bytes[at], bytes.get(at + 1), bytes.get(at + 2)) {
            (b'%', Some(&high), Some(&low)) if hex(high).is_some() && hex(low).is_some() => {
                out.push((hex(high).unwrap_or(0) * 16 + hex(low).unwrap_or(0)) as u8);
                at += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mismatch(text: &str, target: &str) -> Verdict {
        Verdict::Mismatch {
            text: text.into(),
            target: target.into(),
        }
    }

    #[test]
    fn targets_are_found_as_browsers_find_them() {
        let found = target("https://Track.Mailsend.Example./c/8f3a91?u=1#x").unwrap();
        assert_eq!(found.scheme, "https");
        assert_eq!(found.host, "track.mailsend.example");
        assert_eq!(found.rest, "/c/8f3a91?u=1#x");
        assert_eq!(found.userinfo, None);
        assert_eq!(target("HTTP://example.com").unwrap().host, "example.com");
        assert_eq!(target("http://example.com:8080/x").unwrap().rest, ":8080/x");
        assert_eq!(
            target("https:\\\\evil.example\\@bank.example")
                .unwrap()
                .host,
            "evil.example"
        );
        assert_eq!(target("https:///example.com/").unwrap().host, "example.com");
        assert_eq!(
            target("https://%65vil.example/").unwrap().host,
            "evil.example"
        );
        assert_eq!(target("http://[::1]:80/").unwrap().host, "[::1]");
        for refused in [
            "",
            "https://",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "ftp://example.com/",
            "rel-link",
            "cid:logo@example.test",
            "mailto:",
            "mailto:nobody",
        ] {
            assert_eq!(target(refused), None, "{refused}");
        }
    }

    #[test]
    fn userinfo_is_not_the_host() {
        let found = target("https://bank.example@evil.example/login").unwrap();
        assert_eq!(found.host, "evil.example");
        assert_eq!(found.userinfo.as_deref(), Some("bank.example"));
        let found = target("https://user:pw@a@evil.example/").unwrap();
        assert_eq!(found.host, "evil.example");
        assert_eq!(found.userinfo.as_deref(), Some("user:pw@a"));
        assert_eq!(
            check("bank.example", "https://bank.example@evil.example/"),
            Verdict::Disguised {
                shown: "bank.example".into(),
                target: "evil.example".into()
            }
        );
        assert_eq!(
            check("Log in", "https://www.bank.example%40evil.example/"),
            Verdict::Fine,
            "an escaped @ makes no valid host, and such a link never opens"
        );
    }

    #[test]
    fn numeric_hosts_in_every_form() {
        for url in [
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://0x7f.1/",
            "http://0177.0.0.1/",
            "http://127.1/",
            "http://127.0.0.1./",
        ] {
            assert_eq!(target(url).unwrap().host, "127.0.0.1", "{url}");
        }
        assert_eq!(target("http://256.0.0.1/").unwrap().host, "256.0.0.1");
        assert_eq!(check("127.0.0.1", "http://2130706433/"), Verdict::Fine);
        assert_eq!(
            check("bank.example", "http://192.0.2.1/login"),
            mismatch("bank.example", "192.0.2.1")
        );
        assert_eq!(
            check("192.0.2.1", "https://bank.example/"),
            mismatch("192.0.2.1", "bank.example")
        );
        assert_eq!(registrable("[2001:db8::1]"), "[2001:db8::1]");
    }

    #[test]
    fn mailto_targets_name_the_address_domain() {
        let found = target("mailto:Support%40Bank.example?subject=Hi").unwrap();
        assert_eq!(found.host, "bank.example");
        assert_eq!(found.rest, "Support@Bank.example");
        assert_eq!(
            target("mailto:a@one.example,b@two.example").unwrap().host,
            "one.example"
        );
        assert_eq!(
            check("support@bank.example", "mailto:help@bank.example"),
            Verdict::Fine
        );
        assert_eq!(
            check("support@bank.example", "mailto:support@evil.example"),
            mismatch("bank.example", "evil.example")
        );
        assert_eq!(
            check("Write to us", "mailto:support@evil.example"),
            Verdict::Fine
        );
        assert_eq!(
            check("bank.example", "mailto:support@evil.example"),
            mismatch("bank.example", "evil.example")
        );
        assert_eq!(
            check("support@bank.example", "https://evil.example/"),
            mismatch("bank.example", "evil.example")
        );
    }

    #[test]
    fn the_mocks_tracking_redirect_is_a_mismatch() {
        assert_eq!(
            check(
                "northwind.example",
                "https://track.mailsend.example/c/8f3a91?u=northwind.example"
            ),
            mismatch("northwind.example", "track.mailsend.example")
        );
        assert_eq!(
            check(
                "https://northwind.example/news",
                "https://click.example.com/r?x=1"
            ),
            mismatch("northwind.example", "click.example.com")
        );
    }

    #[test]
    fn same_registrable_domain_is_fine() {
        for (text, url) in [
            ("bank.example", "https://www.bank.example/login"),
            ("www.bank.example", "https://bank.example/"),
            ("login.bank.example", "https://secure.bank.example/"),
            ("BANK.EXAMPLE", "https://bank.example./"),
            ("bank.example.", "https://Bank.Example/"),
            ("http://bank.example", "https://bank.example/"),
            ("https://bank.example/a", "http://bank.example/b"),
            ("example.co.uk", "https://www.example.co.uk/"),
            ("<https://bank.example>", "https://bank.example/"),
            ("bank.example:8443", "https://bank.example:8443/"),
        ] {
            assert_eq!(check(text, url), Verdict::Fine, "{text} → {url}");
        }
    }

    #[test]
    fn subdomains_of_public_suffixes_are_different_owners() {
        assert_eq!(
            check("alice.github.io", "https://mallory.github.io/"),
            mismatch("alice.github.io", "mallory.github.io")
        );
        assert_eq!(
            check("example.co.uk", "https://other.co.uk/"),
            mismatch("example.co.uk", "other.co.uk")
        );
        assert_eq!(registrable("www.example.co.uk"), "example.co.uk");
        assert_eq!(registrable("co.uk"), "co.uk");
        assert_eq!(registrable("localhost"), "localhost");
    }

    #[test]
    fn text_that_isnt_a_domain_is_fine() {
        for text in [
            "Read more",
            "Click here",
            "",
            "   ",
            "Unsubscribe",
            "v2.1",
            "3.14",
            "report.pdf",
            "README.md",
            "main.rs",
            "setup.py",
            "invoice.zip",
            "Node.js",
            "e.g.",
            "see bank.example now",
            "ftp://bank.example/",
            "Version 1.2.3",
        ] {
            assert_eq!(text_domain(text), None, "{text:?}");
            assert_eq!(
                check(text, "https://evil.example/"),
                Verdict::Fine,
                "{text:?}"
            );
        }
    }

    #[test]
    fn text_that_is_a_domain() {
        for (text, host) in [
            ("bank.example", "bank.example"),
            ("Bank.Example.", "bank.example"),
            ("www.readme.md", "www.readme.md"),
            ("https://main.rs", "main.rs"),
            ("example.com", "example.com"),
            ("shop.example.com/sale?x=1", "shop.example.com"),
            ("(example.org)", "example.org"),
            ("“example.net”", "example.net"),
            ("example.com,", "example.com"),
            ("ada@example.com", "example.com"),
            ("mailto:ada@example.com", "example.com"),
            ("printer.local.test", "printer.local.test"),
        ] {
            assert_eq!(text_domain(text).as_deref(), Some(host), "{text:?}");
        }
    }

    #[test]
    fn idns_and_punycode_are_the_same_domain() {
        assert_eq!(
            target("https://bücher.example/").unwrap().host,
            "xn--bcher-kva.example"
        );
        assert_eq!(
            check("bücher.example", "https://xn--bcher-kva.example/"),
            Verdict::Fine
        );
        assert_eq!(
            check("xn--bcher-kva.example", "https://BÜCHER.example/"),
            Verdict::Fine
        );
        assert_eq!(
            check("bücher.example", "https://buecher.example/"),
            mismatch("bücher.example", "buecher.example")
        );
        assert_eq!(display_host("xn--bcher-kva.example"), "bücher.example");
        assert_eq!(
            text_domain("blåbær.no"),
            idna::domain_to_ascii("blåbær.no").ok()
        );
    }

    #[test]
    fn mixed_scripts_are_shown_in_punycode() {
        let cyrillic_a = "\u{0430}";
        let mixed = format!("p{cyrillic_a}ypal.com");
        let ascii = idna::domain_to_ascii(&mixed).unwrap();
        assert!(ascii.starts_with("xn--"));
        assert!(confusable(&ascii));
        assert_eq!(display_host(&ascii), ascii);
        assert_eq!(
            check("paypal.com", &format!("https://{mixed}/")),
            mismatch("paypal.com", &ascii)
        );
        let whole = idna::domain_to_ascii("\u{0430}\u{0440}\u{0440}\u{04cf}\u{0435}.com").unwrap();
        assert!(
            confusable(&whole),
            "Cyrillic letters only, posing as apple.com"
        );
        let russian = idna::domain_to_ascii("почта.рф").unwrap();
        assert!(!confusable(&russian), "Cyrillic under a Cyrillic domain");
        assert_eq!(display_host(&russian), "почта.рф");
        let greek = idna::domain_to_ascii("ελλάδα.gr").unwrap();
        assert!(
            !confusable(&greek),
            "Greek letters that don't all look Latin"
        );
        assert!(!confusable("example.com"));
        assert!(!confusable(&idna::domain_to_ascii("bücher.de").unwrap()));
        assert!(!confusable(&idna::domain_to_ascii("日本lab.jp").unwrap()));
    }

    #[test]
    fn the_status_strip_shows_the_real_target() {
        assert_eq!(
            status_text("https://xn--bcher-kva.example/a?b"),
            "https://bücher.example/a?b"
        );
        assert_eq!(
            status_text("mailto:ada%40example.com?subject=x"),
            "ada@example.com"
        );
        assert_eq!(
            status_text("https://bank.example@evil.example/"),
            "https://bank.example@evil.example/"
        );
        assert_eq!(status_text(" weird "), "weird");
    }

    #[test]
    fn plain_text_links_are_found() {
        let text = "See https://example.com/a?b=c. Or (www.example.org/wiki/Foo_(bar)) and \
                    <http://example.net/x>, mail ada@example.com or \
                    mailto:leah@example.com?subject=Hi now.";
        let found = find(text);
        let pieces: Vec<(&str, &str)> = found
            .iter()
            .map(|found| (&text[found.start..found.end], found.target.as_str()))
            .collect();
        assert_eq!(
            pieces,
            [
                ("https://example.com/a?b=c", "https://example.com/a?b=c"),
                (
                    "www.example.org/wiki/Foo_(bar)",
                    "https://www.example.org/wiki/Foo_(bar)"
                ),
                ("http://example.net/x", "http://example.net/x"),
                ("ada@example.com", "mailto:ada@example.com"),
                (
                    "mailto:leah@example.com?subject=Hi",
                    "mailto:leah@example.com?subject=Hi"
                ),
            ]
        );
    }

    #[test]
    fn dangerous_schemes_never_become_links() {
        let text = "javascript:alert(1) file:///etc/passwd data:text/html,<b>x</b> \
                    vbscript:x /relative/path ../up cid:logo@x sealwax:compose";
        let found = find(text);
        assert!(
            found
                .iter()
                .all(|found| found.target.starts_with("mailto:")),
            "{found:?}"
        );
        assert!(find("javascript:alert(1)").is_empty());
        assert!(find("file:///home/user/.ssh/id_ed25519").is_empty());
        assert!(find("data:text/html;base64,PHNjcmlwdD4=").is_empty());
    }

    #[test]
    fn link_edges() {
        assert!(find("http://").is_empty());
        assert!(find("www.").is_empty());
        assert!(find("www.example").is_empty(), "www. needs a second dot");
        assert!(find("nohttps://example.com").is_empty());
        assert!(find("mailto:nobody").is_empty());
        assert!(find("user@localhost").is_empty());
        let text = "HTTPS://EXAMPLE.COM/Ø!";
        let found = find(text);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].start..found[0].end], "HTTPS://EXAMPLE.COM/Ø");
        let text = "Se https://bü.example/ø og";
        let found = find(text);
        assert_eq!(&text[found[0].start..found[0].end], "https://bü.example/ø");
        let text = "https://example.com/a@b and x@example.com";
        let found = find(text);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[1].target, "mailto:x@example.com");
        let text = "'https://example.com/q'";
        let found = find(text);
        assert_eq!(&text[found[0].start..found[0].end], "https://example.com/q");
    }
}
