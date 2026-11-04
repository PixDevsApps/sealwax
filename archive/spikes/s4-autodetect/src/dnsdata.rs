use crate::model::{Protocol, Security, Server, Username};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Submissions,
    Submission,
    Imaps,
    Imap,
    Pop3s,
    Pop3,
}

impl Service {
    pub const ALL: [Service; 6] = [
        Service::Submissions,
        Service::Submission,
        Service::Imaps,
        Service::Imap,
        Service::Pop3s,
        Service::Pop3,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Service::Submissions => "_submissions._tcp",
            Service::Submission => "_submission._tcp",
            Service::Imaps => "_imaps._tcp",
            Service::Imap => "_imap._tcp",
            Service::Pop3s => "_pop3s._tcp",
            Service::Pop3 => "_pop3._tcp",
        }
    }

    fn protocol(self) -> (Protocol, Security) {
        match self {
            Service::Submissions => (Protocol::Smtp, Security::Tls),
            Service::Submission => (Protocol::Smtp, Security::StartTls),
            Service::Imaps => (Protocol::Imap, Security::Tls),
            Service::Imap => (Protocol::Imap, Security::StartTls),
            Service::Pop3s => (Protocol::Pop3, Security::Tls),
            Service::Pop3 => (Protocol::Pop3, Security::StartTls),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrvRecord {
    pub service: Service,
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

pub fn srv_servers(records: &[SrvRecord]) -> Vec<Server> {
    let mut usable: Vec<&SrvRecord> = records
        .iter()
        .filter(|r| r.port != 0 && !r.target.trim_end_matches('.').is_empty())
        .collect();
    usable.sort_by_key(|r| (r.priority, std::cmp::Reverse(r.weight)));
    usable
        .into_iter()
        .map(|r| {
            let (protocol, security) = r.service.protocol();
            Server::new(protocol, r.target.trim_end_matches('.'), r.port, security)
                .with_username(Username::Address)
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

pub fn registrable(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    psl::domain_str(&host).map(str::to_string)
}

pub fn mx_lookup_domains(records: &[Mx], domain: &str, limit: usize) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(service: Service, priority: u16, weight: u16, port: u16, target: &str) -> SrvRecord {
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
        let servers = srv_servers(&[
            srv(Service::Imap, 0, 0, 0, "."),
            srv(Service::Imaps, 0, 1, 993, "imap.example.com."),
        ]);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].host, "imap.example.com");
        assert_eq!(servers[0].security, Security::Tls);
    }

    #[test]
    fn srv_orders_by_priority_then_weight() {
        let servers = srv_servers(&[
            srv(Service::Submission, 10, 0, 587, "b.example."),
            srv(Service::Submission, 0, 5, 587, "c.example."),
            srv(Service::Submission, 0, 10, 587, "a.example."),
        ]);
        let hosts: Vec<&str> = servers.iter().map(|s| s.host.as_str()).collect();
        assert_eq!(hosts, ["a.example", "c.example", "b.example"]);
        assert!(servers.iter().all(|s| s.security == Security::StartTls));
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
    fn mx_registrable_domains_follow_the_public_suffix_list() {
        assert_eq!(
            registrable("aspmx.l.google.com.").as_deref(),
            Some("google.com")
        );
        assert_eq!(
            registrable("mx.zoho.com.au").as_deref(),
            Some("zoho.com.au")
        );
        assert_eq!(
            registrable("oslomet-no.m-v1.mx.microsoft.").as_deref(),
            Some("mx.microsoft")
        );
        assert_eq!(
            registrable("mx1.pub.mailpod12-cph3.one.com").as_deref(),
            Some("one.com")
        );
        assert_eq!(
            registrable("mx.domeneshop.no").as_deref(),
            Some("domeneshop.no")
        );
    }

    #[test]
    fn mx_lookup_domains_skip_self_and_duplicates() {
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
            mx_lookup_domains(&records, "example.org", 2),
            ["google.com", "googlemail.com"]
        );
    }
}
