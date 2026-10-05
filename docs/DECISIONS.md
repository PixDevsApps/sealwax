# Architecture Decisions

Synced: 2026-10-05

The repository docs are the canonical project memory ([ADR-012](#adr-012--repository-docs-canonical-notion-as-a-mirror-2026-10-04)). Dated entries describe the state at that date; newer entries and amendments take precedence. ADRs written before the naming decision call the app "Mail", its working name; it is **Sealwax** ([ADR-010](#adr-010-accepted--sealwax-2026-10-04)).

---

Use this page for decisions with meaningful long-term cost. Keep each entry short, dated and explicit about alternatives. Change an accepted decision with a dated `###` amendment under it; do not rewrite history. Status moves Proposed → Accepted when the user decides or a spike provides the evidence; say which. Library facts below were checked against crates.io and the crate sources on 2026-10-04 ([RESEARCH.md](RESEARCH.md#library-verification-2026-10-04)).

| ADR | Title | Status (2026-10-05) |
| --- | --- | --- |
| 001 | Standalone native application | Accepted (user request) |
| 002 | GTK4 + libadwaita + WebKitGTK 6 | Accepted (S2 and S7 evidence) |
| 003 | Layering with an engine crate; tokio only there | Proposed; store pattern measured in S3, desktop integration in S6, the application shell built in M1, the engine service and autodetect built in M2 |
| 004 | Follow the live Omarchy theme | Accepted (user request); built in M1 |
| 005 | SQLite with FTS5 plus a blob store | SQLite + FTS5 accepted (S3 evidence); schema additions from S1; schema v1 and the blob store built in M1; schema v2 (the account key) and folder sync state in M2 |
| 006 | IMAP library and state-driven sync | Accepted (S1 evidence, GO): async-imap with a mandatory raw layer, UIDNEXT-window recovery; the connection layer built in M2 |
| 007 | SMTP, MIME and privacy defaults | Proposed; S7 confirmed the MIME side with amendments (interop pending); the SMTP login check built in M2 |
| 008 | OAuth2 and secrets | OAuth for Microsoft in 1.0; Sign in with Google deferred to 1.x (user decisions); mechanism accepted on S5's evidence (GO, 2026-10-05); secrets keyed by the account key in M2 |
| 009 | Settings in files: config.toml, keys.toml, accounts.toml | Proposed; config.toml and keys.toml built in M1, accounts.toml in M2 |
| 010 | Name and app-id | Accepted (user decision): **Sealwax**, `io.github.pixdevsapps.Sealwax`; code renamed 2026-10-04 |
| 011 | Reading pane: approach A with hardened WebKit | Approach A accepted (S2 evidence) with amendments; memory judged as PSS with a live-view cap (user decision); built in M1 |
| 012 | Repository docs canonical, Notion as a mirror | Accepted (user decision) |
| 013 | Lean 1.0 scope | Accepted (plan approval); amended at the M0 gate (background mode P0) |
| 014 | WYSIWYG composer in an editable WebKit view | Format accepted (user decision); mechanism confirmed by S7 with conditions (no acceleration; fcitx5 check pending) |
| 015 | Op queue: optimistic overlay, Undo, at-most-once send | Proposed; S1 amendment (idempotence rules) |
| 016 | Conversation threading | Proposed; alias table and merges confirmed in S3, inputs in S1; built in M1 |
| 017 | Keymap: scoped one-key shortcuts under Omarchy rules | Built in M1; new keys wait for the user |
| 018 | Logging and privacy | Accepted (plan approval); amended after S1 (third-party trace logs); the caps and autodetect's requests built in M2 |
| 019 | Corner radius follows Hyprland | Accepted (user decision at the DESIGN gate); built in M1 |

## ADR-001 — Standalone native application (2026-10-04)

**Status:** Accepted — user request, 2026-10-04.

**Decision:** Mail is a standalone native desktop application for Omarchy 4, written in Rust. It is not a web app, an Electron app, a terminal UI or an omarchy-shell plugin.

**Reason:** the user asked for a fast, beautiful, minimal client in Rust that adapts to Omarchy themes. A native app gets Wayland input methods, the portals, the keyring and real windows; a shell plugin can't host a message reader or composer.

**Consequence:** an unread-count bar widget is a separate, later item (1.x), built on the plugin pattern of the user's Hertz Radio and Scarlett Control plugins.

## ADR-002 — GTK4 + libadwaita + WebKitGTK 6 (2026-10-04)

**Status:** Proposed. S2 (rendering, security, NVIDIA) and S7 (composer) confirm it.

**Decision:** gtk4-rs 0.11 + libadwaita 0.9 (`v1_9`) + webkit6 0.6 (WebKitGTK 6.0, 2.52 installed), all on glib 0.22. Plain gtk4-rs with an `ActionId` registry, as in Stet; no relm4.

**Reason:**
- Stet already proves this stack on this machine (ADR-002 there, accepted after its M0), including the Omarchy theme follower this app reuses.
- HTML mail needs a real browser engine. WebKitGTK has the controls a mail reader needs: `enable-javascript-markup`, a construction-time `default-content-security-policy`, content filters, custom URI schemes, an ephemeral network session, isolated script worlds, `related-view` process sharing and printing.
- libadwaita's CSS variables let one generated stylesheet recolour every widget.

**Alternatives:** Qt 6 / QML through cxx-qt (Hertz) with QtWebEngine: a heavier engine (282 MiB installed) and a C++ bridge. iced, Slint or GPUI: no HTML engine, so HTML mail would need an embedded browser anyway.

**Consequence:** WebKit's web processes cost memory and start in roughly 100–300 ms, so views are pooled, share one process and stay off the startup path. Rendering on NVIDIA (nvidia-open 610) is unproven until S2.

### ADR-002 amendment — S2 and S7 results (2026-10-04)

**Status:** Accepted on spike evidence. WebKitGTK renders correctly on nvidia-open 610 without workarounds — no blank frames in 63 bodies, open p95 17.4 ms, scrolling p95 6.95 ms at 144 Hz ([S2 report](spikes/S2-webkit.md)) — and the editable view meets the composer's typing bar ([S7 report](spikes/S7-composer.md)).

**Further consequences:**
- WebKit pages are memory-heavy: about 45 MiB of GPU memory per accelerated page plus a surface as tall as the whole body; the app plus WebKit's network process already take 274 MiB RSS before any message is shown. Reader and composer views therefore render with `hardware-acceleration-policy = NEVER` (no GPU memory, a 4× faster cold start, scrolling still at 144 Hz).
- WebKit paces its own frames with a 60 Hz timer here; inside approach A the GTK side still scrolls at the monitor's rate.

## ADR-003 — Layering with an engine crate; tokio only there (2026-10-04)

**Status:** Proposed.

**Decision:** `email-domain` ← `email-infrastructure` ← `email-engine` ← `email` (the app).
- domain: pure logic.
- infrastructure: local std I/O only (store, blobs, configuration, Omarchy readers, logging).
- engine: the only tokio user (multi-thread runtime, 2 workers): network, sync, the op queue, the outbox and secrets. Parsing and indexing run in `spawn_blocking`; the store writer is its own std thread.
- app: GTK only. It talks to the engine with plain `Send` messages over `async-channel` (commands in, coalesced events out). No tokio type crosses into the app.

**Reason:** Stet avoids tokio, but mail can't: async-imap, reqwest, hickory-resolver and oo7 all need a runtime (Hertz already runs tokio in its infrastructure). Isolating it keeps the GTK thread clean and the app crate testable.

**Consequence:** every engine API is message-based. The GTK thread never runs SQL; reads go through a pool of two read-only connections on worker threads.

### ADR-003 amendment — the store pattern measured in S3 (2026-10-04)

S3 ran the GTK thread with no SQL at 100k messages ([report](spikes/S3-store.md)). The pattern to keep: two read-only readers that serve the newest request first; one writer that commits and emits list diffs; one maintenance thread (checkpoints, see ADR-005); every reply carries the store's `store_version`, and the UI applies a page only when its version matches the last change it applied, because pages and change events travel on different channels.

### ADR-003 amendment — desktop integration measured in S6 (2026-10-04)

S6 ran the layering on the live desktop ([report](spikes/S6-desktop.md)): SQL only on the reader thread (started in `startup`; the first page was ready 2 ms later), tokio only on the engine thread, and GDBus callbacks that run off the GTK thread only forward plain values over async-channel. Additions:

- **Single instance:** `HANDLES_COMMAND_LINE | HANDLES_OPEN`. The `command-line`, `open` and `activate` handlers create the window or take a hold synchronously; when a handler returns with neither, GApplication shuts down and a window created later fails.
- **Background mode** is an application hold while it is on. Closing the window destroys it; a notification click or a launcher activation builds a new one. Quit stops the engine (LOGOUT) before `quit()` (51–91 ms).
- **Resume and network:** on logind `PrepareForSleep(false)` and when `GNetworkMonitor` reports the network back, drop and reconnect every connection at once. Never wait for a socket to fail: a silently dead connection went unnoticed for 10 s.
- **Startup:** nothing but building the window runs on the GTK thread before `present()`. The first frame binds only the visible rows, and the list grows in steps of about 40 rows per frame (M1).

### ADR-003 amendment — the application shell built in M1 (2026-10-04)

Built as the S6 amendment says ([report](m1/shell.md)): an `adw::Application` subclass whose `command-line`, `open` and `activate` handlers build the window synchronously; background mode is a hold; signals, the exit timer and Quit share one quit path (`AppState::quit`). Additions:

- **A command line goes through the bus** when no instance runs: it asks the session bus to start the installed service and hands its `mailto:` links over (Stet's pattern, without `--wait`), so the running Sealwax never belongs to the terminal or browser that started it. Cost not measured (S6 suggests about 50 ms more); development builds have no service file and start directly.
- **Startup order:** early hooks run before GTK initialises (where the store's reader thread starts); after GTK's own startup the theme is read synchronously within its 500 ms budget (about 9 ms here), the settings files are read off the GTK thread, and the first window waits up to 150 ms for them, so its first frame already has the user's corners, fonts and keys.
- **The window reference is released on `window-removed`:** GTK 4 emits `destroy` only once nothing holds the window.

### ADR-003 amendment — the store's API as built (M1, 2026-10-04)

- **Requests** are typed methods on `Store` that return a `Reply<T>`, an async-channel receiver; dropping it cancels a read not yet started.
- **Opening:** `Store::open_in_background` keeps the opening SQL off the GTK thread.
- **Change events** carry exact per-view diffs, computed in the writer's transaction: positions removed before and inserted after the commit, rows changed in place, or a reload past 64 rows. Views register under a lock the writer holds from computing a commit's diffs until the commit, so a view never misses or double-applies a change.
- **Writer order:** interactive requests are served before pending full-text work and bulk batches.
- **Closing:** the app must call `close()` at quit; dropping the store doesn't wait for queued writes.

### ADR-003 amendment — the message list on the store (M1, 2026-10-04)

Built as [m1/list.md](m1/list.md) records.
- **Opening.** One thread opens the blob store and the store at `startup`, reads accounts and folders, and opens the first window's view (100 rows and today's section counts), then hands everything to the GTK thread. The first window waits for the first rows for at most 300 ms, alongside the settings' 150 ms. Headless with a 10k store this took 2.65–2.98 ms.
- **The model.** The list model applies change events strictly in version order. Pages apply only at the model's version; newer pages wait, and older ones are dropped and requested again. A version gap or `Reload` reopens the view.
- **Growth.** After every reset the list shows 40 rows, then grows by 40 a frame (S6's trim).
- **Search** re-runs `search` with a larger limit, because the store has no offset.
- **GTK 4.22 constraints:**
  - GtkListView creates ~205 new row widgets on every full reset.
  - It anchors row 0 under the viewport top.
  - It moves to row 0 when the focused row's widget dies.

  So the list notes its place before a reset, drops the focus for a same-view reload, and restores the place in the layout phase and the focus two frames later.

### ADR-003 amendment — the engine service and autodetect (M2, 2026-10-05)

Built in M2 (`engine::service`, `engine::autoconfig`; reports [m2/imap.md](m2/imap.md), [m2/autoconfig.md](m2/autoconfig.md), [m2/accounts.md](m2/accounts.md)).

- **The service:** one thread (`sealwax-service`) runs the engine's two-worker runtime and the command loop. The app starts it once the first frame is painted, so neither the engine nor the keyring delays it, attaches its `EngineLink` to the engine's channels, and stops it before the store closes. Stopping takes under 1 ms when nothing is in flight.
- **One store:** the app holds the store in an `Arc` and lends it to the engine, which writes what it syncs; the app closes it at shutdown, after the engine has stopped.
- **Order of work:**
  - autodetect and login checks run side by side;
  - adding, removing and reconciling accounts run in one lane, one job at a time, after `recover()` has finished what a crash interrupted;
  - each account has one sync task (connect, folder list, first INBOX sync), which is aborted and awaited before the account is removed.
- **Accounts:** the app sends `Command::Accounts` after every read of `accounts.toml` without mistakes. A faulty file sends nothing, so a typo never detaches an account. In M2 a sync runs once per start and once per added account; re-sync, IDLE and retries are M3's.
- **Self-tests** start no engine unless a script asks (`engine-start`), so the setup scripts keep their fake one.
- **Autodetect:** its lookups live in `engine::autoconfig` behind a `Network` trait (MX, SRV, CNAME, addresses, an HTTPS GET, the TLS check); its rules, presets and parser are pure code in `domain::autoconfig`. One detection is one future over all sources, so dropping it cancels every lookup. A preset answers without building any network. Tests drive the pipeline with a scripted `Network` on paused tokio time; real lookups run only in `devtools/autodetect-measure` and one ignored test.

## ADR-004 — Follow the live Omarchy theme (2026-10-04)

**Status:** Accepted — user request, 2026-10-04.

**Decision:** Port Stet's theme follower ([INTEGRATIONS.md](INTEGRATIONS.md#omarchy-theme)):
- read `~/.local/state/omarchy/current/theme/colors.toml` through `omarchy-theme-color --file … --all`, with Stet's Rust cascade as the fallback;
- map the palette to libadwaita CSS variables and `adw::StyleManager` ForceDark/ForceLight from `mode`;
- reload live with a GFileMonitor on `current/` (`theme.name` ChangesDoneHint, `theme` Renamed), debounced 150 ms, waiting ≤ 500 ms before the first window;
- follow the fontconfig `monospace` family for the chrome;
- regenerate the reader and composer WebView stylesheets on every change.

Mail adds roles: avatar tints (Stet's `distinct`), the unread accent and chips. Outside Omarchy the built-in palettes apply.

**Consequence:** no hook script and no writes under `~/.config`. Roles map to palette *positions*, never hue names (matte-black's `green` is amber).

### ADR-004 amendment — the theme follower as built in M1 (2026-10-04)

Built as decided ([report](m1/theme.md)), with these details:

- The palette is read through `omarchy-theme-color`; the built-in cascade matches its output for all 22 stock themes. `shell.toml` is the theme's generated file with the user's merged over it, as omarchy-shell reads it; it supplies the text size, the control states and the spacing.
- **Lead decisions (2026-10-04):**
  - secondary text keeps DESIGN's 35 % mix where that reaches 4.5:1 against the window and list backgrounds, and mixes less where it doesn't (six stock themes; lowest now 4.51:1);
  - the selected chip keeps the accent where it reaches 4.5:1, else the more readable of accent and foreground;
  - borders are `lighter_background` mixed 8 % (dark) or 14 % (light) toward `foreground`, as the approved mock computed them.
- Status colours keep a theme's own red, yellow and green when every two are at least 20° of hue apart, and are turned otherwise (11 of 22 stock themes).
- Avatar initials take `readable_on(tint, #ffffff, #1b1b1f)`, at least 4.5:1 for any accent.
- Startup blocks for the first read up to 500 ms (about 9 ms here); forcing libadwaita's colour scheme costs about 18 ms more only when the session's scheme disagrees with the theme. Helper programs have deadlines (2 s; `hyprctl` 300 ms). `SEALWAX_OMARCHY_STATE` points the app at a scratch state folder for tests.

## ADR-005 — SQLite with FTS5 plus a blob store (2026-10-04)

**Status:** Proposed. S3 confirms throughput and checks that the system SQLite has FTS5 (otherwise rusqlite's `bundled` feature).

**Decision:** rusqlite 0.40 in WAL mode with `user_version` migrations that refuse a newer schema (Hertz `infrastructure/src/storage.rs`), FTS5 for search, one writer thread and two read-only readers. Raw messages live in a content-addressed blob store: folders 0700, files 0600.

**Reason:** one well-understood embedded database for metadata, overlays, the op queue and search; raw `.eml` files keep attachments streamable and the database small.

**Consequence:** the schema is checked against real servers in S1 before M1 builds on it.

### ADR-005 amendment — S3 results (2026-10-04)

**Status:** SQLite with FTS5 is accepted on S3's evidence ([report](spikes/S3-store.md)); the blob store is built and measured in M1. At 100k synthetic messages (release, system SQLite 3.53.4): ingest 19,116 headers/s, FTS 10,324 messages/s, folder open ≤ 4.5 ms cold, the first 50 search results ≤ 34 ms cold, a one-message insert with its list diff p99 3.4 ms.

- Link the system SQLite (≥ 3.43) and refuse to start without FTS5; `bundled` isn't needed (3.53.4 has FTS5, `contentless_delete` and trigram).
- Schema v1 is S3's (accounts, mailboxes with counts, messages, threads with maintained counts, a per-mailbox conversation table, a Message-ID alias table, contentless FTS5, `store_version` and `meta`), adopted once S1 has checked its columns against real servers.
- **Checkpoints never run in the writer.** The writer's WAL hook only counts frames; a maintenance thread checkpoints at about 1,000 frames and then makes a tiny write so the WAL restart happens there. Otherwise the first commit after a checkpoint pays a ~10 ms WAL-header fsync (p99 38 ms under SQLite's default autocheckpoint).
- Search text is indexed in its own transaction after the header commit; sync batches stay at about 200 messages (500 headers with text hold the writer for up to 258 ms).
- CJK text is indexed as overlapping character pairs and queried as bigram phrases (unicode61 keeps a CJK clause as one token).
- Newest-first search walks the date index against the match set ("date first"); a date-ordered FTS rowid is faster but costs 51 % more index and half the indexing speed — decided in M7.
- Open (M7): diacritic folding. unicode61 folds å→a and ø→o but not æ, so "gar" finds "går".

### ADR-005 amendment — schema checked against the testbed in S1 (2026-10-04)

S1 synced S3's schema from Dovecot and Stalwart ([report](spikes/S1-imap.md)); schema v1 adds:

- mailboxes: the raw wire name (modified UTF-7), the hierarchy delimiter, the LIST attributes and resumable sync state (where an interrupted first sync continues);
- messages: a keywords column (a fixed flags bitmask loses custom keywords) and the stored References and In-Reply-To;
- ENVELOPE strings arrive RFC 2047-encoded and are decoded before display.

Real providers are still to check when test accounts exist.

### ADR-005 amendment — schema v1 and the blob store built (M1, 2026-10-04)

Built as [m1/store.md](m1/store.md) records.

**Schema v1:**
- S3's schema with S1's additions: folder wire name, delimiter, attributes, sync state; message keywords, In-Reply-To, References.
- Also: full recipient lists, Reply-To, List-Id, the body kind, and a `message_parts` table for body part references and attachments. S3's per-folder conversation table waits for the conversation list (ML-046).
- Index changes measured at 100k: the filter indexes hold `(folder_id, date, id, flag)` and `messages_date` holds `(date, id, folder_id)`, so filtered and multi-folder views read indexes in list order.
- Full text is indexed in its own transaction after each header batch. A high-water mark lets a restart index what a crash left unindexed.

**Blob store:** `ab/cd/<sha-256>`, 0700/0600, temp file + fsync + rename + directory fsync, each message stored once. The store reports orphans for the caller to delete. Throughput is bounded by fsync latency and grows with concurrent writers on this disk, so ingest prepares messages on 16 threads.

**At 100k:** 19,986 headers/s, 7,487 full-text messages/s, 2,695 messages/s end to end. The store needs SQLite ≥ 3.43 (FTS5 with `contentless_delete`); the package depends on `sqlite>=3.43`.

### ADR-005 amendment — folder sync state and server counts (M2, 2026-10-05)

Built in M2 ([m2/imap.md](m2/imap.md)); no migration, since schema v1 already holds the attributes, `selectable` and the sync state.
- A folder list is upserted in one commit.
- A new folder without messages shows the server's STATUS counts; the first sync's start replaces them with the store's own, and the recount check skips exactly the new, empty folders.
- The first sync records `Start`, `Floor` (after each batch's ingest has replied) and `Finish` as separate small commits. A UIDVALIDITY change resets the folder through `Store::remove`.

### ADR-005 amendment — schema v2: the account key (M2, 2026-10-05)

Built in M2 ([m2/accounts.md](m2/accounts.md)).
- Migration v2 adds `accounts.key` (unique; NULL for local accounts that no settings file lists: fixtures, self-tests, M1 stores) and `accounts.detached`.
- `remove_account` deletes an account and everything it owns in one transaction, deleting its full-text rows in rowid order (at 100k: 17.2 s in date order, 0.23 s sorted), and reports the raw messages no other account refers to.
- At 100,000 messages the transaction takes 3.8–4.0 s from a cold cache, while readers keep their p99 under 1 ms (under 5.4 ms with a warm cache). The writer is busy for the whole removal; batching it is possible if that hurts sync in M3.

## ADR-006 — IMAP library and state-driven sync (2026-10-04)

**Status:** Proposed. S1 picks the library after a written comparison.

**Decision:**
- Default library: async-imap 0.12 (`default-features = false`, `runtime-tokio`), pinned exactly, with a thin raw-command layer. The 2-day S1 comparison is against io-imap 0.7.
- **Server notices are hints only.** async-imap drops unsolicited responses when its 100-entry buffer is full, so correctness comes from state-based re-sync: CONDSTORE, then `ENABLE QRESYNC` with `UID FETCH … (CHANGEDSINCE m VANISHED)`, otherwise a full UID-list diff. A UIDVALIDITY change resets the folder.
- async-imap returns `()` from APPEND, COPY and MOVE (APPENDUID/COPYUID are lost), so new UIDs are found with `UID SEARCH HEADER Message-ID`.
- Folder names are decoded from modified UTF-7 by our code; special folders are recognised by SPECIAL-USE, then by a name table (nb/nn/en: Sendt, Søppelpost, Papirkurv…).
- One command connection and one IDLE connection (INBOX) per account; other folders are polled.

**Alternatives:** imap-client 0.3 (moved to pimalaya/io-imap), imap-next (depends on imap-codec 2.0.0-alpha), io-imap 0.7 (blocking + sans-I/O, broad extension support, but 7 breaking releases in 4 months).

**Consequence:** COMPRESS (feature `compress`) and Gmail labels wait for 1.x.

### ADR-006 amendment — S1 results: GO (2026-10-04)

**Status:** Accepted on S1's evidence ([report](spikes/S1-imap.md)): on the testbed (Dovecot 2.4.5 full, Dovecot IMAP4rev1-only, Stalwart 0.16.24), 10k headers sync in 0.31–0.46 s (1.77–1.94 s at 50 ms RTT); a no-change re-sync takes p50 7.2 ms (CONDSTORE) and 29.9 ms (UID diff); IDLE delivers in p50 493 ms (Dovecot's own delay) and 1.0 ms (Stalwart); 30 of 30 randomized runs end with zero divergence; connections cut mid-FETCH, mid-IDLE, mid-APPEND and mid-MOVE lose and duplicate nothing — all **with the changes below**.

- **Library:** async-imap 0.12.0, pinned, with a **mandatory** raw-command layer that every command goes through. Its typed `fetch`, `store`, `search`, `list`, `expunge`, `noop`, `capabilities` and `id` calls don't check for NO or BAD: on all three servers a refused UID FETCH came back as `Ok(0)` and a refused UID SEARCH as `Ok(empty)`, which an op queue would record as done. The raw layer checks every completion, sees every untagged response and keeps APPENDUID/COPYUID. Other confirmed defects: the unsolicited-response buffer delivered 100 of 150 notices; an unparseable response (ESEARCH) kills the connection, with raw protocol bytes in the error text; trace logging prints the LOGIN password (ADR-018 amendment). io-imap 0.7.1 was as fast and more complete but isn't ready (six breaking releases in four months, an alpha codec, a blocking client); revisit at 1.x.
- **New UIDs** come from APPENDUID/COPYUID, falling back to the Message-IDs of UIDs at or above the target's UIDNEXT recorded before the write. `UID SEARCH HEADER Message-ID` is dropped: Stalwart never matches it, and recovering a lost APPEND reply with it duplicated the message.
- **Before every IDLE,** send NOOP and re-sync if it reports a change (Stalwart doesn't notify changes made between the last command and IDLE).
- **The account pass includes INBOX in its STATUS check** as a backstop (without it Stalwart diverged in 2 of 10 seeds), and **never trusts STATUS for the connection's own selected mailbox** (Dovecot returns a stale view).
- **Every read has a timeout**, IDLE start and DONE included.

Not yet verified: real providers (Gmail labels, XOAUTH2), the LOGIN fallback (every server offered PLAIN), COMPRESS, lossy links, long IDLE across suspend and resume, mailboxes far beyond 10k.

### ADR-006 amendment — the IMAP connection layer as built (M2, 2026-10-05)

Built in M2 ([m2/imap.md](m2/imap.md)) on S1's raw-command layer.
- **Connect:** TLS from the first byte, or STARTTLS sent whatever the plaintext capabilities say; a NO, BAD, failed handshake or PREAUTH greeting ends the attempt, and capabilities are read again over TLS. AUTHENTICATE PLAIN; LOGIN only without PLAIN and without LOGINDISABLED; XOAUTH2 with a token from the caller. `ID ("name" "Sealwax" "version" …)` where offered; ENABLE QRESYNC where offered. TCP keepalive and a timeout on every read; 30 s for connect and login together.
- **Errors carry no server text:** refused logins are classified into `LoginFailure` (RFC 5530 codes, then provider texts, most of them unverified until test accounts exist) and the text is dropped.
- **Folder list:** `RETURN (SPECIAL-USE STATUS (MESSAGES UNSEEN))` where LIST-STATUS is offered, else LIST plus pipelined STATUS; roles by SPECIAL-USE, then the nb/nn/en name table.
- **First sync:** batches of 200 UIDs newest first: header fields and the first 1,024 bytes of the text (no ENVELOPE), parsed by mail-parser like whole messages, one store transaction per batch, then the floor; the next batch is fetched during the commit. 1,000 headers in 84–229 ms on the testbed.

## ADR-007 — SMTP, MIME and privacy defaults (2026-10-04)

**Status:** Proposed. S7 confirms the output in Gmail web, Outlook web and Thunderbird.

**Decision:**
- lettre 0.11 with `default-features = false` and `smtp-transport`, `tokio1-rustls`, `rustls-platform-verifier`, `ring`; `relay()` for port 465, `starttls_relay()` (Required) for 587; `hello_name` is the literal 127.0.0.1. On an XOAUTH2 failure, refresh the token and retry once (lettre doesn't answer the error challenge).
- mail-builder 1.0 with `default-features = false` (no `gethostname`); the MIME tree is built by hand as `mixed{alternative{text, related{html, inline…}}, attachments}`, because its shortcut API puts inline parts in multipart/mixed.
- Message-IDs are ours, using the sender's domain. **The transmitted copy never carries a Bcc header.**
- mail-parser 0.11 with `full_encoding` (CJK charsets); format=flowed (RFC 3676) decoded by our own code in domain.

**Consequence:** no hostname in Message-ID, EHLO or Received headers. Certificate pinning for SMTP isn't possible with lettre (TlsParameters takes no custom rustls config), so pinned exceptions (Proton Bridge) wait for 1.x.

### ADR-007 amendment — S7 results (2026-10-04)

S7 confirms the MIME side ([report](spikes/S7-composer.md)): 12 messages round-trip through mail-parser (structure, both text parts, inline-image links, attachment names, decoded headers); no Bcc header in the transmitted copy; 0 hostname hits in any output. Additions:

- The Sent copy is the transmitted bytes plus one `Bcc:` field.
- A message with only Bcc recipients gets `To: "undisclosed-recipients":;`.
- Text attachments set `charset=utf-8` explicitly: mail-builder drops its default once a part has other parameters.
- File names use RFC 2231 `filename*` plus an RFC 2047 `name=` for older clients.
- Message-ID domains go through an IDN (punycode) converter in the domain crate.
- `Date:` stays in UTC (mail-builder's choice), which also keeps the sender's time zone private.

Interop in Gmail web, Outlook web and Thunderbird is still pending test accounts and sending approval; the samples are in `spikes/s7-composer/fixtures/`.

### ADR-007 amendment — the SMTP login check (M2, 2026-10-05)

Built in M2 ([m2/imap.md](m2/imap.md)).
- lettre speaks SMTP over a socket from `engine::net` (keepalive; DNS and connect failures told apart). Implicit TLS uses the same rustls configuration as IMAP and is handed to `connect_with_transport`; STARTTLS uses lettre's `TlsParameters` from the same trust (the platform verifier, or exactly the testbed CA with `CertificateStore::None`).
- `hello_name` is `ClientId::Ipv4(127.0.0.1)`, sent as `EHLO [127.0.0.1]` (the RFC 5321 address literal).
- A refused login drops the connection rather than lettre's `abort()`, which waits for QUIT's reply without a timeout; QUIT after success waits 2 s at most. lettre's error displays include server replies, so they are never logged.

## ADR-008 — OAuth2 and secrets (2026-10-04)

**Status:** OAuth2 sign-in for Google and Microsoft in 1.0 is accepted — user decision, 2026-10-04. The mechanism is proposed; S5 is the go/no-go.

**Decision:**
- Our own PKCE + loopback (`http://127.0.0.1:<port>`) + refresh code on reqwest 0.13 (about 200 lines). The `oauth2` 5.0 crate would pull in reqwest 0.12, rand 0.8 and thiserror 1.
- Client IDs come from build-time environment variables, overridable in `config.toml`. The user registers the clients ([OAUTH-SETUP.md](OAUTH-SETUP.md)).
- XOAUTH2 for IMAP, SMTP and POP3; OAUTHBEARER where offered.
- Secrets (passwords, refresh tokens) live only in the Secret Service, through oo7 0.6 behind a `SecretStore` trait. An in-memory implementation serves headless tests and the no-keyring fallback (banner; secrets kept for the session only).
- The keyring never blocks the first frame.

**Reason:** Outlook.com accepts only OAuth for IMAP since 2024; Gmail needs OAuth or an app password. Provider policies are re-checked in S5.

**Consequence:** if Microsoft can't be registered, Outlook.com and Microsoft 365 move to 1.x. Google requires app verification before a public release; until then the app works for the owner and up to 100 test users. gnome-keyring's passwordless Default keyring on this machine stores secrets in plaintext under full-disk encryption ([SECURITY.md](SECURITY.md#known-limitations)).

### ADR-008 amendment — Sign in with Google deferred to 1.x (2026-10-04)

**Status:** Accepted — user decision, 2026-10-04: drop the Gmail integration for now, because the OAuth work is too much and most Gmail users read their mail in Google's web app.

**Decision:** "Sign in with Google" leaves 1.0 for 1.x. Microsoft sign-in stays in 1.0: Outlook.com and Microsoft 365 accept only OAuth for IMAP, so without it those accounts can't be added.

**What remains for Gmail without extra work:** a Gmail or Google Workspace account can still be added as a plain IMAP account with an app password (2-Step Verification on), through the generic path and the existing preset; the setup assistant's app-password message links to Google's page. There is no Gmail-specific work in 1.0 and no Gmail test account in the test matrix.

**Reason:** Google classes the `https://mail.google.com/` scope as restricted: a public release needs app verification with a security assessment, and in testing status refresh tokens expire after 7 days. Microsoft's registration has neither.

**Consequence:** S5 (ML-013) and ML-032 cover Microsoft only, and the user registers only the Microsoft Entra client. The Google section of OAUTH-SETUP.md is kept for 1.x; S5's code keeps its Google flow as history.

### ADR-008 amendment — S5 results: GO for Microsoft (2026-10-05)

**Status:** the mechanism is accepted on S5's evidence ([report](spikes/S5-oauth.md)): our own PKCE loopback code on reqwest completes Microsoft's sign-in in the system browser, IMAP and SMTP accept XOAUTH2 on Outlook.com, and the refresh token survives a restart in the Secret Service. Outlook.com and Microsoft 365 stay in 1.0.

- **No API permissions** are configured for the delegated flow: the Outlook scopes are requested, and consented, at sign-in.
- **The XOAUTH2 username is the identity the token was issued to.** SMTP refused an alias and the mailbox's generated primary address of a Microsoft account first created with an external address, and accepted only its sign-in name; IMAP accepted the alias as well. ML-032 takes the username from the sign-in, not from the address typed into the assistant.
- **Refresh tokens rotate** on every refresh; each new one replaces the stored one.
- **The redirect** is `http://localhost:<port>` without a path; the port is ignored when matching.

### ADR-008 amendment — secrets keyed by the account key (M2, 2026-10-05)

Built in M2 ([m2/accounts.md](m2/accounts.md)).
- Keyring items are keyed by the account key from `accounts.toml`.
- Adding an account stores the secret before the entry is written (the commit point); removing one deletes the secrets last. A journal of keys (`$XDG_STATE_HOME/sealwax/accounts-pending`) lets the engine delete the secret of an add that crashed before its commit point, and finish a removal that crashed or failed after its own.
- The engine opens the keyring on first use, never on the startup path. Self-tests keep secrets in memory; development builds can too, with `SEALWAX_SECRETS=memory` ([SECURITY.md](SECURITY.md#credentials)).

## ADR-009 — Settings in files: config.toml, keys.toml, accounts.toml (2026-10-04)

**Status:** Proposed.

**Decision:** Stet's pattern: hand-editable `config.toml` and `keys.toml` under `$XDG_CONFIG_HOME/<app>/`, live-reloaded, with mistakes reported by line and column and a faulty file never applied. Mail adds `accounts.toml`, written by the setup assistant and editable by hand. None of them ever holds a secret.

**Reason:** Omarchy configures everything through files; a settings window doesn't fit (Stet ADR-013 critique M7). Account setup still needs a guided UI, so the assistant writes the file.

**Consequence:** a grep test asserts that no secret reaches any of these files.

### ADR-009 amendment — config.toml and keys.toml as built in M1 (2026-10-04)

Built in M1 ([report](m1/actions.md)): `config.toml` holds `[appearance]` (`corner_radius`, `chrome_font`, `reading_font`, the theme's own types) and `[oauth.microsoft]` with `client_id` only. Microsoft's client is public, Sign in with Google is deferred, and no settings file holds a secret. `keys.toml` maps action names to key lists. Both files are:
- read with the `toml` crate's spanned tables, so every mistake carries its line and column, with a "did you mean";
- applied whole or not at all;
- reread 150 ms after a change seen on their folder and its parent;
- created with every default commented out when the palette opens a missing one.

### ADR-009 amendment — accounts.toml as built (M2, 2026-10-05)

Built in M2 ([m2/accounts.md](m2/accounts.md)).
- **Format:** `[[account]]` tables with `key`, `name`, `address`, `username` (the address by default) and `auth` (`"password"` by default, or `"microsoft"`), and `[account.imap]` and `[account.smtp]` with `host`, `port` (by protocol and security when left out) and `security` (`"tls"` by default, or `"starttls"`).
- **Reading:** as `config.toml`: line and column for every mistake, "did you mean", applied whole or not at all. A field named like a password, secret or token is a mistake anywhere in the file; keys and addresses are unique; values are never quoted in messages, because the app logs them.
- **The key** is the account's stable name: 12 random lowercase base32 characters, which the engine generates when it adds an account, or a word typed by hand. It names the keyring items and the store row, so a deleted store finds its accounts again.
- **Writing:** a new entry is appended after the file's unchanged text; an entry is removed with `toml_edit`, keeping the user's comments and layout; nothing is added to a file with mistakes. Writes are atomic (0600 in a 0700 folder), follow a symbolic link to its target, and start over when the file changed during the write.
- **Hand removal detaches:** an entry removed by hand stops its account's sync and hides it from the sidebar and All Inboxes; its mail and secret stay until the entry returns or the Remove command deletes it. A hand edit never deletes data.
- **The grep test** this ADR asked for is `engine/tests/no_secret_in_files.rs`.

## ADR-010 — Name and app-id (2026-10-04)

**Status:** Open. The user decides during M0; it must be accepted before M0 closes, because the data paths, the D-Bus name, the keyring schema and the OAuth consent screen depend on it.

**Working values:** name "Mail", crates `email-*`, binary `email`, environment prefix `MAIL_`, app-id `io.github.pixdevsapps.Mail` (`.Devel` for debug builds).

**Rules:** `io.github.pixdevsapps.<Name>`; never `org.omarchy.*`, `TUI.*` or an `oma*` prefix; check for collisions with existing mail software and Arch packages.

### ADR-010 accepted — Sealwax (2026-10-04)

**Status:** Accepted — user decision, 2026-10-04.

**Decision:** the app is **Sealwax**. The user's brand canvas ("Sealwax Logo", a private design artifact) sets the identity:

- **Wordmark:** lowercase "sealwax" in IBM Plex Mono SemiBold, with the tagline "Email for Omarchy".
- **Seal mark:** a wax seal with an @ emblem.
- **App icon:** an envelope tile (paper or ink) with the seal; a simplified seal at 64 px and below.
- **Brand palette:** wax #B3302A, deep wax (wax mixed 38 % toward black), ink #15151C, paper #F2EADB.

The brand colours belong to the icon and wordmark only; the app's chrome keeps following the Omarchy theme (ADR-004).

| | Value |
| --- | --- |
| App-id | `io.github.pixdevsapps.Sealwax` (`io.github.pixdevsapps.Sealwax.Devel` for debug builds) |
| Binary | `sealwax` |
| Crates | `sealwax` (app), `sealwax-domain`, `sealwax-infrastructure`, `sealwax-engine`; the M0 spike crates keep their `email-spike-*` names as history |
| Environment prefix | `SEALWAX_` |
| Config, data and state folders | `$XDG_*_HOME/sealwax` |
| Keyring attribute `application` | the app-id |

**Collision check (2026-10-04):** no `sealwax` in the Arch repositories, the AUR or crates.io, and no command of that name; the nearest names are "Seal", an iPad mail app, and "Sealit", an email-encryption add-in — neither is the same name.

**Consequence:** the working names are replaced in one change once the S6 spike (running at the time of the decision) has finished, so its build isn't broken mid-run; the Notion hub and board are renamed at once; card IDs stay `ML-nnn`.

## ADR-011 — Reading pane: approach A with hardened WebKit (2026-10-04)

**Status:** Proposed. S2 decides; approach B is the fallback.

**Decision (approach A):**
- GTK widgets draw headers, attachment cards, banners and the quick reply. Plain-text mail uses a GTK text view and no WebView.
- Each expanded HTML body gets a pooled WebView. All share one web process (`related-view`) and an ephemeral network session. Height comes from an isolated-world ResizeObserver.
- Security layers: `enable-javascript-markup = false`; the construction-time `default-content-security-policy` `default-src 'none'; img-src cid: data: https:; style-src 'unsafe-inline'; font-src cid: data:; form-action 'none'; base-uri 'none'; frame-src 'none'`; a content filter blocking http(s) until remote content is allowed; `decide-policy` blocking every navigation and opening links with `gtk::UriLauncher`; `cid:` registered as a URI scheme per view; DNS prefetching, hyperlink auditing, page cache, storage, WebGL, audio, media and developer extras (release) off.
- lol_html only feeds the dark-mode heuristic: HTML with its own colours sits on a light "paper" card; simple HTML and plain text take the theme colours.

**Alternative (B):** one WebView per conversation with sandboxed `srcdoc` iframes — fewer views, but `cid:` collisions across messages and weaker native focus and accessibility.

**Consequence:** no HTML sanitizer is needed for display; the layers above do the work. The hostile-HTML suite (25+ cases) guards them in CI.

### ADR-011 amendment — S7 findings (2026-10-04)

From the composer spike ([report](spikes/S7-composer.md)), which uses the same hardening:

- **The page's CSP does not bind isolated-world scripts' `fetch()`.** With the CSP alone, a helper `fetch()` reached the local test server; only the content filter stopped it. Rule: helper scripts fetch nothing but `blob:` URLs, and every view that runs helper scripts keeps the content filter.
- With `enable-javascript-markup = false`, no markup script or event handler ran even without the CSP.
- The `enable-dns-prefetching` and `enable-hyperlink-auditing` setters are deprecated in WebKitGTK 2.48/2.50; they stay off where they still exist, but nothing relies on them.
- Every view uses an ephemeral network session: WebKit's default session writes under `$XDG_CACHE_HOME/<prgname>` and `$XDG_DATA_HOME/<prgname>`.

### ADR-011 amendment — S2 results: approach A accepted (2026-10-04)

**Status:** Approach A is accepted on S2's evidence ([report](spikes/S2-webkit.md)); approach B is rejected (its scrolling runs at WebKit's 60 Hz timer, p95 17.0 ms, missing the bar at 144 Hz; WebKit strips `srcdoc` with script markup off; no memory advantage over A rendered on the CPU). Every S2 bar passes except memory: 47 hostile cases with 0 connections, 0 TLS hellos and 0 script traces; `cid:` 158/158; 0 blank bodies of 63; scrolling p95 6.95 ms; heights 63/63 within ±1 px; open p95 17.4 ms; renders under `tools/headless.sh` in software.

The layers and the sizing change as follows (SECURITY.md carries the security rules):

1. The runtime features `LinkDNSPrefetch`, `LinkPreconnect`, `LinkPrefetch`, `LinkPreload` and `SpeculationRulesPrefetch` are off.
2. Mail's isolated world intercepts link clicks and form submits and hands the URL to Mail, which opens only absolute http(s) and mailto links after the mismatch check; `decide-policy` still blocks everything as the backstop. (As first written, WebKit connected to a clicked link's host — 3 of 47 cases — before `decide-policy` answered.)
3. The content filter blocks `.*`, then one `ignore-previous-rules` rule each re-allows `^cid:`, `^data:` and `^about:` (WebKit's filter regexes reject alternation).
4. The CSP ends with `upgrade-insecure-requests`; without it, http images never load even after the user allows remote content.
5. Reader views render with `hardware-acceleration-policy = NEVER`, as the composer does (S7).
6. Each body sits in a container whose height is exactly the reported height (a WebView's natural height is WebKit's contents size, which never drops below the current viewport).
7. The reader stylesheet adds `html,body{height:max-content!important;min-height:0!important;max-height:none!important}` (quirks-mode bodies otherwise fill the viewport and report a wrong height).
8. Height reports are followed until they settle; growth driven by viewport units is frozen, and views are capped at 16,000 px.
9. `related-view` is mandatory (4× the RSS without it).
10. Scripts-off markup stays the script defence of record (alone it stopped every script in 47 cases); the content filter stays because scripts-off makes `<noscript>` content live.

**Open:** memory. Ten tall newsletter bodies in one shared web process measured 804 MiB RSS with GPU rendering and 628 MiB on the CPU (PSS 512 / 377 MiB), against the bar of 300 MB. The bar's metric and size are the user's decision at the M0 gate; the design answer is to cap live views per conversation (only expanded bodies have views, typically one to three) and release views far off screen.

### ADR-011 amendment — memory bar decided (2026-10-04)

**Status:** Accepted — user decision at the M0 gate, 2026-10-04 ("PSS + design cap").

**Decision:** memory is judged as PSS of the whole process tree (app plus WebKit's web and network processes), so memory shared with the GPU driver and between processes is counted once, plus a separate GPU-memory budget. The reader target is sized for a typical conversation, and the number of live message views is capped: only expanded HTML bodies hold a view, views far off screen are released and re-rendered on return. The cap and the GPU budget are set from the M5 measurement (proposed starting cap: 4 expanded bodies). RSS is still recorded. The targets are in [TESTING.md](TESTING.md#performance-targets).

**Consequence:** S3's list passes its bar (133 MiB PSS against 150 MB). S2's reader bar is re-measured in M5 under the new definition and is part of M5's exit criteria.

**M1 measurement** (added 2026-10-05): the integrated app's process at the first painted rows of a 100k list is 151.7 MiB PSS, live. 37.4 MiB of it is NVIDIA's GL and Vulkan libraries and device mappings; without them it is 114.3 MiB. Whether the driver's share counts toward the UI's 150 MB or toward the GPU budget is decided in M5, with the reader measurement (user decision at the M1 gate, 2026-10-05, "Decide in M5").

### ADR-011 amendment — the reader as built in M1 (2026-10-04)

Built as [m1/reader.md](m1/reader.md) records, with every layer of the S2 amendment. Additions:
- **One engine per process:**
  - one web context and ephemeral session (cookies never accepted, TLS errors fail);
  - one compiled content filter;
  - one `cid:` handler, keyed by the requesting view;
  - every view sharing the first view's web process.

  At most 4 views are live (the memory decision's starting cap).
- **The filter is set per load:** on, unless the user allowed that message's images this session; a released view gets it back before it loads anything else.
- **A message is shown once laid out:** the next page is laid out unseen at the column's width and replaces the shown one once its height is measured (3 s at most). Height reports carry a per-load token.
- **Views are released** 3 s after the reader is hidden, and rendered again when it returns.
- **Keys:** WebKitGTK 2.52 hands unhandled keys back to GTK, so the Reader scope works from a focused message without forwarding.
- **Focus:** before a page leaves, a focus held in it moves to the next body. GTK 4.22 would otherwise move it later, from a widget no longer in the window (a Gtk-CRITICAL the reader's examples now fail on). The lead's wiring clears the reader before the window for the same reason.
- **The app's store:** the reader reads through a `MessageSource`: the app's `Mailbox`, which owns the store and closes it at shutdown, or a store of its own in examples.
- **A crashed web process** renders the message once more, then as text.
- **Started after the first frame** (added 2026-10-04, after the live startup check): the reader, and with it the engine, is created right after the first window's first painted frame instead of in the window build. A selection made before then is shown once the reader exists, and the reader takes the keymap current at that moment. The view prewarm still waits for the reader to be shown.

## ADR-012 — Repository docs canonical, Notion as a mirror (2026-10-04)

**Status:** Accepted — user decision, 2026-10-04.

**Decision:** The Markdown in this repository (`docs/` plus README and AGENTS) is the project memory, as in Stet (its ADR-012). Notion holds a mirror for overview: the private hub "Mail (working name) — Product Development" and its "Mail Development Kanban" (cards ML-001 to ML-096, same fields and views as the Hertz board). When a task changes state in PLAN.md, its card is updated in the same session. If Notion can't be reached, PLAN.md gets a "Notion sync pending" note. PLAN.md wins any conflict; work never waits on Notion.

**Reason:** the user wanted a kanban for overview. Stet showed that a second canonical source drifts and that work must not stop when the connector is unavailable; a mirror keeps both benefits.

**Consequence:** OPERATING.md carries the sync rule. The hub and board are renamed when ADR-010 is accepted.

## ADR-013 — Lean 1.0 scope (2026-10-04)

**Status:** Accepted — plan approval, 2026-10-04.

**Decision:** 1.0 is the scope in PLAN.md "Scope control": IMAP and SMTP with autodetect and OAuth, conversations, safe reading, triage with Undo, the WYSIWYG composer with drafts and outbox, local search, notifications, background mode, the mailto handler, the live theme, the keyboard set and print; POP3 sits in M8 and can move to 1.x by user decision. Everything else is on the 1.x, Later or Never lists.

**Reason:** the user asked for the core functions, the common protocols and automatic setup first. The review on 2026-10-04 moved COMPRESS, pinned certificates, schedule send, snooze, folder management, server search, rules, List-Unsubscribe and Gmail labels to 1.x.

**Consequence:** moving an item into 1.0 needs a user decision, recorded as a dated PLAN.md entry and an amendment here.

### ADR-013 amendment — Sign in with Google to 1.x (2026-10-04)

**Status:** Accepted — user decision, 2026-10-04 ([ADR-008 amendment](#adr-008-amendment--sign-in-with-google-deferred-to-1x-2026-10-04)). "Sign in with Google" moves from 1.0 to the 1.x list; Microsoft sign-in stays in 1.0.

### ADR-013 amendment — notifications and background mode (2026-10-04)

**Status:** Accepted — user decision at the M0 gate, 2026-10-04.

**Decision:** background mode (PRD D6) moves from P1 to P0; opt-in autostart stays P1. D5 is reworded: a click on a new-mail notification opens the message everywhere; Archive and Mark read appear where the notification server shows buttons, and stay one key away in the reader.

**Reason:** S6 showed that on Omarchy a notification click reaches the app only while it runs — GLib's freedesktop path can't start a stopped app, and omarchy-shell's history entries carry no live actions — and that omarchy-shell 0.3.1 shows no action buttons ([report](spikes/S6-desktop.md)).

## ADR-014 — WYSIWYG composer in an editable WebKit view (2026-10-04)

**Status:** The WYSIWYG format is accepted — user decision, 2026-10-04 (over a Markdown composer). The mechanism is proposed; S7 confirms it.

**Decision:** an editable WebKit view (separate from the reader views) using WebKit's native editing commands, with toolbar state from `EditorState`. A small isolated-world script serialises the HTML, cleans pasted content and turns inline images into `cid:` parts. Each message goes out as multipart/alternative with an html2text plain-text part ([ADR-007](#adr-007--smtp-mime-and-privacy-defaults-2026-10-04)). Spell check uses WebKit's enchant support when hunspell dictionaries are installed (optdepends).

**Alternatives:** a Markdown composer in GtkSourceView (faster to build; the user preferred WYSIWYG); a GtkTextView with tags and a hand-written HTML serialiser (poor fidelity for quoted HTML).

**Consequence:** M6 grows by about 6–10 days; fcitx5 and Norwegian dead-key typing are part of S7's pass bar.

### ADR-014 amendment — S7 results (2026-10-04)

S7 confirms the mechanism with conditions ([report](spikes/S7-composer.md)). Formatting, toolbar state, undo and paste cleaning pass; the measured edit-to-painted-frame latency (live, 144 Hz, release) passes only without hardware acceleration:

| Composer view | p50 | p95 |
| --- | --- | --- |
| WebKit default (accelerated) | ≈ 14 ms | 16.0–24.1 ms — fails 16 ms |
| `hardware-acceleration-policy = NEVER` | 8.0–10.0 ms | 9.9–11.9 ms (max 15.8 ms) |

Decisions:

- The composer view sets `hardware-acceleration-policy = NEVER`. The setting is per view; the reader keeps the default unless S2 says otherwise.
- WebKitGTK binds only Ctrl+B, Ctrl+I and Ctrl+A in editable content, so the app binds Ctrl+U, Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y, Ctrl+K and a paste-without-formatting key as actions (under ADR-017's audit).
- Spell check is enabled only when WebKit reports at least one language. Optional dependencies: `hunspell` with `hunspell-en_us` and others; Norwegian comes as `aspell-nb` / `aspell-nn` through enchant.
- Quirks the composer must handle: unlink needs a selection; quote and indent are both `<blockquote>` (indent is told apart by WebKit's margin style); strikethrough writes `<strike>`; lists arrive wrapped in a `div` with a trailing `<br>`; when both are offered, the paste event shows only `text/html`; an image-only clipboard is converted by the helper from a `blob:` image to a `cid:` part.

Still open: the user's fcitx5 and Norwegian dead-key check (`bash spikes/s7-composer/manual-check.sh`); a re-measure on a 60 Hz display and scrolling of long quotes without acceleration, before M6.

## ADR-015 — Op queue: optimistic overlay, Undo, at-most-once send (2026-10-04)

**Status:** Proposed.

**Decision:**
- An op row and its local overlay are written in one transaction. Sync always writes what the server says; the UI shows server state plus overlays; sync never clears an overlay whose op is pending.
- Ops are UID-based and idempotent (`+FLAGS` / `-FLAGS`, never a full replace); a MOVE of a UID that no longer exists counts as done. Ops record `(folder, uidvalidity, uid)` and are re-targeted by Message-ID after a UIDVALIDITY change.
- Archive, delete, move and send carry `not_before = now + 5–10 s`; Undo inside the window cancels locally; after it, Undo queues the inverse op. Only the last action is undoable from the toast.
- Per-message FIFO, batched into UID sets per folder; flag ops merge, move chains collapse.
- After a crash, in-flight ops replay idempotently — **except Send, which is at most once**: a message left in "sending" asks the user.

**Consequence:** a crash-during-queue replay test and undo-window tests gate M4.

### ADR-015 amendment — S1 results (2026-10-04)

From the fault-injection runs ([S1 report](spikes/S1-imap.md)):

- Ops that create messages (APPEND, COPY) record the target's UIDNEXT before the first attempt; a retry first looks for the message among UIDs at or above it.
- Replaying a MOVE of a UID that no longer exists is harmless. A COPY-based move (servers without MOVE) is **not** idempotent — a blind replay made two copies — so it goes through the same UIDNEXT check.
- Without UIDPLUS, EXPUNGE removes every `\Deleted` message in the mailbox, not just the op's. The queue expunges only with `UID EXPUNGE` (UIDPLUS); otherwise it leaves the message flagged `\Deleted` and hidden locally.
- STORE runs in a read-write SELECT: Dovecot answers "OK Store ignored" in an EXAMINEd mailbox.

## ADR-016 — Conversation threading (2026-10-04)

**Status:** Proposed.

**Decision:** threads from References and In-Reply-To with union-find aliases (a parent can arrive after its reply), duplicate Message-IDs merged (Gmail labels, copies to yourself), Outlook `Thread-Index` honoured, Gmail `X-GM-THRID` used only as a hint. Never group by subject alone. Threads over 100 messages collapse their middle.

**Consequence:** property tests cover out-of-order arrival and merges.

### ADR-016 amendment — S3 results (2026-10-04)

The Message-ID alias table and thread merges work at 100k messages (40,548 conversations): a parent arriving late joins its thread, threads merge, duplicates collapse, and the counts the writer maintains match a full recount (kept as a test). The first 100 of 24,844 conversations in a folder load in ≤ 3 ms cold. Not yet exercised: Outlook `Thread-Index`, the Gmail thread-ID hint, and collapsing threads over 100 messages.

S1 (2026-10-04) confirmed the inputs on the testbed: Message-ID, In-Reply-To and References all come from the header sync, and duplicate Message-IDs do occur. Gmail's and Outlook's own thread headers are still unverified.

### ADR-016 amendment — threading as built (M1, 2026-10-04)

Built in M1 ([m1/store.md](m1/store.md)): union-find over `thread_aliases` (every Message-ID, In-Reply-To and References entry, per account); merges go into the largest thread and are reported in change events; a thread's size counts each Message-ID once. Not built yet: Outlook's `Thread-Index`, the Gmail thread-ID hint, collapsing threads over 100 messages, and per-folder conversation rows.

## ADR-017 — Keymap: scoped one-key shortcuts under Omarchy rules (2026-10-04)

**Status:** Proposed.

**Decision:** Gmail-style one-key shortcuts (`j`/`k`, `e`, `#`, `r`, `a`, `f`, `c`, `s`, `/`, `g` `i`…) are handled by the list and reader widgets, not as application accelerators, so text fields keep their letters (Stet's ADR-009 amendment found that app accelerators run before the focused widget). Ctrl shortcuts and the command palette (F1 / Ctrl+Shift+P) cover everything. Stet's audit applies, enforced by tests: nothing with SUPER; nothing that needs AltGr on US or Norwegian layouts (no `[ ] { } @ $ | \`); `#` and `/` (Shift+3 and Shift+7 on Norwegian) get unshifted alternatives (Delete, Ctrl+F); every F-key action has a non-F-key; avoid Ctrl+Space, Ctrl+Shift+U, Ctrl+Alt+Shift+U, Ctrl+Alt+P, Ctrl+Alt+F* and Alt+Tab.

**Consequence:** the keymap is data in `keys.toml`; the audit tests run in CI.

### ADR-017 amendment — the keymap as built in M1 (2026-10-04)

**Status:** built in M1 (ML-022, [report](m1/actions.md)); the keys DESIGN didn't list are proposed and wait for the user at the M1 gate.

- **One rule places every key:** keys with Ctrl or Alt, and F-keys, of App and Window actions are application accelerators (anywhere); every other key, and every key of List, Reader and Composer actions, is scoped to those widgets, after the focused widget. The message commands' Ctrl alternatives are therefore scoped too, and the composer keeps Ctrl+Z.
- **Sequences:** two-key sequences (`g` then `i`) wait 1.5 s.
- **The audit:** one function checks the defaults and `keys.toml` against xkeyboard-config's `us(basic)` and `no(basic)`. On Norwegian, `\` and `|` need no modifier.
- **Proposed keys:**
  - `h` for Keyboard Shortcuts (`?` and Ctrl+? need Shift on both layouts);
  - Ctrl+, for Preferences;
  - Ctrl+Q for Quit;
  - Ctrl+Alt+R and F5 for Refresh;
  - Ctrl+Shift+S and F9 for Toggle Sidebar.
- **Accepted** (user decision at the M1 gate, 2026-10-05, "Accept as proposed"): the proposed keys, and the scoped Ctrl alternatives of the message commands.

## ADR-018 — Logging and privacy (2026-10-04)

**Status:** Accepted — plan approval, 2026-10-04.

**Decision:** logs never contain message bodies, subjects, addresses, folder names, tokens or protocol traces; AUTH lines are always redacted. Development and spikes never put the user's email address or personal data in network requests.

**Consequence:** log statements use account ids and counts. A test greps captured logs from the testbed suites for addresses and secrets.

### ADR-018 amendment — third-party trace logs (2026-10-04)

S1 found that async-imap's trace logging prints the LOGIN password. The engine therefore caps the log level of third-party protocol crates (`async_imap`, `imap_proto`, `lettre`, `rustls`, `hickory`, `reqwest`, `zbus`) at `warn` whatever `RUST_LOG` asks for, and the log-grep test runs the testbed suites at trace level to prove no credential, token, address or folder name appears.

### ADR-018 amendment — the caps and autodetect's requests as built (M2, 2026-10-05)

Built in M2 ([m2/imap.md](m2/imap.md), [m2/autoconfig.md](m2/autoconfig.md)).
- **Caps:** `infrastructure::logging` holds `async_imap`, `imap_proto`, `lettre`, `rustls`, `tokio_rustls`, `hickory`, `reqwest` and `zbus` at `warn` with a `Targets` filter beside `RUST_LOG`'s, so no directive lifts them; they weren't capped before M2. The log-grep test (`engine/tests/log_grep.rs`) runs M2's testbed flows at trace level, with a control that proves async-imap's AUTHENTICATE line reaches the log without the caps.
- **Autodetect's requests:** the full address goes only to `https://autoconfig.<domain>/` as the `emailaddress` parameter; the ISPDB, MX, SRV, CNAME and the MX operator's autoconfig carry the domain only. HTTPS only. Redirects are followed at most three times, to the same host or inside the requested domain, and never when the target would carry the address to a host other than the one first asked (checked after percent-decoding). One hickory resolver serves DNS, HTTPS and the TLS checks, and a host with a non-public address is neither fetched nor probed; environment proxies aren't used, so that check always applies. The User-Agent is generic. The TLS check sends only CAPABILITY, STARTTLS and `EHLO [127.0.0.1]`. Error texts that might quote a URL are dropped, and logs carry only the winning source's name and the time. Measured 2026-10-05: 2,711 requests in five runs, 0 violations.

## ADR-019 — Corner radius follows Hyprland (2026-10-04)

**Status:** Proposed; the DESIGN gate decides.

**Decision:** `corner_radius = "auto" | <px>` in `config.toml`. `auto` reads `decoration:rounding` from `hyprctl -j getoption` at start and after theme changes, as omarchy-shell does (0 on stock Omarchy, 8 on this machine); outside Hyprland it is 0.

**Reason:** stock Omarchy is square (Stet chose square chrome), while the inspiration designs and this desktop are rounded. Following the compositor matches both.

**Alternative:** always square, as Stet. The DESIGN mock shows both.

### ADR-019 accepted — corners follow Hyprland (2026-10-04)

**Status:** Accepted — user decision at the DESIGN gate, 2026-10-04, approving the mock's layout with corners that follow Hyprland's rounding (`corner_radius = "auto"` by default, a px value in `config.toml` to override).

### ADR-019 amendment — small radius and round avatars (2026-10-04)

Built in M1 ([report](m1/theme.md)):
- rows, cards, the compose button and surfaces take `corner_radius`;
- chips, folder rows, icon buttons and controls take three quarters of it;
- avatars and circular buttons are round while corners are rounded, and square at 0.

`hyprctl` is skipped without `HYPRLAND_INSTANCE_SIGNATURE` and reread with every theme change.
