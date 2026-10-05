//! The lookups autodetect makes, behind a trait so tests can script them. `RealNetwork` uses one
//! hickory resolver for DNS, HTTPS and probes, always with fully qualified names (a search domain
//! such as `home` never turns `autoconfig.<domain>` into `autoconfig.<domain>.home`), reqwest on
//! rustls with the platform verifier for HTTPS, and refuses every host that resolves to a
//! non-public address.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hickory_resolver::TokioResolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::{RData, RecordType};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use sealwax_domain::autoconfig::dns::public_address;
use sealwax_domain::autoconfig::dns::{Mx, MxAnswer, SrvRecord, SrvService, classify_mx};
use sealwax_domain::autoconfig::probing::Target;
use sealwax_domain::autoconfig::redirect::{self, Redirect};

use super::DetectOptions;
use super::probe::{self, ProbeError, Stage};

/// The largest clientConfig document read.
pub const MAX_BODY: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NetError(pub String);

/// The answer to an HTTPS GET.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetched {
    Body(String),
    /// Any status but 2xx.
    Status(u16),
    TooLarge,
}

/// What `RealNetwork` sends, for the measurement's request log. URLs are complete: whoever keeps
/// them must elide the address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetEvent {
    Dns { name: String, record: &'static str },
    Https { url: String },
    Redirect { url: String, followed: bool },
    Connect { host: String, port: u16 },
}

pub type Observer = Arc<dyn Fn(NetEvent) + Send + Sync>;

/// Every lookup autodetect makes. Implementations never log names or URLs.
pub trait Network: Sync {
    fn mx(&self, domain: &str) -> impl Future<Output = Result<MxAnswer, NetError>> + Send;

    fn srv(
        &self,
        service: SrvService,
        domain: &str,
    ) -> impl Future<Output = Result<Vec<SrvRecord>, NetError>> + Send;

    /// The target of the CNAME record at `name`, if there is one.
    fn cname(&self, name: &str) -> impl Future<Output = Result<Option<String>, NetError>> + Send;

    /// The host's addresses, sorted and without duplicates; an error when there are none.
    fn addresses(&self, host: &str) -> impl Future<Output = Result<Vec<IpAddr>, NetError>> + Send;

    /// An HTTPS GET, following only the redirects `redirect::follow` allows.
    fn fetch(&self, url: &str) -> impl Future<Output = Result<Fetched, NetError>> + Send;

    /// The guarded TLS check of `target` (`probe.rs`).
    fn probe(
        &self,
        target: &Target,
        limit: Duration,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send;
}

pub struct RealNetwork {
    resolver: Arc<TokioResolver>,
    http: reqwest::Client,
    tls: Arc<ClientConfig>,
    observer: Option<Observer>,
}

impl RealNetwork {
    pub fn new(options: &DetectOptions, observer: Option<Observer>) -> Result<Self, NetError> {
        let resolver = Arc::new(resolver(options)?);
        let tls = Arc::new(tls_config()?);
        let http = reqwest::Client::builder()
            .user_agent(options.user_agent.as_str())
            .https_only(true)
            .no_proxy()
            .tls_backend_preconfigured((*tls).clone())
            .dns_resolver(Fqdn {
                resolver: resolver.clone(),
                observer: observer.clone(),
            })
            .redirect(redirect_policy(observer.clone()))
            .connect_timeout(options.http_timeout)
            .timeout(options.http_timeout)
            .build()
            .map_err(|e| NetError(error_text(&e)))?;
        Ok(Self {
            resolver,
            http,
            tls,
            observer,
        })
    }

    fn observe(&self, event: NetEvent) {
        if let Some(observer) = &self.observer {
            observer(event);
        }
    }

    fn dns(&self, name: &str, record: &'static str) {
        self.observe(NetEvent::Dns {
            name: name.into(),
            record,
        });
    }
}

fn fqdn(name: &str) -> String {
    format!("{}.", name.trim_end_matches('.'))
}

fn no_answer(error: &hickory_resolver::net::NetError) -> bool {
    error.is_nx_domain() || error.is_no_records_found()
}

impl Network for RealNetwork {
    async fn mx(&self, domain: &str) -> Result<MxAnswer, NetError> {
        self.dns(domain, "MX");
        match self.resolver.mx_lookup(fqdn(domain)).await {
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
            Err(e) => Err(NetError(e.to_string())),
        }
    }

    async fn srv(&self, service: SrvService, domain: &str) -> Result<Vec<SrvRecord>, NetError> {
        let name = format!("{}.{domain}", service.label());
        self.dns(&name, "SRV");
        match self.resolver.srv_lookup(fqdn(&name)).await {
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
            Err(e) if no_answer(&e) => Ok(Vec::new()),
            Err(e) => Err(NetError(e.to_string())),
        }
    }

    async fn cname(&self, name: &str) -> Result<Option<String>, NetError> {
        self.dns(name, "CNAME");
        match self.resolver.lookup(fqdn(name), RecordType::CNAME).await {
            Ok(lookup) => Ok(lookup
                .answers()
                .iter()
                .find_map(|record| match &record.data {
                    RData::CNAME(cname) => Some(cname.0.to_ascii()),
                    _ => None,
                })),
            Err(e) if no_answer(&e) => Ok(None),
            Err(e) => Err(NetError(e.to_string())),
        }
    }

    async fn addresses(&self, host: &str) -> Result<Vec<IpAddr>, NetError> {
        self.dns(host, "A/AAAA");
        lookup_ip(&self.resolver, host).await
    }

    async fn fetch(&self, url: &str) -> Result<Fetched, NetError> {
        self.observe(NetEvent::Https { url: url.into() });
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| NetError(error_text(&e)))?;
        let status = response.status();
        if !status.is_success() {
            return Ok(Fetched::Status(status.as_u16()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| NetError(error_text(&e)))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BODY {
                return Ok(Fetched::TooLarge);
            }
        }
        Ok(Fetched::Body(String::from_utf8_lossy(&body).into_owned()))
    }

    async fn probe(&self, target: &Target, limit: Duration) -> Result<(), ProbeError> {
        self.dns(&target.host, "A/AAAA");
        let ips = tokio::time::timeout(limit, lookup_ip(&self.resolver, &target.host))
            .await
            .map_err(|_| ProbeError::new(Stage::Timeout, "no addresses in time"))?
            .map_err(|e| ProbeError::new(Stage::Dns, e.0))?;
        if ips.iter().any(|ip| !public_address(*ip)) {
            return Err(ProbeError::new(Stage::Dns, "non-public address refused"));
        }
        self.observe(NetEvent::Connect {
            host: target.host.clone(),
            port: target.port,
        });
        let addrs: Vec<SocketAddr> = ips
            .into_iter()
            .map(|ip| SocketAddr::new(ip, target.port))
            .collect();
        probe::check(target, &addrs, self.tls.clone(), limit).await
    }
}

async fn lookup_ip(resolver: &TokioResolver, host: &str) -> Result<Vec<IpAddr>, NetError> {
    let lookup = resolver.lookup_ip(fqdn(host)).await.map_err(|e| {
        if no_answer(&e) {
            NetError("no addresses".into())
        } else {
            NetError(e.to_string())
        }
    })?;
    let mut ips: Vec<IpAddr> = lookup.iter().collect();
    ips.sort();
    ips.dedup();
    if ips.is_empty() {
        return Err(NetError("no addresses".into()));
    }
    Ok(ips)
}

fn resolver(options: &DetectOptions) -> Result<TokioResolver, NetError> {
    let mut opts = ResolverOpts::default();
    opts.timeout = options.dns_timeout;
    opts.attempts = 2;
    opts.cache_size = 64;
    let mut builder = match options.dns_server {
        None => TokioResolver::builder_tokio().map_err(|e| NetError(e.to_string()))?,
        Some(ip) => TokioResolver::builder_with_config(
            ResolverConfig::from_name_servers(vec![NameServerConfig::udp_and_tcp(ip)]),
            TokioRuntimeProvider::default(),
        ),
    };
    *builder.options_mut() = opts;
    builder.build().map_err(|e| NetError(e.to_string()))
}

/// rustls with ring and the platform's certificate verifier (SECURITY.md, Transport).
pub fn tls_config() -> Result<ClientConfig, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| NetError(e.to_string()))?
        .with_platform_verifier()
        .map_err(|e| NetError(e.to_string()))?
        .with_no_client_auth())
}

/// reqwest's resolver: hickory with fully qualified names, refusing non-public addresses.
struct Fqdn {
    resolver: Arc<TokioResolver>,
    observer: Option<Observer>,
}

impl Resolve for Fqdn {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.resolver.clone();
        let host = name.as_str().to_string();
        if let Some(observer) = &self.observer {
            observer(NetEvent::Dns {
                name: host.clone(),
                record: "A/AAAA",
            });
        }
        Box::pin(async move {
            let ips = lookup_ip(&resolver, &host).await?;
            if ips.iter().any(|ip| !public_address(*ip)) {
                return Err("non-public address refused".into());
            }
            let addrs: Vec<SocketAddr> = ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect();
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

fn redirect_policy(observer: Option<Observer>) -> Policy {
    Policy::custom(move |attempt| {
        let follow = {
            let previous = attempt.previous();
            let original = previous.first();
            let address = original.and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "emailaddress")
                    .map(|(_, value)| value.into_owned())
            });
            let url = attempt.url();
            let follow = redirect::follow(&Redirect {
                original_host: original.and_then(|u| u.host_str()).unwrap_or(""),
                address: address.as_deref(),
                previous_host: previous.last().and_then(|u| u.host_str()).unwrap_or(""),
                scheme: url.scheme(),
                host: url.host_str().unwrap_or(""),
                url: url.as_str(),
                hops: previous.len().saturating_sub(1),
            });
            if let Some(observer) = &observer {
                observer(NetEvent::Redirect {
                    url: url.as_str().into(),
                    followed: follow,
                });
            }
            follow
        };
        if follow {
            attempt.follow()
        } else {
            attempt.error("redirect refused")
        }
    })
}

/// An error's kind and causes, leaving out any text that might quote the request URL (and with it
/// the address).
fn error_text(error: &reqwest::Error) -> String {
    let mut text = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect failed"
    } else if error.is_redirect() {
        "redirect refused"
    } else {
        "request failed"
    }
    .to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        let message = inner.to_string();
        if !message.contains('@') && !message.contains("%40") && !message.contains("emailaddress") {
            text.push_str(": ");
            text.push_str(&message);
        }
        source = inner.source();
    }
    text
}
