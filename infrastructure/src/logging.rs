use std::io::IsTerminal;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Third-party crates capped at `warn` whatever `RUST_LOG` asks for: the protocol crates' trace
/// output carries protocol traces, and async-imap's prints the LOGIN password (ADR-018
/// amendment); the HTML parser's debug output carries the text it parses, which is mail (M6:
/// html5ever logged a draft's body while the MIME builder made its text part).
pub const CAPPED: [&str; 15] = [
    "async_imap",
    "imap_proto",
    "lettre",
    "rustls",
    "tokio_rustls",
    "hickory",
    "reqwest",
    "hyper",
    "hyper_util",
    "h2",
    "zbus",
    "html5ever",
    "markup5ever",
    "html2text",
    "mail_builder",
];

/// Sealwax's own sources, as cargo names them to rustc: relative to the workspace. A panic
/// anywhere else (a dependency, the standard library) happened in code that may have been
/// parsing a server's reply or a message.
const OWN_SOURCES: [&str; 6] = [
    "app/",
    "domain/",
    "engine/",
    "infrastructure/",
    "devtools/",
    "spikes/",
];

/// Plain-text diagnostics on stderr, filtered by `RUST_LOG` (default `info`), and panics
/// reported without what a dependency was parsing ([`redact_dependency_panics`]).
/// Never log message bodies, subjects, addresses, folder names, tokens or protocol traces.
pub fn init() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()
        .unwrap_or_else(|_| {
            eprintln!("Invalid RUST_LOG filter; using info level.");
            EnvFilter::new("info")
        });
    subscriber(filter, std::io::stderr, std::io::stderr().is_terminal()).try_init()?;
    redact_dependency_panics();
    Ok(())
}

/// A panic outside Sealwax's own sources is reported on stderr by thread and place only: its
/// message may quote what the code was parsing, such as imap-proto's slicing panic, which names
/// a character of the server's reply (M9, F15). Sealwax's own panics keep Rust's default report.
pub fn redact_dependency_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| match info.location() {
        Some(location) if !is_own_source(location.file()) => {
            let thread = std::thread::current();
            eprintln!(
                "thread '{}' panicked at {location}: (the message is left out: it may quote \
                 what was being parsed)",
                thread.name().unwrap_or("<unnamed>")
            );
            let backtrace = std::backtrace::Backtrace::capture();
            if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
                eprintln!("{backtrace}");
            }
        }
        _ => default(info),
    }));
}

fn is_own_source(file: &str) -> bool {
    OWN_SOURCES.iter().any(|own| file.starts_with(own))
}

/// The subscriber `init` installs, writing to `writer`: `filter` decides, and the crates in
/// [`CAPPED`] never log below `warn`. Tests capture logs through it with their own writer.
pub fn subscriber<W>(
    filter: EnvFilter,
    writer: W,
    ansi: bool,
) -> impl tracing::Subscriber + Send + Sync + 'static
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let caps = Targets::new()
        .with_default(LevelFilter::TRACE)
        .with_targets(CAPPED.map(|target| (target, LevelFilter::WARN)));
    tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(ansi)
            .with_filter(filter)
            .with_filter(caps),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn panics_outside_sealwaxs_sources_are_dependencies() {
        assert!(is_own_source(file!()));
        assert!(is_own_source("engine/src/service/actor.rs"));
        for dependency in [
            "/home/user/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/imap-proto-0.17.0/src/parser/rfc3501/mod.rs",
            "/rustc/0123456789abcdef/library/core/src/str/mod.rs",
            "index.crates.io-1949cf8c6b5b557f/mail-parser-0.11.9/src/parsers/message.rs",
            "src/lib.rs",
        ] {
            assert!(!is_own_source(dependency), "{dependency}");
        }
    }

    #[test]
    fn protocol_crates_stay_at_warn_under_a_trace_filter() {
        let capture = Capture::default();
        let sink = capture.clone();
        let subscriber = subscriber(
            EnvFilter::new("trace,async_imap=trace,lettre=trace"),
            move || sink.clone(),
            false,
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "async_imap::client", "LOGIN secret-password");
            tracing::debug!(target: "lettre::transport", "AUTH PLAIN c2VjcmV0");
            tracing::trace!(target: "imap_proto", "trace line");
            tracing::warn!(target: "rustls::conn", "kept warning");
            tracing::trace!(target: "sealwax_engine::imap", "own trace");
        });
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("secret-password") && !text.contains("c2VjcmV0"));
        assert!(!text.contains("trace line"));
        assert!(
            text.contains("kept warning") && text.contains("own trace"),
            "{text}"
        );
    }
}
