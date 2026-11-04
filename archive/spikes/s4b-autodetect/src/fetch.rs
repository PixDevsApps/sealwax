use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hickory_resolver::TokioResolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;

use crate::clientconfig::{self, ClientConfig};
use crate::dnsdata::{Mx, MxAnswer, Service, SrvRecord, classify_mx, public_address};

pub const USER_AGENT: &str = "mail-dev-spike (automated)";
const MAX_BODY: usize = 256 * 1024;
const MAX_REDIRECTS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub kind: &'static str,
    pub target: String,
    pub carries_address: bool,
}

#[derive(Clone, Default)]
pub struct Ledger(Arc<Mutex<Vec<Request>>>);

impl Ledger {
    pub fn record(&self, kind: &'static str, target: impl Into<String>, carries_address: bool) {
        if let Ok(mut entries) = self.0.lock() {
            entries.push(Request {
                kind,
                target: target.into(),
                carries_address,
            });
        }
    }

    pub fn entries(&self) -> Vec<Request> {
        self.0.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

pub fn within(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

pub struct Fqdn(pub Arc<TokioResolver>);

impl Resolve for Fqdn {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.0.clone();
        let host = format!("{}.", name.as_str().trim_end_matches('.'));
        Box::pin(async move {
            let lookup = resolver.lookup_ip(host).await?;
            let ips: Vec<IpAddr> = lookup.iter().collect();
            if ips.iter().any(|ip| !public_address(*ip)) {
                return Err("non-public address refused".into());
            }
            let addrs: Vec<SocketAddr> = ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect();
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

pub fn http_client(
    domain: &str,
    limit: Duration,
    ledger: Ledger,
    resolver: Arc<TokioResolver>,
) -> Result<reqwest::Client, String> {
    let domain = domain.to_string();
    let policy = Policy::custom(move |attempt| {
        let previous = attempt
            .previous()
            .last()
            .and_then(|u| u.host_str())
            .unwrap_or("")
            .to_string();
        let next = attempt.url();
        let scheme = next.scheme().to_string();
        let host = next.host_str().unwrap_or("").to_string();
        let carries_address = next
            .query()
            .is_some_and(|q| q.contains('@') || q.contains("%40"));
        let allowed = scheme == "https"
            && (host == previous || within(&host, &domain))
            && attempt.previous().len() <= MAX_REDIRECTS;
        ledger.record(
            if allowed {
                "redirect-followed"
            } else {
                "redirect-refused"
            },
            format!("{scheme}://{host}"),
            carries_address,
        );
        if allowed {
            attempt.follow()
        } else {
            attempt.error(format!("redirect to {scheme}://{host} refused"))
        }
    });
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .https_only(true)
        .dns_resolver(Fqdn(resolver))
        .redirect(policy)
        .connect_timeout(limit)
        .timeout(limit)
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|e| e.to_string())
}

pub enum Fetched {
    Config(ClientConfig),
    Missing(String),
}

pub async fn fetch_config(
    client: &reqwest::Client,
    url: &str,
    address: &str,
) -> Result<Fetched, String> {
    let mut response = client.get(url).send().await.map_err(|e| error_text(&e))?;
    let status = response.status();
    if !status.is_success() {
        return Ok(Fetched::Missing(format!("HTTP {}", status.as_u16())));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| error_text(&e))? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Ok(Fetched::Missing("body larger than 256 KiB".into()));
        }
    }
    let text = String::from_utf8_lossy(&body);
    match clientconfig::parse(&text, address) {
        Ok(config) => Ok(Fetched::Config(config)),
        Err(e) => Ok(Fetched::Missing(format!(
            "HTTP 200, not a clientConfig ({e})"
        ))),
    }
}

fn error_text(error: &reqwest::Error) -> String {
    let mut text = if error.is_timeout() {
        "timeout".to_string()
    } else if error.is_connect() {
        "connect failed".to_string()
    } else if error.is_redirect() {
        "redirect refused".to_string()
    } else {
        "request failed".to_string()
    };
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        let message = inner.to_string();
        if !message.contains("emailaddress=") {
            text.push_str(": ");
            text.push_str(&message);
        }
        source = inner.source();
    }
    text
}

pub fn resolver(limit: Duration, server: Option<IpAddr>) -> Result<TokioResolver, String> {
    let mut options = ResolverOpts::default();
    options.timeout = limit;
    options.attempts = 2;
    options.cache_size = 64;
    let mut builder = match server {
        None => TokioResolver::builder_tokio().map_err(|e| e.to_string())?,
        Some(ip) => TokioResolver::builder_with_config(
            ResolverConfig::from_name_servers(vec![NameServerConfig::udp_and_tcp(ip)]),
            TokioRuntimeProvider::default(),
        ),
    };
    *builder.options_mut() = options;
    builder.build().map_err(|e| e.to_string())
}

pub async fn addresses(resolver: &TokioResolver, host: &str) -> Result<Vec<IpAddr>, String> {
    let lookup = resolver
        .lookup_ip(format!("{}.", host.trim_end_matches('.')))
        .await
        .map_err(|e| {
            if e.is_nx_domain() || e.is_no_records_found() {
                "no addresses".to_string()
            } else {
                e.to_string()
            }
        })?;
    let mut ips: Vec<IpAddr> = lookup.iter().collect();
    ips.sort();
    ips.dedup();
    Ok(ips)
}

pub async fn mx(resolver: &TokioResolver, domain: &str) -> Result<MxAnswer, String> {
    match resolver.mx_lookup(format!("{domain}.")).await {
        Ok(lookup) => Ok(classify_mx(
            lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::MX(mx) => Some(Mx {
                        preference: mx.preference,
                        exchange: mx.exchange.to_ascii(),
                    }),
                    _ => None,
                })
                .collect(),
        )),
        Err(e) if e.is_nx_domain() => Ok(MxAnswer::NxDomain),
        Err(e) if e.is_no_records_found() => Ok(MxAnswer::NoRecords),
        Err(e) => Err(e.to_string()),
    }
}

pub async fn srv(
    resolver: &TokioResolver,
    service: Service,
    domain: &str,
) -> Result<Vec<SrvRecord>, String> {
    match resolver
        .srv_lookup(format!("{}.{domain}.", service.label()))
        .await
    {
        Ok(lookup) => Ok(lookup
            .answers()
            .iter()
            .filter_map(|record| match &record.data {
                RData::SRV(srv) => Some(SrvRecord {
                    service,
                    priority: srv.priority,
                    weight: srv.weight,
                    port: srv.port,
                    target: srv.target.to_ascii(),
                }),
                _ => None,
            })
            .collect()),
        Err(e) if e.is_nx_domain() || e.is_no_records_found() => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::within;

    #[test]
    fn redirects_stay_inside_the_mail_domain() {
        assert!(within("example.com", "example.com"));
        assert!(within("www.example.com", "example.com"));
        assert!(!within("example.com.evil.net", "example.com"));
        assert!(!within("badexample.com", "example.com"));
    }
}
