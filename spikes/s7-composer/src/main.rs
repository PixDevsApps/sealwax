//! Spike S7 WYSIWYG composer and MIME output (M0). Measurement code only; production crates never depend on it.

mod broadway;
mod canary;
mod composer;
mod fixtures;
mod ids;
mod js;
mod manual;
mod mime;
mod png;
mod probe;
mod roundtrip;
mod selftest;

use std::process::ExitCode;

const USAGE: &str = "usage: email-spike-s7 mime | selftest [--samples N] [--size WxH] [--no-latency] \
                     [--latency-only] [--no-capture] | manual [--minutes N]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mime") => fixtures::run(),
        Some("selftest") => selftest::run(&args[1..]),
        Some("manual") => manual::run(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
