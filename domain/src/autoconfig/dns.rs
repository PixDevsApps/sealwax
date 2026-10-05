//! What DNS answers mean for autodetect: SRV records (RFC 6186, RFC 8314), MX records (a null MX
//! is RFC 7505), host names a network source may offer, and addresses that may be contacted.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::Server;
use crate::account::{Protocol, Security, UsernameForm};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SrvService {
    Submissions,
    Submission,
    Imaps,
    Imap,
}

impl SrvService {
    pub const ALL: [SrvService; 4] = [
        SrvService::Submissions,
        SrvService::Submission,
        SrvService::Imaps,
        SrvService::Imap,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SrvService::Submissions => "_submissions._tcp",
            SrvService::Submission => "_submission._tcp",
            SrvService::Imaps => "_imaps._tcp",
            SrvService::Imap => "_imap._tcp",
        }
    }

    pub fn protocol(self) -> Protocol {
        match self {
            SrvService::Submissions | SrvService::Submission => Protocol::Smtp,
            SrvService::Imaps | SrvService::Imap => Protocol::Imap,
        }
    }

    fn security(self) -> Security {
        match self {
            SrvService::Submissions | SrvService::Imaps => Security::Tls,
            SrvService::Submission | SrvService::Imap => Security::StartTls,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrvRecord {
    pub service: SrvService,
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

/// The servers SRV records offer, by priority, then weight. A target of `.` means the service is
/// not offered. With `implicit_tls_ports`, a STARTTLS service published on the implicit-TLS port
/// (`_submission` on 465, `_imap` on 993) is read as implicit TLS: S4b met an `_submission`
/// record on 465 whose literal reading fails.
pub fn srv_servers(records: &[SrvRecord], implicit_tls_ports: bool) -> Vec<Server> {
    let mut usable: Vec<&SrvRecord> = records
        .iter()
        .filter(|r| r.port != 0 && acceptable_host(&r.target))
        .collect();
    usable.sort_by_key(|r| (r.priority, std::cmp::Reverse(r.weight)));
    usable
        .into_iter()
        .map(|r| {
            let security = match (r.service, r.port) {
                (SrvService::Submission, 465) | (SrvService::Imap, 993) if implicit_tls_ports => {
                    Security::Tls
                }
                (service, _) => service.security(),
            };
            Server::new(r.service.protocol(), &r.target, r.port, security)
                .with_username(UsernameForm::Address)
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mx {
    pub preference: u16,
    pub exchange: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MxAnswer {
    Records(Vec<Mx>),
    /// RFC 7505: the domain accepts no mail.
    NullMx,
    NoRecords,
    NxDomain,
}

pub fn classify_mx(mut records: Vec<Mx>) -> MxAnswer {
    if records.is_empty() {
        return MxAnswer::NoRecords;
    }
    for mx in &mut records {
        mx.exchange = mx.exchange.trim_end_matches('.').to_ascii_lowercase();
    }
    if records.len() == 1 && records[0].exchange.is_empty() {
        return MxAnswer::NullMx;
    }
    records.retain(|mx| !mx.exchange.is_empty());
    records.sort_by_key(|mx| mx.preference);
    MxAnswer::Records(records)
}

/// The registrable domain (public suffix list) of a host.
pub fn registrable(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    psl::domain_str(&host).map(str::to_string)
}

/// Whether `host` is `domain` or a name under it.
pub fn within(host: &str, domain: &str) -> bool {
    let host = host.trim_end_matches('.');
    let domain = domain.trim_end_matches('.');
    let Some(cut) = host.len().checked_sub(domain.len()) else {
        return false;
    };
    host.get(cut..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(domain))
        && (cut == 0 || host.get(..cut).is_some_and(|head| head.ends_with('.')))
}

pub fn ip_literal(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok()
}

/// A name a network source may offer as a server: a dotted DNS name, never an IP literal or
/// `localhost` (Proton's autoconfig offers its Bridge on 127.0.0.1).
pub fn acceptable_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    !host.is_empty()
        && host.contains('.')
        && !ip_literal(&host)
        && host != "localhost"
        && !host.ends_with(".localhost")
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
        && host.split('.').all(|label| !label.is_empty())
}

/// An address autodetect may connect to: not loopback, private, link-local, CGNAT, unique-local,
/// documentation or other reserved ranges.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public_v4(v4),
            None => public_v6(v6),
        },
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || a >= 240
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19)))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let [first, second, ..] = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || (first == 0x2001 && second == 0x0db8)
        || (first == 0x0064 && second == 0xff9b))
}

pub fn conventional_label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Imap => "imap",
        Protocol::Smtp => "smtp",
    }
}

/// R2: the conventional name of an SRV target in the same parent domain (`mail.example.net` →
/// `imap.example.net`), or `None` when the target already has it or is a registrable domain.
pub fn conventional_sibling(host: &str, protocol: Protocol) -> Option<String> {
    let label = conventional_label(protocol);
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let base = registrable(&host)?;
    let (first, parent) = host.split_once('.')?;
    if first == label || host == base || !within(parent, &base) {
        return None;
    }
    Some(format!("{label}.{parent}"))
}

/// The registrable domains of the MX hosts, in preference order, without the user's own domain,
/// at most `limit` (R3 and the MX operator's ISPDB entry).
pub fn mx_operators(records: &[Mx], domain: &str, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for mx in records {
        let Some(base) = registrable(&mx.exchange) else {
            continue;
        };
        if base != domain && !out.contains(&base) {
            out.push(base);
        }
        if out.len() == limit {
            break;
        }
    }
    out
}

/// Exchange Online's DNS setup aliases `autodiscover.<domain>` to this name; an organization whose
/// own MX gateway hides Microsoft 365 still has it.
pub const MICROSOFT_AUTODISCOVER: &str = "autodiscover.outlook.com";

pub fn autodiscover_is_microsoft(cname_target: &str) -> bool {
    cname_target
        .trim_end_matches('.')
        .eq_ignore_ascii_case(MICROSOFT_AUTODISCOVER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(service: SrvService, priority: u16, weight: u16, port: u16, target: &str) -> SrvRecord {
        SrvRecord {
            service,
            priority,
            weight,
            port,
            target: target.into(),
        }
    }

    #[test]
    fn srv_dot_target_means_not_offered() {
        let servers = srv_servers(
            &[
                srv(SrvService::Imap, 0, 0, 0, "."),
                srv(SrvService::Imaps, 0, 1, 993, "imap.example.com."),
            ],
            true,
        );
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].host, "imap.example.com");
        assert_eq!(servers[0].security, Security::Tls);
    }

    #[test]
    fn srv_orders_by_priority_then_weight() {
        let servers = srv_servers(
            &[
                srv(SrvService::Submission, 10, 0, 587, "b.example."),
                srv(SrvService::Submission, 0, 5, 587, "c.example."),
                srv(SrvService::Submission, 0, 10, 587, "a.example."),
            ],
            true,
        );
        let hosts: Vec<&str> = servers.iter().map(|s| s.host.as_str()).collect();
        assert_eq!(hosts, ["a.example", "c.example", "b.example"]);
        assert!(servers.iter().all(|s| s.security == Security::StartTls));
    }

    #[test]
    fn implicit_tls_ports_under_starttls_services_mean_implicit_tls() {
        let records = [
            srv(SrvService::Submission, 0, 0, 465, "ssl0.example.net."),
            srv(SrvService::Imap, 0, 0, 993, "ssl0.example.net."),
            srv(SrvService::Submission, 0, 0, 587, "smtp.example.net."),
        ];
        let read = srv_servers(&records, true);
        assert_eq!(read[0].security, Security::Tls);
        assert_eq!(read[1].security, Security::Tls);
        assert_eq!(read[2].security, Security::StartTls);
        let literal = srv_servers(&records, false);
        assert!(literal.iter().all(|s| s.security == Security::StartTls));
    }

    #[test]
    fn srv_never_yields_ip_literals_or_localhost() {
        let servers = srv_servers(
            &[
                srv(SrvService::Imaps, 0, 0, 993, "127.0.0.1."),
                srv(SrvService::Imaps, 0, 0, 993, "[::1]"),
                srv(SrvService::Imap, 0, 0, 143, "localhost."),
                srv(SrvService::Submission, 0, 0, 587, "smtp.example.com."),
            ],
            true,
        );
        let hosts: Vec<&str> = servers.iter().map(|s| s.host.as_str()).collect();
        assert_eq!(hosts, ["smtp.example.com"]);
    }

    #[test]
    fn null_mx_is_detected() {
        let null = vec![Mx {
            preference: 0,
            exchange: ".".into(),
        }];
        assert_eq!(classify_mx(null), MxAnswer::NullMx);
        assert_eq!(classify_mx(Vec::new()), MxAnswer::NoRecords);
    }

    #[test]
    fn registrable_domains_follow_the_public_suffix_list() {
        assert_eq!(
            registrable("aspmx.l.google.com.").as_deref(),
            Some("google.com")
        );
        assert_eq!(
            registrable("mx.zoho.com.au").as_deref(),
            Some("zoho.com.au")
        );
        assert_eq!(
            registrable("example-com.m-v1.mx.microsoft.").as_deref(),
            Some("mx.microsoft")
        );
        assert_eq!(
            registrable("mx1.pub.mailpod12-cph3.host.example.co.uk").as_deref(),
            Some("example.co.uk")
        );
    }

    #[test]
    fn mx_operators_skip_the_users_domain_and_duplicates() {
        let MxAnswer::Records(records) = classify_mx(vec![
            Mx {
                preference: 10,
                exchange: "ALT1.ASPMX.L.GOOGLE.COM.".into(),
            },
            Mx {
                preference: 1,
                exchange: "aspmx.l.google.com.".into(),
            },
            Mx {
                preference: 20,
                exchange: "mx.example.org.".into(),
            },
            Mx {
                preference: 30,
                exchange: "aspmx2.googlemail.com.".into(),
            },
        ]) else {
            panic!("expected records");
        };
        assert_eq!(
            mx_operators(&records, "example.org", 2),
            ["google.com", "googlemail.com"]
        );
    }

    #[test]
    fn within_respects_label_boundaries() {
        assert!(within("example.com", "example.com"));
        assert!(within("www.Example.com.", "example.com"));
        assert!(!within("example.com.evil.net", "example.com"));
        assert!(!within("badexample.com", "example.com"));
        assert!(!within("com", "example.com"));
        assert!(!within("ä.com", "a.com"));
        assert!(!within("example.com", ""));
    }

    #[test]
    fn acceptable_hosts_are_dns_names() {
        assert!(acceptable_host("imap.example.com."));
        assert!(!acceptable_host("127.0.0.1"));
        assert!(!acceptable_host("[::1]"));
        assert!(!acceptable_host("localhost"));
        assert!(!acceptable_host("mail.localhost"));
        assert!(!acceptable_host("intranet"));
        assert!(!acceptable_host("imap.example.com/path"));
        assert!(!acceptable_host("imap..example.com"));
        assert!(!acceptable_host("."));
    }

    #[test]
    fn public_addresses_exclude_local_and_reserved_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.1",
            "169.254.0.1",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "192.0.2.1",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
            "64:ff9b::1",
        ] {
            assert!(!public_address(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "2a00:1450:4010:c05::6c",
            "::ffff:1.1.1.1",
        ] {
            assert!(public_address(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn conventional_siblings_replace_only_a_generic_first_label() {
        assert_eq!(
            conventional_sibling("mail.example.no", Protocol::Imap).as_deref(),
            Some("imap.example.no")
        );
        assert_eq!(
            conventional_sibling("mail.example.no.", Protocol::Smtp).as_deref(),
            Some("smtp.example.no")
        );
        assert_eq!(
            conventional_sibling("mx1.mail.example.co.uk", Protocol::Imap).as_deref(),
            Some("imap.mail.example.co.uk")
        );
        assert_eq!(
            conventional_sibling("imap.example.no", Protocol::Imap),
            None
        );
        assert_eq!(conventional_sibling("example.de", Protocol::Imap), None);
        assert_eq!(
            conventional_sibling("imap.example.no", Protocol::Smtp).as_deref(),
            Some("smtp.example.no")
        );
    }

    #[test]
    fn autodiscover_points_at_microsoft() {
        assert!(autodiscover_is_microsoft("autodiscover.outlook.com."));
        assert!(autodiscover_is_microsoft("Autodiscover.Outlook.com"));
        assert!(!autodiscover_is_microsoft("autodiscover.example.com"));
        assert!(!autodiscover_is_microsoft(
            "autodiscover.outlook.com.evil.test"
        ));
    }
}
