use crate::model::{Finding, Outcome, Report, Source};
use crate::rank::{Wait, decide};

pub struct Policy {
    pub name: &'static str,
    pub order: &'static [Source],
    pub caps: &'static [(Source, f64)],
    pub presets: bool,
}

const ALL: &[Source] = &Source::ALL;
const WELL_KNOWN_LATE: &[Source] = &[
    Source::Preset,
    Source::Autoconfig,
    Source::Srv,
    Source::Ispdb,
    Source::MxPreset,
    Source::WellKnown,
    Source::MxIspdb,
    Source::Probe,
];
const NO_WELL_KNOWN: &[Source] = &[
    Source::Preset,
    Source::Autoconfig,
    Source::Srv,
    Source::Ispdb,
    Source::MxPreset,
    Source::MxIspdb,
    Source::Probe,
];

pub const POLICIES: &[Policy] = &[
    Policy {
        name: "as implemented",
        order: ALL,
        caps: &[],
        presets: true,
    },
    Policy {
        name: "well-known capped at 1.5 s",
        order: ALL,
        caps: &[(Source::WellKnown, 1500.0)],
        presets: true,
    },
    Policy {
        name: "well-known after MX preset",
        order: WELL_KNOWN_LATE,
        caps: &[],
        presets: true,
    },
    Policy {
        name: "no well-known",
        order: NO_WELL_KNOWN,
        caps: &[],
        presets: true,
    },
    Policy {
        name: "HTTP sources capped at 2 s",
        order: ALL,
        caps: &[
            (Source::Autoconfig, 2000.0),
            (Source::WellKnown, 2000.0),
            (Source::Ispdb, 2000.0),
        ],
        presets: true,
    },
    Policy {
        name: "no presets",
        order: ALL,
        caps: &[],
        presets: false,
    },
    Policy {
        name: "no presets, well-known capped at 1.5 s",
        order: ALL,
        caps: &[(Source::WellKnown, 1500.0)],
        presets: false,
    },
];

pub struct Replay {
    pub outcome: Outcome,
    pub final_ms: f64,
    pub first_usable_ms: Option<f64>,
}

fn without_presets(report: &Report) -> Report {
    let mut report = report.clone();
    let from_preset = matches!(
        report.result,
        Ok(Finding::Servers { .. } | Finding::Unsupported { .. })
    );
    if report.source == Source::Preset || (report.source == Source::MxPreset && from_preset) {
        report.result = Ok(Finding::Nothing);
    }
    report
}

pub fn replay(
    reports: &[Report],
    probe_started_ms: Option<f64>,
    policy: &Policy,
    probe_delay_ms: f64,
    cap_ms: f64,
) -> Replay {
    let mut events: Vec<Report> = reports
        .iter()
        .filter(|r| policy.order.contains(&r.source) && r.source != Source::Probe)
        .map(|r| {
            let mut r = if policy.presets {
                r.clone()
            } else {
                without_presets(r)
            };
            if let Some(&(_, cap)) = policy.caps.iter().find(|(s, _)| *s == r.source)
                && r.elapsed_ms > cap
            {
                r.elapsed_ms = cap;
                r.result = Err("capped".into());
            }
            r
        })
        .collect();
    if policy.order.contains(&Source::Probe)
        && let Some(probe) = reports.iter().find(|r| r.source == Source::Probe)
    {
        let others_done = events.iter().map(|r| r.elapsed_ms).fold(0.0, f64::max);
        let duration = probe.elapsed_ms - probe_started_ms.unwrap_or(0.0);
        let mut probe = probe.clone();
        probe.elapsed_ms = probe_delay_ms.min(others_done) + duration;
        events.push(probe);
    }
    events.sort_by(|a, b| a.elapsed_ms.total_cmp(&b.elapsed_ms));

    let mut seen: Vec<Report> = Vec::new();
    let mut first_usable_ms = None;
    for event in events.into_iter().filter(|e| e.elapsed_ms <= cap_ms) {
        let at = event.elapsed_ms;
        seen.push(event);
        if first_usable_ms.is_none() && decide(policy.order, &seen, Wait::No).outcome.is_usable() {
            first_usable_ms = Some(at);
        }
        let decision = decide(policy.order, &seen, Wait::ForHigherPriority);
        if decision.settled {
            return Replay {
                outcome: decision.outcome,
                final_ms: at,
                first_usable_ms,
            };
        }
    }
    Replay {
        outcome: decide(policy.order, &seen, Wait::No).outcome,
        final_ms: cap_ms,
        first_usable_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Hints, Protocol, Security, Server};

    fn report(source: Source, at: f64, servers: bool) -> Report {
        let result = if servers {
            Ok(Finding::Servers {
                servers: vec![
                    Server::new(Protocol::Imap, "imap.d", 993, Security::Tls),
                    Server::new(Protocol::Smtp, "smtp.d", 465, Security::Tls),
                ],
                hints: Hints::default(),
            })
        } else {
            Ok(Finding::Nothing)
        };
        Report {
            source,
            elapsed_ms: at,
            result,
            detail: String::new(),
        }
    }

    fn reports() -> Vec<Report> {
        vec![
            report(Source::Preset, 0.0, false),
            report(Source::Autoconfig, 50.0, false),
            report(Source::WellKnown, 3000.0, false),
            report(Source::Srv, 60.0, false),
            report(Source::Ispdb, 200.0, true),
            report(Source::MxPreset, 40.0, false),
            report(Source::MxIspdb, 90.0, false),
            report(Source::Probe, 1500.0, false),
        ]
    }

    #[test]
    fn slow_well_known_delays_the_settled_answer() {
        let replayed = replay(&reports(), Some(0.0), &POLICIES[0], 2000.0, 10_000.0);
        assert_eq!(replayed.final_ms, 3000.0);
        assert_eq!(replayed.first_usable_ms, Some(200.0));
        assert_eq!(replayed.outcome.winners(), "ispdb");
    }

    #[test]
    fn capping_well_known_settles_at_the_cap() {
        let replayed = replay(&reports(), Some(0.0), &POLICIES[1], 2000.0, 10_000.0);
        assert_eq!(replayed.final_ms, 1500.0);
        assert_eq!(replayed.outcome.winners(), "ispdb");
    }

    #[test]
    fn dropping_well_known_settles_on_the_ispdb_answer() {
        let replayed = replay(&reports(), Some(0.0), &POLICIES[3], 2000.0, 10_000.0);
        assert_eq!(replayed.final_ms, 200.0);
    }

    #[test]
    fn the_probe_starts_after_the_delay_or_when_the_rest_is_done() {
        let mut list = reports();
        list[4] = report(Source::Ispdb, 200.0, false);
        list[7] = report(Source::Probe, 1500.0, true);
        let replayed = replay(&list, Some(0.0), &POLICIES[0], 2000.0, 10_000.0);
        assert_eq!(replayed.final_ms, 3500.0);
        assert_eq!(replayed.outcome.winners(), "probe");
        let replayed = replay(&list, Some(0.0), &POLICIES[3], 2000.0, 10_000.0);
        assert_eq!(replayed.final_ms, 200.0 + 1500.0);
    }

    #[test]
    fn dropping_presets_keeps_no_mail_answers() {
        let mut list = reports();
        list[5] = Report {
            source: Source::MxPreset,
            elapsed_ms: 40.0,
            result: Ok(Finding::NoMail("null MX")),
            detail: String::new(),
        };
        list[4] = report(Source::Ispdb, 200.0, false);
        list[2] = report(Source::WellKnown, 100.0, false);
        let replayed = replay(&list, Some(0.0), &POLICIES[5], 2000.0, 10_000.0);
        assert!(matches!(replayed.outcome, Outcome::NoMail { .. }));
    }
}
