use crate::dnsdata::acceptable_host;
use crate::model::Security;
use crate::truth::{Accept, Expect, Truth, User};

pub struct Row {
    pub domain: &'static str,
    pub group: &'static str,
    pub truth: Option<Truth>,
    pub evidence: &'static str,
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_string().into_boxed_str())
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
    Ok(Box::leak(list.into_boxed_slice()))
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
                host: leak(&host),
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
    Ok(Box::leak(list.into_boxed_slice()))
}

pub fn parse(text: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |e: String| format!("line {}: {e}", number + 1);
        let fields: Vec<&str> = line.split('\t').collect();
        let [domain, group, expect, imap, smtp, evidence] = fields[..] else {
            return Err(at(format!("expected 6 fields, found {}", fields.len())));
        };
        let domain = leak(&domain.trim().to_ascii_lowercase());
        let group = leak(group.trim());
        let evidence = leak(evidence.trim());
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
        if rows.iter().any(|r: &Row| r.domain == domain) {
            return Err(at(format!("duplicate domain {domain}")));
        }
        rows.push(Row {
            domain,
            group,
            truth: expect.map(|expect| Truth {
                domain,
                group,
                expect,
                evidence,
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
        example.no\tnordic\tsettings\timap.example.no:993:tls:addr\tsmtp.example.no:465:tls:addr ; smtp.example.no:587:starttls:local/addr\thttps://example.no/help\n\
        \n\
        gone.example\tnordic\tno-mail\t-\t-\tclosed 2013\n\
        mystery.example\teurope\tunknown\t-\t-\tno docs found\n";

    #[test]
    fn parses_rows_endpoints_and_username_forms() {
        let rows = parse(SAMPLE).unwrap();
        assert_eq!(rows.len(), 3);
        let Some(Truth {
            expect: Expect::Settings { imap, smtp },
            ..
        }) = &rows[0].truth
        else {
            panic!("settings row");
        };
        assert_eq!(imap[0].host, "imap.example.no");
        assert_eq!((imap[0].port, imap[0].security), (993, Security::Tls));
        assert_eq!(smtp.len(), 2);
        assert_eq!(smtp[1].security, Security::StartTls);
        assert_eq!(smtp[1].users, [User::LocalPart, User::Address]);
        assert!(matches!(
            rows[1].truth.as_ref().map(|t| t.expect),
            Some(Expect::NoMail)
        ));
        assert!(rows[2].truth.is_none());
        assert_eq!(rows[2].group, "europe");
    }

    #[test]
    fn rejects_malformed_rows() {
        assert!(parse("a.example\tnordic\tsettings\timap.a.example:993:tls\t-\tx").is_err());
        assert!(parse("a.example\tnordic\tsettings\t127.0.0.1:993:tls:addr\t-\tx").is_err());
        assert!(parse("a.example\tnordic\tmaybe\t-\t-\tx").is_err());
        assert!(parse("a.example\tnordic\tno-mail\t-\t-").is_err());
        assert!(parse("a.example\tx\tno-mail\t-\t-\tx\na.example\tx\tno-mail\t-\t-\tx").is_err());
    }
}
