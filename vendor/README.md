# Vendored crates

## lettre 0.11.23, with a longer reply-line limit

`vendor/lettre/` is [lettre](https://crates.io/crates/lettre) 0.11.23 exactly as published on crates.io (MIT, `LICENSE`), with one change: `MAX_RESPONSE_LINE_BYTES` in `src/transport/smtp/client/mod.rs` is 16,384 bytes instead of 1,000, with its comment. The 100,000-byte cap on a whole reply is unchanged.

Why: Microsoft 365 can answer the end of a message with a reply line longer than 1,000 bytes, which lettre can't read, so Sealwax couldn't tell whether the message was sent ([issue #2](https://github.com/PixDevsApps/sealwax/issues/2)). The limit isn't configurable in lettre; the upstream request is [lettre/lettre#1168](https://github.com/lettre/lettre/issues/1168).

The workspace's `Cargo.toml` and `fuzz/Cargo.toml` use it through `[patch.crates-io]`; it is excluded from the workspace, so `cargo fmt`, Clippy and the tests don't run on it.

A build prints four dead-code warnings from lettre: Cargo shows warnings for a crate in a local folder, which it hides for one from crates.io. They are lettre's own, from features Sealwax leaves off.

To check that nothing else differs, compare it with the published crate, a gzipped tarball whose SHA-256 is the checksum the lock file had before the patch (`f2c646bd5cc763b1087b15493e29a64be6147ba8f19342004fa52048ee596eae`):

```bash
mkdir -p target/lettre-check && curl -sSL https://static.crates.io/crates/lettre/lettre-0.11.23.crate -o target/lettre-check/lettre-0.11.23.crate
```

```bash
sha256sum target/lettre-check/lettre-0.11.23.crate && tar -xzf target/lettre-check/lettre-0.11.23.crate -C target/lettre-check
```

```bash
diff -r target/lettre-check/lettre-0.11.23 vendor/lettre
```

The only difference should be `src/transport/smtp/client/mod.rs`'s constant and its comment.

To drop it once lettre releases a fix: remove both `[patch.crates-io]` entries and this folder, raise `lettre` in `engine/Cargo.toml` (and `spikes/s5-oauth/Cargo.toml`), update the lock files, and keep `engine/src/smtp/submit/tests.rs`'s long-reply cases passing. To move it to a newer lettre, copy the new published crate here and make the same one-line change.
