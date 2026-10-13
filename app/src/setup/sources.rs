//! The sources autodetect asks, as the assistant lists them while it waits ("Looking up your
//! provider…"): each one as `DetectProgress` reports it, in `DetectSource`'s order, and what
//! came of it once the result is in.

use sealwax_domain::account::{DetectSource, Detection};

/// What came of asking a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Checking,
    /// The settings came from here.
    Found,
    /// It had nothing for this address.
    NoMatch,
    /// Another source's settings were used.
    NotUsed,
    /// The search ended without settings to use (a provider without IMAP, a domain without
    /// mail).
    Done,
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            Self::Checking => "checking…",
            Self::Found => "found",
            Self::NoMatch => "no match",
            Self::NotUsed => "not used",
            Self::Done => "checked",
        }
    }
}

const ORDER: [DetectSource; 6] = [
    DetectSource::Preset,
    DetectSource::Autoconfig,
    DetectSource::Ispdb,
    DetectSource::Mx,
    DetectSource::Srv,
    DetectSource::Probe,
];

fn rank(source: DetectSource) -> usize {
    ORDER
        .iter()
        .position(|listed| *listed == source)
        .unwrap_or(ORDER.len())
}

pub fn name(source: DetectSource) -> &'static str {
    match source {
        DetectSource::Preset => "preset",
        DetectSource::Autoconfig => "autoconfig",
        DetectSource::Ispdb => "ispdb",
        DetectSource::Mx => "mx",
        DetectSource::Srv => "srv",
        DetectSource::Probe => "probe",
    }
}

pub fn from_name(name: &str) -> Option<DetectSource> {
    ORDER.into_iter().find(|source| self::name(*source) == name)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    rows: Vec<(DetectSource, State)>,
}

impl Sources {
    pub fn rows(&self) -> &[(DetectSource, State)] {
        &self.rows
    }

    /// Autodetect started asking `source`.
    pub fn started(&mut self, source: DetectSource) {
        if self.rows.iter().any(|(listed, _)| *listed == source) {
            return;
        }
        let at = self
            .rows
            .iter()
            .position(|(listed, _)| rank(*listed) > rank(source))
            .unwrap_or(self.rows.len());
        self.rows.insert(at, (source, State::Checking));
    }

    /// The result is in. A preset match is final at once, so a listed preset that didn't
    /// give the settings had none; of the other sources only the one used is certain.
    pub fn finished(&mut self, detection: &Detection) {
        let found = match detection {
            Detection::Found(settings) => Some(settings.source),
            _ => None,
        };
        if let Some(source) = found {
            self.started(source);
        }
        for (source, state) in &mut self.rows {
            *state = match (detection, found) {
                (_, Some(used)) if used == *source => State::Found,
                (_, Some(_)) if *source == DetectSource::Preset => State::NoMatch,
                (_, Some(_)) => State::NotUsed,
                (Detection::NotFound, None) => State::NoMatch,
                _ => State::Done,
            };
        }
    }

    /// `preset:no match,autoconfig:found`, for the self-test.
    pub fn summary(&self) -> String {
        self.rows
            .iter()
            .map(|(source, state)| format!("{}:{}", name(*source), state.word()))
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::{
        AuthMethod, DetectedSettings, Endpoint, Security, UnsupportedReason, UsernameForm,
    };

    fn found(source: DetectSource) -> Detection {
        let endpoint = Endpoint {
            host: "mail.example.test".to_owned(),
            port: 993,
            security: Security::Tls,
            certificate: None,
        };
        Detection::Found(Box::new(DetectedSettings {
            imap: endpoint.clone(),
            smtp: endpoint,
            username: UsernameForm::Address,
            second_username: None,
            auth: AuthMethod::Password,
            provider: None,
            confirm: false,
            app_password: None,
            enable_imap: None,
            source,
        }))
    }

    #[test]
    fn sources_are_listed_in_order_as_they_start() {
        let mut sources = Sources::default();
        for source in [
            DetectSource::Preset,
            DetectSource::Mx,
            DetectSource::Autoconfig,
            DetectSource::Mx,
            DetectSource::Srv,
        ] {
            sources.started(source);
        }
        assert_eq!(
            sources.summary(),
            "preset:checking…,autoconfig:checking…,mx:checking…,srv:checking…"
        );
        sources.finished(&found(DetectSource::Autoconfig));
        assert_eq!(
            sources.summary(),
            "preset:no match,autoconfig:found,mx:not used,srv:not used"
        );
    }

    #[test]
    fn a_source_never_reported_still_shows_what_it_found() {
        let mut sources = Sources::default();
        sources.finished(&found(DetectSource::Ispdb));
        assert_eq!(sources.summary(), "ispdb:found");
    }

    #[test]
    fn dead_ends() {
        let mut sources = Sources::default();
        sources.started(DetectSource::Preset);
        sources.started(DetectSource::Probe);
        let mut not_found = sources.clone();
        not_found.finished(&Detection::NotFound);
        assert_eq!(not_found.summary(), "preset:no match,probe:no match");
        sources.finished(&Detection::Unsupported {
            provider: "Tuta".to_owned(),
            reason: UnsupportedReason::NoImap,
        });
        assert_eq!(sources.summary(), "preset:checked,probe:checked");
        assert_eq!(from_name("probe"), Some(DetectSource::Probe));
        assert_eq!(from_name("dns"), None);
    }
}
