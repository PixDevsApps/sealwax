use std::io::IsTerminal;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Third-party protocol crates capped at `warn` whatever `RUST_LOG` asks for: their trace output
/// carries protocol traces, and async-imap's prints the LOGIN password (ADR-018 amendment).
pub const CAPPED: [&str; 8] = [
    "async_imap",
    "imap_proto",
    "lettre",
    "rustls",
    "tokio_rustls",
    "hickory",
    "reqwest",
    "zbus",
];

/// Plain-text diagnostics on stderr, filtered by `RUST_LOG` (default `info`).
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
    Ok(())
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
