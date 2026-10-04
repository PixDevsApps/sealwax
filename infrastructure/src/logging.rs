use std::io::IsTerminal;
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

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
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init()
}
