# Testing & Acceptance

Synced: 2026-10-04

Evidence rules ([AGENTS.md](../AGENTS.md#evidence)): record only what was actually run, with date, command, machine and caveats. "Not verified" is an acceptable result; a guessed number is not.

## Test layers

| Layer | What | Where | Runs in CI |
| --- | --- | --- | --- |
| Unit | Pure logic: threading, query parser, autoconfig XML, reply/forward building, format=flowed, mailto, palette and CSS | `domain/` (`cargo test`), insta snapshots, proptest | Yes |
| Store | Migrations, writer/reader split, overlays, crash consistency (`kill -9` during writes) | `infrastructure/` with `tempfile` | Yes |
| Protocol (scripted) | IMAP, SMTP and POP3 against scripted mock servers: capabilities, auth, fallbacks, faults | `engine/` tests | Yes |
| Protocol (testbed) | Real servers in Docker: Dovecot 2.4, Stalwart, an IMAP4rev1-only server, a mail catcher; randomized two-client and fault-injection suites | `tools/testbed/` + `engine/` integration tests | Yes, once the testbed is authorized |
| UI (headless) | The app on GTK Broadway with a private D-Bus bus, self-terminating; self-test scripts that drive `ActionId`s (from M1) | `tools/headless.sh`, `bash tools/ci.sh --ui` | Yes |
| Security | Hostile-HTML suite against a local HTTP canary; secret grep over config, database and logs; outgoing-mail checks (no Bcc, no hostname) | `app/` and `engine/` tests | Yes |
| Live | Real Wayland session (NVIDIA rendering, notifications, activation, fcitx5) and real test accounts | Manual or agent with authorization | No |

## Headless runs

- Always through `tools/headless.sh <display> <cmd...>`: Broadway backend, a private D-Bus bus that starts no services, `GSETTINGS_BACKEND=memory`, `ADW_DISABLE_PORTAL=1`, a time limit (`MAIL_HEADLESS_TIMEOUT`, default 300 s).
- The app exits by itself after `MAIL_EXIT_AFTER_MS`; the CI smoke run greps for `first-frame`.
- Parallel agents use different display numbers; CI uses `MAIL_CI_DISPLAY` (default 19).
- Whether WebKitGTK renders under Broadway is part of S2. If it doesn't, the WebView tests need another harness, documented here.

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
| Memory | UI ≤ 150 MB; ≤ 300 MB with a 10-message conversation (web processes included); < 20 MB growth after opening 200 messages | S2, S3, M9 |
| Sync | 10k-header first sync ≤ 20 s (testbed); no-change re-sync ≤ 200 ms (CONDSTORE) / ≤ 2 s (UID diff) | S1 |
| Push | New mail visible ≤ 3 s (S1, testbed), ≤ 5 s (product) | S1, M3 |
| Composer | Keystroke to paint < 16 ms, including fcitx5 | S7 |
| Autodetect | ≥ 45/50 correct; p50 ≤ 2 s, p95 ≤ 6 s, cap 10 s | S4 |
| Resume | Reconnect ≤ 10 s after suspend | S6, M3 |

## Acceptance by milestone

The exit criteria in [PLAN.md](PLAN.md#milestone-plan) are the acceptance list. Results go here as dated entries, newest first, with links to `docs/spikes/` reports and screenshots in `tools/progress/`.

## Results

### Workspace scaffold — 2026-10-04

- `bash tools/ci.sh`: fmt, clippy `-D warnings`, tests (1 test: the engine runtime runs a task) and build passed.
- `MAIL_CI_DISPLAY=19 bash tools/ci.sh --ui`: passed; the placeholder window logged `first-frame 46.2` (debug build, Broadway). Harness check only, not a startup measurement. No `gtk4-broadwayd` left running.
- Machine: Omarchy 4.0.4, Rust 1.98.1, GTK 4.22.4, libadwaita 1.9.3.
