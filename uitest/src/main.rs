//! `sealwax-selftest`: Sealwax built with the `test-hooks` feature, so `--self-test <script>`
//! runs a `.sealwax-test` script against the real app (docs/TESTING.md). Without that option it
//! is a whole Sealwax, which the scripts start again as a second instance. Never packaged.

fn main() -> impl std::process::Termination {
    sealwax_app::main()
}
