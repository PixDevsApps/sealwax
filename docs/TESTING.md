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
