# Testing & Acceptance

Synced: 2026-10-04

Evidence rules ([AGENTS.md](../AGENTS.md#evidence)): record only what was actually run, with date, command, machine and caveats. "Not verified" is an acceptable result; a guessed number is not.

## Test layers

| Layer | What | Where | Runs in CI |
| --- | --- | --- | --- |
| Unit | Pure logic: threading, query parser, autoconfig XML, reply/forward building, format=flowed, mailto, palette and CSS (22 stock themes, contrast rules), the action registry and keymap audit (US and Norwegian), `config.toml` and `keys.toml` | `domain/` (`cargo test`), insta snapshots, proptest | Yes |
| Store | Migrations, writer/reader split, overlays, crash consistency (a SIGKILLed child mid-ingest), change events kept exact against a model | `infrastructure/` (scratch stores under `target/scratch`) | Yes |
| Protocol (scripted) | IMAP, SMTP and POP3 against scripted mock servers: capabilities, auth, fallbacks, faults | `engine/` tests | Yes |
| Protocol (testbed) | Real servers in Docker: Dovecot 2.4, Stalwart, an IMAP4rev1-only server, a mail catcher; randomized two-client and fault-injection suites | `tools/testbed/` + `spikes/s1-imap/tests/testbed.rs` (`engine/` from M3) | Not yet: `tools/testbed/up.sh && tools/testbed/seed.sh`, then `CARGO_TARGET_DIR=target/s1 cargo test -p email-spike-s1 --release -- --ignored --test-threads=1` |
| UI (headless) | The app on GTK Broadway with a private D-Bus bus, self-terminating; `sealwax --self-test` scripts (`tests/selftest/`) that resize, activate actions, assert layout and state, take screenshots, and load fixture stores (`store-dir`, `fixtures`); `app/examples/theme_switch.rs` and `app/examples/palette.rs` | `tools/headless.sh`, `bash tools/ci.sh --ui` | Yes (scripts except `preview*` and `perf-*`) |
| Security | Hostile-HTML suite against a local HTTP canary; secret grep over config, database and logs; outgoing-mail checks (no Bcc, no hostname) | `app/` and `engine/` tests | Yes |
| Live | Real Wayland session (NVIDIA rendering, notifications, activation, fcitx5) and real test accounts | Manual or agent with authorization | No |

## Headless runs

- Always through `tools/headless.sh <display> <cmd...>`: Broadway backend, a private D-Bus bus that starts no services, `GSETTINGS_BACKEND=memory`, `ADW_DISABLE_PORTAL=1`, a time limit (`SEALWAX_HEADLESS_TIMEOUT`, default 300 s).
- The app exits by itself after `SEALWAX_EXIT_AFTER_MS`; the CI smoke run greps for `first-frame`.
- Parallel agents use different display numbers; CI uses `SEALWAX_CI_DISPLAY` (default 19).
- `sealwax --self-test <script>` (ported from Stet) runs the real app on Broadway and a `.sealwax-test` script against it, printing TAP; the commands are in [m1/shell.md](m1/shell.md#self-test-commands-sealwax-test). `tools/ci.sh --ui` runs every `tests/selftest/*.sealwax-test` except `preview*`, which write the gate's screenshots into `tools/progress/public/`. The harness attaches a stand-in Broadway client reporting a 2560×1600 screen (without one Broadway clamps windows to 1024×768), switches `GTK_A11Y=none` to `test` so accessible labels can be checked, turns animations off and waits for a painted frame after every step.
- Self-tests read a scratch Omarchy state (`SEALWAX_OMARCHY_STATE` under the run's scratch root), never the desktop's: the built-in palette until a script runs `theme NAME` (a stock palette from `tests/theme/`, swapped in as `omarchy-theme-set` does); `corners auto|PX` sets the corner radius. Look assertions compare what GTK draws with the applied theme: `assert-color`, `assert-contrast`, `assert-radius`, `assert-font`, `assert-height`, `assert-tooltip` ([m1/polish.md](m1/polish.md)).
- `app/examples/theme_switch.rs` swaps a scratch Omarchy theme (`SEALWAX_OMARCHY_STATE`) as `omarchy-theme-set` does and checks colours, sizes and fonts after each switch; `app/examples/palette.rs` checks the actions, palette, scoped shortcuts, menu, shortcuts dialog and live settings.
- WebKitGTK renders under `tools/headless.sh` in software (GTK has no GL there, so WebKit turns acceleration off). Security checks, heights, `cid:` loads and snapshots work headless. Without a Broadway client attached GTK paints about once per second, so frame timing, open-to-paint latency and scrolling are measured on the live session; headless WebView tests wait for events and allocations, never fixed delays (S2).

## Hostile-HTML suite (ML-065)

47 cases, from `spikes/s2-webkit/src/hostile.rs` until M5 moves them into the app's tests. Each points its network vectors at a local HTTP canary (127.0.0.1, random port) that counts connections, requests per path, connections that send nothing, and TLS ClientHellos; each also carries a script payload with four traces (title, `data-pwned`, `alert()`, a beacon). Clicks are DOM `click()` calls from Sealwax's isolated world.

Pass with the production reader: 0 connections (not just 0 requests — WebKit pre-connects), 0 TLS hellos, 0 script traces, no allowed navigation, new window or download; the `cid:` test image renders and a foreign `cid:` part never loads. In the same run, a no-defence control must show the vectors are live, and a remote-allowed control (CSP widened with the canary's origin, no filter) must load images only.

## Performance targets

Measured with release builds unless noted; each entry names its spike or milestone.

| Area | Target | Where measured |
| --- | --- | --- |
| Startup | Process start → first frame with rows on screen, cached 10k INBOX: ≤ 400 ms on a warm start; ≤ 600 ms on a cold start, measured in M9 (user decision 2026-10-04) | S6, M1, M9 |
| List | p99 frame ≤ 16.7 ms while scrolling 100k messages at 3440×1440; jump to end ≤ 100 ms; one-insert diff ≤ 20 ms | S3, M1, M9 |
| Folder open | ≤ 100 ms | S3 |
| Open message | Plain text ≤ 50 ms; cached HTML ≤ 150 ms p95 | S2, M5 |
| Search | First 50 results ≤ 100 ms p95 over 100k messages | S3, M7 |
| Ingest | ≥ 2,000 headers/s; FTS ≥ 1,000 messages/s | S3 |
| Memory (redefined 2026-10-04 by the user: PSS plus a GPU budget, live views capped) | UI process PSS ≤ 150 MB. Reader: PSS of the whole process tree (app + WebKit web and network processes) ≤ 300 MB with a typical 10-message conversation open and live message views capped (proposed cap: 4 expanded HTML bodies; set when M5 measures it). GPU memory has its own budget, sized from the M5 measurement (reader and composer views render without acceleration, ADR-011 and ADR-014). < 20 MB PSS growth after opening 200 messages. RSS is recorded but not judged: an empty GTK4 window is already 120 MiB RSS on this NVIDIA machine | S2, S3, M5, M9 |
| Sync | 10k-header first sync ≤ 20 s (testbed); no-change re-sync ≤ 200 ms (CONDSTORE) / ≤ 2 s (UID diff) | S1 |
| Push | New mail visible ≤ 3 s (S1, testbed), ≤ 5 s (product) | S1, M3 |
| Composer | Keystroke to paint < 16 ms, including fcitx5 | S7 |
| Autodetect | ≥ 45/50 correct; p50 ≤ 2 s, p95 ≤ 6 s, cap 10 s | S4 |
| Resume | Reconnect ≤ 10 s after suspend | S6, M3 |

## Acceptance by milestone

The exit criteria in [PLAN.md](PLAN.md#milestone-plan) are the acceptance list. Results go here as dated entries, newest first, with links to `docs/spikes/` reports and screenshots in `tools/progress/`.

## Results

### M1 reader (ML-027) and the integrated app — 2026-10-04

Headless on Broadway; report [m1/reader.md](m1/reader.md).

| Check | Result |
| --- | --- |
| Hostile suite, 47 cases, against the real reader (`app/examples/hostile.rs`) | Three runs: as shipped (DOM clicks), with real pointer clicks (8 cases), and in pooled views after Show images. Every run: 0 connections, 0 requests, 0 TLS hellos, 0 script traces, 0 GLib criticals; after Show images, TLS hellos only, in 13 image cases. No-defence control: network in 37 cases, scripts in 11 |
| Live theme switch with an open themed and paper HTML message (`app/examples/reader_theme.rs`, headless) | Passes both ways; restyled in place ~24 ms after the theme applied; the paper card keeps its design |
| Cached open, dry run, release (`app/examples/reader_perf.rs`) | show → on screen p95 13.7 ms settled, 105.9 ms back to back; show → first painted frame p95 102.1 ms settled (paper newsletters 212.7 ms), 146.9 ms back to back. Broadway, software rendering; **not judged** |
| Keys and focus (`app/examples/reader_keys.rs`, real input through Broadway) | Keys reach the Reader scope from a focused message; a text field in a message keeps its letters; the next message's body takes the focus |
| Integrated app (lead: the list's selection drives the reader through `MessageSource`) | `tools/ci.sh --ui`: 473 passed, 0 failed, 11 ignored; all 13 self-test scripts pass, reader 83/83; no Gtk-CRITICAL or Gtk-WARNING in the log |

### M1 message list (ML-026) — 2026-10-04

Debug builds for tests and self-tests, release builds for the dry runs; headless on Broadway; fictional data only. Report: [m1/list.md](m1/list.md).

| Part | Evidence | Verdict |
| --- | --- | --- |
| Store in the app, list, keys | 18 unit tests; self-tests list-sections 57/57, list-chips 71/71, list-search 55/55, list-select 56/56, list-insert 42/42, list-first-frame 21/21, list-churn 32/32 (change events checked against the store's own view) | Pass headless; real key presses not verified |
| 100k list (`perf-list`, headless dry run) | work per frame p99 0.23 ms at 1,500 px/s, 4.21 ms at 12,000 px/s, 4.90 ms in a 5,000 rows/s sweep; jump to end p50 89.6 ms, to start 105.2 ms; folder open p50 63.8 ms; insert → row on screen p50 8.2 ms | **Not judged**: Broadway; the live run is pending |
| Startup (headless dry run) | 10k store: first-frame p50 137.0 ms (11 runs); empty store 91.9 ms | **Not judged** |
| After the merge (lead) | `tools/ci.sh --ui`: 447 passed, 0 failed, 11 ignored; all 12 self-test scripts pass | Pass |

### M1 store, blob store and fixtures (ML-023, ML-024, ML-025) — 2026-10-04

Release builds, headless; 100,000 synthetic messages in one folder (`sealwax-fixtures import --count 100000 --seed 1`), then 50,000 more in a second account; system SQLite 3.53.4; other agents building in parallel (load 2–10). Report: [m1/store.md](m1/store.md).

| Bar | Result | Verdict |
| --- | --- | --- |
| Ingest ≥ 2,000 headers/s | 19,986 headers/s in the writer; 2,695 messages/s end to end from raw bytes (durable blobs, parsing, rows, full text; 16 workers) | Pass |
| FTS ≥ 1,000/s | 7,487 messages/s | Pass |
| Folder open ≤ 100 ms | cold ≤ 3.25 ms (INBOX), ≤ 15.95 ms (Attachments filter); warm p95 ≤ 0.53 ms | Pass |
| First 50 FTS results ≤ 100 ms p95 | warm p95 ≤ 9.53 ms over 11 query shapes (≤ 13.76 ms at 150k); cold single runs ≤ 56.4 ms | Pass |
| One-insert diff ≤ 20 ms | writer p99 4.23 ms, max 16.23 ms after cache evictions; warm p99 3.43, max 3.84 ms; two accounts p99 6.08 ms, 2 of 400 over 20 ms (cold cache) | Pass at p99 |
| `kill -9` leaves a consistent database (ML-023) | 6 SIGKILL rounds, some between a header commit and its full-text commit: `integrity_check` ok, FTS integrity-check ok, committed messages present, blobs match their keys, a reopen passes the recount and re-indexes what the kill left | Pass |

`cargo test`: domain 19 (4,500 mutated inputs never panic), infrastructure 29 (+1 ignored child process), fixtures 8; after the merge `tools/ci.sh --ui` passes (425 tests, every self-test). Tests write under `target/scratch` because `/tmp` is tmpfs here, where fsync costs nothing.

### M1 phase A: theme, actions, shell (ML-018 to ML-022, ML-028) — 2026-10-04

Built on three branches in parallel and merged by the lead (reports: [theme](m1/theme.md), [actions](m1/actions.md), [shell](m1/shell.md)). Debug builds, headless on Broadway.

| Part | Evidence | Verdict |
| --- | --- | --- |
| Theme follower and mail roles (ML-018, ML-019) | Domain 48 unit + 16 integration tests (insta snapshots of five themes, contrast rules over all 22 stock themes: secondary text and selected chip ≥ 4.5:1, initials ≥ 4.5:1 for any accent); infrastructure 31 tests (the cascade matches `omarchy-theme-color` for all 22 themes; helper deadlines); `theme_switch` headless: dark → light → dark → built-in, every check passed, applied ~178 ms after the swap (150 ms debounce); live read-only `theme_dump`: 8.8–9.0 ms | Pass headless; the live dark↔light switch with WebViews waits for the reader |
| Actions, palette, settings (ML-022) | Domain 43, infrastructure 5, app 15 tests (keymap audit for US and Norwegian); `palette` example 36 of 36 checks, five runs, and again after the lead moved preferences onto the shared watcher | Pass; real key presses not verified (GTK can't synthesize them) |
| Shell, window, packaging (ML-020, ML-021, ML-028) | Self-tests: window 107/107, window-controls 24/24, activation 62/62 (second launch, `mailto:` from a second instance and D-Bus `Open` with `attach` dropped, background hold, D-Bus activation on a private bus), signals 17/17; `desktop-file-validate` clean; `package-source.py` deterministic | Pass; `makepkg` not run yet |
| `makepkg` build (lead, authorized, no install) | `python3 tools/package-source.py` (commit `02c3e6e`), then `makepkg --cleanbuild --force --nodeps` in `.local/package/`: release build 48.6 s; `check()` ran the GTK-free crates' tests, 153 passed, 0 failed, 2 ignored; `sealwax-0.0.1-1-x86_64.pkg.tar.zst` (1.25 MB) holds the binary, desktop file, D-Bus service, the icon at six sizes, docs and the license. Without `--nodeps` makepkg stopped at `rust` and `cargo` (rustup's toolchain isn't a pacman package) | Pass (ML-028) |
| Shell visual pass (ML-021, [report](m1/polish.md)) | One `sw-` vocabulary, `style.css` free of literals; `theme.sealwax-test` 84/84 in CI (colours by role, contrast, corners measured 0.0/6.1/8.1 px for 0/6/8, fonts, tooltips with keys); compose contrast 5.57:1 (matte-black) and 5.90:1 (catppuccin-latte), was 2.63:1; eight themed previews in `tools/progress/public/` | Pass headless; tooltips, Yaru icons and Hyprland rounding not checked live |
| After merging and wiring (lead) | `tools/ci.sh --ui`: 371 tests passed, 0 failed, 10 ignored; all four self-tests pass; the app reads and applies the theme at startup (9.2 ms read, 20 ms applied), then `first-frame` | Pass |

### S6 desktop integration (ML-014) — 2026-10-04

Release builds, 16:00–16:18 CEST: 18 self-closing live runs on Hyprland under the live-window authorization (127 windows, most under 1 s; 3 fictional low-urgency notifications), headless runs on Broadway display 26, Dovecot and Stalwart from the testbed (15:57–16:21, removed afterwards). Report: [spikes/S6-desktop.md](spikes/S6-desktop.md); logs in `spikes/s6-desktop/evidence/`.

| Bar | Result | Verdict |
| --- | --- | --- |
| First frame with a cached 10k INBOX ≤ 400 ms | Process start → first frame with rows on screen, fullscreen 3440×1440, Vulkan: warm p50 467 / p95 490 ms (20 runs); cold-ish p50 570 / p95 580 ms (12 runs); with the first frame limited to 40 rows, warm p50 326–330 / p95 341–361 ms (two batches of 6) | **Fail as built**; passes warm with the first-frame trim (cold with the trim not measured) |
| Second-instance `mailto:` ≤ 100 ms | p50 50.8 / p95 52.7 ms (20 runs); `attach` and `attachment` dropped in 5 of 5 runs; D-Bus `Open` 0.5 ms | Pass |
| A notification click focuses the window and opens the message | omarchy-shell accepts a notification in 0.8 ms; headless, the click path works (default action → `app.open-message` in 0.05 ms); live, `present()` refocuses an unfocused window in 2.2–3.9 ms (3 of 3) | Pending the user's click (`bash spikes/s6-desktop/manual-check.sh`) |
| Background mode keeps IDLE alive | APPEND → notification sent p50 1.3 ms (Stalwart), 492 ms (Dovecot's own IDLE delay); 16 cut connections back in IDLE in ~14 ms; a 330 s hold crossed one IDLE restart without a disconnect | Pass (minutes on the testbed; hours not verified) |
| Reconnect after resume ≤ 10 s | A silently frozen connection went unnoticed for 10 s; after a resume signal, idling again in p50 14.0 ms (Stalwart) / 14.5 ms (Dovecot) on localhost | Pending the user's suspend (`bash spikes/s6-desktop/manual-resume.sh`) |
| GDK_SCALE=2 doesn't change the scale | Same scale factors, sizes and timing with it set or unset | Pass — GTK 4 on Wayland ignores it |

`cargo test -p email-spike-s6`: 14 tests pass; fmt and clippy clean (re-run by the lead).

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
| ≤ 300 MB with 10 views | 10 tall newsletters: 804 MiB RSS GPU / 628 MiB RSS CPU (PSS 512 / 377); 10 short messages, GPU: 711 MiB; without process sharing 3,413 MiB | **Fail** as measured; the bar is now process-tree PSS plus a GPU budget, with live views capped (user decision 2026-10-04) — re-measured in M5 |
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
| UI ≤ 150 MB | RSS 158 MiB at first paint, 202 MiB after the 40 s script; PSS 91–100 → 133 MiB; an empty GTK4 + libadwaita window is already 120 MiB RSS (Vulkan; 216 MiB with GL) | Missed as RSS; **passes as PSS** (bar redefined by the user on 2026-10-04) |
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
| Dummy addresses only; full address only to the domain's own HTTPS autoconfig | 5,658 requests logged, 0 violations | Pass — **corrected 2026-10-04 (S4b):** the audit checked only first requests; runs C, D1 and D2 followed `autoconfig.msn.com` → `www.msn.com`, a redirect that keeps the query, so the dummy address `test@msn.com` most likely reached `www.msn.com` there (S4's log kept only the redirect target). A dummy address, no personal data; the rule as written was not met |
| ≤ 2 login attempts | 0 (the probe can only send CAPABILITY, STARTTLS and EHLO) | Pass |

`cargo test -p email-spike-s4`: 40 tests pass; fmt and clippy clean (re-run by the lead).

At the M0 gate (2026-10-04) the user declined the Norwegian-preset fix; S4b (ML-098) looks for general rules that pass the bar.

### S4b autodetect, general rules (ML-098) — 2026-10-04

Release builds, 14:23–14:45 UTC, residential network in Norway, cold DNS at 1.1.1.1; report [spikes/S4b-autodetect.md](spikes/S4b-autodetect.md). Rubric frozen 14:10 UTC before any code, rules frozen 14:28:35, held-out list frozen 14:42:23 (sha256 `a8d32d12…6281c`, re-checked by the lead) before any request to its domains.

| Bar | Result | Verdict |
| --- | --- | --- |
| ≥ 45/50 correct on S4's list, without Norwegian presets | 44/50 under S4's strict rubric; 45/50 under the extended rubric (E1: the product's second login attempt with the other username form; E2: an undocumented alias on the same addresses, verified). Same binary with the new rules off: 42 and 44 | Strict: missed by one; with the second login attempt counted: **met** (user decision below) |
| Held-out list (26 domains: 10 Nordic ISPs and hosts, 8 other European ISPs, 8 organizations' custom domains) | 13/26 strict (50 %); 16/26 extended (62 %); one measured run | No bar set; far below the 90 % that 45/50 implies, because presets answer 34 of S4's 50 rows and none of these |
| TLS-only | 50/50; held-out 26/26 | Pass |
| p50 ≤ 2 s / p95 ≤ 6 s / cap 10 s | 52 ms / 4,602 ms / max 5,091 ms (held-out 272 / 5,344 / 5,519 ms) | Pass |
| Dummy addresses only; full address only to the domain's own HTTPS autoconfig | 3,884 requests logged across six runs; 197 carried the dummy address, 196 to the domain's own autoconfig and 1 through the same-domain `autoconfig.msn.com` → `www.msn.com` redirect (exhaustive run only, where preset domains are queried) | One deviation, the same one S4 missed; fixed by the redirect rule in SECURITY.md |
| ≤ 2 login attempts | 0 (CAPABILITY, STARTTLS and EHLO only) | Pass |

**Rubric decided by the user (2026-10-04):** the accuracy bar counts the app's second login attempt with the other username form (E1), so S4b's 45/50 meets it and S4b is accepted. Alias equivalence (E2) was not part of the decision; it adds nothing on S4's list. Later autodetect results are reported strict, with E1, and with E2, and M2's re-measure (ML-097) runs on a fresh held-out list.

No row S4 got right is wrong now (checked row by row by the lead). `cargo test -p email-spike-s4b`: 53 tests pass; the whole-workspace `tools/ci.sh --ui` passed afterwards (187 tests, 10 ignored; smoke run `first-frame 45.0`).

### SecretStore (ML-006) — 2026-10-04

- `cargo test -p email-engine`: `memory_store_round_trip` and `memory_store_keeps_accounts_apart` pass.
- `cargo test -p email-engine -- --ignored keyring_round_trip` in the desktop session: passes against gnome-keyring 50 (0.05 s) under the test app-id `io.github.pixdevsapps.Mail.Devel.test`; `secret-tool search --all application io.github.pixdevsapps.Mail.Devel.test` afterwards finds no items.
- `tools/headless.sh 24 cargo test -p email-engine -- --ignored falls_back`: passes; on the private bus `Secrets::open` returns the memory store and the round trip works.

### Workspace scaffold — 2026-10-04

- `bash tools/ci.sh`: fmt, clippy `-D warnings`, tests (1 test: the engine runtime runs a task) and build passed.
- `MAIL_CI_DISPLAY=19 bash tools/ci.sh --ui`: passed; the placeholder window logged `first-frame 46.2` (debug build, Broadway). Harness check only, not a startup measurement. No `gtk4-broadwayd` left running.
- Machine: Omarchy 4.0.4, Rust 1.98.1, GTK 4.22.4, libadwaita 1.9.3.
