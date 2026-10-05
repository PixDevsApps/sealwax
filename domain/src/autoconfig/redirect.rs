//! Which HTTPS redirects autodetect follows (SECURITY.md, Privacy). A redirect is followed only
//! over HTTPS, at most three times, to the same host or inside the requested domain; and one that
//! would carry the address anywhere but the host first asked is refused. S4b found
//! `autoconfig.msn.com` redirecting to `www.msn.com` with the address kept.

pub const MAX_REDIRECTS: usize = 3;

/// One redirect, as the HTTP client sees it.
#[derive(Clone, Copy, Debug)]
pub struct Redirect<'a> {
    /// The host of the first request.
    pub original_host: &'a str,
    /// The address the first request carried (its `emailaddress` parameter), if any.
    pub address: Option<&'a str>,
    /// The host that answered with this redirect.
    pub previous_host: &'a str,
    pub scheme: &'a str,
    pub host: &'a str,
    /// The whole target URL.
    pub url: &'a str,
    /// Redirects followed before this one.
    pub hops: usize,
}

/// The domain a request may redirect within: the user's domain for `autoconfig.<domain>`, the
/// ISPDB's for `autoconfig.thunderbird.net`, the host itself otherwise.
pub fn scope(original_host: &str) -> &str {
    original_host
        .strip_prefix("autoconfig.")
        .filter(|rest| rest.contains('.'))
        .unwrap_or(original_host)
}

pub fn follow(redirect: &Redirect<'_>) -> bool {
    let host = redirect.host.trim_end_matches('.');
    let same_host = host.eq_ignore_ascii_case(redirect.previous_host);
    let inside = super::dns::within(host, scope(redirect.original_host));
    let keeps_address = redirect
        .address
        .is_some_and(|address| carries(redirect.url, address));
    redirect.scheme == "https"
        && redirect.hops < MAX_REDIRECTS
        && (same_host || inside)
        && (!keeps_address || host.eq_ignore_ascii_case(redirect.original_host))
}

/// Whether `url` contains `address`, its local part followed by an `@`, or any `@` in its query,
/// after up to two rounds of percent-decoding, ignoring case.
pub fn carries(url: &str, address: &str) -> bool {
    let address = address.to_lowercase();
    let local = address.rsplit_once('@').map_or("", |(local, _)| local);
    let once = percent_decode(url);
    let twice = percent_decode(&once);
    [
        url.to_lowercase(),
        once.to_lowercase(),
        twice.to_lowercase(),
    ]
    .iter()
    .any(|text| {
        text.contains(&address)
            || (!local.is_empty() && text.contains(&format!("{local}@")))
            || text
                .split_once('?')
                .is_some_and(|(_, query)| query.contains('@'))
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(&h), Some(&l)) if hex(h).is_some() && hex(l).is_some() => {
                out.push((hex(h).unwrap_or(0) * 16 + hex(l).unwrap_or(0)) as u8);
                i += 3;
            }
            (b'+', ..) => {
                out.push(b' ');
                i += 1;
            }
            (b, ..) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "test@example.com";

    fn redirect<'a>(original: &'a str, address: Option<&'a str>, url: &'a str) -> Redirect<'a> {
        let scheme = url.split_once("://").map_or("", |(s, _)| s);
        let host = url
            .split_once("://")
            .map_or("", |(_, rest)| rest.split(['/', '?']).next().unwrap_or(""));
        Redirect {
            original_host: original,
            address,
            previous_host: original,
            scheme,
            host,
            url,
            hops: 0,
        }
    }

    #[test]
    fn a_redirect_carrying_the_address_to_another_host_is_refused() {
        let msn = redirect(
            "autoconfig.example.com",
            Some(ADDRESS),
            "https://www.example.com/?emailaddress=test%40example.com",
        );
        assert!(!follow(&msn));
        let same_host = redirect(
            "autoconfig.example.com",
            Some(ADDRESS),
            "https://autoconfig.example.com/mail/config-v1.1.xml?emailaddress=test%40example.com",
        );
        assert!(follow(&same_host));
        let dropped = redirect(
            "autoconfig.example.com",
            Some(ADDRESS),
            "https://auto.example.com/mail/config-v1.1.xml",
        );
        assert!(follow(&dropped));
    }

    #[test]
    fn redirects_stay_on_https_inside_the_domain() {
        for url in [
            "http://autoconfig.example.com/mail/config-v1.1.xml",
            "https://www.other.net/",
            "https://example.com.evil.net/",
        ] {
            assert!(
                !follow(&redirect("autoconfig.example.com", None, url)),
                "{url}"
            );
        }
        assert!(follow(&redirect(
            "autoconfig.thunderbird.net",
            None,
            "https://autoconfig.thunderbird.net/v1.1/example.com"
        )));
        assert!(!follow(&redirect(
            "autoconfig.thunderbird.net",
            None,
            "https://example.com/"
        )));
        let mut far = redirect("autoconfig.example.com", None, "https://example.com/x");
        far.hops = MAX_REDIRECTS;
        assert!(!follow(&far));
    }

    #[test]
    fn encoded_addresses_are_recognized() {
        assert!(carries("https://h/?e=TEST%40Example.com", ADDRESS));
        assert!(carries("https://h/?e=test%2540example.com", ADDRESS));
        assert!(carries("https://h/test@example.com/config", ADDRESS));
        assert!(carries("https://h/?login=test%40", ADDRESS));
        assert!(carries("https://h/?x=a@b", ADDRESS));
        assert!(!carries("https://h/mail/config-v1.1.xml", ADDRESS));
        assert!(!carries("https://h/%zz%4", ADDRESS));
    }

    #[test]
    fn scopes() {
        assert_eq!(scope("autoconfig.example.com"), "example.com");
        assert_eq!(scope("autoconfig.thunderbird.net"), "thunderbird.net");
        assert_eq!(scope("example.com"), "example.com");
        assert_eq!(scope("autoconfig.com"), "autoconfig.com");
    }
}
