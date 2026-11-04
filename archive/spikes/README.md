# M0 spikes (archive)

The measurement code behind M0's spike reports (S1 IMAP, S2 WebKit, S3 store, S4/S4b autodetect,
S5 OAuth, S6 desktop integration, S7 composer), kept as history. They are their own workspace with
their own `Cargo.lock`, so their dependencies and features never reach Sealwax's build or CI
(ADR-025). Nothing in Sealwax depends on them; a few spikes borrow files that now live with the
code that uses them (`uitest/examples/reader_support/{corpus,png}.rs`,
`uitest/examples/composer_support/probe.rs`, `devtools/autodetect-measure/src/truth.rs`, S7's MIME
fixtures in `tests/outgoing/`).

```sh
cargo build --manifest-path archive/spikes/Cargo.toml -p email-spike-s1
```

They are not built or tested by `tools/ci.sh`. S1's testbed suite runs by hand:
`cargo test --release --manifest-path archive/spikes/Cargo.toml -p email-spike-s1 --test testbed -- --ignored --test-threads=1`.
