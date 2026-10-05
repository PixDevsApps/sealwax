//! The measured lists: S4's 50 domains (its truth table, compiled in from the spike so it can't
//! drift) and a held-out TSV in S4b's format.

use std::path::{Path, PathBuf};

use sealwax_domain::account::Security;
use sealwax_domain::autoconfig::dns::acceptable_host;

use crate::truth::{self, Accept, Expect, Truth, User};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    S4,
    Heldout,
}

pub struct Entry {
    pub domain: &'static str,
    pub group: &'static str,
    /// `None` when the documentation was not found ("truth unknown"): not scored.
    pub truth: Option<&'static Truth>,
    pub evidence: &'static str,
}

pub fn heldout_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data/heldout.tsv")
}

pub fn entries(list: List) -> Result<Vec<Entry>, String> {
    match list {
        List::S4 => Ok(truth::TRUTH
            .iter()
            .map(|t| Entry {
                domain: t.domain,
                group: t.group,
                truth: truth::find(t.domain),
                evidence: t.evidence,
            })
            .collect()),
        List::Heldout => {
            let path = heldout_path();
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            parse(&text)
        }
    }
}

fn leak<T: ?Sized>(value: Box<T>) -> &'static T {
    Box::leak(value)
}

fn users(text: &str) -> Result<&'static [User], String> {
    let list = text
        .split('/')
        .map(|user| match user.trim() {
            "addr" => Ok(User::Address),
            "local" => Ok(User::LocalPart),
            "other" => Ok(User::Other),
            other => Err(format!("unknown username form {other:?}")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(leak(list.into_boxed_slice()))
}

fn endpoints(text: &str) -> Result<&'static [Accept], String> {
    let list = text
        .split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|endpoint| {
            let parts: Vec<&str> = endpoint.split(':').collect();
            let [host, port, mode, forms] = parts[..] else {
                return Err(format!("bad endpoint {endpoint:?}"));
            };
            let host = host.trim().to_ascii_lowercase();
            if !acceptable_host(&host) {
                return Err(format!("bad host {host:?}"));
            }
            let security = match mode.trim() {
                "tls" | "ssl" => Security::Tls,
                "starttls" => Security::StartTls,
                other => return Err(format!("bad mode {other:?}")),
            };
            Ok(Accept {
                host: leak(host.into_boxed_str()),
                port: port
                    .trim()
                    .parse()
                    .map_err(|_| format!("bad port {port:?}"))?,
                security,
                users: users(forms)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if list.is_empty() {
        return Err("no endpoints".into());
    }
    Ok(leak(list.into_boxed_slice()))
}

/// S4b's held-out format: `domain group expect imap smtp evidence`, tab-separated, `#` comments.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let mut rows: Vec<Entry> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |e: String| format!("line {}: {e}", number + 1);
        let fields: Vec<&str> = line.split('\t').collect();
        let [domain, group, expect, imap, smtp, evidence] = fields[..] else {
            return Err(at(format!("expected 6 fields, found {}", fields.len())));
        };
        let domain: &'static str = leak(domain.trim().to_ascii_lowercase().into_boxed_str());
        let group: &'static str = leak(group.trim().to_string().into_boxed_str());
        let evidence: &'static str = leak(evidence.trim().to_string().into_boxed_str());
        let expect = match expect.trim() {
            "settings" => Some(Expect::Settings {
                imap: endpoints(imap).map_err(at)?,
                smtp: endpoints(smtp).map_err(at)?,
            }),
            "unsupported" => Some(Expect::Unsupported),
            "no-mail" => Some(Expect::NoMail),
            "unknown" => None,
            other => return Err(at(format!("bad expect {other:?}"))),
        };
        if rows.iter().any(|r| r.domain == domain) {
            return Err(at(format!("duplicate domain {domain}")));
        }
        rows.push(Entry {
            domain,
            group,
            truth: expect.map(|expect| {
                leak(Box::new(Truth {
                    domain,
                    group,
                    expect,
                    evidence,
                }))
            }),
            evidence,
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# comment\n\
        example.test\tnordic\tsettings\timap.example.test:993:tls:addr\tsmtp.example.test:465:tls:addr ; smtp.example.test:587:starttls:local/addr\thttps://example.test/help\n\
        \n\
        gone.test\tnordic\tno-mail\t-\t-\tclosed 2013\n\
        mystery.test\teurope\tunknown\t-\t-\tno docs found\n";

    #[test]
    fn parses_rows_endpoints_and_username_forms() {
        let rows = parse(SAMPLE).unwrap();
        assert_eq!(rows.len(), 3);
        let Some(Truth {
            expect: Expect::Settings { imap, smtp },
            ..
        }) = rows[0].truth
        else {
            panic!("settings row");
        };
        assert_eq!(imap[0].host, "imap.example.test");
        assert_eq!((imap[0].port, imap[0].security), (993, Security::Tls));
        assert_eq!(smtp.len(), 2);
        assert_eq!(smtp[1].security, Security::StartTls);
        assert_eq!(smtp[1].users, [User::LocalPart, User::Address]);
        assert!(matches!(
            rows[1].truth.map(|t| t.expect),
            Some(Expect::NoMail)
        ));
        assert!(rows[2].truth.is_none());
        assert_eq!(rows[2].group, "europe");
    }

    #[test]
    fn rejects_malformed_rows() {
        assert!(parse("a.test\tnordic\tsettings\timap.a.test:993:tls\t-\tx").is_err());
        assert!(parse("a.test\tnordic\tsettings\t127.0.0.1:993:tls:addr\t-\tx").is_err());
        assert!(parse("a.test\tnordic\tmaybe\t-\t-\tx").is_err());
        assert!(parse("a.test\tnordic\tno-mail\t-\t-").is_err());
        assert!(parse("a.test\tx\tno-mail\t-\t-\tx\na.test\tx\tno-mail\t-\t-\tx").is_err());
    }

    #[test]
    fn s4s_list_has_fifty_rows() {
        let rows = entries(List::S4).unwrap();
        assert_eq!(rows.len(), 50);
        assert!(rows.iter().all(|r| r.truth.is_some()));
    }
}
