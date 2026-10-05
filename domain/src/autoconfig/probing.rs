//! Guarded probing: which names and ports to try, in what order, and when the answer is known.
//! A probe only completes a TLS handshake with a verified certificate, reads the greeting and asks
//! CAPABILITY or EHLO (STARTTLS when the port needs it); it never logs in.

use super::{Finding, Hints, Server};
use crate::account::{Protocol, Security, UsernameForm};

/// One TLS check.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub security: Security,
}

impl Target {
    pub fn new(protocol: Protocol, host: &str, port: u16, security: Security) -> Self {
        Self {
            protocol,
            host: host.trim_end_matches('.').to_ascii_lowercase(),
            port,
            security,
        }
    }

    pub fn of(server: &Server) -> Self {
        Self::new(server.protocol, &server.host, server.port, server.security)
    }

    pub fn server(&self) -> Server {
        Server::new(self.protocol, &self.host, self.port, self.security)
            .with_username(UsernameForm::Address)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Names under the user's own domain.
    Own,
    /// R3: names at the MX operator's registrable domain.
    MxOperator,
}

/// One host's ports for one protocol, implicit TLS first (RFC 8314). `order` ranks the chain
/// among those of its protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    pub protocol: Protocol,
    pub scope: Scope,
    pub order: usize,
    pub targets: Vec<Target>,
}

fn chain_targets(host: &str, protocol: Protocol) -> Vec<Target> {
    let ports = match protocol {
        Protocol::Imap => [(993, Security::Tls), (143, Security::StartTls)],
        Protocol::Smtp => [(465, Security::Tls), (587, Security::StartTls)],
    };
    ports
        .iter()
        .map(|&(port, security)| Target::new(protocol, host, port, security))
        .collect()
}

/// The user's `imap.`/`smtp.`, `mail.` and bare domain, then each MX operator's `imap.`/`smtp.`
/// and `mail.` names.
pub fn chains(domain: &str, mx_operators: &[String]) -> Vec<Chain> {
    let mut chains = Vec::new();
    for protocol in [Protocol::Imap, Protocol::Smtp] {
        let label = match protocol {
            Protocol::Imap => "imap.",
            Protocol::Smtp => "smtp.",
        };
        let mut hosts: Vec<(Scope, String)> = [label, "mail.", ""]
            .iter()
            .map(|prefix| (Scope::Own, format!("{prefix}{domain}")))
            .collect();
        for operator in mx_operators {
            for prefix in [label, "mail."] {
                hosts.push((Scope::MxOperator, format!("{prefix}{operator}")));
            }
        }
        for (order, (scope, host)) in hosts.into_iter().enumerate() {
            chains.push(Chain {
                protocol,
                scope,
                order,
                targets: chain_targets(&host, protocol),
            });
        }
    }
    chains
}

/// What a chain's probes showed: `Some(target)` that answered, or `None` when all failed.
pub type ChainResult = Option<Target>;

/// Whether `protocol`'s answer is known: a chain answered and every higher-ranked chain has
/// failed, or all have failed.
pub fn decided(chains: &[Chain], results: &[(usize, ChainResult)], protocol: Protocol) -> bool {
    let mut ordered: Vec<(usize, &Chain)> = chains
        .iter()
        .enumerate()
        .filter(|(_, c)| c.protocol == protocol)
        .collect();
    ordered.sort_by_key(|(_, c)| c.order);
    for (index, _) in ordered {
        match results.iter().find(|(i, _)| *i == index) {
            None => return false,
            Some((_, Some(_))) => return true,
            Some((_, None)) => {}
        }
    }
    true
}

/// The servers of one scope's best chains. At the MX operator a host counts only when the
/// operator answers both IMAP and submission, as a mailbox host does: an IMAP server alone may
/// be the operator's own (S4b offered a domain marketplace's server for a domain without mail).
pub fn finding(chains: &[Chain], results: &[(usize, ChainResult)], scope: Scope) -> Finding {
    let mut servers = Vec::new();
    for protocol in [Protocol::Imap, Protocol::Smtp] {
        let mut answered: Vec<(usize, &Target)> = results
            .iter()
            .filter_map(|(index, result)| {
                let chain = &chains[*index];
                (chain.protocol == protocol && chain.scope == scope)
                    .then_some(result.as_ref().map(|t| (chain.order, t)))
                    .flatten()
            })
            .collect();
        answered.sort_by_key(|(order, _)| *order);
        if let Some((_, target)) = answered.first() {
            servers.push(target.server());
        }
    }
    let both = servers.len() == 2;
    if servers.is_empty() || (scope == Scope::MxOperator && !both) {
        Finding::Nothing
    } else {
        Finding::Servers {
            servers,
            hints: Hints::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_of(chains: &[Chain], protocol: Protocol, host: &str) -> usize {
        chains
            .iter()
            .position(|c| c.protocol == protocol && c.targets[0].host == host)
            .unwrap()
    }

    fn ok(chains: &[Chain], index: usize) -> (usize, ChainResult) {
        (index, Some(chains[index].targets[0].clone()))
    }

    #[test]
    fn own_domain_hosts_come_before_mx_hosts() {
        let chains = chains("example.test", &["host.test".to_string()]);
        let imap: Vec<&str> = chains
            .iter()
            .filter(|c| c.protocol == Protocol::Imap)
            .map(|c| c.targets[0].host.as_str())
            .collect();
        assert_eq!(
            imap,
            [
                "imap.example.test",
                "mail.example.test",
                "example.test",
                "imap.host.test",
                "mail.host.test"
            ]
        );
        let smtp = &chains[index_of(&chains, Protocol::Smtp, "smtp.host.test")];
        assert_eq!(smtp.scope, Scope::MxOperator);
        assert_eq!(
            smtp.targets
                .iter()
                .map(|t| (t.port, t.security))
                .collect::<Vec<_>>(),
            [(465, Security::Tls), (587, Security::StartTls)]
        );
        assert_eq!(chains.len(), 10);
        assert_eq!(super::chains("example.test", &[]).len(), 6);
    }

    #[test]
    fn a_preferred_host_decides_without_waiting_for_the_rest() {
        let chains = chains("d.test", &[]);
        let first = index_of(&chains, Protocol::Imap, "imap.d.test");
        let results = vec![ok(&chains, first)];
        assert!(decided(&chains, &results, Protocol::Imap));
        assert!(!decided(&chains, &results, Protocol::Smtp));
    }

    #[test]
    fn an_mx_host_waits_for_every_own_domain_host() {
        let chains = chains("d.test", &["host.test".to_string()]);
        let mx = index_of(&chains, Protocol::Imap, "imap.host.test");
        let mut results = vec![ok(&chains, mx)];
        assert!(!decided(&chains, &results, Protocol::Imap));
        for host in ["imap.d.test", "mail.d.test", "d.test"] {
            results.push((index_of(&chains, Protocol::Imap, host), None));
        }
        assert!(decided(&chains, &results, Protocol::Imap));
    }

    #[test]
    fn all_failed_is_decided() {
        let chains = chains("d.test", &[]);
        let results: Vec<_> = chains
            .iter()
            .enumerate()
            .filter(|(_, c)| c.protocol == Protocol::Smtp)
            .map(|(i, _)| (i, None))
            .collect();
        assert!(decided(&chains, &results, Protocol::Smtp));
    }

    #[test]
    fn an_mx_host_needs_both_imap_and_submission() {
        let chains = chains("d.test", &["host.test".to_string()]);
        let imap = ok(&chains, index_of(&chains, Protocol::Imap, "imap.host.test"));
        let smtp = ok(&chains, index_of(&chains, Protocol::Smtp, "smtp.host.test"));
        assert_eq!(
            finding(&chains, std::slice::from_ref(&imap), Scope::MxOperator),
            Finding::Nothing
        );
        assert!(matches!(
            finding(&chains, &[imap.clone(), smtp], Scope::MxOperator),
            Finding::Servers { .. }
        ));
        assert_eq!(
            finding(&chains, std::slice::from_ref(&imap), Scope::Own),
            Finding::Nothing
        );
        let own = ok(&chains, index_of(&chains, Protocol::Imap, "mail.d.test"));
        let Finding::Servers { servers, .. } = finding(&chains, &[own, imap], Scope::Own) else {
            panic!("an own host alone counts");
        };
        assert_eq!(servers[0].host, "mail.d.test");
        assert_eq!(servers[0].username, UsernameForm::Address);
    }

    #[test]
    fn the_best_ranked_answer_wins() {
        let chains = chains("d.test", &[]);
        let results = vec![
            ok(&chains, index_of(&chains, Protocol::Imap, "d.test")),
            ok(&chains, index_of(&chains, Protocol::Imap, "imap.d.test")),
        ];
        let Finding::Servers { servers, .. } = finding(&chains, &results, Scope::Own) else {
            panic!("expected servers");
        };
        assert_eq!(servers[0].host, "imap.d.test");
    }
}
