# Vendored crates

## lettre 0.11.23, with a longer reply-line limit and bounded reply reading

`vendor/lettre/` is [lettre](https://crates.io/crates/lettre) 0.11.23 exactly as published on crates.io (MIT, `LICENSE`), with three changes, each marked "Sealwax's patch" in the code. The 100,000-byte cap on a whole reply is unchanged.

1. **A longer reply line** (2026-10-13): `MAX_RESPONSE_LINE_BYTES` in `src/transport/smtp/client/mod.rs` is 16,384 bytes instead of 1,000, with its comment. Microsoft 365 can answer the end of a message with a reply line longer than 1,000 bytes, which lettre can't read, so Sealwax couldn't tell whether the message was sent ([issue #2](https://github.com/PixDevsApps/sealwax/issues/2)). The limit isn't configurable in lettre; the upstream request is [lettre/lettre#1168](https://github.com/lettre/lettre/issues/1168).
2. **A line is read only up to its limit** (2026-10-24): `read_response` in `src/transport/smtp/client/async_connection.rs` and `client/connection.rs` reads a line through `take(MAX_RESPONSE_LINE_BYTES + 1)` and then checks it. lettre read the whole line first and checked its length afterwards, so a server that never ended a line grew the buffer until Sealwax's timeout: 60 s for a command, 300 s at the message's end. A test server sent it 80.7 MB in half a second, over TLS on one machine.
3. **A reply is parsed once** (2026-10-24): the same two `read_response`s parse the whole reply only once it can be more than incomplete, using `is_continuation` and `may_continue` after `parse_response` in `src/transport/smtp/response.rs`. lettre parsed the whole reply again after every line, in time quadratic in its lines. In a release build, a reply of 14,284 short lines, just under the cap, took 3.1 s of CPU (16,000 lines took 268 s in a debug build), and no timeout stopped it: the reply was already read, so the parse never yielded. It now takes 1.6 ms. Every reply is read as before.

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

## imap-proto 0.17.0, with upstream's nesting limit

`vendor/imap-proto/` is [imap-proto](https://crates.io/crates/imap-proto) 0.17.0 exactly as published on crates.io (MIT or Apache-2.0, `LICENSE-MIT` and `LICENSE-APACHE`), the parser async-imap reads every IMAP response with, with two commits from imap-proto's own `main` applied (2026-10-25), both in `src/rfc3501/body_structure.rs`:

1. [`f06dc5a0`](https://github.com/djc/imap-proto/commit/f06dc5a00c9376018fc74d155495f741468582b6) "Limit nesting depth of body extensions": a body extension may be nested at most 8 deep.
2. [`b93964df`](https://github.com/djc/imap-proto/commit/b93964dfb41d6c31a1ccc6c753eed3bed4547994) "Limit nesting depth of body structures" ([djc/imap-proto#212](https://github.com/djc/imap-proto/pull/212)): a BODYSTRUCTURE may be nested at most 20 deep. Its `src/tests.rs` hunk is left out: it changes a test that came after 0.17.0.

A deeper one is a parse failure, which async-imap reports as a reply it can't read; Sealwax then fetches the message without its BODYSTRUCTURE ([ADR-006 amendment](../docs/DECISIONS.md#adr-006-amendment--a-reply-async-imap-cant-read-no-longer-stops-a-folder-2026-10-22)). Without the limit, imap-proto parsed a BODYSTRUCTURE recursively however deep it was, and about 370 nested multiparts (well under 1 KB) overflowed the 2 MiB stack of the engine's threads in a release build and ended the whole process. A server could send that at any time, and anyone on the network path could send it in place of the greeting on a STARTTLS port (143), before the connection is encrypted. imap-proto's `main` has had the limit since 2026-10-12, but no release has it yet: `main` is 0.18.0, with changes async-imap 0.12 doesn't take.

The workspace's `Cargo.toml` and `fuzz/Cargo.toml` use it through `[patch.crates-io]`; it is excluded from the workspace, so `cargo fmt`, Clippy and the tests don't run on it. Its own tests pass with both commits (97, including upstream's five new ones; run outside the repository, where its dev-dependency can be fetched).

To check it, fetch the published crate, whose SHA-256 is the checksum the lock file had before the patch (`5ccf963d57074747b455398a1763d174da80bcaba6f51e3671a82252b531a68b`):

```bash
mkdir -p target/imap-proto-check && curl -sSL https://static.crates.io/crates/imap-proto/imap-proto-0.17.0.crate -o target/imap-proto-check/imap-proto-0.17.0.crate
```

```bash
sha256sum target/imap-proto-check/imap-proto-0.17.0.crate && tar -xzf target/imap-proto-check/imap-proto-0.17.0.crate -C target/imap-proto-check
```

```bash
diff -r target/imap-proto-check/imap-proto-0.17.0 vendor/imap-proto
```

The only difference should be in `src/rfc3501/body_structure.rs`, and it should be the two commits above (`b93964df` without its `src/tests.rs` hunk), applied in that order.

To drop it once an imap-proto release has the limit and async-imap takes that release: remove both `[patch.crates-io]` entries and this folder, raise `async-imap` in the workspace's `Cargo.toml`, update the lock files, and keep `a_greeting_nested_too_deep_is_unreadable_not_a_crash` (`engine/src/imap/connection/tests.rs`) and `a_body_structure_nested_too_deep_costs_one_reconnect` (`engine/tests/sync.rs`) passing.

## async-imap 0.12.0, with a buffer that grows with a literal's data

`vendor/async-imap/` is [async-imap](https://crates.io/crates/async-imap) 0.12.0 exactly as published on crates.io (MIT or Apache-2.0, `LICENSE-MIT` and `LICENSE-APACHE`), the IMAP client Sealwax builds on, with one change (2026-10-27), marked "Sealwax's patch" in `src/imap_stream.rs`:

- **A literal's announced size isn't allocated ahead of its data:** `Buffer::ensure_capacity` grows the buffer only once it is full, by at most its own size at a time towards the size still needed. async-imap grew it to a literal's whole announced size as soon as the line announcing it (`{400000000}`) was read, and zero-filled it, so one line from a server made Sealwax allocate up to 512 MiB, async-imap's limit for one response, and hold it while the read waited. Now the buffer holds at most twice what has arrived: in a test, 2.1 MB for a 400 MB literal of which 1 MiB arrived, against 399 MB before. A line with no literal grows the buffer 4 KiB at a time, as before, and a literal of more than 512 MiB is now refused once that much has arrived rather than at once. `test_buffer_ensure_capacity` checks the new behaviour in place of the old, and `test_ensure_capacity_follows_the_data` is new.

Upstream knows about the allocation: [chatmail/async-imap#148](https://github.com/chatmail/async-imap/pull/148), still open, proposes a configurable limit and a reader that keeps only a body's beginning. The patch here is Sealwax's own, smaller one.

The workspace's `Cargo.toml` and `fuzz/Cargo.toml` use it through `[patch.crates-io]`; it is excluded from the workspace, so `cargo fmt`, Clippy and the tests don't run on it. Its own tests pass with the change (74 with `runtime-tokio`, as Sealwax builds it, and 73 with its default `runtime-async-std`; run outside the repository, where its dev-dependencies can be fetched).

A build prints four warnings from async-imap about its use of the deprecated `unsafe_pinned` macro, for the same reason as lettre's: Cargo shows a local crate's warnings.

To check it, fetch the published crate, whose SHA-256 is the checksum the lock file had before the patch (`f97b87216c9f0ccc63c516263169085fa34bba25633f6d96c6a9b9ca1fd5f70a`):

```bash
mkdir -p target/async-imap-check && curl -sSL https://static.crates.io/crates/async-imap/async-imap-0.12.0.crate -o target/async-imap-check/async-imap-0.12.0.crate
```

```bash
sha256sum target/async-imap-check/async-imap-0.12.0.crate && tar -xzf target/async-imap-check/async-imap-0.12.0.crate -C target/async-imap-check
```

```bash
diff -r target/async-imap-check/async-imap-0.12.0 vendor/async-imap
```

The only differences should be in `src/imap_stream.rs`: `Buffer::ensure_capacity` with its comment, the end of `test_buffer_ensure_capacity`, and the new `test_ensure_capacity_follows_the_data`.

To drop it once an async-imap release no longer allocates a literal's announced size ahead of its data: remove both `[patch.crates-io]` entries and this folder, raise `async-imap` in the workspace's `Cargo.toml` (and `spikes/s1-imap/Cargo.toml`), update the lock files, and keep `a_literals_announced_size_isnt_allocated_before_its_data` (`engine/tests/literal_size.rs`) and `a_literal_that_arrives_in_pieces_comes_whole` (`engine/src/imap/raw.rs`) passing. To move it to a newer async-imap, copy the new published crate here and make the same change.
