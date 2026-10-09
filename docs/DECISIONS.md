# Architecture Decisions

Synced: 2026-10-08

The repository docs are the canonical project memory ([ADR-012](#adr-012--repository-docs-canonical-notion-as-a-mirror-2026-10-04)). Dated entries describe the state at that date; newer entries and amendments take precedence. ADRs written before the naming decision call the app "Mail", its working name; it is **Sealwax** ([ADR-010](#adr-010-accepted--sealwax-2026-10-04)).

---

Use this page for decisions with meaningful long-term cost. Keep each entry short, dated and explicit about alternatives. Change an accepted decision with a dated `###` amendment under it; do not rewrite history. Status moves Proposed → Accepted when the user decides or a spike provides the evidence; say which. Library facts below were checked against crates.io and the crate sources on 2026-10-04 ([RESEARCH.md](RESEARCH.md#library-verification-2026-10-04)).

| ADR | Title | Status (2026-10-06) |
| --- | --- | --- |
| 001 | Standalone native application | Accepted (user request) |
| 002 | GTK4 + libadwaita + WebKitGTK 6 | Accepted (S2 and S7 evidence) |
| 003 | Layering with an engine crate; tokio only there | Proposed; store pattern measured in S3, desktop integration in S6, the application shell built in M1, the engine service and autodetect built in M2 |
| 004 | Follow the live Omarchy theme | Accepted (user request); built in M1 |
| 005 | SQLite with FTS5 plus a blob store | SQLite + FTS5 accepted (S3 evidence); schema additions from S1; schema v1 and the blob store built in M1; schema v2 (the account key) and folder sync state in M2; v3–v6 in M3; v7 (the op queue, the overlay in the rows) in M4; v8 (the senders whose images load) in M5; v9 (drafts, the outbox, contacts) in M6 |
| 006 | IMAP library and state-driven sync | Accepted (S1 evidence, GO): async-imap with a mandatory raw layer, UIDNEXT-window recovery; the connection layer built in M2; the sync core and actor in M3; the op queue's writes in M4; parts and originals on demand in M5 |
| 007 | SMTP, MIME and privacy defaults | Proposed; S7 confirmed the MIME side with amendments; the SMTP login check built in M2; interop checked on the testbed, rendering in real clients the user's own test (user decision, M6 start); the builder, submission and APPEND built in M6 |
| 008 | OAuth2 and secrets | OAuth for Microsoft in 1.0; Sign in with Google deferred to 1.x (user decisions); mechanism accepted on S5's evidence (GO, 2026-10-05); secrets keyed by the account key in M2 |
| 009 | Settings in files: config.toml, keys.toml, accounts.toml | Proposed; config.toml and keys.toml built in M1, accounts.toml in M2, its signatures and identities in M6 |
| 010 | Name and app-id | Accepted (user decision): **Sealwax**, `io.github.pixdevsapps.Sealwax`; code renamed 2026-10-04 |
| 011 | Reading pane: approach A with hardened WebKit | Approach A accepted (S2 evidence) with amendments; memory judged as PSS with a live-view cap (user decision); built in M1; conversations, links, remote content per sender and print in M5; memory measured live in M5, NVIDIA's mappings counted in the GPU budget (user decision, M5 gate) |
| 012 | Repository docs canonical, Notion as a mirror | Accepted (user decision) |
| 013 | Lean 1.0 scope | Accepted (plan approval); amended at the M0 gate (background mode P0) |
| 014 | WYSIWYG composer in an editable WebKit view | Format accepted (user decision); mechanism confirmed by S7 with conditions (no acceleration); the user's fcitx5 check passed the latency bar (2026-10-06); the composer in its own window (user decision, M6 start); built in M6, its live typing and scrolling measured |
| 015 | Op queue: optimistic overlay, Undo, at-most-once send | Accepted (user decisions): triage at the M4 gate, the at-most-once send and the outbox at the M6 gate |
| 016 | Conversation threading | Proposed; alias table and merges confirmed in S3, inputs in S1; built in M1; the conversation view is the reader's, the list keeps message rows (user decision, M5 start); built in M5 |
| 017 | Keymap: scoped one-key shortcuts under Omarchy rules | Built in M1; new keys wait for the user; M1's proposed keys accepted (M1 gate), the list's own M4 keys accepted (M4 gate), M5's keys accepted (M5 gate), M6's keys accepted (M6 gate) |
| 018 | Logging and privacy | Accepted (plan approval); amended after S1 (third-party trace logs); the caps and autodetect's requests built in M2; the HTML parser capped in M6 |
| 019 | Corner radius follows Hyprland | Accepted (user decision at the DESIGN gate); built in M1 |
| 020 | Folders and rules: Sieve run by Sealwax | Accepted (user decision, 2026-10-09); the Sieve subset (A2) and the rules on new mail (B1) built; folders and the app being built |

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

### ADR-003 amendment — resume and network as built (ML-048, M3, 2026-10-05)

Built in M3 ([report](m3/status.md)). The app watches logind's `PrepareForSleep` (a read-only subscription on the system bus, connected asynchronously) and GIO's network monitor, both only for the engine it starts, and sends `Command::Reconnect` after a resume and when the network comes back. GIO picked `GNetworkMonitorNM` here, which connects to the system bus synchronously, so the monitor is first created on a thread of its own; GIO keeps it as the process's default and emits its signals on the main context. The engine acts on `Reconnect` from M3's phase B.

### ADR-003 amendment — the engine's actors (M3 phase B, 2026-10-05)

Built in M3 ([report](m3/actor.md)).
- **Tasks:** the runtime keeps its 2 workers. Each account is one task (the command connection's loop) with its IDLE task and, while downloading, a body task.
- **Commands and stop:** commands reach an account's actor over a channel; `Reconnect` and stop arrive through a `watch`, so every network wait is abandoned at once.
- **Events:** statuses are sent only when they change, and `SyncProgress` at most 10 a second per account, always with the last batch's numbers. The store's change events stay exact, as before; a pass that changes nothing publishes nothing. This is how ML-040's "coalesced to ≤ 10/s" was read, a proposal for the user.
- **Stopping:** `Engine::stop` LOGOUTs within 300 ms per connection and aborts after 1.2 s.
- **Reconnecting:** on resume and when the network comes back the app sends `Reconnect`, which drops every connection of every account and connects again at once (S6).

### ADR-003 amendment — background mode as built (M7, 2026-10-07)

Built in M7 ([report](m7/background.md)); accepted at the M7 gate (the user's delegation, 2026-10-07).
- **The hold** follows `[background] enabled` (on by default, [ADR-013](#adr-013-amendment--background-mode-on-by-default-notifications-show-sender-and-subject-m7-start-2026-10-06)) and changes as soon as `config.toml` does; with it off and no window left, Sealwax quits through the one quit path. `--background` holds until a main window has shown, even with background mode off, so an autostart entry still means "sync from login".
- **The engine without a window:** when Sealwax starts with no main window (`--background`, or a composer opened alone), the engine starts once the store and the settings are read, with the desktop watch (resume and network), and only once: a window shown later never starts a second engine. Start → engine running with its accounts p50 47.4 ms (release, headless).
- **A closed window is freed** with its list and reader: the window's handlers look the window and the list up through `AppState`, and no closure the window owns keeps a clone of `Window`, `MessageList` or `Reader` (`background-memory` and `assert-freed` check it). Before the fix every close and reopen kept about 7.5 MiB (debug: 155.7 → 381.2 MiB over 30 cycles); after it memory stays flat. A window built again creates its reader after its own first frame; activation from background mode → first frame p50 63.8 ms, p95 67.1 ms (release, headless).
- **Opt-in autostart** (PRD D6's P1 part) is Start Sealwax at Login: an XDG autostart entry running `sealwax --background`, written only after the user agrees; Omarchy runs XDG autostart entries through uwsm and systemd's generator (read on this machine, 2026-10-06).

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

### ADR-004 amendment — account colours (2026-10-08)

**Status:** Accepted — the user's UI tweak, 2026-10-08 ("have an option to choose a beautiful muted color to that account").

An account may have a colour the user picks, the one colour in the chrome that doesn't come from the palette. Eight named colours (rose, clay, olive, sage, teal, slate, iris, mauve) are fixed OKLCH hues 35–55° apart; the theme layer sets their lightness and chroma per mode (0.74 and 0.065 on dark themes, 0.52 and 0.075 on light ones, as muted as the live theme's own colours at about 0.03–0.07) and moves the lightness away from the background until each reads at 4.5:1 on the window and the sidebar, in every stock theme (`domain/tests/theme.rs`). OKLCH rather than HSL keeps the eight equally light; palette positions couldn't give named hues (matte-black's `green` is amber). The colours reach CSS as `--sw-account-<name>-color`; the sidebar's heading name and synced dot take them, never the warning and error dots.

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

### ADR-005 amendment — schema v3 to v5, All Inboxes pages and downloaded bodies (M3, 2026-10-05)

Built in M3's phase A by three agents, renumbered at the lead's merge ([sync](m3/sync.md), [bodies](m3/bodies.md), [threading](m3/threading.md)).
- **v3** adds `messages.thread_index`: the 22-byte conversation block of an Outlook `Thread-Index` (ADR-016). Rows from before it have none.
- **v4** adds `folders.below_floor`, the server's messages under a folder's floor when last counted. It is a hint for the count check and the STATUS skip, recounted by the UID diff, never a reason to store or drop a row. A floor of 1 means nothing older is left; v4 sets it for every folder M2 synced completely.
- **v5** adds `bodies(blob_key, size, used)` with an index on `(used, blob_key)`, a trigger that drops a body with the last message that refers to it, and `message_parts.section` (the IMAP section of an attachment fetched on demand).
- **Sync writes:** a pass's changes go to the store by UID in one transaction with one exact change event (`sync_changes`); an account's folder list is replaced in one transaction, and folders gone from the server leave with their messages (`replace_folders`).
- **Body upgrade:** a downloaded body upgrades its header-only row in one transaction after its blob is durable. The row keeps its id and place and gets the blob key, `body_state` `Full` or `Text` (a large message rebuilt from its text parts), the parts with their sections, the snippet and a full-text row with the whole text; one change event lists it as changed. A full-text batch skips a message whose body already indexed it.
- **Disk cap:** the bodies used longest ago are evicted back to headers and preview (rows, snippets and full text stay), 200 per transaction, without a change event; their blobs are deleted after the commit.
- **All Inboxes pages:** a view over several folders merges the folders' index walks in one compound SELECT (no sort; only `offset + limit` rows read) for every filter. An unfiltered view whose folders hold at least half the store's messages walks the global date index instead. On 3–5 fixture accounts with 90,000–100,000 messages, deep filtered pages went from 0.7–5.4 ms warm (up to 50 ms cold) to 0.3–1.7 ms warm (≤ 17.7 ms cold). Opening All Inboxes took ≤ 32.3 ms cold, and one new message's diff p99 ≤ 5.2 ms.

### ADR-005 amendment — schema v6: the server's counts for windowed folders (M3 gate, 2026-10-06)

Built at the user's choice ("Show server counts"; [report](m3/counts.md)).
- **Migration v6** adds `folders.below_unread`, the server's unread messages under a windowed folder's floor.
- **The counts the app sees** (the folder list and every change event's folder counts) are the store's own plus `below_floor` and `below_unread`. List views keep counting the rows the store holds.
- **Where they come from:** the account pass sets both from each windowed folder's STATUS, in one transaction against the store's own rows (`count_below`): MESSAGES less the stored messages, and UNSEEN less the stored unread, at most the messages below. A STATUS older than the store is ignored (another UIDVALIDITY, or a lower UIDNEXT or HIGHESTMODSEQ). A change event goes out only when the counts change.
- **Older mail** moves its messages and unread from below the floor into the store in one transaction, so the counts shown stay.
- **They remain hints:** never a reason to store or drop a row, recounted by the UID diff and the next STATUS. A new folder keeps showing STATUS counts until its first sync; a complete folder has nothing below.

### ADR-005 amendment — schema v7: the op queue and hidden rows (M4, 2026-10-06)

Built in M4's phase A ([report](m4/ops-store.md)).
- **The overlay lives in the rows:** `messages.flags` is what the user sees, the server's flags with pending ops applied, plus bit 256 while a pending move or expunge takes the message out of its folder. `messages.server_flags` holds the server's flags where they differ (NULL otherwise) and is what the sync engine reads and writes.
- **Hidden rows:** a row with bit 256 or `\Deleted` (whoever set it) is in no view, count or search. The five list indexes are partial on that and hold `flags`, so their walks stay covering.
- **New tables:** `op_batches`, `ops` (one change to one message, or to the server's unread below a folder's floor) and `op_refusals`.
- **Measured at 100,000 messages** (release): S3's bars hold (folder open ≤ 12.8 ms cold, the one-insert diff p99 4.66 ms, pages as before); a flag change costs about 0.4 ms more at p99 because the date indexes hold `flags`; the migration takes about 1 s on the first start and leaves the file about 7 MiB larger.
- **Rejected:** an overlay table beside unchanged rows. Its anti-join made a deep page and every position count 6–7 ms slower, a frame's budget at 144 Hz on its own.

### ADR-005 amendment — schema v8: the senders whose images load (M5, 2026-10-06)

Built as [m5/files.md](m5/files.md) records. Migration v8 adds `remote_senders(address PRIMARY KEY, added) WITHOUT ROWID`: lower-case addresses whose remote images load without asking (ML-058), with when each was added. The list is the user's, not an account's: it stays when an account is removed. Its writes bump no store version and send no change event. The conversation view (ML-057) added no migration: `Store::conversation` walks v1's `messages_thread (thread_id, date)` index (≤ 1.66 ms cold for 150 messages at 100,000).

### ADR-005 amendment — schema v9: drafts, the outbox and contacts (M6, 2026-10-06)

Built in M6's phase A ([report](m6/drafts-store.md)). Migration v9 adds:
- `drafts` (the composer's draft, its revision, whether its window is open, its Drafts-folder copy and the upload under way) and `draft_parts`;
- `draft_blobs` (every blob a draft ever named), which the disk cap's eviction and every orphan sweep leave alone, as they do the outbox's built messages;
- `outbox` (one send per draft: queued, sending, uncertain, sent; the built bytes, the envelope and the Sent copy's mark) and `draft_deletions` (server copies the store holds no row for);
- `contacts` and `contact_terms`, filled from the stored messages in the migration's transaction (0.74–0.82 s at 100,000 messages, +0.42 MiB) and kept on ingest (header transactions about 20–35 % slower).

Draft saves bump no version and send no event (p99 1.24 ms for a 200 KB body at 100,000 messages); outbox writes send one with `ChangeEvent::outbox`. Contacts are found by prefixes of the address, its domain and the name's words, folded (p95 ≤ 0.6 ms with the fixture's contacts, ≤ 13.2 ms with 50,000 more). `Store::reply_draft` (phase B) finds a quick reply's closed draft by the message it answers. A draft is uploaded to the Drafts folder 30 s after its last save while its window is open and 10 s after it when none is (phase B's integration: the quick reply keeps its draft closed while the user types).

### ADR-005 amendment — schema v10: search (M7, 2026-10-07)

Built in M7 ([report](m7/search.md)); its proposals accepted at the M7 gate (the user's delegation, 2026-10-07).
- **The query language** is pure code in `domain::search`: `parse`, and `parse_typed` for text as typed, where the last word is the start of a word until a space follows it (never inside quotes or for an excluded word). `from:`, `to:`, `subject:`, `has:attachment`, `is:unread|read|starred`, `before:`/`after:` (local days; six forms), quoted values, `-word`, `-"phrase"`, `-has:`, `-is:` and negated dates; prefixes need two letters or digits.
- **Plans:** FTS first when a term has at most 200 matches, date first otherwise; a query without words walks a date index; partial indexes within one folder. **Date first is kept** over S3's date-keyed rowid: measured faster warm, but 54 % larger and reachable only through a fresh index, which would lose the preview words of messages whose body isn't stored. "Every folder" leaves out Trash, Junk and detached accounts.
- **Folding, corrected:** `unicode61 remove_diacritics 2` folds å, but neither ø nor æ (S3's amendment said it folded ø). The index and every query store æ as "ae" and ø as "o", and queries also ask for the old spelling while old entries remain; `aa` and `oe` spellings stay unfolded. Attachment names are indexed in the body column; the 256 KiB body cap stays.
- **Schema v10** builds the date index again with `has_attachments` (the slowest combined shape went from 108 to 49 ms cold) and schedules one background pass over the index, through the full-text lane that yields to interactive writes: 24.7–25.0 s at 100,000 messages, a flag change meanwhile p95 ≤ 10.4 ms. Opening and migrating a v9 store of 100,000 took 84–113 ms (and 1,859 ms once, right after an unsynced 1.1 GB copy). v0.3.0 can't open a v10 store.
- **Rebuild Search Index** works in place, a few messages a transaction with automatic merging off and a merge at the end; it resumes after a restart, keeps the entry of a message whose body the store doesn't hold, and answers a second request with the first one's reply: 19.3–25.4 s at 100,000.
- **Measured** on the merged build (release, 2026-10-07, `bench-search`): the first 50 results of 38 shapes at 100,000 messages, worst p95 82.3 and 88.5 ms cold, 24.4 and 24.5 ms warm, against the 100 ms bar; the agent's runs 66.2–74.8 ms cold.
- `Query`, `Term` and the store's FTS expression print counts or lengths in `Debug` (ADR-018).

### ADR-005 amendment — schema v11: POP3 accounts (M8 phase A, 2026-10-07)

Built in M8's phase A ([report](m8/pop3-accounts.md)); its proposals wait for the M8 gate. `pop3_accounts` marks an account as POP3 and records its first complete pass; `pop3_uidls` keeps each downloaded UIDL with the row it became (`ON DELETE SET NULL`), when it was downloaded, whether whole or header only, and when Sealwax deleted it on the server; a message and its UIDL are stored in one transaction, so a crash neither loses nor repeats one. `blob_deletions` hands the POP3 actor the raw messages that ops done at once left behind. A POP3 account's local folders (Inbox, Sent, Drafts, Junk, Trash, Archive) are complete from the start (`sync_floor` 1); its mail is never in `bodies`, so the disk cap never evicts it; `replace_folders` refuses an IMAP folder list for it, so a protocol changed in `accounts.toml` can't wipe local mail. The migration only creates tables.

### ADR-005 amendment — the HTML read for the search text (M9, 2026-10-07)

Built on the user's "HTML size cap". The search text and snippet of an HTML-only message come from its HTML's first 256 KiB (`message::HTML_PARSE_LIMIT`; 1 MiB before): html5ever's tree builder is quadratic in the nesting depth (the review's 990,000-byte nested lists: 19.8 s at sync on an idle machine, now 7.4 s). A reply or forward to an original with more HTML quotes its text (ADR-014's composer). The body text kept for the index stays 256 KiB.

### ADR-005 amendment — schema v12: building marks (M9, 2026-10-07)

`outbox.building` and `drafts.upload_building` record that a send's message or a draft's Drafts-folder copy is being built, until the build returns ([ADR-015 amendment](#adr-015-amendment--a-build-that-stops-sealwax-isnt-run-again-m9-2026-10-07)); v11 stores migrate with both clear, and a newer store is refused as before ([report](m9/depth.md#2-the-outboxs-crash-loop-guard)).

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

### ADR-006 amendment — the sync core and body downloads as built (M3 phase A, 2026-10-05)

Built in M3 ([sync](m3/sync.md), [bodies](m3/bodies.md)) on S1's model and M2's connection, first sync and store.
- **Folder pass:** EXAMINE (CONDSTORE, or QRESYNC enabled), with a view kept current from every EXISTS, EXPUNGE and VANISHED; a UIDVALIDITY change resets the folder; an unfinished first sync continues. Then:
  - the one-round-trip fast path;
  - new UIDs (`UID FETCH n:*`, or SEARCH and batches of 200);
  - flags and keywords over the folder's window (`CHANGEDSINCE m VANISHED`, `CHANGEDSINCE m`, or a full scan);
  - the count check against EXISTS, with a UID diff as the fallback.

  Each step is one store transaction. VANISHED UIDs are matched against stored ones only.
- **Account pass:** UNSELECT pipelined with `LIST … RETURN (SPECIAL-USE STATUS (MESSAGES UNSEEN UIDNEXT UIDVALIDITY HIGHESTMODSEQ))`, else LIST and pipelined STATUS. The store's folder list is replaced by the server's; a folder pass runs only where STATUS differs from the store; a NO skips the folder until the next LIST. INBOX on the IDLE connection keeps a STATUS backstop. Passes over one folder are serialized by a per-folder async lock shared by an account's connections.
- **Which folders sync** (a proposal for the user): INBOX and the role folders, and any folder synced before (opened once); other folders show STATUS counts; `\All` never syncs by itself.
- **The first-sync window** (a proposal for the user): the messages of the last 90 days, at most 5,000 and at least 200, UID-contiguous from a floor; older mail loads 200 at a time below it.
- **Body downloads:** EXAMINE and a UIDVALIDITY check, then only `BODY.PEEK`. Up to 10 MiB by RFC822.SIZE the whole message, several per FETCH (≤ 50, ≤ 16 MiB). Larger ones by BODYSTRUCTURE: the header, every part's MIME header and the text/plain and text/html parts, rebuilt in the message's own MIME structure with attachments empty and their sections recorded for on-demand fetches. A missing UID or a new UIDVALIDITY answers `Gone`.
- **Verified on the testbed:**
  - 0 divergences in 40 of 40 randomized runs (Dovecot with and without QRESYNC, IMAP4rev1 Dovecot, Stalwart; polling only, the IDLE connection is phase B's);
  - mid-FETCH cuts of a first sync, a flag fetch and new mail lose and duplicate nothing;
  - a no-change account pass of the 10,000-message `perf` account took p50 5.9–7.3 ms on Dovecot, 0.3–0.7 ms on Stalwart and 8.8–35.1 ms with IMAP4rev1's full flag scans (worst 41.3 ms), within S1's bars;
  - 1 KB, 5 MB and 12 MB body fixtures stored within 82–109 ms of connecting on each server, the 12 MB one as 810 bytes, with nothing marked `\Seen`.

### ADR-006 amendment — the per-account actor as built (M3 phase B, 2026-10-05)

Built in M3 ([report](m3/actor.md)).
- **At most three connections per account:**
  - the command connection: account passes, the opened folder, older mail, and INBOX where the server lacks IDLE;
  - the IDLE connection on INBOX;
  - a body connection, made on demand and closed after 60 s without work.

  Passes over one folder never overlap.
- **IDLE:** a pass, then NOOP (another pass on a change), then IDLE until a notice, 25 minutes or a reconnect; a pass on every wake. IDLE and DONE have 30 s timeouts; the end of the stream is a lost connection, made again at once.
- **Schedule** (proposals for the user): an account pass every 2 minutes, with INBOX's STATUS as the backstop; the opened folder at once and every 30 s; INBOX every 60 s without IDLE.
- **Resilience:**
  - NOOP (10 s timeout) before reusing a connection unused for 30 s;
  - reconnect at once after a drop, then a backoff from 1 s doubling to 5 minutes, jittered ×½–1;
  - `offline` only when connecting fails;
  - a refused login or a changed certificate stops the retries until `Reconnect`, a new password or a sign-in.
- **Verified on the testbed:**
  - new mail in the store p50 500 ms after APPEND on Dovecot (its own notification delay) and 6 ms on Stalwart;
  - cuts while idling and mid-FETCH, and a black hole lifted with `Reconnect` (all stored within 24 ms), lose and duplicate nothing;
  - 0 divergences in 80 of 80 randomized runs through the engine (four server variants, bursts and paced changes);
  - a 30-minute soak (four accounts, 2,548 changes, six faults) matched the servers at every check.
- **Not built:** a limit on IDLE's silence. A silently dead IDLE connection without `Reconnect` lasts until TCP keepalive ends it (about two minutes on a real network), meanwhile the 2-minute STATUS backstop keeps INBOX current.

### ADR-006 amendment — M3 gate decisions (2026-10-06)

**Status:** the proposals of M3's sync core and actor are accepted by the user ("accept the defaults", 2026-10-06), and a windowed folder shows the server's counts ("Show server counts").
- **Folders that sync by themselves:** INBOX, Sent, Drafts, Archive, Junk and Trash, and any folder opened once; other folders show STATUS counts until opened; `\All` never.
- **The first-sync window:** the newest 90 days, at most 5,000 and at least 200 messages per folder; older mail 200 at a time.
- **The actor:**
  - an account pass every 2 minutes, with INBOX's STATUS as the backstop;
  - the opened folder at once and every 30 s; INBOX every 60 s without IDLE;
  - IDLE re-issued every 25 minutes;
  - NOOP before reusing a connection unused for 30 s;
  - reconnect at once, then a backoff from 1 s to 5 minutes, jittered;
  - progress at most 10 events a second per account.
- **Counts:** a folder with older mail left on the server shows the server's totals: the store's own counts plus the messages and unread messages under the floor, from the account pass's STATUS (ADR-005, schema v6).

### ADR-006 amendment — the op queue's writes as built (M4 phase A, 2026-10-06)

Built in M4 ([report](m4/imap-writes.md)) on S1's raw layer, without the store.
- **Every write SELECTs its folder read-write** and checks its UIDVALIDITY first (a different one is reported for re-targeting); `[READ-ONLY]` is refused. The passes keep EXAMINEing. CLOSE and a plain EXPUNGE are never sent.
- **Flags:** `UID STORE +FLAGS.SILENT` and `-FLAGS.SILENT` in UID sets of at most 1,000 UIDs and 4,000 bytes, pipelined; keywords only where PERMANENTFLAGS keeps them (all three testbed servers list `\*`).
- **Moves:** UID MOVE where offered; else UID COPY, `\Deleted` and UID EXPUNGE with UIDPLUS; without UIDPLUS the source copies stay `\Deleted` (and hidden). COPYUID is read from the tagged or an untagged OK and **checked against the target's Message-IDs** before it is reported: Stalwart 0.16.24 writes both sets in ascending order whatever order it copied in, which would put the wrong body on a moved row. The check costs two round trips per move. A quirk switch (`ConnectOptions::moves`) takes the COPY path on servers with MOVE.
- **Refusals** keep their response code for classification only: retry (a lost connection, timeout, BYE, `[UNAVAILABLE]`, `[INUSE]`, a NO without a code), refuse (`[TRYCREATE]`, `[NONEXISTENT]`, `[READ-ONLY]`, `[CANNOT]`, `[NOPERM]`, BAD), quota full (`[OVERQUOTA]`).
- **Server behaviour found:** Stalwart keeps a message once across mailboxes (a COPY into a folder that holds it adds nothing; flags are shared between the copies). IMAP4rev1-only Dovecot answers COPY with COPYUID without advertising UIDPLUS.
- **Measured** (release, testbed): flags on 500 messages p50 11.9–14.0 ms; a move of 500 p50 32.1 ms (Stalwart, MOVE), 60.9 ms (Dovecot, MOVE), 68.4 ms (IMAP4rev1 Dovecot, COPY).

### ADR-006 amendment — new mail refreshes the flags of rows already held (M4 phase B, 2026-10-06)

Found by M4's randomized triage suite ([report](m4/runner.md#the-flag-refresh-fix)): the folder pass's new-mail fetch also updates the flags and keywords of fetched UIDs the store already holds. These are rows the op runner re-homed into a target above its stored UIDNEXT, which the flag scan doesn't reach; before the fix, a flag another client set just before the move never reached the store.

### ADR-006 amendment — parts and originals on demand (M5, 2026-10-06)

Built as [m5/files.md](m5/files.md) records. `Command::FetchPart` fetches `BODY.PEEK[<section>.MIME]` and `BODY.PEEK[<section>]` by the section the store recorded, and `Command::FetchOriginal` fetches `BODY.PEEK[]`, on the account's body connection after EXAMINE and the UIDVALIDITY check, ahead of body batches, one at a time, retried once after a cut. Nothing is stored and nothing is marked `\Seen`; a vanished UID or a new UIDVALIDITY is `Gone`. The reader uses them for a large message stored as its text parts: its attachments, its inline images (served as the type the stored message declares) and Show Original and Save as .eml. On the testbed a 12 MB original took 25–63 ms and its 4.4 MB attachment 28–43 ms, byte for byte.

### ADR-006 amendment — new mail reported per committed step (M7, 2026-10-07)

Built in M7 ([report](m7/notifications.md)). `Event::NewMail { account, folder, messages }` is sent from the account's actor as each step of a folder pass commits (not once per pass, which a cut after a commit would lose), for the rows at or above the UIDNEXT the store held, the UID diff's included. Never for a first sync (also after a UIDVALIDITY reset), older mail (`LoadOlder`), or the account's own moves: re-homed rows are never inserted again, and the actor's record of its moves (10 minutes) keeps out a move whose new UID stayed unknown. Mail that arrived while Sealwax was stopped is reported at the next start. Testbed, release: APPEND OK → `NewMail` median 1.7 ms on Stalwart and 497.8 ms on Dovecot (its own IDLE delay); end to end in background mode with no window, APPEND OK → the notification 10–23 ms on Stalwart (5 runs).

### ADR-006 amendment — POP3: the client and the account's actor (M8 phase A, 2026-10-07)

Built in M8's phase A ([client](m8/pop3-client.md), [accounts](m8/pop3-accounts.md)); the proposals wait for the M8 gate.
- **The client** is Sealwax's own code over `net`'s sockets and TLS, no crate. Implicit TLS, or STLS right after the greeting with nothing else in plaintext; a refused STLS, a failed handshake or plaintext after STLS's `+OK` refuses the account. CAPA over TLS, before and after the login. SASL PLAIN, else USER/PASS, XOAUTH2 for a token (scripted only). RFC 2449 and 3206 codes classify refusals (the text dropped); `[IN-USE]`, `[LOGIN-DELAY]` and `[SYS/TEMP]` are `Busy`. UIDL is required (a server without it is refused at setup, `LoginFailure::NoUidl`); messages sharing a UIDL are left alone. RETR keeps the bytes and line ends as sent, up to 100 MiB; a larger message's header comes by TOP. Timeouts: 30 s to connect and log in, 60 s a reply, RETR 60 s plus its size at 16 KiB/s. Deletions happen only at a successful QUIT; a QUIT without its answer is unknown (on the testbed it had always deleted).
- **The actor:** one session per pass, at start, on `Reconnect`, on `Command::CheckMail` (Refresh, M8; IMAP accounts run an account pass for it) and every 5 minutes, never sooner than LOGIN-DELAY; newest first, stored 25 messages or 500 ms at a time; `NewMail` only after the account's first complete pass; the policy's deletions only of what the store recorded whole, by this session's numbers, never of oversized or shared-UIDL messages; a QUIT without its answer settled by the next listing; a busy maildrop changes no status; EXPIRE logged; the outbox runs beside the passes. POP3 accounts are password accounts in 1.0 (Microsoft would need a separate POP scope).

### ADR-006 amendment — a crashed actor started again (M9, 2026-10-07)

Built on the user's "Restart a crashed sync" (2026-10-07; [report](m9/hardening.md#2-restarting-a-crashed-sync-f15)). An account's actor, IMAP or POP3, runs under a supervisor in its task: one that ends in a panic (imap-proto 0.17.0 panics on a reply code followed at once by a non-ASCII character, F15) is started again after the backoff a second failed connection waits (1 s doubling to 5 minutes, jittered; a run longer than 5 minutes starts the count over), the account showing offline meanwhile; one that ends on a stop (removal, quit, `accounts.toml`) stays ended, also when the stop comes during the wait; `Reconnect` restarts it at once. The IDLE connection goes with its actor. The log names the account's id and the count, never the panic's message. Verified against the fake IMAP server with imap-proto's real panic and with injected panics (IMAP's supervisor, POP3's actor); not on a real server.

### ADR-006 amendment — other mail programs' housekeeping folders left out (2026-10-09)

Built on the user's "hide Microsoft's housekeeping folders and the mailspring folders altogether if they are not needed, the mailspring folders does not show up in any other mail client I use" (2026-10-09). The folder list leaves out, before roles are assigned (`imap::hidden`): **Outlook's sync logs**, a top-level "Sync Issues" or "Synkroniseringsfeil" and every folder under it, on any server (Outlook on Windows writes them to Exchange mailboxes); **Notes and Outbox** at the top of a mailbox on Microsoft's servers (an IMAP host ending in `office365.com` or `outlook.com`), Outlook's sticky notes and the mail Microsoft's own apps haven't sent yet, where elsewhere such a folder is the user's; **Mailspring's folder**, "Mailspring" at the top or under INBOX, and the folders under it, each while STATUS says it holds no message (or it is `\Noselect`): mail Mailspring snoozes there shows the folder again, and a folder whose count the server didn't give stays. Left out means never stored, synced, counted, searched or offered as a move target; a store that held them loses them at the next account pass, as when a folder leaves the server (the store's own copies only). Names are matched case-insensitively on the decoded name; only the names seen are in the table (the English ones and the Norwegian "Synkroniseringsfeil" from the user's Outlook.com account), and another language's name joins when seen. There is no setting to show them; the server still has them. Verified with unit tests and against the fake IMAP server on all three flavours (a store holding the folders loses them; a snoozed message brings Mailspring's back); not run against a real server.

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

### ADR-007 amendment — interop checked on the testbed (M6 start, 2026-10-06)

**Status:** user decision at M6's start ("Mailpit + your test (Recommended)", 2026-10-06). S7's interop check, rendering Sealwax's mail in Gmail web, Outlook web and Thunderbird, needed test accounts and send approvals; real accounts became the user's own test once the app is finished (2026-10-05), and Thunderbird isn't installed here. So M6's exit criterion is met on the testbed: every message Sealwax sends in the testbed suites is caught by Mailpit (or delivered by Stalwart's submission port), round-tripped through mail-parser, and scanned for a Bcc header and the host name. Rendering in the three clients moves to the user's own test (a P1 follow-up in PLAN.md).

### ADR-007 amendment — the MIME builder and submission as built (M6, 2026-10-06)

Built in M6 ([compose rules](m6/compose-domain.md), [sending](m6/send.md)).
- **The builder** (`domain::compose::mime`, from S7): three copies of a draft: the transmitted copy (never a Bcc header; `To: "undisclosed-recipients":;` when only Bcc), the Sent copy (the transmitted bytes plus one `Bcc:` field), and the Drafts folder's copy (Bcc kept). The envelope lists To, Cc and Bcc once each, in ASCII; international domains go as punycode, non-ASCII local parts are refused (no SMTPUTF8 in 1.0). The Message-ID is the draft's, made from 128 random bits on the From's domain, and the builder refuses one on another domain, so a change of sender to another domain makes a new one. No User-Agent, X-Mailer, Sender or Received field; `Date:` in UTC. Replies carry In-Reply-To and up to 20 References (the root and the 19 most recent); forwards carry neither. `estimate` gives the encoded size for the SIZE check without reading the parts.
- **Submission** (`engine::smtp::submit`): two steps, MAIL FROM and every RCPT TO, then (after the store's durable start, ADR-015) DATA; Sealwax answers XOAUTH2 itself (lettre can't answer the error challenge) and refreshes a refused token once; EHLO SIZE is checked before MAIL and sent as `SIZE=`. One refused recipient refuses the whole send (RSET, nothing sent). Every reply is definite; a connection lost before the message's bytes go out is retried, one lost while they go out or before the reply to their end is uncertain.
- **APPEND** (`engine::imap::write::append`) takes the folder's mark first; a retry reads the Message-IDs from the mark up (S1: Stalwart never matches `SEARCH HEADER Message-ID`) and appends only what isn't there; a draft's new revision is appended before the old copy is removed (left `\Deleted` without UIDPLUS).
- **Measured on the testbed:** 240 cut APPENDs on three servers, 0 duplicates, 0 lost; S7's six fixtures arrived byte for byte at Stalwart (14 deliveries) and Mailpit (6).

### ADR-007 amendment — the STARTTLS stream (M9, 2026-10-07)

M9's security review found STARTTLS response injection in submission: lettre 0.11.23 upgrades the stream beneath its own read buffer, so bytes a man in the middle sent right after STARTTLS's `220` were read after the handshake as the server's answers (the scripted test's login check passed while every reply was read one step behind; the password still went over TLS to the real server only). Sealwax now hands lettre a plaintext stream of its own (`engine::smtp::Plain`) that gives at most one line per read until the handshake starts and fails the handshake if anything was read ahead; the login check then fails with `NoTls` and nothing goes over TLS. IMAP and POP3 were already safe (an IMAP injection test added). Checked against scripted servers on the branch ([report](m9/security.md#findings)); the testbed's STARTTLS servers after the merge ([PLAN.md](PLAN.md#m9-phase-a-the-security-review-and-fuzzing-merged--2026-10-07)).

### ADR-007 amendment — the text part of deeply nested HTML (M9, 2026-10-07)

The plain part is html2text's text of the HTML, rendered from a tree at most 1,024 levels deep; HTML whose tags nest deeper than 512 levels (WebKit's parser limit), counted in one pass with html5ever's tokenizer (`domain::html_depth`), gets mail-parser's one-pass text instead. html2text dropped an unfinished render tree recursively and overflowed a 2 MiB thread on 14,563 nested lists, which aborted Sealwax while it built a reply (found by M9's hardening, confirmed end to end by the depth branch's control run). The same rule gives the search text and the snippet (ADR-005), and the cleaner (ADR-014) leaves out start tags deeper than 512, keeping their content. Mail within the limit is converted and cleaned exactly as before (20,061 inputs compared; [report](m9/depth.md)).

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

### ADR-008 amendment — Microsoft sign-in in the app (ML-032, 2026-10-05)

Built on S5's code ([engine report](m2/oauth-engine.md), [app report](m2/oauth-app.md)); checked live on the S5 account on 2026-10-05 ([TESTING.md](TESTING.md#ml-032-microsoft-sign-in-in-the-app--2026-10-05)).
- **Flow:** our own PKCE (S256) authorization-code flow in the system browser, at Microsoft's `common` endpoints. The redirect is `http://localhost:<port>`, with listeners on 127.0.0.1 and ::1 that ignore requests without the right state. `prompt=select_account` and no `login_hint`; 5 minutes for the browser. Scopes: `https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid profile`.
- **The XOAUTH2 username is the ID token's `preferred_username`**, for the sign-in's checks, `accounts.toml` and every sync. The ID token comes from the token endpoint over HTTPS with the platform verifier, which stands in for its signature (OpenID Connect Core 3.1.3.7). The engine checks its issuer against its own tenant, its audience, its expiry, and that it names a user. `tid` picks Outlook.com's or Microsoft 365's servers for a new account.
- **A sign-in checks IMAP and SMTP** with the new token before it answers, and keeps its tokens in memory for 30 minutes for the one add that names it.
- **Tokens:** refresh tokens only in the Secret Service, replaced on every rotation and on a re-sign-in. Access tokens only in memory, refreshed with 5 minutes left or once after a server refuses one. A refused refresh means "sign in again" (`OAuthRequired`); a re-sign-in must be the account's own user.
- **Errors** keep only Microsoft's AADSTS number. Consent not given (`access_denied`, `consent_required`, AADSTS 65001, 65004, 90094, 90095) is `SignInDeclined`.
- **The client ID:** `config.toml`'s, else the build's `SEALWAX_MICROSOFT_CLIENT_ID`. The engine gets it at start and through `Command::MicrosoftClient` after every change to `config.toml`; without one, sign-in is unavailable.
- **The app** opens the page with `gtk::UriLauncher`, can open it again while waiting, and cancels with `CancelSignIn`. A banner asks an account whose refresh was refused to sign in again.

### ADR-008 amendment — secrets keyed by the account key (M2, 2026-10-05)

Built in M2 ([m2/accounts.md](m2/accounts.md)).
- Keyring items are keyed by the account key from `accounts.toml`.
- Adding an account stores the secret before the entry is written (the commit point); removing one deletes the secrets last. A journal of keys (`$XDG_STATE_HOME/sealwax/accounts-pending`) lets the engine delete the secret of an add that crashed before its commit point, and finish a removal that crashed or failed after its own.
- The engine opens the keyring on first use, never on the startup path. Self-tests keep secrets in memory; development builds can too, with `SEALWAX_SECRETS=memory` ([SECURITY.md](SECURITY.md#credentials)).

### ADR-008 amendment — the no-keyring banner and a password typed again (ML-048, M3, 2026-10-05)

Built in M3 ([report](m3/status.md)).
- **No keyring:** when the Secret Service doesn't answer, the engine keeps secrets in memory, as before, and tells the app once (`KeyringUnavailable`). The window's banner says passwords and sign-ins stay in memory until Sealwax quits, until dismissed for the session. The memory backend that self-tests and `SEALWAX_SECRETS=memory` choose is not a fallback and isn't reported.
- **A password typed again** (`UpdatePassword`) is checked on IMAP and then SMTP with the account's username before the account lane stores it under the account key; a refused one is never stored. The account's sync restarts after it is stored.

### ADR-008 amendment — Google dropped (2026-10-07)

**Status:** Accepted — user decision, 2026-10-07 ("Also drop Gmail guidance"). Supersedes the amendment above's "kept for 1.x" and its "the existing preset".

**Decision:** Sign in with Google is never built (it moves from 1.x to Never). Gmail is no longer a listed provider: the autodetect preset for gmail.com, googlemail.com and Google-hosted domains and its app-password help link are removed, and the docs stop presenting Gmail as supported. A Gmail account found through the Mozilla ISPDB or typed in by hand may still connect with an app password, unsupported. The generic IMAP behaviour that also suits Gmail stays (`\All` never syncs by itself, no second Sent copy where the server keeps one, an app-password refusal recognised). OAUTH-SETUP.md's Google section goes.

**Reason:** the restricted `https://mail.google.com/` scope needs a yearly security assessment (CASA, $500–$4,500 a year by Google's support page) and verification paperwork for a public release, and in testing status sign-ins expire after 7 days for at most 100 listed users; most Gmail users read their mail in Google's own app or web version (the user, 2026-10-07).

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

### ADR-009 amendment — `[storage]` in config.toml (ML-044, M3, 2026-10-05)

Built in M3 ([report](m3/bodies.md)): `cache_size` (a size such as `"2 GiB"` or `"500 MB"`, at least 100 MiB, or `"unlimited"`) and `prefetch_days` (0–365; 0 turns prefetching off). The defaults, 2 GiB and 14 days, are proposals for the user. The section is read like the rest of the file and sent to the engine with `Command::Storage`; a file with a mistake at startup sends the defaults.

### ADR-009 amendment — `[storage]` defaults accepted (M3 gate, 2026-10-06)

The user accepted the proposed defaults ("accept the defaults"): `cache_size = "2 GiB"` and `prefetch_days = 14`, with whole messages up to 10 MiB.

### ADR-009 amendment — signatures and identities in accounts.toml (M6, 2026-10-06)

Built in M6's phase A ([report](m6/compose-domain.md)). `signature = """…"""` on an `[[account]]` table and `[[account.identity]]` tables (an alias: `address`, an optional `name` that defaults to the account's, an optional `signature`) are read with the file's error style (spans, did-you-mean, secrets refused, an address listed twice refused); the setup assistant writes neither. Signatures are plain text, inserted below what the user types and above the quote after a `-- ` line, at most 10,000 characters (a proposal for the gate). The default file documents both in comments.

### ADR-009 amendment — `[notifications]` and `[background]` in config.toml (M7 contract, 2026-10-06)

Two sections, read with the file's error style, following the user's decisions at M7's start ([ADR-013 amendment](#adr-013-amendment--background-mode-on-by-default-notifications-show-sender-and-subject-m7-start-2026-10-06)): `[notifications]` with `enabled` (default `true`) and `show` (`"sender-and-subject"`, the default, or `"count"`), and `[background]` with `enabled` (default `true`: closing the window keeps Sealwax running). A bare `enabled` at the top level is pointed to both sections. The default file documents them in comments.

### ADR-009 amendment — POP3 accounts in accounts.toml (M8 phase A, 2026-10-07)

Built in M8's phase A ([report](m8/pop3-setup.md)). A POP3 account has `[account.pop3]` in place of `[account.imap]`, exactly one of the two: `host`, `port` (995 for `"tls"`, 110 for `"starttls"` when left out), `security`, `leave_on_server` (default `true`) and `delete_after_days` (1–3650, only with `leave_on_server = true`; left out, mail stays on the server). Read with the file's error style (positions, did-you-mean, both tables or neither, the policy's contradictions); the assistant writes it with `leave_on_server` spelled out and offers no policy choice (the lead's recommendation, for the gate). In code, `AccountSettings::imap` became `incoming`, with `receive` saying which protocol (the M8 contract).

### ADR-009 amendment — an account's label and colour (2026-10-08)

The user's UI tweak (2026-10-08): two accounts with the same sender name had the same sidebar heading. Two optional settings on `[[account]]`: `label` (plain text, what Sealwax calls the account in the sidebar, banners and menus; left out, the address) and `color` (one of `"rose"`, `"clay"`, `"olive"`, `"sage"`, `"teal"`, `"slate"`, `"iris"`, `"mauve"`, case ignored; left out, none). `name` is now only the sender name. Read with the file's error style (an empty label, an unknown colour listing the eight, "did you mean color?" for `colour`); the setup assistant writes each only when set. The store's account row keeps the label or the address as its name (`AccountSettings::title`), updated whenever `accounts.toml` changes, so existing accounts show their address from the next start; the colour isn't stored, the app reads it from the file. The default file documents both.

### ADR-009 amendment — Edit Account writes the label and the colour (2026-10-08)

The user asked for renaming and recolouring an existing account in the app (2026-10-08, "Add the in-app rename and recolour for existing accounts"). Edit Account sends `Command::EditAccount` (key, label, colour); the engine runs it on its account lane with Add and Remove, so writes never race, and answers `Event::AccountEdited`. It sets the entry's `label` and `color` lines with `toml_edit` (`accounts_file::relabel`): a line already there is replaced in place with its comment, a new one goes after `address`, `None` takes it out, and the rest of the file stays byte for byte; a file with mistakes is refused, and the result must read back as the same accounts with only that entry's label and colour changed. The assistant now writes both after `address` too. The store's name follows with the `Accounts` the app sends once it has read the file.

### ADR-009 amendment — the window's layout in layout.toml (2026-10-08)

The user's UI tweaks (2026-10-08) let the sidebar's and the message list's borders drag and an account's folders fold away. Sealwax remembers both in `$XDG_STATE_HOME/sealwax/layout.toml` (`sidebar_width`, `list_width`, `collapsed` as account keys): state, not a setting, so it isn't among the settings files, it is read leniently (anything unknown, or a width out of range, means the default) and nothing in it is ever logged. It is written atomically (0600) by the `accounts.toml` writer as the user drags or folds, off the GTK thread, and read before the first window shows (waiting at most 150 ms). A width equal to the default isn't written. Deleting the file puts the defaults back.

### ADR-009 amendment — the accounts' rules files (2026-10-09)

Each account's rules are a Sieve script in `$XDG_CONFIG_HOME/sealwax/rules/<account key>.sieve` ([ADR-020](#adr-020--folders-and-rules-sieve-run-by-sealwax-2026-10-09)), a settings file the user may edit by hand (`infrastructure::rules`): a missing file is no rules; it is read as at most 1 MiB of UTF-8 and compiled again whenever its modification time changes; it is written atomically, 0600 in a 0700 folder, through a symbolic link as `accounts.toml` is; it goes with its account. A file that can't run (unreadable, or a script that doesn't check) runs nothing and is reported once per change (`Event::RulesFailed`).

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

### ADR-010 amendment — the app icon is the wax seal alone (2026-10-08)

**Status:** user decision, 2026-10-08: "I only want the red Sealwax with the "@" symbol inside, it should look authentic, like it was done in the old days. No background or frame around it", and "You can also use Higgsfield if you need".

**Decision:** the app icon is a photographic red wax seal with an @ in its impression (a raised border ring and a ring of beads around it), on a transparent background: no envelope tile, no frame, no shadow. It replaces the brand canvas's envelope tile and its simplified 64 px seal.

- **Source:** generated with Higgsfield's GPT Image 2.5 (high quality, 2K, transparent background) from a written prompt (an antique hand-poured crimson seal seen from above, irregular rounded edge, a brass stamp's impression with a serif @ in relief, light from the upper left); the first of two candidates (job `5001b9d2-677b-459d-9c5e-1831a2486dbc`), trimmed to the seal and kept at 1024 px as `packaging/icons/source/sealwax-seal.png`. A procedurally drawn SVG seal was tried first and looked more like a digital emboss.
- **Files:** PNGs at 16, 24, 32, 48, 64, 128, 256 and 512 px (`tools/make-icons.py`), the sizes up to 64 px with a little more contrast and sharpening; no scalable SVG, since a photograph-lit seal can't be redrawn the same at every size and GTK would pick a scalable icon between the fixed ones. Below 32 px the @ no longer reads; the seal's shape does.
- **Unchanged:** the brand colours of the wordmark, and the chrome following the Omarchy theme (ADR-004).
- *Amended the same day (the user: "restart the shell and add the scalable SVG"):* a scalable SVG carrying the 512 px PNG joins the fixed sizes. Omarchy's shell (Quickshell, Qt) keeps the icon paths it found at its start: after the upgrade it still asked for the envelope tile's `scalable/apps/…svg`, gone, and its dock showed the "icon missing" checkerboard until the shell restarted. Its icon index also lists SVGs before PNGs and otherwise takes whichever fixed size it finds first. The SVG is at that path again and is the same picture at every size.

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

### ADR-011 amendment — the reader as built in M5 (2026-10-06)

Built as [m5/conversation.md](m5/conversation.md), [m5/files.md](m5/files.md) and [m5/rendering.md](m5/rendering.md) record. Every layer of the S2 amendment stays; the hostile suite runs in `tools/ci.sh --ui` (60 cases) with a DNS canary.
- **Live views:** at most `MAX_LIVE_VIEWS` (4). Expanded HTML bodies within one column height of the visible part hold one, nearest first; the others give theirs back and keep their measured height; a focused body keeps its view; a spare view measures the nearest unmeasured body. A view that comes back gets the content filter again unless its message or sender is allowed. Changes above the visible part are held still by moving the column's scroll in the frame clock's layout phase.
- **`cid:`** answers come from the main loop, asynchronously, and only for the load that asked (epochs per view); parts the store doesn't hold come from `FetchPart`.
- **Links:** the isolated world reports the hovered or focused link; the reader shows its target and, when the link's text names another registrable domain (`domain::links`, the PSL) or its address hides a name before `@`, warns and asks before opening. Plain-text bodies link only http(s), `www.` and mailto.
- **Paper** keeps `color-scheme: light` and paints no root background, so a message's body background reaches the canvas; WebKit's `prefers-color-scheme` follows the app's scheme, so a message with dark-mode rules shows its own dark design in a dark theme. Themed links take the accent; colours set only for links don't make a body paper.
- **Remote content per sender:** images load for a message the user allowed this session, or for a sender on the store's list (ADR-005, v8). The CSP and the cookieless session are unchanged; a forged From address borrowing a listed sender's permission is the accepted risk.
- **Print** renders the selected message in a pooled hardened view held by a window that is never shown (WebKit loads only under a window), with Sealwax's header added from the isolated world. **Show Original** shows the source as text and never renders it.

### ADR-011 amendment — the memory measured live (M5 phase B, 2026-10-06)

`reader_memory --live --judge`, five runs on the live session (release; NVIDIA RTX 3070 Ti; the window tiled to 1709×1400): with a typical 10-message conversation open, process-tree PSS **383.5–397.8 MB**, of which NVIDIA's libraries and device mappings are 96.9–97.1 MiB (71.5 in the app, 16.6 in WebKit's web process, 9.0 in its network process); **without them 281.9–296.0 MB**. At most 3 views were live (cap 4). GPU memory by `nvidia-smi`: 47 MiB steady, 55 MiB at peak, all the app's (reader views render without acceleration). Growth after 200 opens: 15.7–26.6 MB (3 of 5 runs over the 20 MB target; the app's share about 10 MiB each run). Whether the driver's mappings count toward the 300 MB bar or the GPU budget (the M1 question, deferred to M5), and the cap and the GPU budget, are the user's decisions at the M5 gate ([PLAN.md](PLAN.md#m5-integrated-the-gate-is-the-users--2026-10-06)).

### ADR-011 amendment — memory decided at the M5 gate (2026-10-06)

**Status:** Accepted — user decision at the M5 gate, 2026-10-06 ("GPU budget (Recommended)").

**Decision:** the memory bars judge PSS without NVIDIA's driver libraries and device mappings: the reader's process tree ≤ 300 MB with a typical 10-message conversation open, the UI process ≤ 150 MB. The mappings count toward the GPU budget instead, which covers them ("~100 MiB") plus GPU memory ("~64 MiB"). The live-view cap stays at 4. Judged so, the reader measured 281.9–296.0 MB live and M1's UI measurement 114.3 MiB; both pass, the reader with little headroom. Growth after 200 opens (15.7–26.6 MB live, 3 of 5 runs over 20 MB) is a P1 follow-up: measured over 1,000 opens, and fixed before 1.0 if it doesn't level off ("Follow-up (Recommended)").

**Supersedes:** the M1 measurement's open question (whether the driver's share counts toward the UI bar or the GPU budget) and the amendment above's "the user's decisions at the M5 gate".

### ADR-011 amendment — the growth levels off (2026-10-07)

`reader_memory --opens=N` measured the follow-up the M5 gate set: live, the app's process is flat after 200 opens and WebKit's web process after about 1,000–1,200; from 1,000 to 3,000 opens the process tree stays within 394–422 MiB with no trend (two runs, 399.0 and 399.2 MiB at 3,000); headless the tree stops growing at 200 ([TESTING.md](TESTING.md#reader-memory-over-1000-and-3000-opens--2026-10-07)). By the gate's rule ("fixed before 1.0 if it doesn't level off") no fix is needed; the follow-up is closed. The growth over the first 200 opens stays as measured at M5.

### ADR-011 amendment — the corpus's GPU growth was the harness (2026-10-07)

The 118 → 1,350 MiB of GPU memory during M5's `reader_corpus` run came from the harness's own blank-frame checks, which render each whole view (up to 13,000 px) as textures: with them the run reached 1,352 MiB again; with `--no-snapshots`, three live runs held 47 MiB from start to end ([TESTING.md](TESTING.md#the-corpus-harnesss-gpu-memory--2026-10-07)). The reader stays within the GPU budget's ~64 MiB of GPU memory on the whole corpus.

### ADR-011 amendment — WebKit's sandbox can't be turned off from the environment (M9, 2026-10-07)

Built on the user's "Keep WebKit's sandbox on". WebKitGTK 2.52.6 runs its web processes outside bubblewrap when `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS` is set (tried: the web process became Sealwax's own child, in Sealwax's namespaces). `main` removes it before any thread starts, in every build, and logs a warning without its value. The self-tests run with it set, and `assert-web-sandbox` checks that the reader's and the composer's web processes run under bubblewrap in a PID namespace of their own. A container without unprivileged user namespaces can't run WebKit's web processes (ML-095 takes that into account).

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

### ADR-013 amendment — background mode on by default; notifications show sender and subject (M7 start, 2026-10-06)

**Status:** Accepted — user decisions at M7's start, 2026-10-06.

**Decision:**
- **Background mode is on by default** ("On by default (Recommended)"): closing the window keeps Sealwax running, so sync, IDLE and notifications go on; Quit (Ctrl+Q) stops it. The first close shows a one-time notice saying so, and `[background]` in `config.toml` can turn it off.
- **A new-mail notification shows the sender and the subject** ("Sender and subject (Recommended)"). `[notifications]` in `config.toml` can switch to a private mode that shows only that new mail came, with a count.

**Reason:** on Omarchy a notification click reaches Sealwax only while it runs (S6), so background mode is what makes D5 work; the sender and subject are what most mail clients show, and the private mode covers shared screens.

### ADR-013 amendment — M7 gate decisions: notifications and background mode as built (2026-10-07)

**Status:** Accepted — the user's delegation at M7's end ("I trust you and go with your recommendations"), 2026-10-07.

**Decision:**
- **Notifications** ([report](m7/notifications.md)): unread mail in an Inbox only; quiet while the focused window's list shows that folder or All Inboxes; up to three of their own within 30 s, then one "N new messages" that replaces itself; normal urgency; the sender (else the address) as the title and the subject as the body, capped at 80 and 200 characters, or "New mail" and a count in the count mode; Archive and Mark Read as buttons where the server shows them; withdrawn when the message is read, moved or deleted anywhere, and when the user switches to the count mode or off. The actions behind a notification take a message id, so they are application actions outside the registry. Per-account muting isn't built (a follow-up).
- **Background mode** ([report](m7/background.md)): the one-time notice shows when the last window (main or a composer opened alone) closes into background mode, once per user.
- **Opt-in autostart** is built as Start Sealwax at Login (ADR-003's M7 amendment).

**Reason:** the agents' proposals as built; the live check passed on omarchy-shell on 2026-10-07 (a click opened the right message, 14.9 ms with the window open, 73.4 ms from background mode).

### ADR-013 amendment — POP3 kept in 1.0; POP3 accounts as M8 starts them (2026-10-07)

**Status:** Accepted — the lead's recommendations, by the user's delegation at M8's start ("Then start M8, still with your recommendations"), 2026-10-07.

**Decision:** POP3 stays in 1.0 (PRD A9, P1), as the user chose at planning. A POP3 account is added through the setup assistant's manual settings (an "Incoming mail: IMAP or POP3" choice) or `accounts.toml`; autodetect stays IMAP. Its mail is downloaded into local folders (Inbox, Sent, Drafts, Junk, Trash, Archive). By default downloaded mail stays on the server and is never deleted there; deleting after N days, or at once, is the user's setting. The first download doesn't notify; later mail does.

**Reason:** the user wanted POP3 kept (2026-10-04); the defaults are the ones that can't lose mail on the server.

### ADR-013 amendment — Google and Gmail out of scope (2026-10-07)

**Status:** Accepted — user decision, 2026-10-07 ([ADR-008 amendment](#adr-008-amendment--google-dropped-2026-10-07)). Sign in with Google moves from 1.x to Never; Gmail labels leave 1.x; 1.0's app-password guidance no longer names Gmail, which is no longer a listed provider.

### ADR-013 amendment — folders and rules into 1.0 (2026-10-09)

**Status:** Accepted — user decision, 2026-10-09: "Now I need the possibility for users to make subfolders for the inboxes, and to make rules for emails to be routed to the different folders (use industry standards for rules, spam/not spam rules as well)", then "In Sealwax (Recommended)" and "Create, rename, delete (Recommended)". Folder create, rename and delete and local rules move from 1.x into 1.0, with Sieve as the rules' language, run by Sealwax ([ADR-020](#adr-020--folders-and-rules-sieve-run-by-sealwax-2026-10-09)); uploading scripts to servers (ManageSieve) stays on Later.

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

### ADR-014 amendment — the user's fcitx5 check (2026-10-06)

The user ran S7's manual check on the live session with fcitx5 and approved it ("approved", 2026-10-06). With acceleration off, key press → painted frame was p95 10.51 ms for every text key (n=85) and 11.24 ms for æ/ø/å, under the 16 ms bar; dead keys arrived as fcitx5 commits after preedits. One of the five instruction lines was typed exactly; € (Compose), ô and ü weren't exercised ([PLAN.md](PLAN.md#the-fcitx5-and-norwegian-typing-check-done-by-the-user--2026-10-06)). Still open: the 60 Hz re-measure and long-quote scrolling, before M6.

### ADR-014 amendment — the composer in its own window (M6 start, 2026-10-06)

**Status:** user decision at M6's start ("Its own window (Recommended)", 2026-10-06). The full composer is a window of its own for each draft, as the approved mock draws it ("New message" with its From chip): Hyprland tiles it beside the main window, several drafts can be open at once, and a `mailto:` link from a browser opens one without taking over the reader. The quick reply stays in the reader, under the conversation, and expands into a composer window. The composer window installs its own `win.` actions for the Composer scope (Send, the formatting commands, Attach File…, Discard Draft); the alternative, the composer in the reader's pane, was declined.

Still open from S7: the 60 Hz re-measure (the user's; it changes the monitor's settings). Typing in the real composer and long-quote scrolling without acceleration are measured live in M6 (authorized 2026-10-06).

### ADR-014 amendment — the composer as built (M6, 2026-10-06)

Built in M6 ([composer](m6/composer.md), [compose rules](m6/compose-domain.md), [reply](m6/reply.md)).
- **The window:** one application window per draft, shown at once and filled when the draft is ready; S7's hardened editable view in a web context and process of its own, with every layer S7 proved and `hardware-acceleration-policy` NEVER; the toolbar follows the editor's state; every Composer command is a `win.` action of the composer window, its keys installed on the window and before WebKit on the body, so Ctrl+B and Ctrl+I toggle once and Ctrl+Z never reaches the main window's mail Undo. Pasted and dropped images become `cid:` parts in the blob store, served only to the view that holds them.
- **Drafts** save to the store 1.5 s after the typing stops and at most 4 s after the first unsaved change; drafts left open reopen after the first frame. A `kill -9` while typing lost at most 3.51 s in 12 runs (the agent's) and 3.34 s in 6 (the lead's, load average about 19); every reopened draft held exactly the start of what was typed.
- **Quoted HTML cleaned in Rust:** HTML that enters a draft without the paste cleaner (a quoted or forwarded original, which the quick reply sends without a WebView, and a draft read back from the Drafts folder) goes through `domain::compose::clean`: an html5ever 0.39 pre-pass with S7's presentational rules, then **ammonia** (pinned `~4.1.5`, sharing html5ever with html2text) as the allowlist with S7's rules (links only http(s) and mailto; images only `https:`, `cid:` and typed base64 `data:`). Remote images in a quote are kept as the original had them and never loaded in the composer (a proposal for the gate).
- **Live, on the user's session** (authorized at M6's start; `app/examples/composer_live.rs`, 144 Hz, the window tiled 1709×1400): an edit to the painted frame in the real composer's editor, p50 8.15 ms and p95 9.94 ms in a small document (n=80) and p50 10.15 ms, p95 12.35 ms above a 199 KB quoted thread (n=40); scrolling that thread at 1,500 px/s, frame interval p95 14.62 ms over 282 frames. S7's open items "typing in the real composer" and "long-quote scrolling without acceleration" are closed; the 60 Hz re-measure stays the user's.
- **Spell check** stays off without a dictionary (this machine has none).

### ADR-014 amendment — the composer docked in the main window (2026-10-08)

**Status:** user decision, 2026-10-08 (a UI tweak, with a screenshot of Gmail's composer: "I want the email composer to open inline (like the Gmail composer in the screenshot), not in a new tile as it does now"). It reverses the M6-start amendment above: the composer is no longer a window of its own.

**Decision:** each draft is a **card docked over the panes** at the main window's bottom right (`window::dock`), with the same content as the window had. Its header adds **Minimize** (the card shrinks to its header, 300 px wide, named by the subject; a click on the header opens it), **Expand** (up to 1,000 × 820 px in the middle of the panes over a dimmed backdrop, the other cards hidden; Shrink or a click on the backdrop puts it back) and **Close** ("Save and close": the draft is kept, or discarded when nothing was typed into a new one). An open card is 640 × 600 px, 16 px from the right edge; newer cards open to the left of older ones and the oldest open ones minimize when they no longer fit; while one pane fills the window an open card fills the panes above the minimized ones.

- **Actions:** each card inserts its own `win` action group holding only the Composer commands (`actions::install_composer_actions`); inside the card it hides the main window's actions of the same names. The Composer scope's keys are attached to the card (bubble) and before WebKit on the body (capture), as they were to the window. The application accelerators (Compose's Ctrl+N, the command palette, Search, Refresh…) are the main window's and work from a card; the palette opened in a card lists and runs the card's commands too (`palette::present_titled` takes the card's group).
- **Focus:** a card the user opens takes the focus (To while it is empty, else the body); drafts reopened after a crash open without taking it (the main window's list keeps it). Minimizing, closing or sending gives the focus back to where it was before the card took it, else to the message list. In the Tab order the cards come after the panes. Escape in a card stops there, so it never leaves the list's select mode behind it.
- **No main window:** a `mailto:` link, `--compose` or a notification's Open with no main window shown shows one and docks the card in it (a composer window alone before, ML-078). `--compose` to a running Sealwax brings its window forward.
- **Closing the main window** (into background mode) first saves every card's draft and closes the cards, the drafts kept; Quit saves them too (`composer::save_all`, `composer::keep_drafts_on_close`).

**Consequences:** a composer can no longer be tiled beside the main window or moved to another workspace; Hyprland's close no longer applies to it. The self-test commands take the card (`assert-composer state|bounds|backdrop`, `composer-pointer`); `reply-mailto-alone` became `reply-mailto-start`; new `composer-dock`.

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

### ADR-015 amendment — the op queue's store side and writes as built (M4 phase A, 2026-10-06)

Built in M4's phase A ([store](m4/ops-store.md), [writes](m4/imap-writes.md)); the runner on the actor is phase B.
- **Ops:** one per message per action, FIFO per message, each recording the folder, UIDVALIDITY, UID and Message-ID, the flags and folder the user saw (for Undo), `not_before`, attempts and, for moves, the target's UIDVALIDITY and UIDNEXT before the first attempt (the "mark"). Written with the overlay and the counts in one transaction and one change event, at the writer's interactive priority: one message's change event p99 ≤ 3.85 ms at 100,000 messages.
- **Planning** is `domain::triage` (M4's contract): the store groups an action's messages by folder and plans each group with the account's folders.
- **Due:** a message's not-started ops whose `not_before` has passed fold into one action (flags merge, flags before a move travel with it, move chains collapse, A→B→A is done without the server); a started run is retried as it was. Groups per folder and action carry UID sets.
- **Outcomes:** done turns the overlay into the server's state in the same transaction (a move with a confirmed new UID re-homes the row, body and full text kept; without one the target's sync fetches it); transient failures keep the overlay; permanent ones drop the ops, show the server's state and are reported (`Event::OpsRefused`). A MOVE of a UID that no longer exists counts as done, as does a pending move or expunge of a row sync saw go.
- **Retries are idempotent:** an attempt that may have done part of the change first reads the Message-IDs of the target's UIDs from the mark up, keeps what is already there and removes only its source copy, then moves or copies the rest. Measured: 250 runs with the reply of MOVE, COPY, the `\Deleted` STORE or UID EXPUNGE lost or the connection cut on send, 2,325 messages moved, 0 duplicates, 0 lost; a blind replay duplicated 5 of 5 on both Dovecots.
- **Undo:** ops not started are dropped (nothing reaches the server); otherwise inverse ops from the state at queue time, without a window. Deleted for good, or moved where no UID was reported and no copy is found, is reported lost.
- **UIDVALIDITY:** a reset folder's ops wait for its next sync and find their messages by Message-ID; those still waiting when its first sync finishes are refused.
- **A folder marked read:** the messages the store holds as per-message ops, and the server's unread below a windowed folder's floor as one op; the writes search `UNSEEN UNDELETED` first so the UIDs they mark are known.
- **Local accounts** have no server: their ops and Undo apply at once.
- **The runner** (phase B) holds the source's and the target's pass locks, in folder order, while it runs a group and records it.

### ADR-015 amendment — the op runner as built (M4 phase B, 2026-10-06)

Built in M4 ([report](m4/runner.md)) on the actor, the store's op queue and the write module.
- **The runner** lives on the account's command connection and runs before its passes: after every login, on `OpsQueued`, and when the next undo window or retry ends (`DueOps.next`). Never on the IDLE connection.
- **A group** (one folder, one action, a UID set) holds its folders' pass locks (the source's, a move's target's, in folder order) from before its first command until its outcome is recorded. A move takes the target's mark before its first attempt; every attempt is counted in the store before it starts, so a cut, `Reconnect`, a stop or a crash leaves it to run again as a retry.
- **Failures:** retried with the actor's backoff up to 5 attempts, then refused (a proposal); refusals for good put the messages back and reach the app as `OpsRefused`, those the sync paths made too; a full mailbox keeps the ops and shows quota full until a move or delete gets through; a changed UIDVALIDITY hands the folder to its pass, and the store re-targets by Message-ID.
- **A folder marked read:** the store's op for the unread below the floor runs as the write module's `mark_folder_read` up to the floor. Undoing it after its window would need the UIDs it marked kept in the store (a proposal); until then Undo reports them lost.
- **Measured on the testbed:** the crash replay (MOVE, COPY, STORE, EXPUNGE; the connection cut or the engine stopped hard, then a new engine on the same store; three servers; 110 runs, 506 messages): 0 duplicates, 0 lost, the store equal to the server every time. An Undo inside the window sends nothing (the proxy recorded every client line); one after it brings the same row back with its body. 15 randomized two-client runs with local triage, Undo and a UIDVALIDITY reset each: 0 differences.

### ADR-015 amendment — the app on the op queue (M4 phase B, 2026-10-06)

Built in M4 ([report](m4/triage-app.md)). The app queues every triage action with `Store::queue_triage` (Mark Folder as Read with `mark_folder_read`) and undoes with `Store::undo`, awaiting the replies on the main loop, then sends `Command::OpsQueued` for each account the reply names.
- **The undo window** is 7 s (a proposal within this ADR's 5–10 s): archive, delete, junk, move and Mark Folder as Read carry it; flag changes run at once. Only the last action is undoable from the toast; `z` and Ctrl+Z undo it after the toast too.
- **Undo of deleting for good** asks the store, which brings back what hasn't reached the server; a local account (no server) deletes at once and offers no Undo.
- **The offline banner counts the pending ops** ("3 waiting"), read on the store's reader after each action, Undo or change event and every 3 s while ops wait.
- **Same frame:** the store's change event arrives within the frame on fixture stores (handler → list changed p50 0.33–0.76 ms, one message's p95 ≤ 4.86 ms, headless), so the list applies the store's events and predicts nothing.

### ADR-015 amendment — M4 gate decisions (2026-10-06)

**Status:** accepted for triage by the user at the M4 gate ("Accept all", 2026-10-06); the at-most-once send stays M6's.
- **The defaults as built:** the undo window 7 s; no toast for one message's read or star change; a message marked read when it opens; the next message selected after an action; deleting from the Trash asks nothing and can be undone only while it hasn't reached the server; Mark Folder as Read waits for the window too; done ops kept for Undo from the newest 50 actions; 5 attempts, then a change is refused; moves and deletions retried while the mailbox is full; the offline banner counts the changes waiting.
- **Not built:** Undo of a folder marked read after its window (a proposal, P2 in PLAN.md).

### ADR-015 amendment — the outbox as built (M6, 2026-10-06)

Built in M6 ([store](m6/drafts-store.md), [sending](m6/send.md), [reply](m6/reply.md)); the at-most-once send this ADR left to M6.
- **States** (the store): queued until `not_before` (the undo window, 7 s like triage's: a proposal), sending (from just before DATA, committed with a full sync, with the built bytes and envelope), uncertain, sent. Each step is a compare-and-set: only a queued entry starts, so an Undo between the due list and the start wins and nothing is sent. Before DATA, or on a 4xx, it is queued again with the same bytes; a 5xx or a refusal puts it back as an editable draft with the reason.
- **At most once:** at the engine's start every sending entry becomes uncertain (`Event::SendUncertain`) and is never sent again until the user says send again (the same bytes and Message-ID), it was sent, or back to a draft. A property test over random queues, Undo, outcomes, stops and the user's answers passed 3,000 cases, and a planted fault that lets uncertain sends go again fails it.
- **The runner** (the account's actor, after the op runner): one SMTP connection per round of due sends; the store's start before DATA; a stop or `Reconnect` while the data goes out records the send uncertain first; offline or a login the user must fix keeps it queued. On Stalwart through the fault proxy, cuts and hard stops at five steps (4 seeds, 40 runs): the 20 that couldn't have delivered went exactly once, the 20 uncertain ones were never resent before the answer, "send again" sent once more (10 of 10), "it was sent" nothing (10 of 10), and an Undo inside the window put no MAIL FROM on the wire.
- **After the send**, one transaction records it, queues the source's `\Answered` or `$Forwarded` in the op queue and the draft's Drafts copies for deletion. The Sent copy is skipped for servers that save their own (Microsoft, Gmail); otherwise it is looked for from the mark taken before the send and appended `\Seen` when absent.
- **The app:** "Message queued" with Undo for the window (the send becomes the last action, so `z` and Ctrl+Z undo it too), "Message sent", a 10 s toast when a send fails for good with Open, and the window's banner for a send that may have gone ("Was the message sent?": Send Again, It Was Sent, Back to Drafts, Later); the offline banner's count includes waiting sends.

### ADR-015 amendment — M6 gate decisions (2026-10-06)

**Status:** accepted by the user at the M6 gate ("Accept all (Recommended)", 2026-10-06); this ADR's at-most-once send is built and accepted. The proposals in this ADR's, ADR-007's, ADR-009's and ADR-014's M6 amendments stand as built: the undo-send window 7 s; one refused recipient refuses the whole send; a send uncertain once the message's bytes start going out; the Sent copy skipped for servers that save their own (Microsoft, Gmail), else looked for from the mark and appended; one SMTP connection per round of due sends; sends kept queued while the login needs fixing, retried without a cap; drafts to the Drafts folder 30 s after the last save while open, 10 s once closed; plain-text signatures; remote images in quotes kept and never loaded in the composer; forwards without In-Reply-To or References; no User-Agent or X-Mailer; References trimmed to 20; the HTML parser crates capped in the logs. At the same gate: text on the accent keeps the 3:1 `docs/m1/theme.md` states; Enter, `o` and a double click open a Drafts-folder message in the composer; the keys in [ADR-017's M6 amendment](#adr-017-amendment--m6s-keys-accepted-m6-gate-2026-10-06).

### ADR-015 amendment — POP3 accounts' ops and outbox (M8 phase A, 2026-10-07)

Built in M8's phase A ([report](m8/pop3-accounts.md)). A POP3 account's ops are done at once in the store, with Undo, like a local account's. The outbox's sending is shared by the IMAP and POP3 actors (`outbox::Submission`), at most once as before; a POP3 account's Sent copy is filed into its local Sent folder (`SentCopyDone::Filed`), also when its SMTP server saves one of its own, and its drafts' copies are rows of its local Drafts folder (IMAP's timing: 30 s after the last save, 10 s once closed). Raw messages left by ops done at once are deleted by the POP3 actor after each pass.

### ADR-015 amendment — a build that stops Sealwax isn't run again (M9, 2026-10-07)

Before a send's message or a draft's Drafts-folder copy is built, the store records it (schema v12) until the build returns. At the engine's start, a send left so goes back to its draft, failed (`SendFailure::Interrupted`; never sent, the user sends it again), and a copy left so waits for the draft to change: a message whose build ends the process is built at most once per user action, not at every start. At-most-once sending is unchanged. Verified by store tests, the at-most-once property test (3,000 cases, with a planted fault failing it) and an engine start; no real abort mid-build was tested ([report](m9/depth.md#2-the-outboxs-crash-loop-guard)). The toast's text ("Your message wasn't sent: Sealwax stopped while preparing it. It's back in your drafts.") is a proposal for the user.

### ADR-015 amendment — rules' ops and the copy (2026-10-09)

Built for rules ([ADR-020](#adr-020--folders-and-rules-sieve-run-by-sealwax-2026-10-09), part B1): `Store::queue_rules` queues what an account's rules decided for messages that arrived as one batch (kind 4) due at once, with its overlay: a move and flags as triage queues them, and a new op kind, the **copy** (4), for `fileinto :copy` and a second `fileinto`. A copy changes nothing the user sees; in a message's chain it runs on its own, after the changes before it and before those after it (the due run is cut at it). The runner sends `UID COPY` after taking the target's mark, as a move does, and a retry looks for the copies by Message-ID above the mark first and copies only what's missing (`imap::write::copy_messages`). A refused copy is reported as its own kind (`Refused::Copy`); Undo drops a copy that hasn't started and leaves one made (neither restored nor lost); a copy pending when another program moves the message away counts as done with the move. Local accounts' copies are left to the caller, which stores them again from the message's bytes. Verified by store tests, the fake server and the testbed (Dovecot, Dovecot IMAP4rev1, Stalwart: copied once, the retry copied nothing).

## ADR-016 — Conversation threading (2026-10-04)

**Status:** Proposed.

**Decision:** threads from References and In-Reply-To with union-find aliases (a parent can arrive after its reply), duplicate Message-IDs merged (Gmail labels, copies to yourself), Outlook `Thread-Index` honoured, Gmail `X-GM-THRID` used only as a hint. Never group by subject alone. Threads over 100 messages collapse their middle.

**Consequence:** property tests cover out-of-order arrival and merges.

### ADR-016 amendment — S3 results (2026-10-04)

The Message-ID alias table and thread merges work at 100k messages (40,548 conversations): a parent arriving late joins its thread, threads merge, duplicates collapse, and the counts the writer maintains match a full recount (kept as a test). The first 100 of 24,844 conversations in a folder load in ≤ 3 ms cold. Not yet exercised: Outlook `Thread-Index`, the Gmail thread-ID hint, and collapsing threads over 100 messages.

S1 (2026-10-04) confirmed the inputs on the testbed: Message-ID, In-Reply-To and References all come from the header sync, and duplicate Message-IDs do occur. Gmail's and Outlook's own thread headers are still unverified.

### ADR-016 amendment — threading as built (M1, 2026-10-04)

Built in M1 ([m1/store.md](m1/store.md)): union-find over `thread_aliases` (every Message-ID, In-Reply-To and References entry, per account); merges go into the largest thread and are reported in change events; a thread's size counts each Message-ID once. Not built yet: Outlook's `Thread-Index`, the Gmail thread-ID hint, collapsing threads over 100 messages, and per-folder conversation rows.

### ADR-016 amendment — Outlook's Thread-Index and the property tests (M3, 2026-10-05)

Built in M3 ([report](m3/threading.md)).
- **Thread-Index honoured:** the 22-byte header block of a valid `Thread-Index` (base64; reserved byte 0x01, a time, a non-zero GUID, whole 5-byte reply blocks) is filed per account as one more alias beside Message-ID, In-Reply-To and References, so Outlook replies without References join their conversation. Malformed values are ignored. Stored in `messages.thread_index` (schema v3). Unverified against real Outlook and Exchange mail.
- **Gmail's X-GM-THRID isn't used,** by the user's decision of 2026-10-04 (no Gmail-specific work). Gmail mail threads by its headers.
- **Removals never split a conversation:** its aliases stay until its last message goes, so a moved message rejoins it and a merge outlives the message that caused it.
- **Checked:** a property test (64 cases per CI run; 5,000 passed once) ingests random corpora over three accounts in any order and batch size, with resends, expunges and moves, and compares the conversations and their sizes with the connected components of the alias graph. `Store::check` verifies that every threading key of every message aliases its conversation.
- Still not built: collapsing threads over 100 messages and per-folder conversation rows (ML-057).

### ADR-016 amendment — the conversation view is the reader's (M5 start, 2026-10-06)

**Status:** Accepted — user decision at M5's start, 2026-10-06 ("Reader shows conversation").

**Decision:** the message list keeps one row per message, with the conversation's size on the row (M1). Selecting a row shows the row's whole conversation in the reader: older messages collapsed to one line, unread messages and the latest expanded, the middle of a conversation over 100 messages collapsed (ML-057). Triage keeps acting on the selected messages (M4).

**Alternative rejected:** one list row per conversation (Gmail's grouping), which would rework the store's views, counts, sections and change events at 100,000 messages and make triage act on whole conversations; about 4–6 days more.

**Consequence:** "per-folder conversation rows", listed as not built in the M1 and M3 amendments above, aren't built in 1.0.

### ADR-016 amendment — the conversation view as built (M5, 2026-10-06)

Built as [m5/conversation.md](m5/conversation.md) records. `Store::conversation(id)` reads the selected message's conversation in one snapshot, oldest first: rows a pending move or expunge or `\Deleted` hides are left out (never the selected one); copies of one Message-ID show once (the selected copy, else Inbox, Archive, All Mail, Sent, other folders, Drafts, Junk, Trash); the Trash and Junk show only when the selected message is there. The reader expands the selected message, the unread ones and the latest; over 100 messages it keeps the first, the latest 10 and the expanded ones and folds the rest into "N earlier messages", revealed 50 a click. Messages arriving while it is shown join as unread rows. The list's conversation size still counts every message of the conversation (a design question at the gate). Threads over 100 messages are folded by the reader, which this ADR's "collapse their middle" asked for.

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

### ADR-017 amendment — the list's own keys accepted (M4 gate, 2026-10-06)

Accepted by the user at the M4 gate ("Keep all four", 2026-10-06). Beside the registry, as the message list's own keys (no `keys.toml` name, like GtkListView's Down and Up): Shift+`j` and Shift+`k` extend the selection as Shift+Down and Shift+Up do; Escape leaves the select mode; Menu and Shift+F10 open a sidebar folder's menu. GtkListView's Ctrl+A is swallowed (rows not loaded have no ids). The keyboard pass on the US and Norwegian layouts was approved headless (`triage-keys` and M1's audit); the live pass stays a P1 follow-up. *(Done 2026-10-06: the user's own live pass, "Keyboard pass approved".)*

### ADR-017 amendment — M5's keys accepted (M5 gate, 2026-10-06)

Accepted by the user at the M5 gate ("Ctrl+U: Show Original", "Ctrl+S: Save as .eml", "n / p in conversations", 2026-10-06), beside Print's Ctrl+P from the approved mock. Show Original takes Ctrl+U and Save as .eml Ctrl+S, scoped to the list and the reader like the other message commands' Ctrl keys (the composer's own Ctrl+U, ADR-014, is in another scope). Two registry commands are new: Next Message in Conversation (`n`) and Previous Message in Conversation (`p`), in the list and the reader; `j` and `k` stay the list's next and previous. All four keys pass the audit (no AltGr, unshifted on US and Norwegian). Expand and Collapse All Messages, Show Images and Always Show Images from This Sender keep no default key.

### ADR-017 amendment — M6's keys accepted (M6 gate, 2026-10-06)

Accepted by the user at the M6 gate ("Keep both", 2026-10-06): Paste as Plain Text takes Ctrl+Shift+V in the composer (Move to…'s Ctrl+Shift+V is in the list's and the reader's scope), and Open Reply in Composer takes the approved mock's Ctrl+Shift+E in the reader (IBus uses it for emoji typing; fcitx5, Omarchy's input method, doesn't by default). The composer's other keys are the approved mock's (Ctrl+B, Ctrl+I, Ctrl+U, Ctrl+K, Ctrl+Z, Ctrl+Enter) and ADR-014's (Ctrl+Shift+Z and Ctrl+Y for redo). No Close Composer command: Hyprland's own close closes a composer and keeps its draft. Quick Reply, Edit Draft, Attach File…, Add Cc and Bcc, Discard Draft, the other formatting commands and Make Sealwax the Default Mail App… keep no default key. Enter, `o` and a double click on a message of a Drafts folder open it in the composer (the user's choice at the same gate, "Open in the composer").

### ADR-017 amendment — M7 adds no keys (M7 gate, 2026-10-07)

**Status:** Accepted at the M7 gate (the user's delegation, 2026-10-07). No registry key was added: Rebuild Search Index and Start Sealwax at Login have none. The search field's own keys while its suggestions show (Down, Up, Tab, Enter, Escape; Down with them closed lists the operators) are the field's, as the composer's recipient suggestions are. The notification actions aren't registry commands.

### ADR-017 amendment — M9's keyboard behaviour accepted (2026-10-07)

**Status:** Accepted — user decision, 2026-10-07 ("Keep all as built (Recommended)").

M9's accessibility pass ([report](m9/accessibility.md#proposals-for-the-users-gate)) added no key; these behaviours stay as built: Tab in the composer's body moves to Send and Shift+Tab to the toolbar (no tab character); the folder list and the message list are one Tab stop each, with Up and Down inside, and the rows of the palette and Move to… are none (their field keeps the focus and the row is announced); a plain-text body's links are reached with Tab and opened with Enter; the recipient chips' × are Tab stops; Go to Starred turns on the Starred chip (there is no Starred folder); the composer's Tab order is the visual order (From, Attach, Discard, To, Cc · Bcc, Subject, the toolbar, the body, Send). Go to Inbox, Starred, Sent and Drafts and Back to List now run with the keys the registry already had.

### ADR-017 amendment — Edit Account has no default key (2026-10-08)

Edit Account (the user's UI tweak, 2026-10-08) is in the palette and the main menu with no default key, as Add Account and Remove Account; `keys.toml` can bind one. Its colour swatches' Left, Right, Up, Down, Home, End, Space and Enter are the radio group's own, as the setup assistant's are.

### ADR-017 amendment — the folder list's heading keys, proposed (2026-10-08)

Built with the UI tweaks of 2026-10-08, for the user's decision: account headings are rows of the folder list, reached with Up and Down; on one, Enter or Space folds or opens the account, Left folds and Right opens it (the tree convention), and Menu or Shift+F10 opens its menu (Edit Account), as on folder rows. No registry command or key was added; dragging the borders has no keyboard way.

### ADR-017 amendment — the docked composer adds no key (2026-10-08)

The composer docked in the main window (ADR-014's amendment of 2026-10-08) adds no registry command and no key: Minimize, Expand and Close are buttons in the card's header, reached with Tab. The Composer scope's keys are unchanged. M6's note that Hyprland's close closes a composer no longer applies; its Close button does. Escape in a card does nothing, as in the composer window, and no longer reaches the main window. Keys for the card (closing it, moving between it and the mail) are the user's decision and none was proposed yet.

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

### ADR-018 amendment — the HTML parser capped (M6, 2026-10-06)

`html5ever`, `markup5ever`, `html2text` and `mail_builder` are capped at `warn` like the protocol crates: html5ever's debug output carries the text it parses, and the log grep caught a draft's body in it while the MIME builder made the plain part ([report](m6/send.md)). The default `info` filter never showed it; `RUST_LOG=debug` would have, in the reader too.

### ADR-018 amendment — the caps as built, and no Referer in autodetect (M9, 2026-10-07)

- **Caps as built:** `infrastructure::logging::CAPPED` holds 15 crates at `warn`: `async_imap`, `imap_proto`, `lettre`, `rustls`, `tokio_rustls`, `hickory`, `reqwest`, `hyper`, `hyper_util`, `h2`, `zbus`, `html5ever`, `markup5ever`, `html2text` and `mail_builder` (the amendments above named all but `hyper`, `hyper_util` and `h2`).
- **No Referer:** reqwest sent the previous URL, query and all, as the Referer of every redirect it followed, so a redirect the policy allowed (to another host inside the user's domain, without the address in its URL) still carried the address to that host in a header. Autodetect's client now sends no Referer (`.referer(false)`), found by M9's security review ([report](m9/security.md#findings)). The test reads the client's configuration; no real redirect was observed.

### ADR-018 amendment — panics reported without their message (M9, 2026-10-07)

`logging::init` installs a panic hook: a panic located outside Sealwax's own sources (a dependency's or the standard library's code) is reported on stderr by thread and place only, as its message may quote what was being parsed (imap-proto's names a character of the server's reply); Sealwax's own panics keep Rust's report. An actor's panic is logged with the account's id and a count.

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

## ADR-020 — Folders and rules: Sieve run by Sealwax (2026-10-09)

**Status:** Accepted — user decision, 2026-10-09 ("In Sealwax (Recommended)", "Create, rename, delete (Recommended)"); the design is [m9/folders-rules.md](m9/folders-rules.md), built in parts A1, A2, B1 and B2.

**Decision:**
- **Folders:** New Folder (or Subfolder), Rename Folder and Delete Folder, online only, on the account's command connection: IMAP CREATE, RENAME and DELETE with SUBSCRIBE and UNSUBSCRIBE; local folders for POP3 accounts. The store follows in the same session (a rename keeps the messages). Delete always asks, with the number of messages. The sidebar shows subfolders as a foldable tree.
- **Rules are Sieve** (RFC 5228) with `fileinto`, `copy`, `imap4flags`, `environment` and `imapsieve`'s `imap.mailbox`; one script per account in `$XDG_CONFIG_HOME/sealwax/rules/<account key>.sieve`, written by the Rules dialog in Roundcube's `# rule:[…]` form and editable by hand. **Sealwax runs it** on new mail in the Inbox and in Junk and on request for a folder; its actions are ops in the op queue (ADR-015). Spam rules are an "always junk" list (to Junk with `$Junk`) and a "never junk" list that brings mail from Junk back to the Inbox with `$NotJunk`.
- No new keys (ADR-017); the commands are in the palette and the right-click menus of folder rows, account headings and message rows.

**Reason:** the user asked for industry standards; Sieve is the standard language for mail rules, and Roundcube's comment form is the most common editor's. Running it in Sealwax works on every account, Outlook.com and POP3 included, where ManageSieve would cover only servers that have it and could replace a script another program made.

**Alternatives:** ManageSieve upload (RFC 5804), alone or beside the local run (rules running while the computer is off; not on Outlook.com) — Later, a new user decision; a Sealwax-only rule format (Thunderbird's way) — not a standard.

**Consequences:** rules run only while Sealwax runs (background mode is on by default) and see headers, not bodies; a header beyond the stored ones costs one header fetch for the new messages. The pure language lives in `domain::sieve`, the file in `infrastructure`, the run in `engine`.

### ADR-020 amendment — the Sieve subset as built (2026-10-09)

`domain::sieve` (A2, [report](m9/sieve.md)): a hand-written parser for RFC 5228's whole grammar; a checker for the subset — `require` (`fileinto`, `copy`, `imap4flags`, `environment`, `imapsieve`, and the two comparators' names), `if`/`elsif`/`else`, `stop`, `keep`, `fileinto` (`:copy`), `discard`, `addflag`/`setflag`/`removeflag`; tests `address` (on address headers only), `header`, `exists`, `size`, `allof`, `anyof`, `not`, `true`, `false`, `hasflag`, `environment` (`imap.mailbox` only; other items are false); `:is`, `:contains`, `:matches`; `i;ascii-casemap` and `i;octet` — refusing everything else by name; and an evaluator. Decided while building: flags start as the message's own (RFC 6785) and each delivery reports its own change to add and remove; `fileinto` of the arrival folder is a keep; `:flags` on `keep`/`fileinto` is refused (use `addflag` first); `\Deleted` can't be set and flags must be IMAP keywords; scripts are at most 1 MiB and nest at most 32 levels. The rules model writes Roundcube's form with the actions in a fixed order (flags before `fileinto`), "From is/begins/ends/matches" as `address` and "contains" as `header`, the junk actions removing the opposite keyword as triage does; the never-junk section is always written first and ends with `stop`, always junk next, each under a `# Sealwax: …` marker. Reading back takes a rule only when writing it again gives the same commands; the rest stays as hand-written text in place. Folder names are the store's decoded full names, as `fileinto` takes them. Merged as `4d6239d`; CI on `main` 1,422 passed, 0 failed.

### ADR-020 amendment — the rules on new mail as built (B1, 2026-10-09)

[Report](m9/rules-engine.md). The engine runs an account's script on the mail its passes report as new in the Inbox or Junk, IMAP and POP3, after Sealwax's own moves are taken out and before `Event::NewMail`; a message a rule moves to Junk or Trash isn't new mail, one it files elsewhere still is; mail rescued from Junk into the Inbox doesn't notify (its arrival in the Inbox is Sealwax's own move). The first pass's backlog is never run. Tests see the stored headers (From, To, Cc, Reply-To, Subject, List-Id, Message-ID, In-Reply-To, References); when the script names another, the messages' full headers come from the stored original or one `UID FETCH … BODY.PEEK[HEADER]` on the body connection (`domain::message::header_fields`: unfolded and RFC 2047-decoded), and a message whose headers can't be had runs on the stored ones. What the script decides becomes one move (the first `fileinto` without `:copy`), copies for the rest, flags, or a move to Trash for `discard`; a `fileinto` of a folder the account lacks is a keep (RFC 5228 section 4.1); keywords outside Sealwax's flags are left out and counted in the log. `Command::ApplyRules` runs the script on a folder's stored messages as if they arrived there (`Event::RulesApplied` with the number changed). The script is compiled once per change of its file, and a file that can't run is reported once per change and runs nothing.
