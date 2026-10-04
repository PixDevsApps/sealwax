# Architecture Decisions

Synced: 2026-10-04

The repository docs are the canonical project memory ([ADR-012](#adr-012--repository-docs-canonical-notion-as-a-mirror-2026-10-04)). Dated entries describe the state at that date; newer entries and amendments take precedence.

---

Use this page for decisions with meaningful long-term cost. Keep each entry short, dated and explicit about alternatives. Change an accepted decision with a dated `###` amendment under it; do not rewrite history. Status moves Proposed → Accepted when the user decides or a spike provides the evidence; say which. Library facts below were checked against crates.io and the crate sources on 2026-10-04 ([RESEARCH.md](RESEARCH.md#library-verification-2026-10-04)).

| ADR | Title | Status (2026-10-04) |
| --- | --- | --- |
| 001 | Standalone native application | Accepted (user request) |
| 002 | GTK4 + libadwaita + WebKitGTK 6 | Proposed; S2 and S7 confirm |
| 003 | Layering with an engine crate; tokio only there | Proposed |
| 004 | Follow the live Omarchy theme | Accepted (user request) |
| 005 | SQLite with FTS5 plus a blob store | Proposed; S3 confirms |
| 006 | IMAP library and state-driven sync | Proposed; S1 decides the library |
| 007 | SMTP, MIME and privacy defaults | Proposed; S7 confirms |
| 008 | OAuth2 and secrets | OAuth for Google and Microsoft accepted (user decision); mechanism proposed; S5 is go/no-go |
| 009 | Settings in files: config.toml, keys.toml, accounts.toml | Proposed |
| 010 | Name and app-id | Open; the user decides in M0 |
| 011 | Reading pane: approach A with hardened WebKit | Proposed; S2 decides |
| 012 | Repository docs canonical, Notion as a mirror | Accepted (user decision) |
| 013 | Lean 1.0 scope | Accepted (plan approval) |
| 014 | WYSIWYG composer in an editable WebKit view | Format accepted (user decision); mechanism proposed; S7 confirms |
| 015 | Op queue: optimistic overlay, Undo, at-most-once send | Proposed |
| 016 | Conversation threading | Proposed |
| 017 | Keymap: scoped one-key shortcuts under Omarchy rules | Proposed |
| 018 | Logging and privacy | Accepted (plan approval) |
| 019 | Corner radius follows Hyprland | Proposed; decided at the DESIGN gate |

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

## ADR-003 — Layering with an engine crate; tokio only there (2026-10-04)

**Status:** Proposed.

**Decision:** `email-domain` ← `email-infrastructure` ← `email-engine` ← `email` (the app).
- domain: pure logic.
- infrastructure: local std I/O only (store, blobs, configuration, Omarchy readers, logging).
- engine: the only tokio user (multi-thread runtime, 2 workers): network, sync, the op queue, the outbox and secrets. Parsing and indexing run in `spawn_blocking`; the store writer is its own std thread.
- app: GTK only. It talks to the engine with plain `Send` messages over `async-channel` (commands in, coalesced events out). No tokio type crosses into the app.

**Reason:** Stet avoids tokio, but mail can't: async-imap, reqwest, hickory-resolver and oo7 all need a runtime (Hertz already runs tokio in its infrastructure). Isolating it keeps the GTK thread clean and the app crate testable.

**Consequence:** every engine API is message-based. The GTK thread never runs SQL; reads go through a pool of two read-only connections on worker threads.

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

## ADR-005 — SQLite with FTS5 plus a blob store (2026-10-04)

**Status:** Proposed. S3 confirms throughput and checks that the system SQLite has FTS5 (otherwise rusqlite's `bundled` feature).

**Decision:** rusqlite 0.40 in WAL mode with `user_version` migrations that refuse a newer schema (Hertz `infrastructure/src/storage.rs`), FTS5 for search, one writer thread and two read-only readers. Raw messages live in a content-addressed blob store: folders 0700, files 0600.

**Reason:** one well-understood embedded database for metadata, overlays, the op queue and search; raw `.eml` files keep attachments streamable and the database small.

**Consequence:** the schema is checked against real servers in S1 before M1 builds on it.

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

## ADR-007 — SMTP, MIME and privacy defaults (2026-10-04)

**Status:** Proposed. S7 confirms the output in Gmail web, Outlook web and Thunderbird.

**Decision:**
- lettre 0.11 with `default-features = false` and `smtp-transport`, `tokio1-rustls`, `rustls-platform-verifier`, `ring`; `relay()` for port 465, `starttls_relay()` (Required) for 587; `hello_name` is the literal 127.0.0.1. On an XOAUTH2 failure, refresh the token and retry once (lettre doesn't answer the error challenge).
- mail-builder 1.0 with `default-features = false` (no `gethostname`); the MIME tree is built by hand as `mixed{alternative{text, related{html, inline…}}, attachments}`, because its shortcut API puts inline parts in multipart/mixed.
- Message-IDs are ours, using the sender's domain. **The transmitted copy never carries a Bcc header.**
- mail-parser 0.11 with `full_encoding` (CJK charsets); format=flowed (RFC 3676) decoded by our own code in domain.

**Consequence:** no hostname in Message-ID, EHLO or Received headers. Certificate pinning for SMTP isn't possible with lettre (TlsParameters takes no custom rustls config), so pinned exceptions (Proton Bridge) wait for 1.x.

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

## ADR-009 — Settings in files: config.toml, keys.toml, accounts.toml (2026-10-04)

**Status:** Proposed.

**Decision:** Stet's pattern: hand-editable `config.toml` and `keys.toml` under `$XDG_CONFIG_HOME/<app>/`, live-reloaded, with mistakes reported by line and column and a faulty file never applied. Mail adds `accounts.toml`, written by the setup assistant and editable by hand. None of them ever holds a secret.

**Reason:** Omarchy configures everything through files; a settings window doesn't fit (Stet ADR-013 critique M7). Account setup still needs a guided UI, so the assistant writes the file.

**Consequence:** a grep test asserts that no secret reaches any of these files.

## ADR-010 — Name and app-id (2026-10-04)

**Status:** Open. The user decides during M0; it must be accepted before M0 closes, because the data paths, the D-Bus name, the keyring schema and the OAuth consent screen depend on it.

**Working values:** name "Mail", crates `email-*`, binary `email`, environment prefix `MAIL_`, app-id `io.github.pixdevsapps.Mail` (`.Devel` for debug builds).

**Rules:** `io.github.pixdevsapps.<Name>`; never `org.omarchy.*`, `TUI.*` or an `oma*` prefix; check for collisions with existing mail software and Arch packages.

## ADR-011 — Reading pane: approach A with hardened WebKit (2026-10-04)

**Status:** Proposed. S2 decides; approach B is the fallback.

**Decision (approach A):**
- GTK widgets draw headers, attachment cards, banners and the quick reply. Plain-text mail uses a GTK text view and no WebView.
- Each expanded HTML body gets a pooled WebView. All share one web process (`related-view`) and an ephemeral network session. Height comes from an isolated-world ResizeObserver.
- Security layers: `enable-javascript-markup = false`; the construction-time `default-content-security-policy` `default-src 'none'; img-src cid: data: https:; style-src 'unsafe-inline'; font-src cid: data:; form-action 'none'; base-uri 'none'; frame-src 'none'`; a content filter blocking http(s) until remote content is allowed; `decide-policy` blocking every navigation and opening links with `gtk::UriLauncher`; `cid:` registered as a URI scheme per view; DNS prefetching, hyperlink auditing, page cache, storage, WebGL, audio, media and developer extras (release) off.
- lol_html only feeds the dark-mode heuristic: HTML with its own colours sits on a light "paper" card; simple HTML and plain text take the theme colours.

**Alternative (B):** one WebView per conversation with sandboxed `srcdoc` iframes — fewer views, but `cid:` collisions across messages and weaker native focus and accessibility.

**Consequence:** no HTML sanitizer is needed for display; the layers above do the work. The hostile-HTML suite (25+ cases) guards them in CI.

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

## ADR-014 — WYSIWYG composer in an editable WebKit view (2026-10-04)

**Status:** The WYSIWYG format is accepted — user decision, 2026-10-04 (over a Markdown composer). The mechanism is proposed; S7 confirms it.

**Decision:** an editable WebKit view (separate from the reader views) using WebKit's native editing commands, with toolbar state from `EditorState`. A small isolated-world script serialises the HTML, cleans pasted content and turns inline images into `cid:` parts. Each message goes out as multipart/alternative with an html2text plain-text part ([ADR-007](#adr-007--smtp-mime-and-privacy-defaults-2026-10-04)). Spell check uses WebKit's enchant support when hunspell dictionaries are installed (optdepends).

**Alternatives:** a Markdown composer in GtkSourceView (faster to build; the user preferred WYSIWYG); a GtkTextView with tags and a hand-written HTML serialiser (poor fidelity for quoted HTML).

**Consequence:** M6 grows by about 6–10 days; fcitx5 and Norwegian dead-key typing are part of S7's pass bar.

## ADR-015 — Op queue: optimistic overlay, Undo, at-most-once send (2026-10-04)

**Status:** Proposed.

**Decision:**
- An op row and its local overlay are written in one transaction. Sync always writes what the server says; the UI shows server state plus overlays; sync never clears an overlay whose op is pending.
- Ops are UID-based and idempotent (`+FLAGS` / `-FLAGS`, never a full replace); a MOVE of a UID that no longer exists counts as done. Ops record `(folder, uidvalidity, uid)` and are re-targeted by Message-ID after a UIDVALIDITY change.
- Archive, delete, move and send carry `not_before = now + 5–10 s`; Undo inside the window cancels locally; after it, Undo queues the inverse op. Only the last action is undoable from the toast.
- Per-message FIFO, batched into UID sets per folder; flag ops merge, move chains collapse.
- After a crash, in-flight ops replay idempotently — **except Send, which is at most once**: a message left in "sending" asks the user.

**Consequence:** a crash-during-queue replay test and undo-window tests gate M4.

## ADR-016 — Conversation threading (2026-10-04)

**Status:** Proposed.

**Decision:** threads from References and In-Reply-To with union-find aliases (a parent can arrive after its reply), duplicate Message-IDs merged (Gmail labels, copies to yourself), Outlook `Thread-Index` honoured, Gmail `X-GM-THRID` used only as a hint. Never group by subject alone. Threads over 100 messages collapse their middle.

**Consequence:** property tests cover out-of-order arrival and merges.

## ADR-017 — Keymap: scoped one-key shortcuts under Omarchy rules (2026-10-04)

**Status:** Proposed.

**Decision:** Gmail-style one-key shortcuts (`j`/`k`, `e`, `#`, `r`, `a`, `f`, `c`, `s`, `/`, `g` `i`…) are handled by the list and reader widgets, not as application accelerators, so text fields keep their letters (Stet's ADR-009 amendment found that app accelerators run before the focused widget). Ctrl shortcuts and the command palette (F1 / Ctrl+Shift+P) cover everything. Stet's audit applies, enforced by tests: nothing with SUPER; nothing that needs AltGr on US or Norwegian layouts (no `[ ] { } @ $ | \`); `#` and `/` (Shift+3 and Shift+7 on Norwegian) get unshifted alternatives (Delete, Ctrl+F); every F-key action has a non-F-key; avoid Ctrl+Space, Ctrl+Shift+U, Ctrl+Alt+Shift+U, Ctrl+Alt+P, Ctrl+Alt+F* and Alt+Tab.

**Consequence:** the keymap is data in `keys.toml`; the audit tests run in CI.

## ADR-018 — Logging and privacy (2026-10-04)

**Status:** Accepted — plan approval, 2026-10-04.

**Decision:** logs never contain message bodies, subjects, addresses, folder names, tokens or protocol traces; AUTH lines are always redacted. Development and spikes never put the user's email address or personal data in network requests.

**Consequence:** log statements use account ids and counts. A test greps captured logs from the testbed suites for addresses and secrets.

## ADR-019 — Corner radius follows Hyprland (2026-10-04)

**Status:** Proposed; the DESIGN gate decides.

**Decision:** `corner_radius = "auto" | <px>` in `config.toml`. `auto` reads `decoration:rounding` from `hyprctl -j getoption` at start and after theme changes, as omarchy-shell does (0 on stock Omarchy, 8 on this machine); outside Hyprland it is 0.

**Reason:** stock Omarchy is square (Stet chose square chrome), while the inspiration designs and this desktop are rounded. Following the compositor matches both.

**Alternative:** always square, as Stet. The DESIGN mock shows both.
