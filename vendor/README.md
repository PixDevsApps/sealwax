# Vendored crates

## lettre 0.11.23, with a longer reply-line limit and bounded reply reading

`vendor/lettre/` is [lettre](https://crates.io/crates/lettre) 0.11.23 exactly as published on crates.io (MIT, `LICENSE`), with three changes, each marked "Sealwax's patch" in the code. The 100,000-byte cap on a whole reply is unchanged.

1. **A longer reply line** (2026-10-13): `MAX_RESPONSE_LINE_BYTES` in `src/transport/smtp/client/mod.rs` is 16,384 bytes instead of 1,000, with its comment. Microsoft 365 can answer the end of a message with a reply line longer than 1,000 bytes, which lettre can't read, so Sealwax couldn't tell whether the message was sent ([issue #2](https://github.com/PixDevsApps/sealwax/issues/2)). The limit isn't configurable in lettre; the upstream request is [lettre/lettre#1168](https://github.com/lettre/lettre/issues/1168).
2. **A line is read only up to its limit** (2026-10-24): `read_response` in `src/transport/smtp/client/async_connection.rs` and `client/connection.rs` reads a line through `take(MAX_RESPONSE_LINE_BYTES + 1)` and then checks it. lettre read the whole line first and checked its length afterwards, so a server that never ended a line grew the buffer until Sealwax's timeout: 60 s for a command, 300 s at the message's end. A test server sent it 80.7 MB in half a second, over TLS on one machine.
3. **A reply is parsed once** (2026-10-24): the same two `read_response`s parse the whole reply only once it can be more than incomplete, using `is_continuation` and `may_continue` after `parse_response` in `src/transport/smtp/response.rs`. lettre parsed the whole reply again after every line, in time quadratic in its lines. In a release build, a reply of 14,284 short lines, just under the cap, took 3.1 s of CPU (16,000 lines took 268 s in a debug build), and no timeout stopped it: the reply was already read, so the parse never yielded. It now takes 1.6 ms. Every reply is read as before.

For port 587, the greeting and the first EHLO reply come before STARTTLS, so before changes 2 and 3 anyone on the network path could have done either, not only the server.

The workspace's `Cargo.toml` and `fuzz/Cargo.toml` use it through `[patch.crates-io]`; it is excluded from the workspace, so `cargo fmt`, Clippy and the tests don't run on it.

Cargo shows the warnings of a crate in a local folder, which it hides for one from crates.io, and lettre has four dead-code warnings from features Sealwax leaves off. So that a build prints none, `Cargo.toml` ends with a `[lints.rust.dead_code]` table that allows them (2026-11-12, marked "Sealwax's patch"); no code is changed for it.

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

The only differences should be the three changes: `src/transport/smtp/client/mod.rs`'s constant and its comment, the two `read_response`s with their imports, and the two functions after `parse_response`. `Cargo.toml` differs by the lint table at its end.

To drop it once lettre releases fixes for all three: remove both `[patch.crates-io]` entries and this folder, raise `lettre` in the workspace `Cargo.toml` (and `archive/spikes/s5-oauth/Cargo.toml`), update the lock files, and keep `engine/src/smtp/submit/tests.rs`'s cases for long replies, a line that never ends and a reply of many lines passing. To move it to a newer lettre, copy the new published crate here and make the same changes.

## imap-proto 0.17.0, with upstream's nesting limit, Proton Mail Bridge's replies, ESEARCH and quoted mailbox names

`vendor/imap-proto/` is [imap-proto](https://crates.io/crates/imap-proto) 0.17.0 exactly as published on crates.io (MIT or Apache-2.0, `LICENSE-MIT` and `LICENSE-APACHE`), the parser async-imap reads every IMAP response with, with two commits from imap-proto's own `main` applied (2026-10-25), both in `src/rfc3501/body_structure.rs`:

1. [`f06dc5a0`](https://github.com/djc/imap-proto/commit/f06dc5a00c9376018fc74d155495f741468582b6) "Limit nesting depth of body extensions": a body extension may be nested at most 8 deep.
2. [`b93964df`](https://github.com/djc/imap-proto/commit/b93964dfb41d6c31a1ccc6c753eed3bed4547994) "Limit nesting depth of body structures" ([djc/imap-proto#212](https://github.com/djc/imap-proto/pull/212)): a BODYSTRUCTURE may be nested at most 20 deep. Its `src/tests.rs` hunk is left out: it changes a test that came after 0.17.0.

A deeper one is a parse failure, which async-imap reports as a reply it can't read; Sealwax then fetches the message without its BODYSTRUCTURE ([ADR-006 amendment](../docs/DECISIONS.md#adr-006-amendment--a-reply-async-imap-cant-read-no-longer-stops-a-folder-2026-10-22)). Without the limit, imap-proto parsed a BODYSTRUCTURE recursively however deep it was, and about 370 nested multiparts (well under 1 KB) overflowed the 2 MiB stack of the engine's threads in a release build and ended the whole process. A server could send that at any time, and anyone on the network path could send it in place of the greeting on a STARTTLS port (143), before the connection is encrypted. imap-proto's `main` has had the limit since 2026-10-12, but no release has it yet: `main` is 0.18.0, with changes async-imap 0.12 doesn't take.

Three changes of Sealwax's own (2026-11-03, [PR #28](https://github.com/PixDevsApps/sealwax/pull/28) by @chrisonline), each marked "Sealwax's change" in the code, read the BODYSTRUCTUREs Proton Mail Bridge sends, which break RFC 3501's grammar. imap-proto refused the whole FETCH reply, so the #20 fallback above fetched those messages without their structure: one failed FETCH per batch first, no attachment clip, and a large message downloaded whole.

1. **An empty parameter list** (`src/rfc3501/body_structure.rs`, `body_param`): `("inline" ())` reads `()` as no parameters, as `NIL` is. The grammar allows only `NIL` or a list of at least one pair.
2. **No space after a message's envelope** (the same file, `body_type_message`): a `message/rfc822` part whose envelope is followed at once by its body, `…"<id>")("text" …`, is read; the grammar has a space there.
3. **Other escapes in a quoted string** (`src/core.rs`, `quoted`): a backslash followed by any TEXT-CHAR is kept as it is (Bridge writes JSON-style escapes such as `\u00ad` in file names); the grammar escapes only `\` and `"`. A line break after the backslash is still refused. Sealwax uses a BODYSTRUCTURE only for whether a message has plain text, HTML or attachments, never for a file name, so a name kept as written shows nowhere.

Each only accepts what was refused before, so a reply that follows the grammar is read exactly as before, and the nesting limits are unchanged. Upstream's `main` has none of the three, and its newer `quoted` unescapes the string and refuses any other escape, so moving to a newer imap-proto means making the third change there again.

A fourth change of Sealwax's own (2026-11-03, `src/rfc3501/mod.rs`, `mailbox_data_esearch`): **ESEARCH** ([RFC 4731](https://www.rfc-editor.org/rfc/rfc4731), and [RFC 9051](https://www.rfc-editor.org/rfc/rfc9051) for every SEARCH an IMAP4rev2-only server answers) is read as the `MailboxDatum::Search` it answers: `* ESEARCH (TAG "A7") UID ALL 1:3,5` gives the UIDs 1, 2, 3 and 5; MIN, MAX, COUNT and MODSEQ are passed over. imap-proto 0.17.0 has no ESEARCH at all, so such a server's every `UID SEARCH` was a reply Sealwax couldn't read, and the folder never synced. An ALL standing for more than `ESEARCH_MAX` (10,000,000) numbers is refused, so one short line can't make a list of billions. `test_esearch_as_search` is new.

A fifth change of Sealwax's own (2026-11-09, `src/rfc3501/mod.rs`, `mailbox`, [issue #31](https://github.com/PixDevsApps/sealwax/issues/31)): **a quoted mailbox name is decoded once**. `quoted` keeps a quoted string's escapes, so LIST's `"Labels\\Archive"` came out as `Labels\\Archive`, and Sealwax quoting that again for STATUS sent `"Labels\\\\Archive"`, a different name, which the server refused. One such folder ended the account's folder discovery, as an error in the pipelined STATUS commands does, so no folder synced; a Proton Mail Bridge account with a `\` in a folder name did that. A `"` in a name went the same way, and so would the folders under a parent on a server whose delimiter is `\`. `mailbox` now decodes `\\` and `\"` in a quoted name wherever imap-proto reads one with it (LIST, LSUB, STATUS and the ACL replies), before INBOX is recognised; a literal or an atom is left as it is, and any other escape `quoted` keeps (change 3) is kept here too. `quoted` itself is unchanged, so a BODYSTRUCTURE is read as before. `test_quoted_mailbox_name_in_list`, `test_quoted_mailbox_name_in_status`, `test_literal_and_atom_mailbox_names_are_verbatim`, `test_tolerated_escapes_in_mailbox_names_are_kept` and `test_inbox_is_normalized_after_decoding` are new.

The workspace's `Cargo.toml` and `fuzz/Cargo.toml` use it through `[patch.crates-io]`; it is excluded from the workspace, so `cargo fmt`, Clippy and the tests don't run on it. Its own tests pass with both commits and the five changes (105: upstream's five new ones and three for the changes, with `test_quoted` changed for the third, `test_esearch_as_search` and five for mailbox names; run outside the repository, where its dev-dependency can be fetched).

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

The differences should be in `src/rfc3501/body_structure.rs`, the two commits above (`b93964df` without its `src/tests.rs` hunk) applied in that order and changes 1 and 2 with their tests, in `src/core.rs`, change 3 with `test_quoted`'s cases, and in `src/rfc3501/mod.rs`, ESEARCH (`ESEARCH_MAX`, `mailbox_data_esearch`, its place in `MailboxDatum::parse`, `test_esearch_as_search`) and quoted mailbox names (`mailbox`, `unescape_quoted_mailbox` and their five tests).

To drop it once an imap-proto release has the limit, Bridge's three cases, ESEARCH and quoted mailbox names decoded once, and async-imap takes that release: remove both `[patch.crates-io]` entries and this folder, raise `async-imap` in the workspace's `Cargo.toml`, update the lock files, and keep `a_greeting_nested_too_deep_is_unreadable_not_a_crash` (`engine/src/imap/connection/tests.rs`) and `a_body_structure_nested_too_deep_costs_one_reconnect` (`engine/tests/sync.rs`) `bridges_empty_disposition_parameters_parse` (`engine/src/imap/fetch.rs`) `esearch_replies_sync_on_every_flavour` and `folder_names_with_backslashes_and_quotes_sync_on_every_flavour` (`engine/tests/sync.rs`) passing.

## async-imap 0.12.0, with responses framed before they are parsed, a buffer that grows with its data and an IDLE that can be refused

`vendor/async-imap/` is [async-imap](https://crates.io/crates/async-imap) 0.12.0 exactly as published on crates.io (MIT or Apache-2.0, `LICENSE-MIT` and `LICENSE-APACHE`), the IMAP client Sealwax builds on, with these changes, each marked "Sealwax's patch":

- **A literal's announced size isn't allocated ahead of its data:** `Buffer::ensure_capacity` grows the buffer only once it is full, by at most its own size at a time towards the size still needed. async-imap grew it to a literal's whole announced size as soon as the line announcing it (`{400000000}`) was read, and zero-filled it, so one line from a server made Sealwax allocate up to 512 MiB, async-imap's limit for one response, and hold it while the read waited. Now the buffer holds at most twice what has arrived: in a test, 2.1 MB for a 400 MB literal of which 1 MiB arrived, against 399 MB before. A line with no literal grows the buffer 4 KiB at a time, as before, and a literal of more than 512 MiB is now refused once that much has arrived rather than at once. `test_buffer_ensure_capacity` checks the new behaviour in place of the old, and `test_ensure_capacity_follows_the_data` is new.
- **A response is parsed once, when all of it has arrived** (2026-11-03, `src/imap_stream.rs`): `ImapStream::decode` parsed the whole buffer after every read, and a read brought at most 4 KiB because a full buffer grew 4 KiB at a time, so a long response without literals (a FLAGS list, a SEARCH result, a quoted string) took time growing with the square of its size: in a release build, a 4 MB FLAGS response took 12 s and a 4 MB SEARCH result 8 s, from a server or, before STARTTLS, anyone on the network path. Now a `Framing` looks at each new byte once for a line end that doesn't announce a literal (`{n}` or `{n+}` before the CRLF), passes over a literal's data, and imap-proto parses the response once it's whole: the same responses take 55 ms and 37 ms. Until a response's end has come, it is test-parsed only each time it has doubled, so bytes imap-proto refuses still fail at once, as before, and that costs at most about twice the response's size. Only a response's first line can be a status whose text ends like a literal announcement (`* OK {5}`), so a first line ending that way is parsed alone once to tell which. A full buffer with no size known doubles, up to the 512 MiB limit; a parsed response takes its bytes out of the buffer without copying them, and what is left is copied into a small block of its own once it is small.
- **A parse error holds none of the response** (2026-11-03, `src/imap_stream.rs`): the error for a response imap-proto can't read held the whole buffer twice over, nom's input as a list of numbers and then the text, several times the response's size: 80 MB for a 10 MB literal followed by one wrong byte, and up to several GB for a response near the 512 MiB limit. It now says only where parsing failed: `Tag at byte 11 during parsing of a 27-byte response`. Sealwax never logged it, but it was built in memory. `decode`'s two trace lines, which printed the response, are gone with the old parsing. `test_parsing_error` in `src/client.rs` checks the new text in place of the old.
- **An IDLE the server refuses is an error, never a panic** (`src/extensions/idle.rs`): `Handle::init` read a tagged `BAD`'s text with `unwrap()`, so a server answering IDLE with a bare `BAD`, which RFC 3501 allows, panicked the connection's task ([chatmail/async-imap#151](https://github.com/chatmail/async-imap/issues/151), item 1). It now gives the same error as a `BAD` with text, and a tagged `NO` ends IDLE's start the same way instead of being waited out until the caller's timeout (Sealwax's 30 s). `idle_refused` in `src/client.rs`'s tests is new.

Upstream knows about both: [chatmail/async-imap#148](https://github.com/chatmail/async-imap/pull/148), still open, proposes a configurable limit and a reader that keeps only a body's beginning, and #151, open, reports the IDLE panic with the same fix as here. The patches here are Sealwax's own, smaller ones.

The workspace's `Cargo.toml` and `fuzz/Cargo.toml` use it through `[patch.crates-io]`; it is excluded from the workspace, so `cargo fmt`, Clippy and the tests don't run on it. Its own tests pass with the changes (84 with `runtime-tokio`, as Sealwax builds it, and 78 with its default `runtime-async-std`, against the vendored imap-proto; run outside the repository, where its dev-dependencies can be fetched).

async-imap uses the deprecated `unsafe_pinned` macro in four places, which Cargo would warn about for the same reason as lettre's dead code. `Cargo.toml` ends with a `[lints.rust.deprecated]` table that allows it (2026-11-12, marked "Sealwax's patch"); no code is changed for it.

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

The only differences should be in `src/imap_stream.rs` (`ImapStream::decode` with `probe`, `parses_alone` and `parse`, the `framing` field, `parse_error`, `Frame`, `Framing` and `literal_len`, `Buffer::ensure_capacity`, `Buffer::split_to` and `Buffer::unsplit_front` in place of `take_block` and `return_block`, and the tests: `test_buffer_take_and_return_block` gone with those two, the end of `test_buffer_ensure_capacity`, a restart in `test_ensure_capacity_loop`, and the new `test_ensure_capacity_follows_the_data`, `test_buffer_split_to_and_unsplit_front`, `test_buffer_split_to_leaves_a_small_block`, `test_buffer_doubles_when_full`, `test_literal_len`, `test_framing_passes_over_literals`, `test_a_long_response_is_parsed_once`, `test_responses_in_pieces`, `test_random_pieces_read_as_whole`, `test_refused_bytes_fail_before_a_line_end` and `test_a_parse_error_holds_no_response`), `src/extensions/idle.rs` (the refusal in `Handle::init` with its comment) and `src/client.rs` (the new `idle_refused` test and the new text in `test_parsing_error`). `Cargo.toml` differs by the lint table at its end.

To drop it once an async-imap release no longer allocates a literal's announced size ahead of its data, parses a response once, builds its parse errors without the response and no longer panics on a bare `BAD` to IDLE: remove both `[patch.crates-io]` entries and this folder, raise `async-imap` in the workspace's `Cargo.toml` (and `spikes/s1-imap/Cargo.toml`), update the lock files, and keep `a_literals_announced_size_isnt_allocated_before_its_data` (`engine/tests/literal_size.rs`), `a_literal_that_arrives_in_pieces_comes_whole` (`engine/src/imap/raw.rs`) `an_idle_the_server_refuses_ends_in_an_error_at_once` (`engine/tests/newmail.rs`) and `a_long_reply_without_literals_is_read_at_once` (`engine/tests/long_reply.rs`) passing. To move it to a newer async-imap, copy the new published crate here and make the same changes.
