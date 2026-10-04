# Testing & Acceptance

Synced: 2026-10-04

Evidence rules ([AGENTS.md](../AGENTS.md#evidence)): record only what was actually run, with date, command, machine and caveats. "Not verified" is an acceptable result; a guessed number is not.

## Test layers

| Layer | What | Where | Runs in CI |
| --- | --- | --- | --- |
| Unit | Pure logic: threading, query parser, autoconfig XML, reply/forward building, format=flowed, mailto, palette and CSS | `domain/` (`cargo test`), insta snapshots, proptest | Yes |
| Store | Migrations, writer/reader split, overlays, crash consistency (`kill -9` during writes) | `infrastructure/` with `tempfile` | Yes |
| Protocol (scripted) | IMAP, SMTP and POP3 against scripted mock servers: capabilities, auth, fallbacks, faults | `engine/` tests | Yes |
| Protocol (testbed) | Real servers in Docker: Dovecot 2.4, Stalwart, an IMAP4rev1-only server, a mail catcher; randomized two-client and fault-injection suites | `tools/testbed/` + `spikes/s1-imap/tests/testbed.rs` (`engine/` from M3) | Not yet: `tools/testbed/up.sh && tools/testbed/seed.sh`, then `CARGO_TARGET_DIR=target/s1 cargo test -p email-spike-s1 --release -- --ignored --test-threads=1` |
| UI (headless) | The app on GTK Broadway with a private D-Bus bus, self-terminating; self-test scripts that drive `ActionId`s (from M1) | `tools/headless.sh`, `bash tools/ci.sh --ui` | Yes |
| Security | Hostile-HTML suite against a local HTTP canary; secret grep over config, database and logs; outgoing-mail checks (no Bcc, no hostname) | `app/` and `engine/` tests | Yes |
| Live | Real Wayland session (NVIDIA rendering, notifications, activation, fcitx5) and real test accounts | Manual or agent with authorization | No |

## Headless runs

- Always through `tools/headless.sh <display> <cmd...>`: Broadway backend, a private D-Bus bus that starts no services, `GSETTINGS_BACKEND=memory`, `ADW_DISABLE_PORTAL=1`, a time limit (`MAIL_HEADLESS_TIMEOUT`, default 300 s).
- The app exits by itself after `MAIL_EXIT_AFTER_MS`; the CI smoke run greps for `first-frame`.
- Parallel agents use different display numbers; CI uses `MAIL_CI_DISPLAY` (default 19).
- WebKitGTK renders under `tools/headless.sh` in software (GTK has no GL there, so WebKit turns acceleration off). Security checks, heights, `cid:` loads and snapshots work headless. Without a Broadway client attached GTK paints about once per second, so frame timing, open-to-paint latency and scrolling are measured on the live session; headless WebView tests wait for events and allocations, never fixed delays (S2).

## Hostile-HTML suite (ML-065)

47 cases, from `spikes/s2-webkit/src/hostile.rs` until M5 moves them into the app's tests. Each points its network vectors at a local HTTP canary (127.0.0.1, random port) that counts connections, requests per path, connections that send nothing, and TLS ClientHellos; each also carries a script payload with four traces (title, `data-pwned`, `alert()`, a beacon). Clicks are DOM `click()` calls from Mail's isolated world.

Pass with the production reader: 0 connections (not just 0 requests — WebKit pre-connects), 0 TLS hellos, 0 script traces, no allowed navigation, new window or download; the `cid:` test image renders and a foreign `cid:` part never loads. In the same run, a no-defence control must show the vectors are live, and a remote-allowed control (CSP widened with the canary's origin, no filter) must load images only.

## Performance targets

Measured with release builds unless noted; each entry names its spike or milestone.

| Area | Target | Where measured |
| --- | --- | --- |
| Startup | First frame with a cached 10k INBOX ≤ 400 ms | S6, M9 |
| List | p99 frame ≤ 16.7 ms while scrolling 100k messages at 3440×1440; jump to end ≤ 100 ms; one-insert diff ≤ 20 ms | S3, M1, M9 |
| Folder open | ≤ 100 ms | S3 |
| Open message | Plain text ≤ 50 ms; cached HTML ≤ 150 ms p95 | S2, M5 |
| Search | First 50 results ≤ 100 ms p95 over 100k messages | S3, M7 |
| Ingest | ≥ 2,000 headers/s; FTS ≥ 1,000 messages/s | S3 |
| Memory | UI ≤ 150 MB (metric open since S3: an empty GTK4 window is 120 MiB RSS here; proposed PSS); ≤ 300 MB with a 10-message conversation (web processes included); < 20 MB growth after opening 200 messages | S2, S3, M9 |
| Sync | 10k-header first sync ≤ 20 s (testbed); no-change re-sync ≤ 200 ms (CONDSTORE) / ≤ 2 s (UID diff) | S1 |
| Push | New mail visible ≤ 3 s (S1, testbed), ≤ 5 s (product) | S1, M3 |
| Composer | Keystroke to paint < 16 ms, including fcitx5 | S7 |
| Autodetect | ≥ 45/50 correct; p50 ≤ 2 s, p95 ≤ 6 s, cap 10 s | S4 |
| Resume | Reconnect ≤ 10 s after suspend | S6, M3 |

## Acceptance by milestone

The exit criteria in [PLAN.md](PLAN.md#milestone-plan) are the acceptance list. Results go here as dated entries, newest first, with links to `docs/spikes/` reports and screenshots in `tools/progress/`.

## Results

### S1 IMAP library and sync model (ML-009) and the testbed (ML-005) — 2026-10-04

Release build, localhost testbed (Dovecot 2.4.5 full, Dovecot IMAP4rev1-only, Stalwart 0.16.24), Ryzen 9 5900X; final run 14:54–15:03 on a freshly recreated and reseeded testbed. Evidence: `spikes/s1-imap/evidence/2026-10-04.log` (produced by `evidence/run.sh`); report [spikes/S1-imap.md](spikes/S1-imap.md).

| Bar | Result | Verdict |
| --- | --- | --- |
| Library comparison | written; async-imap recommended with a mandatory raw layer | Done |
| 10k-header first sync ≤ 20 s | 0.31 s Dovecot, 0.45–0.46 s Stalwart, 0.33–0.34 s IMAP4rev1-only; 1.77–1.94 s at 50 ms simulated RTT (7.1–7.5 MB on the wire) | Pass |
| No-change re-sync ≤ 200 ms (CONDSTORE) | p50 7.2 ms Dovecot, 0.5 ms Stalwart (20 runs, max 17.2 ms); 53–59 ms at 50 ms RTT | Pass |
| No-change re-sync ≤ 2 s (UID diff) | p50 29.9 ms, max 37.9 ms; 659 ms at 50 ms RTT | Pass |
| IDLE new mail ≤ 3 s | p50 493 ms Dovecot (the server's own ~0.49 s delay), 1.0 ms Stalwart (20 samples) | Pass |
| Randomized two-client test | 0 divergences in 30 of 30 runs (10 seeds × 3 servers, 1,000 ops and one UIDVALIDITY reset each); two deliberately broken builds gave 327 and 339 divergences | Pass with the INBOX STATUS backstop (without it Stalwart diverged in 2 of 10) |
| Connection cut mid-FETCH / IDLE / APPEND (and a lost MOVE reply) | nothing lost or duplicated with UIDNEXT-window recovery | Pass with that recovery; the Message-ID search duplicated on Stalwart |
| Schema checked against real servers | testbed: both Dovecots and Stalwart | Partly — real providers pending test accounts |

`cargo test -p email-spike-s1`: 27 unit tests pass; the 8 testbed tests (`--ignored`) passed when run; fmt and clippy clean (re-run by the lead). The testbed was left down; the agent removed only the anonymous volumes its own early probe containers created.

### S2 WebKit reader (ML-010) — 2026-10-04

Release builds; 13 short self-closing live runs on Hyprland (3440×1440 at 144 Hz, NVIDIA; one stopped by the harness's own 55 s watchdog and not used) and headless runs on display 21. Corpus: 55 synthetic newsletters and 8 simple messages. Report: [spikes/S2-webkit.md](spikes/S2-webkit.md); raw logs in `spikes/s2-webkit/results/`.

| Bar | Result | Verdict |
| --- | --- | --- |
| 25+ hostile cases: 0 canary requests, 0 page scripts | 47 cases: 0 connections, 0 TLS hellos, 0 script traces; the no-defence control showed traffic in 37 cases and scripts in 11 | Pass with the ADR-011 S2 amendment (pre-click connections) |
| `cid:` images render | 158/158 in 63 bodies; a foreign part never loads | Pass |
| No blank frames, 50 newsletters | 0 of 63 bodies blank (WebKit snapshot and GTK render), 0 of 10 in conversations | Pass |
| Scroll ≤ 16.7 ms p95 | p95 6.95 ms at 144 Hz (2 of 1,146 frames over 16.7 ms), GPU and CPU rendering | Pass |
| Height ±1 px, 50/50 | 63/63 (GPU, CPU and headless) | Pass with the sizing fixes |
| Open ≤ 150 ms p95 | load → first WebKit frame p95 17.4 ms; first GTK paint at final height p95 32.6 ms (CPU rendering 56.9 ms) | Pass |
| ≤ 300 MB with 10 views | 10 tall newsletters: 804 MiB RSS GPU / 628 MiB RSS CPU (PSS 512 / 377); 10 short messages, GPU: 711 MiB; without process sharing 3,413 MiB | **Fail** — metric and design to decide |
| Renders under `tools/headless.sh` | yes, in software; timing not meaningful there | Pass |

`cargo test -p email-spike-s2`: 19 tests pass; fmt and clippy clean (re-run by the lead).

### S3 store and message list (ML-011) — 2026-10-04

Release builds; Ryzen 9 5900X, RTX 3070 Ti, 3440×1440 at 144 Hz, system SQLite 3.53.4, GTK 4.22.4; 100k synthetic messages over 5 years (29.4 % unread, 21.3 % with attachments, Norwegian/English plus about 10 % Chinese and Japanese, 40,548 conversations). Report: [spikes/S3-store.md](spikes/S3-store.md).

| Bar | Result | Verdict |
| --- | --- | --- |
| Ingest ≥ 2,000 headers/s | 19,116 headers/s (batches of 500, threading, counts and 7 indexes included) | Pass |
| FTS ≥ 1,000/s | 10,324 messages/s alone; 5,733/s sharing the header transaction | Pass |
| Folder open ≤ 100 ms | ≤ 4.5 ms cold (page cache evicted, new connection), ≤ 0.34 ms warm | Pass |
| List p99 frame ≤ 16.7 ms | presented-frame interval p99 7.94 ms at 1,500 px/s, 7.96 ms at 12,000 px/s; a 5,000 rows/s sweep p99 48–57 ms with text rows, 7.94 ms with placeholder rows | Pass at normal speeds; the full sweep only with placeholder rows |
| Jump to end ≤ 100 ms | cache dropped, until the last row is painted: p50 44–51 ms, p95 ≤ 56 ms (the row exactly as mocked: p50 93 ms) | Pass with the row changes |
| One-insert diff ≤ 20 ms | store p50 0.38 ms, p99 3.4 ms (38 ms under SQLite's default autocheckpoint); live request → row on screen p50 3 ms | Pass with the checkpoint handoff |
| First 50 FTS results ≤ 100 ms p95 | date-first plan: ≤ 10.3 ms warm, ≤ 34 ms cold (FTS-first: 116 ms cold) | Pass |
| UI ≤ 150 MB | RSS 158 MiB at first paint, 202 MiB after the 40 s script; PSS 91–100 → 133 MiB; an empty GTK4 + libadwaita window is already 120 MiB RSS (Vulkan; 216 MiB with GL) | **Missed as RSS** — metric to be decided |
| System SQLite FTS5 | present, with `contentless_delete` and trigram | Pass |

`cargo test -p email-spike-s3`: 14 tests pass (migrations; threading merges; aggregates after inserts, flag changes and expunges; insert-diff positions; FTS for æøå, CJK bigrams and prefixes); fmt and clippy clean (re-run by the lead). Nine self-closing live runs, programmatic scrolling only; the headless smoke run on display 22 passes. Live run 1 (a script bug) and the steady-scroll phase of live run 4 (the compositor sent no frames for 7.3 s) are discarded.

### S7 composer and MIME (ML-015) — 2026-10-04

Release builds; four headless runs (display 23) and five self-closing live runs on Hyprland (DP-2, 3440×1440 at 144 Hz, NVIDIA); no synthetic input in the live session. Report: [spikes/S7-composer.md](spikes/S7-composer.md).

| Bar | Result | Verdict |
| --- | --- | --- |
| Formatting commands | 98 checks live, 102 headless | Pass |
| Toolbar follows the selection | caret move → toolbar update, live p50 0.77–1.57 ms, p95 1.46–13.83 ms | Pass |
| Undo | every format; a typed run and a paste each undo in one step | Pass (the app must bind Ctrl+Z, Ctrl+Shift+Z/Ctrl+Y, Ctrl+U) |
| Paste cleaned | hostile HTML stripped to the allowlist, 0 canary requests | Pass |
| Typing < 16 ms | accelerated p95 16.0–24.1 ms; `hardware-acceleration-policy = NEVER` p95 9.9–11.9 ms | Pass without acceleration only |
| fcitx5 and Norwegian dead keys | `bash spikes/s7-composer/manual-check.sh` | Pending the user |
| Round trip through mail-parser | 12 `.eml` files | Pass |
| No Bcc in the transmitted copy; no hostname | 0 hits | Pass |
| Gmail web / Outlook web / Thunderbird | samples ready | Pending test accounts |

**Method:** an editing command sent by the app, timed to the end of the first frame that shows it; a one-child probe widget around the WebView marks frames where it drew; the frame clock's after-paint ends the frame; caret blink off; 50 ms without frames before each edit; a pixel pass checks that the first frame shows the edit (19–20 of 20 per run). Headless Broadway frame time grows with WebView area (about 18 ms at 1100×595, 3.3 ms at 535×195), so paint latency is judged live only. `cargo test -p email-spike-s7`: 10 tests pass; fmt and clippy clean (re-run by the lead).

**Harness change:** the S7 probe's first headless run wrote WebKit data to `~/.cache/email-spike-s7` and `~/.local/share/email-spike-s7`, because `tools/headless.sh` passed the user's XDG folders through. Since 2026-10-04 the wrapper gives every run scratch XDG folders under `.local/headless-<display>/`.

### S4 autodetect (ML-012) — 2026-10-04

Release builds, 12:01–12:25 UTC, residential network in Norway; report [spikes/S4-autodetect.md](spikes/S4-autodetect.md).

| Bar | Result | Verdict |
| --- | --- | --- |
| ≥ 45/50 correct | 42/50 (32/50 without presets) | Missed — all misses Norwegian |
| 50/50 TLS-only | 50/50 in every run | Pass |
| p50 ≤ 2 s / p95 ≤ 6 s / cap 10 s | 134 ms / 5,008 ms / max 5,096 ms (tuned: 56 ms / 3,162 ms) | Pass |
| Dummy addresses only; full address only to the domain's own HTTPS autoconfig | 5,658 requests logged, 0 violations | Pass |
| ≤ 2 login attempts | 0 (the probe can only send CAPABILITY, STARTTLS and EHLO) | Pass |

`cargo test -p email-spike-s4`: 40 tests pass; fmt and clippy clean (re-run by the lead).

### SecretStore (ML-006) — 2026-10-04

- `cargo test -p email-engine`: `memory_store_round_trip` and `memory_store_keeps_accounts_apart` pass.
- `cargo test -p email-engine -- --ignored keyring_round_trip` in the desktop session: passes against gnome-keyring 50 (0.05 s) under the test app-id `io.github.pixdevsapps.Mail.Devel.test`; `secret-tool search --all application io.github.pixdevsapps.Mail.Devel.test` afterwards finds no items.
- `tools/headless.sh 24 cargo test -p email-engine -- --ignored falls_back`: passes; on the private bus `Secrets::open` returns the memory store and the round trip works.

### Workspace scaffold — 2026-10-04

- `bash tools/ci.sh`: fmt, clippy `-D warnings`, tests (1 test: the engine runtime runs a task) and build passed.
- `MAIL_CI_DISPLAY=19 bash tools/ci.sh --ui`: passed; the placeholder window logged `first-frame 46.2` (debug build, Broadway). Harness check only, not a startup measurement. No `gtk4-broadwayd` left running.
- Machine: Omarchy 4.0.4, Rust 1.98.1, GTK 4.22.4, libadwaita 1.9.3.
