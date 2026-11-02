//! Account autodetect (ML-030): finds the IMAP and SMTP settings for an address without a
//! login. The sources and their order are in `sealwax_domain::autoconfig`; this module makes the
//! lookups: DNS with hickory, HTTPS with reqwest on rustls, and TLS checks that send only
//! CAPABILITY, STARTTLS and EHLO.
//!
//! Privacy (SECURITY.md): the full address goes only to the domain's own HTTPS autoconfig; the
//! ISPDB, MX and SRV lookups carry the domain only; plain HTTP is never used; a redirect that
//! would carry the address to another host is refused; hosts that resolve to non-public
//! addresses are neither fetched nor probed. Nothing here logs an address or a domain.

mod network;
mod pipeline;
pub(crate) mod probe;

#[cfg(test)]
mod tests;

use std::net::IpAddr;
use std::time::Duration;

use sealwax_domain::account::{DetectSource, Detection};
use sealwax_domain::autoconfig::Rules;

pub use network::{Fetched, MAX_BODY, NetError, NetEvent, Network, Observer, RealNetwork};
pub use pipeline::{SourceTrace, Trace, ascii_domain};
pub use probe::{ProbeError, Stage};

/// Time limits and switches. The defaults are S4's, measured on 2026-10-04: p50 56 ms, p95
/// 3.2 s on its list, never near the 10 s cap.
#[derive(Clone, Debug)]
pub struct DetectOptions {
    /// When the best answer so far is taken, settled or not.
    pub cap: Duration,
    /// Each HTTPS request, connection included.
    pub http_timeout: Duration,
    /// Each DNS attempt (two attempts).
    pub dns_timeout: Duration,
    /// The domain's autoconfig, the ISPDB, SRV with its checks, MX, and the MX operator's ISPDB.
    pub source_timeout: Duration,
    /// `/.well-known/autoconfig` on the bare domain (slow web servers, rarely useful in S4).
    pub well_known_timeout: Duration,
    /// R3's autoconfig at the MX operators.
    pub mx_autoconfig_timeout: Duration,
    /// Each TLS check.
    pub connect_timeout: Duration,
    /// Probing starts when every other source is done, or after this long.
    pub probe_delay: Duration,
    /// All probes together.
    pub probe_budget: Duration,
    /// A DNS server to ask instead of the system's (the measurement uses 1.1.1.1 for cold
    /// lookups).
    pub dns_server: Option<IpAddr>,
    /// Generic on purpose: no product, version or platform (ADR-018).
    pub user_agent: String,
    pub rules: Rules,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            cap: Duration::from_secs(10),
            http_timeout: Duration::from_secs(4),
            dns_timeout: Duration::from_secs(2),
            source_timeout: Duration::from_millis(4500),
            well_known_timeout: Duration::from_millis(1500),
            mx_autoconfig_timeout: Duration::from_millis(1500),
            connect_timeout: Duration::from_secs(3),
            probe_delay: Duration::from_secs(2),
            probe_budget: Duration::from_secs(5),
            dns_server: None,
            user_agent: "Sealwax".into(),
            rules: Rules::default(),
        }
    }
}

/// Finds the settings for `address`. `progress` hears each kind of source as it is first asked,
/// in the assistant's order. A preset answers at once without any request.
pub async fn detect(
    address: &str,
    options: &DetectOptions,
    progress: impl Fn(DetectSource),
) -> Detection {
    if let Some((detection, _)) = pipeline::preset(address) {
        progress(DetectSource::Preset);
        return detection;
    }
    match RealNetwork::new(options, None) {
        Ok(network) => detect_with(&network, address, options, progress).await.0,
        Err(_) => {
            progress(DetectSource::Preset);
            Detection::NotFound
        }
    }
}

/// `detect` on any network, with the trace of every source's answer.
pub async fn detect_with<N: Network>(
    network: &N,
    address: &str,
    options: &DetectOptions,
    progress: impl Fn(DetectSource),
) -> (Detection, Trace) {
    pipeline::run(network, address, options, progress).await
}
