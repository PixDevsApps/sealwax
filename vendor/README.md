# Vendored crates

## lettre 0.11.23, with a longer reply-line limit and bounded reply reading

`vendor/lettre/` is [lettre](https://crates.io/crates/lettre) 0.11.23 exactly as published on crates.io (MIT, `LICENSE`), with three changes, each marked "Sealwax's patch" in the code. The 100,000-byte cap on a whole reply is unchanged.

1. **A longer reply line** (2026-10-13): `MAX_RESPONSE_LINE_BYTES` in `src/transport/smtp/client/mod.rs` is 16,384 bytes instead of 1,000, with its comment. Microsoft 365 can answer the end of a message with a reply line longer than 1,000 bytes, which lettre can't read, so Sealwax couldn't tell whether the message was sent ([issue #2](https://github.com/PixDevsApps/sealwax/issues/2)). The limit isn't configurable in lettre; the upstream request is [lettre/lettre#1168](https://github.com/lettre/lettre/issues/1168).
2. **A line is read only up to its limit** (2026-10-24): `read_response` in `src/transport/smtp/client/async_connection.rs` and `client/connection.rs` reads a line through `take(MAX_RESPONSE_LINE_BYTES + 1)` and then checks it. lettre read the whole line first and checked its length afterwards, so a server that never ended a line grew the buffer until Sealwax's timeout: 60 s for a command, 300 s at the message's end. A test server sent it 80.7 MB in half a second, over TLS on one machine.
3. **A reply is parsed once** (2026-10-24): the same two `read_response`s parse the whole reply only once it can be more than incomplete, using `is_continuation` and `may_continue` after `parse_response` in `src/transport/smtp/response.rs`. lettre parsed the whole reply again after every line, in time quadratic in its lines. In a debug build, 4,000 short lines took 22.6 s and 16,000 took 268 s, and no timeout stopped it: the reply was already read, so the parse never yielded. Every reply is read as before.

For port 587, the greeting and the first EHLO reply come before STARTTLS, so before changes 2 and 3 anyone on the network path could have done either, not only the server.

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

The only differences should be the three changes: `src/transport/smtp/client/mod.rs`'s constant and its comment, the two `read_response`s with their imports, and the two functions after `parse_response`.

To drop it once lettre releases fixes for all three: remove both `[patch.crates-io]` entries and this folder, raise `lettre` in `engine/Cargo.toml` (and `spikes/s5-oauth/Cargo.toml`), update the lock files, and keep `engine/src/smtp/submit/tests.rs`'s cases for long replies, a line that never ends and a reply of many lines passing. To move it to a newer lettre, copy the new published crate here and make the same changes.
