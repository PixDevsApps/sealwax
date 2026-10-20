# Testing & Acceptance

Synced: 2026-10-09

Evidence rules: record only what was actually run, with date, command, machine and caveats. "Not verified" is an acceptable result; a guessed number is not.

## Test layers

| Layer | What | Where | Runs in CI |
| --- | --- | --- | --- |
| Unit | Pure logic: threading, query parser, autoconfig XML, reply/forward building, format=flowed, mailto, palette and CSS (22 stock themes, contrast rules), the action registry and keymap audit (US and Norwegian), `config.toml` and `keys.toml` | `domain/` (`cargo test`), insta snapshots, proptest | Yes |
| Store | Migrations, writer/reader split, overlays, crash consistency (a SIGKILLed child mid-ingest), change events kept exact against a model | `infrastructure/` (scratch stores under `target/scratch`) | Yes |
| Protocol (scripted) | IMAP, SMTP and POP3 against scripted mock servers: capabilities, auth, fallbacks, faults; `FetchPart` and `FetchOriginal` through the service (`engine/tests/parts.rs`, M5) | `engine/` tests | Yes |
| Protocol (testbed) | Real servers in Docker: Dovecot 2.4, Stalwart, an IMAP4rev1-only server, a mail catcher; the login check, folder list and first sync (M2), the log grep at trace level; body downloads (ML-044); S1's randomized two-client and fault-injection suites, for the sync core's passes (M3 phase A) and through the engine's per-account actor (phase B: IDLE latency, a cut while idling, a cut mid-FETCH, a black hole lifted with `Reconnect`) | `tools/testbed/`, `engine/tests/testbed.rs` and `engine/tests/log_grep.rs` (M2), `engine/tests/bodies_testbed.rs`, `sync_testbed.rs` and `actor_testbed.rs` (M3), the writes, outbox and sending suites (M4, M6), `engine/tests/newmail_testbed.rs` (M7: new mail through IDLE), `engine/tests/pop3_testbed.rs` and `pop3_accounts_testbed.rs` (M8: POP3), `engine/tests/folders_testbed.rs` (M9: folder create, rename and delete), `spikes/s1-imap/tests/testbed.rs` | No: `bash tools/ci.sh --testbed` runs every one (it starts the testbed when it is down) |
| Soak (testbed) | The engine with four accounts through the fault proxy for hours: a slow other client, cuts, black holes with and without `Reconnect`, suspends; every account compared with the server at each check (ML-049) | `engine/tests/soak.rs` | No: by hand, `SEALWAX_SOAK_MINUTES` long ([BUILD.md](BUILD.md#mail-testbed)) |
| Rules (B1) | The store's rules queue and copy op; the planner and the message view; rules on new mail end to end against the fake IMAP server and on a POP3 account; a copy whose reply is lost; a rule's copy on Dovecot, Dovecot IMAP4rev1 and Stalwart | `infrastructure/src/store/rules_tests.rs`, `engine/src/service/rules.rs`, `engine/tests/newmail.rs` (`rules_run_on_new_mail_before_it_is_announced`), `engine/src/service/actor/pop3/tests.rs`, `engine/tests/write.rs`, `engine/tests/write_testbed.rs` (`a_rules_copy_on_every_server_and_its_retry_makes_no_second`) | Yes, but the testbed suite |
| Rules (B2) | The editor's draft and its checks, the rules' words (summaries, toasts, the banner), the rules file read, written, changed and followed after a rename off the GTK thread and never over an edit, an action with a change beside it undone with it | `app/src/rules/` (`draft.rs`, `text.rs`, `file.rs`, `mod.rs`), `app/src/triage.rs` (`an_action_with_a_change_beside_it_says_both_and_undoes_both`); the self-tests `rules-dialog`, `rules-menu`, `rules-events` | Yes |
| UI (headless) | The app on GTK Broadway with a private D-Bus bus, self-terminating; `sealwax --self-test` scripts (`tests/selftest/`) that resize, activate actions, assert layout and state, take screenshots, and load fixture stores (`store-dir`, `fixtures`); `app/examples/theme_switch.rs` and `app/examples/palette.rs` | `tools/headless.sh`, `bash tools/ci.sh --ui` | Yes (scripts except `preview*`, `perf-*`, `live-*` and `testbed-*`) |
| Security | Hostile-HTML suite against a local HTTP canary and a DNS canary (M5); secret grep over config, database and logs; outgoing-mail checks (no Bcc, no hostname); STARTTLS and STLS injection; every fuzz target's entry point over `fuzz/corpus/` (M9) | `app/examples/hostile.rs` in `tools/ci.sh --ui`; `app/` and `engine/` tests | Yes |
| Fuzzing | 13 coverage-guided libFuzzer targets on the stable toolchain (M9, ML-093): message parsing, HTML, text, links, attachment names, autoconfig XML, search, `mailto:`, the config files, the theme, POP3, IMAP, and Sieve rules (parse, check, run, and the rules model written and read back); crashes kept as regression inputs | `fuzz/` (its own workspace), `tools/fuzz.sh <target> [seconds]` | The corpus only, through `cargo test`; fuzzing by hand |
| Live | Real Wayland session (NVIDIA rendering, notifications, activation, fcitx5) and real test accounts | Manual or agent with authorization | No |

## Headless runs

- Always through `tools/headless.sh <display> <cmd...>`: Broadway backend, a private D-Bus bus that starts no services, `GSETTINGS_BACKEND=memory`, `ADW_DISABLE_PORTAL=1`, a time limit (`SEALWAX_HEADLESS_TIMEOUT`, default 300 s).
- From GTK 4.24 the Broadway daemon is GTK's own with Sealwax's patch (`tools/broadwayd/`, 2026-10-19). GTK 4.24's icons can put one render node twice in a frame, and the system `gtk4-broadwayd` then aborts on `!reused_node->consumed` or `node != NULL` (`reuse.patch` says why); the patched daemon copies the node instead, or stands in an empty one. `tools/headless.sh` runs `tools/broadwayd/build.sh`, which builds it once per GTK version into `target/broadwayd/<version>/` from the release tarball (downloaded into `.local/broadwayd/`, checked against `tools/broadwayd/SHA256SUMS`); a GTK version with no line there stops the run until someone checks the patch against it and adds the tarball's checksum. `SEALWAX_BROADWAYD` names another daemon. The patch only affects what a browser would show (a copied or blank icon), never what GTK draws or the self-tests check.
- The app exits by itself after `SEALWAX_EXIT_AFTER_MS`; the CI smoke run greps for `first-frame`.
- Parallel agents use different display numbers; CI uses `SEALWAX_CI_DISPLAY` (default 19).
- `sealwax --self-test <script>` (ported from Stet) runs the real app on Broadway and a `.sealwax-test` script against it, printing TAP; the commands are listed in the module comments of `app/src/selftest/`. `tools/ci.sh --ui` runs every `tests/selftest/*.sealwax-test` except `preview*`, which write the gate's screenshots into `tools/progress/public/`. The harness attaches a stand-in Broadway client reporting a 2560×1600 screen (without one Broadway clamps windows to 1024×768), switches `GTK_A11Y=none` to `test` so accessible labels can be checked, turns animations off and waits for a painted frame after every step. Since M4 its clicks go to the surface under the point (a popover's over the window's), in surface coordinates, with the crossing events and grab replies `broadway.js` sends, so buttons, checkboxes and popover items take real clicks.
- Self-tests read a scratch Omarchy state (`SEALWAX_OMARCHY_STATE` under the run's scratch root), never the desktop's: the built-in palette until a script runs `theme NAME` (a stock palette from `tests/theme/`, swapped in as `omarchy-theme-set` does); `corners auto|PX` sets the corner radius. Look assertions compare what GTK draws with the applied theme: `assert-color`, `assert-contrast`, `assert-radius`, `assert-font`, `assert-height`, `assert-tooltip`; since 2026-10-08 `assert-color` also takes the account colours as roles (`account-sage`, …), and `assert-right-of` and `hover` check where a widget sits and move the pointer onto it (the setup assistant's scrollbar).
- Body downloads (ML-044) run the app's own reader against the same fake engine: `bodies-add` stores synthetic samples as headers and a preview, `engine-expect fetch-bodies` takes the reader's request, `bodies-store` does the engine's store upgrade and `engine-reply body-fetched ok|offline|needs-sign-in|gone|store|other` answers; `assert-body` and `assert-body-text` check the reader (`app/src/selftest/bodies.rs`, `reader-bodies.sealwax-test`).
- The conversation view (M5, ML-057) runs the app's own list and reader (`app/src/selftest/conversation.rs`): sample conversations filed into the app's store, real clicks on rows, headers and folds through Broadway, programmatic scrolling that checks the live-view cap, blank bodies and kept heights at every step, arrivals and moves (`reader-conversation`, `reader-conversation-views`).
- Files and remote content (M5, ML-058, ML-061, ML-063) run the app's own reader (`app/src/selftest/files.rs`, `files.sealwax-test`): `files-add` and `files-add-text` store samples whole or as a large message's text parts (attachments and inline parts empty), the fake engine answers `engine-reply part-fetched|original-fetched …` with their bytes, the reader's hooks take the would-be launches and answer the file choosers with paths under `$TMP`, and `files-print` prints into a PDF with GTK's file printer only (`assert-files pdf` needs `pdftotext`). `app/examples/remote_senders.rs` checks "Always for this sender" against the hostile suite's canary.
- `app/examples/reader_memory.rs` measures the process tree's PSS with a 10-message conversation open and scrolled, and the growth after 200 opens (`--opens=N` goes on to N opens, printing the PSS every 200); `app/examples/reader_corpus.rs` renders the real-world corpus (`python3 tools/corpus/fetch.py` first) and S2's synthetic set in a dark and a light scratch theme, and counts the styled bodies sitting on the theme (`--no-snapshots` skips its blank-frame checks, which hold GPU memory; `--bodies` saves every HTML body's whole document into `.local/corpus/bodies/` to look at). Both run headless, and on the live session only for an authorized measurement (`reader_memory --live`); neither injects input, and both measure at the compositor's window size when live.
- A folder's older mail (ML-042, ML-043) runs the app's own list against the same fake engine: picking a folder sends `folder-opened`, the end of its first-sync window sends `load-older` and shows "Loading older messages…" (`list-older`), and `engine-reply older-loaded FETCHED complete|more` answers (`list-older.sealwax-test`).
- The setup assistant runs against a fake engine on the app's `EngineLink` (`engine-attach`, `engine-expect`, `engine-reply`; `app/src/selftest/setup.rs`): scripted detection results, login failures and save and removal results; `setup-type`, `setup-click`, `setup-key enter|escape` and `setup-respond` drive it as a user, and `assert-setup`, `assert-field`, `assert-text-contains` and `assert-account-status` check it. The password is compared, never printed. `engine-start CA` starts the real engine instead, trusting only that CA and keeping secrets in memory: `testbed-setup.sealwax-test` (M2) and `testbed-triage.sealwax-test` (M4, after `tools/testbed/triage_mailbox.py fill`; [BUILD.md](BUILD.md#accounts-in-development-builds)) use it against the Docker testbed and are run by hand.
- Notifications (M7, ML-083) run against a stand-in `org.freedesktop.Notifications` on the wrapper's private bus (`app/src/selftest/notify/server.rs`, ported from S6's; it refuses any other bus): it records every `Notify` (app name, summary, body, actions, hints) and clicks a default action or a button; scripts emit `Event::NewMail` through the fake engine (`notify-*`, the commands in `app/src/selftest/notify.rs`). `testbed-notify` with `tools/testbed/notify_e2e.py` runs the real engine on the testbed instead and appends the message as another client. `app/examples/notify_live.rs` is the live click check: a dry run headless, `--live` only under a live-window authorization, the user clicking.
- WebKit's sandbox (M9): `tools/ci.sh` runs the smoke run and every self-test with `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS` set, and `assert-web-sandbox` (in `reader` and the composer's scripts) checks the variable is gone and every WebKit web process Sealwax started runs under bubblewrap in a PID namespace of its own. `engine/tests/actor_panic.rs` restarts IMAP and POP3 actors after a panic (imap-proto's real one from the fake server, and injected ones); `domain/tests/html_cap.rs` (ignored; release builds) times nested HTML at the 256 KiB cap.
- Accessibility (M9, ML-092) is checked by `a11y-window`, `a11y-reader`, `a11y-composer`, `a11y-setup` and `a11y-dialogs` (`app/src/selftest/a11y.rs`): `assert-accessible` walks the controls on screen and fails on one without a name, a meaningful role or the states its role needs (a tooltip isn't a name); `tab-chain window|composer forward|backward` presses real Tab or Shift+Tab until the focus comes back and fails on a trap, and `assert-tab-chain`, `assert-tab-stops` and `assert-focus-stop` check the chain; `assert-accessible-name`, `-role` and `-state`, the link checks (`link-hooks`, `assert-link-strip`, `assert-web-focus` …) and `assert-announced` check one thing each. What Orca says is the user's check.
- The rules in the app (M9, ADR-020) run the app's own Rules dialog, editor, message menu and banner against the fake engine (`app/src/selftest/rules.rs`, `rules-dialog`, `rules-menu` and `rules-events`): the dialog's fields and choices take `setup-type`, `setup-choose` and `setup-click` (they work on any named widget of the window), and real clicks `click`; the file is the run's `$TMP/config/sealwax/rules/<key>.sieve`, written with `write-file` and checked with `assert-file-contains` and `assert-file-lacks` (behind `wait-until`, as the dialog writes off the GTK thread; `rules-wait` waits for its saves). `rules-opener` records what Open File would open (`assert-rules-opened NAME`); `rules-failed KEY LINE COLUMN TEXT` (or `KEY file`) and `rules-applied N|store|file [FOLDER]` are the engine's `RulesFailed` and `RulesApplied` (for the oldest Apply Rules request; with FOLDER the list's first N rows move there through the store's rules queue, as the engine queues a run, and the event carries the batch, so Undo is real); the fake engine records an `ApplyRules`' `rule` as the name of the one rule it runs (`none` for the account's rules); `row-menu POS` right-clicks a list row (opened as the gesture does when Broadway's click doesn't reach it headless), `row-menu-click NAME` clicks a menu item, and `assert-row-menu`, `assert-row-menu-items`, `assert-rules-dialog`, `assert-rules-set`, `assert-rules-senders`, `assert-rule-row`, `assert-rules-problem` and `assert-rules-banner` check them. `mail-add` takes `list=ID` for a List-Id header, and `to=HEADER` and `cc=HEADER` (2026-10-12, issue #8) for a To and a Cc as written. The engine side is B1's.
- Folder management (M9, ADR-020) runs the app's own sidebar and dialogs against the fake engine (`app/src/selftest/folders.rs`, `folders-tree` and `folders-dialogs`): `folder-add ACCOUNT NAME [noselect] [noinferiors]` stores a folder with `/` between its levels; a folder is named `ACCOUNT:NAME` (`Work:Kunder/Acme`) wherever `ACCOUNT:ROLE` was taken; `folder-arrow` clicks a folder's fold arrow and `focus-folder` focuses its row; `assert-sidebar-tree ACCOUNT ROWS` (`Inbox,>Kvitteringer,Drafts`), `assert-folder-level`, `assert-folder-expanded`, `assert-folder-count SPEC N [NAME]` (the count the row shows, 0 for none, and its accessible name), `assert-folder-stored` and `assert-response DIALOG RESPONSE enabled|disabled|default` check the tree and the dialogs; `sidebar-menu-click [NAME]` and `assert-sidebar-menu STATE [NAME]` name a menu item. The fake engine takes `create-folder`, `rename-folder` and `delete-folder` (`account`, `parent`, `folder`, `name`) and answers `engine-reply folder-changed ok|offline|taken|not-allowed|missing|lost|store` or `refused TEXT`, `ok` first changing the app's store as the engine would; `engine-expect none but load-older,folder-opened` lets the list's own commands through. The engine side is `engine/tests/folders.rs` against the fake IMAP server, which takes CREATE, RENAME (with the folders under it), DELETE (refused for a folder with folders under it, as Dovecot does), SUBSCRIBE and UNSUBSCRIBE with a set of subscriptions, LIST patterns and a delimiter of choice; and `engine/tests/folders_testbed.rs` on the testbed.
- Background mode (M7, ML-084) is checked by `background-*`: closing the window keeps the process and its engine, activation builds a new window, `assert-freed` checks a closed window, its list and its reader are gone, `background-memory` prints PSS over close/reopen cycles, and Start Sealwax at Login writes only in the run's scratch `XDG_CONFIG_HOME` (`app/src/selftest/background.rs`).
- `app/examples/theme_switch.rs` swaps a scratch Omarchy theme (`SEALWAX_OMARCHY_STATE`) as `omarchy-theme-set` does and checks colours, sizes and fonts after each switch; `app/examples/palette.rs` checks the actions, palette, scoped shortcuts, menu, shortcuts dialog and live settings.
- WebKitGTK renders under `tools/headless.sh` in software (GTK has no GL there, so WebKit turns acceleration off). Security checks, heights, `cid:` loads and snapshots work headless. Without a Broadway client attached GTK paints about once per second, so frame timing, open-to-paint latency and scrolling are measured on the live session; headless WebView tests wait for events and allocations, never fixed delays (S2).

## Hostile-HTML suite (ML-065)

60 cases in `app/examples/hostile.rs`, run by `tools/ci.sh --ui` (M5, ML-065): S2's 47, 12 for M5's surfaces (link hover and the mismatch warning, plain-text links, collapsed quotes in HTML and plain text, format=flowed, inline PGP, PGP/MIME, S/MIME signed data, a `cid:` answer for a reused view, page-initiated printing, names only DNS sees) and a three-message conversation of hostile bodies. The run preloads `devtools/dns-canary`, a `getaddrinfo` that records every name the app or WebKit's network process looks up and refuses it. Each points its network vectors at a local HTTP canary (127.0.0.1, random port) that counts connections, requests per path, connections that send nothing, and TLS ClientHellos; each also carries a script payload with four traces (title, `data-pwned`, `alert()`, a beacon). Clicks are DOM `click()` calls from Sealwax's isolated world.

Pass with the production reader: 0 connections (not just 0 requests — WebKit pre-connects), 0 TLS hellos, 0 DNS lookups, 0 script traces, each case's own expectation, no allowed navigation, new window or download; the `cid:` test image renders and a foreign `cid:` part never loads. After Show images, the DNS canary may see only the images' names: `<link rel=dns-prefetch>` (an absolute URL, since the reader's pages have no base URL) must stay unlooked-up, because WebKitGTK 2.54 has no switch for it and only the reader's pings filter stops it. In the same run, a no-defence control must show the vectors are live, and a remote-allowed control (CSP widened with the canary's origin, no filter) must load images only.

## Performance targets

Measured with release builds unless noted; each entry names its spike or milestone.

| Area | Target | Where measured |
| --- | --- | --- |
| Startup | Process start → first frame with rows on screen, cached 10k INBOX: ≤ 400 ms on a warm start; ≤ 600 ms on a cold start, measured in M9 (user decision 2026-10-04) | S6, M1, M9 |
| List | p99 frame ≤ 16.7 ms while scrolling 100k messages at 3440×1440; jump to end ≤ 100 ms; one-insert diff ≤ 20 ms | S3, M1, M9 |
| Folder open | ≤ 100 ms | S3 |
| Open message | Plain text ≤ 50 ms; cached HTML ≤ 150 ms p95 | S2, M5 |
| Search | First 50 results ≤ 100 ms p95 over 100k messages | S3, M7 (pass: worst p95 82.3–88.5 ms cold, 24.4–24.5 ms warm over 38 shapes; a key → results painted p95 64.7–66.8 ms) |
| Ingest | ≥ 2,000 headers/s; FTS ≥ 1,000 messages/s | S3 |
| Memory (redefined 2026-10-04 by the user: PSS plus a GPU budget, live views capped; at the M5 gate, 2026-10-06, the user put NVIDIA's driver mappings in the GPU budget: "~100 MiB" of mappings plus "~64 MiB" of GPU memory, cap 4) | UI process PSS ≤ 150 MB, without the GPU driver's mappings. Reader: PSS of the whole process tree (app + WebKit web and network processes) ≤ 300 MB with a typical 10-message conversation open and live message views capped (proposed cap: 4 expanded HTML bodies; set when M5 measures it). GPU memory has its own budget, sized from the M5 measurement (reader and composer views render without acceleration, ADR-011 and ADR-014). < 20 MB PSS growth after opening 200 messages. RSS is recorded but not judged: an empty GTK4 window is already 120 MiB RSS on this NVIDIA machine | S2, S3, M5, M9 |
| Sync | 10k-header first sync ≤ 20 s (testbed); no-change re-sync ≤ 200 ms (CONDSTORE) / ≤ 2 s (UID diff) | S1 |
| Push | New mail visible ≤ 3 s (S1, testbed), ≤ 5 s (product) | S1, M3 |
| Composer | Keystroke to paint < 16 ms, including fcitx5 | S7, M6 (live in the real composer: p95 9.94 ms, 12.35 ms above a 199 KB quote) |
| Autodetect | ≥ 45/50 correct; p50 ≤ 2 s, p95 ≤ 6 s, cap 10 s | S4 |
| Resume | Reconnect ≤ 10 s after suspend | S6, M3 |

## Acceptance by milestone

Each milestone's exit criteria are the acceptance list. Results go here as dated entries, newest first, with screenshots in `tools/progress/`.

## Results

### 0.5.1 — 2026-10-20

The release of Check for Updates. This machine.

| Check | Result | Verdict |
| --- | --- | --- |
| CI on the release commit `07684fe`, `SEALWAX_CI_DISPLAY=80 bash tools/ci.sh --ui` | exit 0; 1,531 passed, 0 failed, 99 ignored; 82 scripts, 8,047 of 8,047 steps; the hostile suite clean (60 cases, `dns=production=0 control=11`, `glib_criticals=0`, 272 s) | Pass |
| `tools/release.py build` on `v0.5.1` | makepkg's `check()`: 1,096 passed, 0 failed; the binary says `sealwax 0.5.1` and holds no home-folder path | Pass |
| The push range (`c223379..07684fe`) scanned before pushing | No personal address, home path or client ID; no working doc | Pass |
| The release [v0.5.1](https://github.com/PixDevsApps/sealwax/releases/tag/v0.5.1) | Not a draft or pre-release; its four files; `releases/latest/download/…` and `releases/download/v0.5.1/…` both give the build's package (SHA-256 `7acd6a0e…5ac8`); Check for Updates' own request (`updates::tests::github`) reads 0.5.1 | Pass |
| Installed here | Not yet: the owner's, with INSTALL.md's command | Not verified |

### Check for Updates — 2026-10-20

[ADR-021 amendment](DECISIONS.md#adr-021-amendment--check-for-updates-only-when-the-user-asks-2026-10-20). This machine, debug, headless (Broadway) unless said.

| Check | Result | Verdict |
| --- | --- | --- |
| `sealwax_domain::updates`: only `vX.Y.Z` with plain numbers reads (a suffix, a sign, spaces, shell text, other digits, a number of 10 digits don't); the running version against the latest; the command for a release | Unit tests pass | Pass |
| `sealwax_engine::updates` against a server on 127.0.0.1: the tag read from a release's answer; the request is `GET /repos/PixDevsApps/sealwax/releases/latest` with `User-Agent: Sealwax`, GitHub's Accept, no cookie, Referer or Authorization; an answer without a version, with shell text in the tag, or over 512 KiB is unreadable; 404 no release, 403 and 429 busy, 500 unreadable; nothing listening is offline; plain HTTP to another host is refused | Unit tests pass | Pass |
| The real answer: `cargo test -p sealwax-engine updates::tests::github -- --ignored` (one request to api.github.com, nothing personal in it) | "the latest release is 0.5.0", GitHub's latest | Pass |
| The registry and the menu: `check-for-updates` in a section of its own between the settings and Quit; the USER_GUIDE table | `cargo test -p sealwax-domain`: 496 passed; the menu's unit test | Pass |
| `updates` self-test: nothing asked at startup; the dialog checks, then says the latest (this build), newer than the latest, a newer release with its command (selectable, Tab reaches it and Copy Command, the clipboard holds the command, "Copied"), offline, busy; a Cancel before the answer leaves it unused; no engine says it couldn't check; `assert-accessible dialog` | 62 of 62 steps (`tools/headless.sh 57`) | Pass |
| CI, `SEALWAX_CI_DISPLAY=80 bash tools/ci.sh --ui` | exit 0; 1,531 passed, 0 failed, 99 ignored; 82 scripts, 8,047 of 8,047 steps; the hostile suite clean (60 cases, `dns=production=0 control=11`, `glib_criticals=0`, 272 s). Log: `.local/ci-ui-update-check-2026-10-20.log` | Pass |
| In an installed build, on a live desktop, against GitHub | Not tried: it reaches users with the next release | Not verified |

### The keys for issues #10 and #12 — 2026-10-20

[ADR-017 amendment](DECISIONS.md#adr-017-amendment--the-keys-for-issues-10-and-12-accepted-and-built-2026-10-19). This machine, debug, headless (Broadway).

| Check | Result | Verdict |
| --- | --- | --- |
| The registry: `l` on Open Message, `d`/Shift+D, `g a`, `g 1`–`g 9`; the go-to sequences resolve in the list and the reader; the USER_GUIDE table matches | `cargo test -p sealwax-domain`: 492 passed | Pass |
| `go_to::account_inbox`: the Inbox of the Nth account in the sidebar's order; none for a missing place, an account without an Inbox, or an Inbox that can't be selected | Unit test passes | Pass |
| `triage-keys`: `l` opens the message, `g a` shows All Inboxes, `g 1` the first account's Inbox, `g 9` (no ninth account) leaves the folder | 264 of 264 steps | Pass |
| `reader-conversation`: from the list, Shift+D twelve times reaches the top of a conversation 3,559 px down, `d` leaves it, Shift+D comes back | 127 of 127 steps | Pass |
| CI, `SEALWAX_CI_DISPLAY=80 bash tools/ci.sh --ui` | exit 0; 1,518 passed, 0 failed, 98 ignored; 81 scripts, 7,985 of 7,985 steps; the hostile suite clean (60 cases, `dns=production=0 control=11`, `glib_criticals=0`, 273 s) | Pass |
| On the owner's keyboard (US and Norwegian layouts) | Not tried; the keymap audit checks the layouts | Not verified |

### The reader doesn't wait, a folded folder's unread, the Microsoft sign-in note — 2026-10-19

The owner's decisions of 2026-10-19 ([ADR-011](DECISIONS.md#adr-011-amendment--moving-on-and-large-messages-dont-wait-2026-10-19), [ADR-009](DECISIONS.md#adr-009-amendment--a-folded-folder-counts-its-subfolders-unread-2026-10-19) and [ADR-008](DECISIONS.md#adr-008-amendment--work-and-school-accounts-explained-now-the-publisher-verified-later-2026-10-19) amendments). This machine, debug, headless under `tools/headless.sh` (Broadway), unless said otherwise.

| Check | Result | Verdict |
| --- | --- | --- |
| `reader-leave` (rewritten): `hold-reader-layout true` keeps the next layout from finishing, then `j` and a delete | 20 of 20 steps, three runs: the left message gone 158–176 ms after `j` and 182–187 ms after the delete. With the selection's change still going through `Reader::show` (the old behaviour), both steps fail at 3,022.8 and 3,038.9 ms | Pass |
| `reader-conversation`, a page shown before it is laid out | Failed at step 81 (the selected message 1,859 px down a scrolled column) until a settling page anchors its selected message; then 118 of 118 | Pass |
| `files`: a large message whose inline image waits for `FetchPart` shows within 1.5 s (`wait-within 1500`), before the image is answered | 307 of 307 | Pass |
| The first message after a start (a side agent, release, Sealwax's Broadway client attached, the fictional 2,400-message mailbox) | 107–119 ms (10 runs) with the web process started at the first selection; 27–31 ms (8 runs) with it started at startup; the first frame 300–309 ms either way. In the app, the reader's view came up 35 ms after the first frame with nothing selected | Pass; the owner's ~2 s live not reproduced |
| `folders-tree`, a folded folder's sum (`assert-folder-count`), and `a11y-window` (side agent) | 141 of 141 and 178 of 178 | Pass |
| The note after 60 s of waiting for Microsoft (side agent; the wait shortened to 300 ms by `setup-still-waiting-after`) | `setup-microsoft` 225/225, `setup-microsoft-banner` 179/179, `a11y-setup` 144/144 (the announcement), `a11y-dialogs` 168/168 | Pass; not on a work tenant |
| CI, `SEALWAX_CI_DISPLAY=80 bash tools/ci.sh --ui` on `120e1f2` | exit 0; 1,517 passed, 0 failed, 98 ignored; 81 scripts, 7,962 of 7,962 steps; the hostile suite clean (60 cases, `dns=production=0 control=11`, `glib_criticals=0`, 273 s) | Pass |
| On the owner's machine | Not measured | Not verified |

### GTK 4.24's Broadway daemon, patched for the headless checks — 2026-10-19

GTK went from 4.22.4 to 4.24.1 on this machine on 2026-10-18, and `gtk4-broadwayd` then aborted in about 30 of the 81 CI scripts. This machine, debug build, on `aa56a8f` with the change in the working tree; headless displays 73 and 74.

| Check | Result | Verdict |
| --- | --- | --- |
| The cores (35 of `gtk4-broadwayd` since 2026-10-18, symbolized through Arch's debuginfod) | 32 abort on `!reused_node->consumed` (`append_node_ops`, broadway-output.c:458), 3 on `node != NULL` (`decode_nodes`, broadway-server.c:1797). In the core walked in full, the decoded tree holds one node twice, an 11 px icon (a scale of 0.6875 over a 12×8 texture) under two sibling rows; the node the other 31 abort on is the same kind of icon-sized subtree (that shape, a scale of 0.75 over a texture, or a lone texture up to 17×16) | Cause found |
| The cause in GTK's source (4.22.4 and 4.24.1 compared) | The Broadway code is unchanged; 4.24's `GtkIconPaintable` draws symbolic icons through `GtkSvg`, which hands out its cached render node again, so two identical icons put one node twice into a frame, and `gskbroadwayrenderer.c` keys reuse on the node pointer. No issue or merge request about it in GNOME/gtk | Upstream bug |
| `GSK_RENDERER=cairo` instead | Aborts at once on `node == reused_node`: Broadway's cairo context sends its texture node without an id (the same on GTK `main`) | Not usable |
| A 30-line PyGObject program (four identical 11 px `pan-down-symbolic` images that redraw every 100 ms) with a browser on the daemon | The system daemon aborts on `!reused_node->consumed` within seconds; the patched daemon runs all 300 ticks, and the browser shows the four icons with no console errors | Reproduced, fixed |
| `a11y-dialogs` and `reader-conversation`, a daemon built from the unpatched 4.24.1 tarball and the patched one, twice each | Unpatched: stops at steps 75 and 87–88, as the system daemon does; patched: 168/168 and 118/118 | Pass |
| Every CI script, past failures, patched daemon | 81 of 81 scripts, 7,831 of 7,831 steps, 409 s, no daemon abort | Pass |
| `a11y-window` step 155, the Tab chain at 1,100 px (patched daemon) | Failed in 2 of 8 runs at first: Tab went from the reader's Next button to Compose, past the whole message. The cause is a race in the script. `list-select 0` turns the reader pane to `message` at once, but the conversation is laid out unseen for up to `READY_TIMEOUT` before it is presented, so `wait-until assert-reader-page message` passes early. With the reader's debug log, "reader: shown" came 10–110 ms after the selection, during or after `focus list`, in 14 of 15 runs; the chain passes only when the page is presented before Tab reaches the reader. The script now waits with `wait-until assert-reader-selected` (the presented page shows the list's selection). The same script before and after, 20 runs each: 17 of 20 passed (3 stopped at step 155), then 20 of 20. GTK's "needs at least 477" warning for the staged page appeared in 9 of the 20 passing runs, so it isn't the cause. No other script tabs into the reader right after a selection without such a wait. Whether this is new with GTK 4.24 isn't known: on the system daemon the script aborts at step 110 (8 runs of 8) | Fixed (script) |
| CI, `tools/ci.sh --ui` with its `cargo test` step left out (two unit tests fail on this machine for other reasons, fixed in other worktrees) | Three runs before the `a11y-window` fix (display 73): the first stopped at `a11y-window` step 155 (the race above); the second and third passed whole, 692 s and 682 s: the smoke run, 81 of 81 scripts, 7,831 of 7,831 steps, the hostile suite clean (60 cases, `dns=production=0 control=10`, `glib_criticals=0`, 274 s and 273 s); Clippy clean, no daemon left running, no core A fourth run with the `a11y-window` fix passed whole in 700 s: 81 of 81 scripts, 7,832 of 7,832 steps, the hostile suite clean (274 s). | Pass |
| `SEALWAX_CI_DISPLAY=76 bash tools/ci.sh --ui` on `main` after the three merges (`61b68d4`, 2026-10-19, this machine) | Exit 0: format and Clippy clean, 1,511 tests passed, 0 failed, 98 ignored; 81 of 81 scripts, 7,832 of 7,832 steps; the hostile suite clean (60 cases, `dns=production=0 control=11`, `glib_criticals=0`, 274 s); no daemon left running | Pass |
| A browser rendering of Sealwax itself through the patched daemon | Not looked at; the self-tests' client draws nothing and screenshots come from GTK in the app | Not verified |

### DNS prefetch on WebKitGTK 2.54 — 2026-10-19

WebKitGTK went from 2.52.6 to 2.54.1 on 2026-10-18 (GTK from 4.22.4 to 4.24.1), and 2.54 has no `LinkDNSPrefetch` feature any more ([ADR-011 amendment](DECISIONS.md#adr-011-amendment--a-pings-filter-in-every-view-as-webkitgtk-254-dropped-linkdnsprefetch-2026-10-19)). This machine, rustc 1.98.1, headless display 61, the DNS canary preloaded in every run.

| Check | Result | Verdict |
| --- | --- | --- |
| A bare WebKitGTK 2.54.1 view (Python, `tools/headless.sh`) loading `<link rel=dns-prefetch>` pages at 0.5 s, 7 s and 14 s | Without a filter: the 7 s and 14 s names looked up, the 0.5 s one not (WebKit drops prefetches until it has read the proxy settings). With the pings filter: no name looked up | The vector is live on 2.54; the filter stops it |
| The hostile suite before the change, the cases' hrefs `//host` | Clean, but blind: with no base URL `//host` is never http, so WebKit never prefetched it, not even in the control | — |
| The hostile suite with absolute hrefs and the Show images DNS check, the pings filter taken out | Fails: `shown/dns-names` looked up `sealwax-dns-prefetch.test` | The check catches it |
| The same with the change | Clean: 60 cases, 273 s, `production=0 control=11` DNS lookups; `shown/dns-names` looked up only its images' two names; the control now looks up `sealwax-dns-prefetch.test` | Pass |
| `cargo fmt --all --check`, Clippy with `-D warnings` | Clean | Pass |
| `cargo test --workspace -- --skip bodies_download_beside_the_actor_and_the_prefetch_follows_new_mail` | 1,508 passed, 0 failed, 98 ignored. The skipped `engine/tests/actor.rs` test fails on `main` too | Pass |
| `tools/ci.sh --ui`'s self-tests, run on past failures | 51 passed; 30 stopped when `gtk4-broadwayd` (GTK 4.24.1) aborted on a GLib assertion inside itself (core dumps). `a11y-dialogs` (step 75) and `reader-conversation` (step 88) stop at the same step on `fb5c5a2` without the change; `window-view` passed on a second run | Not this change; Broadway on GTK 4.24.1 |

### 0.5.0 — 2026-10-20

`v0.5.0` is `cdb9ce7`; this machine, rustc 1.98.1, WebKitGTK 2.54.1, GTK 4.24.1 (the headless runs through ADR-022's patched daemon). The release: [v0.5.0](https://github.com/PixDevsApps/sealwax/releases/tag/v0.5.0), at the owner's word ("go ahead, release 0.5.0").

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `cdb9ce7`, `SEALWAX_CI_DISPLAY=80 bash tools/ci.sh --ui` | 1,518 passed, 0 failed, 98 ignored; 81 scripts, 7,985 of 7,985 steps; the hostile suite clean (60 cases, `dns=production=0 control=11`, `glib_criticals=0`, 273 s) | Pass |
| `python3 tools/release.py build` on `v0.5.0` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,087 passed, 0 failed; the binary says `sealwax 0.5.0` and has no path of the home folder; the client ID built in (from the owner's `config.toml`) | Pass |
| The push | A scan of `2f7d980..cdb9ce7`'s diff: no working doc, no address but fictional `example.test` ones, no client ID; `main` and `v0.5.0` pushed | Pass |
| The release `v0.5.0` (`tools/release.py github`) | GitHub's latest, four files; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build (`2ae75a98…`), read by pacman as `sealwax 0.5.0-1`, `hunspell` among its optional dependencies | Pass |
| Installing 0.5.0 on this machine (the owner, with INSTALL.md's command) | pacman's log: `upgraded sealwax (0.4.9-1 -> 0.5.0-1)` at 09:05:29; `pacman -Q` reads `sealwax 0.5.0-1`; `/usr/bin/sealwax` byte for byte the build's (SHA-256 `65c3d3df…`) and says `sealwax 0.5.0`; the running Sealwax started at 09:05:48 from it | Pass |

### 0.4.9 — 2026-10-19

`v0.4.9` is `6ae9bcb`; this machine, rustc 1.98.1, WebKitGTK 2.54.1, GTK 4.24.1 (the headless runs through ADR-022's patched daemon). The release: [v0.4.9](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.9), at the owner's word ("Yes please, go ahead", to releasing 0.4.9 and installing it).

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `6ae9bcb`, `SEALWAX_CI_DISPLAY=76 bash tools/ci.sh --ui` | 1,511 passed, 0 failed, 98 ignored; 81 scripts, 7,832 of 7,832 steps; the hostile suite clean (60 cases, `dns=production=0 control=11`, 273 s); no daemon left running | Pass |
| `python3 tools/release.py build` on `v0.4.9` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,087 passed, 0 failed; `pacman -Qp` reads `sealwax 0.4.9-1`; the binary says `sealwax 0.4.9` and has no path of the home folder; `SHA256SUMS` checks | Pass |
| The release `v0.4.9` (`tools/release.py github`) | GitHub's latest; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build (`ad161ae8…`), read by pacman as `sealwax 0.4.9-1` | Pass |
| Installing 0.4.9 on this machine (the owner, with INSTALL.md's command, then Ctrl+Q and a restart) | pacman's log: `upgraded sealwax (0.4.7-1 -> 0.4.9-1)` at 14:16; `pacman -Q` reads `sealwax 0.4.9-1`; `/usr/bin/sealwax` byte for byte the build's (SHA-256 `a4e1b07a…`) and says `sealwax 0.4.9`; the running Sealwax started at 14:16:30 from it | Pass |

### 0.4.8 — 2026-10-13

`v0.4.8` is `b59166c`; this machine, rustc 1.98.1. The release: [v0.4.8](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.8), at the owner's word ("post the reply and release 0.4.8").

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `b59166c`, `SEALWAX_CI_DISPLAY=47 bash tools/ci.sh --ui` | 1,509 passed, 0 failed, 98 ignored; 81 scripts, 7,805 of 7,805 steps; the hostile suite clean (60 cases, 273 s) | Pass |
| `python3 tools/release.py build` on `v0.4.8` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,087 passed, 0 failed, 90 ignored; warnings: "Skipping dependency checks", the patched lettre's four dead-code warnings, and a new line, "libfakeroot internal error: payload not recognized!", during packaging (earlier builds' logs weren't kept to compare); `pacman -Qp` reads `sealwax 0.4.8-1`, the same file list as 0.4.7's package; the binary says `sealwax 0.4.8`, builds lettre from `vendor/lettre/` and has no path of the home folder; `SHA256SUMS` checks | Pass |
| The release `v0.4.8` (`tools/release.py github`) | Four files, not a draft, GitHub's latest; the notes' link to issue #9 resolves; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build | Pass |
| Installing 0.4.8 on this machine | Not done (not asked) | Not verified |

### Proton Mail Bridge: a pinned certificate on this computer (issue #9) — 2026-10-13

[ADR-007 amendment](DECISIONS.md#adr-007-amendment--a-pinned-certificate-for-a-server-on-this-computer-2026-10-13). This machine; scripted servers with an rcgen certificate made like Bridge's (self-signed, CA:TRUE, for 127.0.0.1) and the fake engine; no real Bridge, nothing sent.

| Check | Result | Verdict |
| --- | --- | --- |
| Domain unit tests | Loopback hosts (`localhost`, 127.0.0.0/8, `::1`, `[::1]`; not `localhost.example.test`, `0.0.0.0`, `::`); a pin honoured only there; fingerprints read with or without colons, `sha256:` or openssl's `sha256 Fingerprint=`, and refused when short, long or not hex; `accounts.toml`'s `certificate` read, written and read back, refused off a loopback host and when malformed; Proton's preset is Bridge (127.0.0.1:1143 and 1025, STARTTLS) by domain and by MX | Pass |
| Engine unit tests | Bridge's certificate refused even as the only trusted root (`Certificate`, the reporter's failure); the pinned certificate connects and another is refused before any data; a pin off this computer ignored; the certificate read over TLS and after IMAP's, POP3's and SMTP's STARTTLS, with no LOGIN, AUTH, USER or PASS sent and the handshake stopped; a plaintext server gives `NoTls`; an IMAP login over STARTTLS refused without a pin and with the wrong one (no credentials sent) and logged in with the right one; SMTP STARTTLS with the right pin logs in, with the wrong one stops before AUTH; Proton behind an MX found as Bridge | Pass |
| `setup-bridge` self-test (new, 91 steps) | A Proton address: Bridge's settings, "Bridge password", its hint and link; the check fails on the certificate, `read-certificate` for IMAP, the trust step with the fingerprint and the focus on Trust and Sign In; Trust checks again pinned; SMTP's same certificate trusted without asking; the account saved with both pins. Don't Trust, Escape and an unreadable certificate show the failure; a server elsewhere gets no trust step. Screenshots of the password and trust steps looked at | Pass |
| CI, `SEALWAX_CI_DISPLAY=47 bash tools/ci.sh --ui` | 1,509 passed, 0 failed, 98 ignored; 81 scripts, 7,805 of 7,805 steps; the hostile suite clean (60 cases, 274 s). The first run stopped at Clippy (`large_enum_variant`: `Endpoint` grew), fixed by boxing `Detection::Found`'s settings | Pass |
| A real Proton Mail Bridge | None here | Not verified |
| SMTP STARTTLS with a pin against a real server | lettre's `dangerous_accept_invalid_certs` path and `peer_certificate`, scripted only | Not verified |

### 0.4.7 — 2026-10-13

`v0.4.7` is `6212d9b`; this machine, rustc 1.98.1. The release: [v0.4.7](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.7), at the owner's word ("push and release 0.4.7").

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `6212d9b`, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,494 passed, 0 failed, 98 ignored; 80 scripts, 7,721 of 7,721 steps; the hostile suite clean (60 cases, 273 s) | Pass |
| `python3 tools/release.py build` on `v0.4.7` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,074 passed, 0 failed, 90 ignored; warnings: "Skipping dependency checks" and the patched lettre's four dead-code warnings; `pacman -Qp` reads `sealwax 0.4.7-1`; the binary says `sealwax 0.4.7`, builds lettre from `vendor/lettre/` and has no path of the home folder; `SHA256SUMS` checks | Pass |
| The release `v0.4.7` (`tools/release.py github`) | Four files, not a draft or prerelease, GitHub's latest; the notes' link resolves; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build | Pass |
| Installing 0.4.7 on this machine | Not done (not asked) | Not verified |

### lettre patched for longer reply lines (issue #2) — 2026-10-13

[ADR-015 amendment](DECISIONS.md#adr-015-amendment--lettre-patched-for-longer-reply-lines-2026-10-13). This machine; the fake SMTP server, no real account.

| Check | Result | Verdict |
| --- | --- | --- |
| `vendor/lettre/` against the published crate ([vendor/README.md](../vendor/README.md)'s commands) | The `.crate`'s SHA-256 is the old lock entry's checksum (`f2c646bd…`); `diff -r` shows only `MAX_RESPONSE_LINE_BYTES` (1000 → 16_384) and its comment | Pass |
| `cargo test -p sealwax-engine --lib smtp::submit` | 16 passed: a 1,200-byte 554 is `Refused`, a 16,000-byte 451 is `Transient`, a 4,000-byte 250 is sent; 17,000-byte 554 and 250 lines stay `UnreadableReply`, logged as "a reply line longer than 16384 bytes" without the server's text | Pass |
| CI, `bash tools/ci.sh` (no `--ui`: the app is unchanged) | 1,494 passed, 0 failed, 98 ignored; formatting and Clippy clean; lettre's four dead-code warnings shown (a local crate's warnings aren't capped) | Pass |
| `cargo check --manifest-path fuzz/Cargo.toml --locked` | Builds with the patched lettre | Pass |
| The advisory check on the vendored lettre | Not run (no RustSec clone here); `tools/advisories.py` now reads path packages | Not verified |
| Live, `cargo run -p sealwax-engine --example oauth_live -- <address>` on `2312b6a`, the owner's Outlook.com account (the user's authorization of 2026-10-13) | Browser sign-in in 15.3 s (consumer account; IMAP and SMTP scopes); IMAP and SMTP took the token through the patched lettre; added, 6 folders synced (7 INBOX headers); after a restart a refresh from the keyring with a rotated token; removed, 0 keyring items left, scratch folders deleted; "live check passed". The runner's "folders listed" count said 0 both times though 6 folders synced (not looked into). Nothing sent | Pass |
| Live, `send_live` on `2312b6a` plus the runner, the owner's Microsoft 365 business account (the user's authorization of 2026-10-13) | Browser sign-in in 4.7 s (work account; SMTP scope granted); SMTP login through the patched lettre; 1. from the account to itself, 1,020 bytes: accepted in 0.5 s; 2. from `sendas-check@example.com`, which the mailbox doesn't own, 1,007 bytes: `Refused { code: 554 }` in 0.5 s, the refusal's text 4,356 bytes (the engine's debug length, lines joined) where 0.4.6 would have shown an uncertain send. The longest single line wasn't measured | Pass |
| A real Microsoft 365 reply over 1,000 bytes | The SendAsDenied refusal above, 4,356 bytes, read as a 554 | Pass |

### 0.4.6 — 2026-10-12

`v0.4.6` is `2531d52`; this machine, rustc 1.98.1. The release: [v0.4.6](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.6), at the owner's word ("post the reply and release 0.4.6"), held until the stall below was found.

| Check | Result | Verdict |
| --- | --- | --- |
| CI on the first 0.4.6 commit (`b06c89c`, then `1881810` with the thanks), three runs | Each stopped at `triage-frames`' junk step (one stall of 40.8–41.6 ms in 10 presses); 1,493 passed, 0 failed; the script alone passed 6 of 6 | Held; the stall investigated (below) |
| CI on `2531d52` (the same release commit, after the test's fix `01028ef`), `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui` | 1,493 passed, 0 failed, 98 ignored; 80 scripts, 7,721 of 7,721 steps (`triage-frames` 41/41); the hostile suite clean (60 cases, 273 s) | Pass |
| `python3 tools/release.py build` on `v0.4.6` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,073 passed, 0 failed, 90 ignored; only "Skipping dependency checks"; `pacman -Qp` reads `sealwax 0.4.6-1`; the binary says `sealwax 0.4.6`, carries the client ID (compared, never printed) and no path of the home folder; the packaged README has the Thanks; `SHA256SUMS` checks | Pass |
| The release `v0.4.6` (`tools/release.py github`) | Four files, not a draft or prerelease, GitHub's latest; the notes' three links resolve; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build, `sealwax 0.4.6-1` | Pass |
| Installing 0.4.6 on this machine | Not done (not asked) | Not verified |

### `triage-frames`' stall found: the backend's frames, not the action — 2026-10-12

The P2 follow-up of 2026-10-07 (the junk step's ~40 ms stall). CI on the 0.4.6 release commit stopped on it three times in a row. This machine, debug build, `tools/headless.sh 72` with CI's environment; timing prints added for the investigation only and removed after.

| Check | Result | Verdict |
| --- | --- | --- |
| `triage-frames` as CI runs it, 5 runs | Failed 2 of 5, the junk step: one press in 10 took 39.9–40.2 ms from the handler to the list's change (p50 0.8–1.3 ms) | Reproduced |
| Where the time goes (timing prints, 9 runs; frame prints in the last 3) | The store's write and its change event take about a millisecond; the GTK main loop stops for up to 85 ms at a time, and the gaps looked at were frames (from the frame clock's before-paint to its after-paint). In the 3 runs, 34 frames took over 30 ms, 15 of them within 200 ms after the reader put a message on screen; the presses came 20–200 ms after the reader's last present. A change that arrives while such a frame paints waits for it | Found: Broadway draws in software, and a frame that redraws the reader costs 40–60 ms |
| Waiting before each press for the reader to show the selected message and paint twice (`measure-triage`, kept) | The junk step clean (worst 12.8 ms over 10 runs), but the delete step stalled 34.9 and 38.0 ms in 2 of 10: later frames redraw the reader too (views attached, bodies loading) | Not enough alone |
| The same with the reader off screen (one pane: `resize 800 900`, the list pane) | 0 of 10 runs failed, 41 of 41 steps each; the worst change over every step and run 4.65 ms | Pass |
| Live (GTK on the GPU) | Not run here (no live-window authorization); earlier live measurements: the list's frames p99 7.94 ms (S3), the reader's scrolling frames p95 14.6 ms (M5) | Not verified for this step |

### The others in the header and Reply All by default (issue #8) — 2026-10-12

[ADR-013 amendment](DECISIONS.md#adr-013-amendment--the-others-in-the-header-and-reply-all-by-default-into-10-2026-10-12). This machine; the fake engine and the store backend, nothing sent.

| Check | Result | Verdict |
| --- | --- | --- |
| Unit tests | `recipients_line`: "to me" alone, "to me, Leah Kim", "and 1 other", an address in To and Cc once; `[reader] reply` parsed, refused values and a misplaced key; whom Reply All names (`everyone`, the user's addresses left out whatever their case) and the quick reply's label; the cards' buttons in either mode | Pass |
| `reply-default` self-test (new, 79 steps) | A message to the user, Tomas Berg (To) and Sana Malik (Cc) from Leah: the header says "to me, Tomas Berg, Sana Malik". By default `r`, the card's first button and Reply to Sender ask for a reply, `a` and the card's Reply all for a reply-all, the message menu lists Reply, Reply All, Forward. With `reply = "all"` written to `config.toml`, the shown message changes at once: the quick reply says "Reply to Leah and 2 others"; `r`, Ctrl+R and the card's first button ask for a reply-all, the card's Reply to sender and `win.reply-sender` for a reply; the menu lists Reply All, Reply to Sender, Forward; the quick reply opened in the composer asks for a reply-all; a message to the user alone says "Reply to Ingrid". Back to the default, `r` replies again | Pass |
| `reader` self-test | Its samples to the user and one other now say "to me, Tomas Berg" and "to me, Leah Kim" (they said "to me"); 93 of 93 | Pass |
| CI, `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui` | 1,493 passed, 0 failed, 98 ignored; 80 scripts, 7,719 of 7,719 steps; the hostile suite clean (60 cases, 273 s). Two earlier runs stopped: the keymap table in a working doc lacked Reply to Sender (USER_GUIDE.md's row was in another form), then `reader`'s old "to me" | Pass |
| The quick reply's Send with `reply = "all"`, and its label for a message whose card isn't expanded (read from the store) | Not exercised by a test: Send takes its kind from the same `reply_kind` as `r`, which is tested | Not verified |
| A reply-all's recipients in the real composer | The compose rules' `recipients` (unchanged, unit-tested since M6); not looked at in a composer window here | Not verified |

### 0.4.5 — 2026-10-12

`v0.4.5` is `f4e6cfa`; this machine, rustc 1.98.1. The release: [v0.4.5](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.5), made at the owner's word ("push and release 0.4.5").

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `f4e6cfa`, `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui` | 1,490 passed, 0 failed, 98 ignored; 79 scripts, 7,640 of 7,640 steps; the hostile suite clean (60 cases, 273 s) | Pass |
| `python3 tools/release.py build` on `v0.4.5` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,072 passed, 0 failed, 90 ignored; "Skipping dependency checks", and one `libfakeroot internal error: payload not recognized!` line while tidying, after which the package was made; `pacman -Qp` reads `sealwax 0.4.5-1`; the binary says `sealwax 0.4.5`, carries the client ID (compared, never printed) and no path of the home folder; `SHA256SUMS` checks | Pass |
| The release `v0.4.5` (`tools/release.py github`) | Four files, not a draft or prerelease, GitHub's latest; the notes' link absolute and resolving; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build, `sealwax 0.4.5-1` | Pass |
| Installing 0.4.5 on this machine | Not done (not asked) | Not verified |

### A message that left the list leaves the reader — 2026-10-12

[ADR-011 amendment](DECISIONS.md#adr-011-amendment--a-message-that-left-the-list-leaves-the-reader-within-150-ms-2026-10-12). From a user's report on X (Fastmail, a laptop): a deleted message stayed "a second or two" in the reader. This machine (Ryzen 9 5900X), headless under `tools/headless.sh` (Broadway: GTK and WebKit both draw in software, so paint times are the backend's, not a live session's). `measure-leave KEY COUNT [MS]` (new, `app/src/selftest/triage.rs`) times each delete from the command's handler to the first painted frame without the message in the reader; `conversation-add singles` (new) files 24 one-message conversations: a styled newsletter with 5 inline images, simple HTML and plain text in turn. The CPU-starved runs pin the app and its WebKit processes to one core shared with 2 or 5 busy loops (a scratch script), standing in for a slower machine.

| Check | Result | Verdict |
| --- | --- | --- |
| Before the change, release, 15 deletes | Gone p50 22.4, p95 80.6 ms; a newsletter laid out 23–30 ms after the reader was asked | Instant here, as the owner sees it |
| Before, release, a third of a core | Gone p50 82.6, p95 275.6 ms; the newsletter rounds 252–270 ms | Grows on a slower machine |
| After, release, a third of a core, timed in parts | The reader asked 22–31 ms after the handler; a newsletter laid out 92–127 ms later and the frame with it painted 149–157 ms after that; every next message laid out within 150 ms, so the change didn't act | Recorded |
| A sixth of a core, before → after, release, two runs each | The newsletter rounds 496–527 ms → 226–262 ms in 8 of 10: the next one shows 150 ms after the reader is asked, before its body is laid out, and that frame paints in 32–45 ms instead of ~290; the other 2 took 443 and 505 ms, laid out just inside the limit (144.7 and 150.0 ms) with the frame painting the whole newsletter; the other messages unchanged. The first round of a run (551 and 574 ms in the second pair, before and after) spends ~450 ms laying out a simple message, most likely making the reader's second WebView (not confirmed) | Pass |
| The real-world corpus, `reader_corpus --no-snapshots`, release | 276/276 heights, 0 blank, PASS; laid out p50 11.4 / p95 35.1 ms (143.7 ms for the first template rendered); painted p50 46.7 / p95 113.6 ms, the three tallest newsletters 337–462 ms | Recorded |
| `reader-leave` self-test (new, debug): the next message's layout can't finish (its inline image waits for an unanswered `FetchPart`) | Before the change the deleted message stayed 3,021.8 ms (the 3 s limit) and the step failed; after, 164.7 ms, 18 of 18 steps | Pass |
| The left message hidden while the next one isn't read yet | Not exercised by a test (reading the next one never took 150 ms here) | Not verified |
| CI, `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui` | 1,490 passed, 0 failed, 98 ignored; 79 scripts, 7,640 of 7,640 steps (`reader-leave` 18/18, `triage-frames` 39/39); the hostile suite clean (60 cases, 274 s); Clippy with `-D warnings` clean | Pass |
| On the reporter's machine | Not measured. The reader's debug log (`RUST_LOG=sealwax_app::reader=debug`) now gives each conversation's read, prepared and shown times | Not verified |

### 0.4.4 — 2026-10-12

`v0.4.4` is `babf75d`; this machine, rustc 1.98.1. The release: [v0.4.4](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.4), made unattended at the owner's word.

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `babf75d`, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,490 passed, 0 failed, 98 ignored; 78 scripts, 7,622 of 7,622 steps; the hostile suite clean (60 cases, 274 s) | Pass |
| `python3 tools/release.py build` on `v0.4.4` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,072 passed, 0 failed, 90 ignored; only "Skipping dependency checks"; `pacman -Qp` reads `sealwax 0.4.4-1`; the binary says `sealwax 0.4.4`, carries the client ID (compared, never printed) and no path of the home folder; `SHA256SUMS` checks | Pass |
| The release `v0.4.4` (`tools/release.py github`) | Four files, not a draft or prerelease, GitHub's latest; the notes' links absolute; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build, `sealwax 0.4.4-1` | Pass |
| Installing 0.4.4 on this machine | Not done (not asked) | Not verified |

### Sending from any account (issue #7) — 2026-10-12

[ADR-014 amendment](DECISIONS.md#adr-014-amendment--sending-from-any-account-2026-10-12). This machine; the store backend, no real account, nothing sent.

| Check | Result | Verdict |
| --- | --- | --- |
| `composer-accounts` self-test | 36 of 36 steps: from All Inboxes the From chip offers both accounts' addresses with the first chosen; picking the second account's moves the draft there in the store (account, From, the body kept); reopened it is still there; back to the first moves it back; Compose from the second account's Inbox starts from its address | Pass |
| The composer's other self-tests | `composer-store` 103/103, `composer-drafts` 116/116, `reply-quick` 95/95, `reply-commands` 137/137, `a11y-composer` 102/102 | Pass |
| CI, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,490 passed, 0 failed, 98 ignored; 78 scripts, 7,622 of 7,622 steps; the hostile suite clean (60 cases, 273 s); `cargo clippy --workspace --all-targets -D warnings` clean | Pass |
| The old account's Drafts-folder copy deleted on a real server after a move | Not run (the store's deletion is the existing tested path; no testbed run here) | Not verified |

### 0.4.3 — 2026-10-11

`v0.4.3` is `5d64449`; this machine, rustc 1.98.1. The release: [v0.4.3](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.3) (published just after midnight, 2026-10-12 local time).

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `5d64449`, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,490 passed, 0 failed, 98 ignored; 77 scripts, 7,586 of 7,586 steps; the hostile suite clean (60 cases, 273 s) | Pass |
| `python3 tools/release.py build` on `v0.4.3` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,072 passed, 0 failed, 90 ignored; only "Skipping dependency checks"; `pacman -Qp` reads `sealwax 0.4.3-1`; the binary says `sealwax 0.4.3`, carries the client ID (compared, never printed) and no path of the home folder; `SHA256SUMS` checks | Pass |
| The release `v0.4.3` (`tools/release.py github`) | Four files, not a draft or prerelease, GitHub's latest; the notes' links absolute; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build, `sealwax 0.4.3-1` | Pass |
| Installing 0.4.3 on this machine | Not done (not asked) | Not verified |

### The view a window opens on, and the conversation order (issues #5 and #3) — 2026-10-11

[ADR-009 amendment](DECISIONS.md#adr-009-amendment--the-view-a-new-window-opens-on-2026-10-11) and [ADR-016 amendment](DECISIONS.md#adr-016-amendment--the-order-of-a-conversations-messages-2026-10-11). This machine.

| Check | Result | Verdict |
| --- | --- | --- |
| `window-view` self-test | 20 of 20 steps: All Inboxes and Home's Sent each written to `layout.toml` and shown again by a new window after the window closed (background mode) | Pass |
| `reader-conversation-order` self-test | 25 of 25 steps: oldest first by default; `newest-first` written to `config.toml` turns the shown conversation (the latest on top, the quick reply above it), Next goes to older messages, a reply arriving goes on top; back to oldest first | Pass |
| Unit tests | `layout.toml`'s view round trip, broken values and a rename; the remembered view and its fallback to the first Inbox; `[reader] conversation_order` and its mistakes | Pass |
| A newest-first conversation, looked at (a scratch screenshot) | The heading, the quick reply, the latest card, then the older rows | Pass |
| CI, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,490 passed, 0 failed, 98 ignored; 77 scripts, 7,586 of 7,586 steps; the hostile suite clean (60 cases, 273 s); `cargo clippy --workspace --all-targets -D warnings` clean (a first run stopped on an example's `SidebarModel`, fixed) | Pass |
| A restart with a remembered view (the open thread's preload) | Covered by the unit tests of the same lookup, not by a restart | Not verified |

### 0.4.2 — 2026-10-11

`v0.4.2` is `01fe475`; this machine, rustc 1.98.1. The release: [v0.4.2](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.2).

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `01fe475`, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,487 passed, 0 failed, 98 ignored; 75 scripts, 7,541 of 7,541 steps; the hostile suite clean (60 cases, 273 s) | Pass |
| `python3 tools/release.py build` on `v0.4.2` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,070 passed, 0 failed, 90 ignored; only "Skipping dependency checks"; `pacman -Qp` reads `sealwax 0.4.2-1`; the binary says `sealwax 0.4.2`, carries the client ID (compared, never printed) and no path of the home folder; `SHA256SUMS` checks | Pass |
| The release `v0.4.2` (`tools/release.py github`) | Four files, not a draft or prerelease, GitHub's latest; the notes' links absolute; INSTALL.md's download URL downloaded with curl: the same SHA-256 as the build, `sealwax 0.4.2-1` | Pass |
| Installing 0.4.2 on this machine | Not done (not asked) | Not verified |

### The sending address after Microsoft sign-in, and the warning naming the message (issue #2) — 2026-10-11

[ADR-008 amendment](DECISIONS.md#adr-008-amendment--the-sending-address-after-microsoft-sign-in-2026-10-11) and [ADR-015 amendment](DECISIONS.md#adr-015-amendment--why-a-sends-outcome-is-unknown-2026-10-11). This machine; scripted engines, no real account.

| Check | Result | Verdict |
| --- | --- | --- |
| `setup-microsoft` self-test | 178 of 178 steps: signed in as another address, the step "Check the sending address" with the focus on Send from; Escape sends `CancelSignIn` and returns to the button; Keep adds the typed address, Send from the signed-in one; signed in as the typed address (case aside), no question | Pass |
| `reply-outbox` self-test | 101 of 101 steps: the banner and the question name "“Notes fourth” from Work (today at …)" | Pass |
| Unit tests | the engine's `Kept::fits` (typed or signed-in address, case aside, not a third); the store's outbox entries carry the subject and start time and leave the subject out of `Debug`; the app's `described` (today, yesterday, this year, another year, no subject, no account, a long subject) | Pass |
| The banner and the question with a subject, 1,440 and 900 px wide (a scratch script, screenshots looked at) | Three lines at 900 px; the dialog's text wraps | Pass |
| CI, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,487 passed, 0 failed, 98 ignored; 75 scripts, 7,541 of 7,541 steps (`a11y-setup` 132 of 132); the hostile suite clean (60 cases, 273 s) | Pass |
| A real Microsoft sign-in as another address | Not run | Not verified |

### Why a send's outcome is unknown (issue #2) — 2026-10-11

[ADR-015 amendment](DECISIONS.md#adr-015-amendment--why-a-sends-outcome-is-unknown-2026-10-11). This machine; the fake SMTP server, no real account.

| Check | Result | Verdict |
| --- | --- | --- |
| `cargo test -p sealwax-engine --lib smtp::submit` | 15 passed: replies to the end of data with a 1,200-byte line (554 and 250) and a non-SMTP reply are `UnreadableReply`, a closed connection and silence `NoReply`; the log says "a reply line longer than lettre's 1000 bytes" and holds none of the server's text | Pass |
| `cargo test -p sealwax --lib outbox`, `status` | 3 and 3 passed: the banner's and the question's wording for each reason, and for none | Pass |
| `reply-outbox` self-test (`outbox-engine uncertain NAME unreadable`, `no-reply`) | 101 of 101 steps; the banner and the question say "couldn't read the server's answer" and "didn't answer" | Pass |
| CI, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,485 passed, 0 failed, 98 ignored; 75 scripts, 7,510 of 7,510 steps; the hostile suite clean (60 cases, 274 s) | Pass |
| A real Microsoft 365 reply over 1,000 bytes | Not run (the reporter's diagnosis; no account here) | Not verified |

### 0.4.1 — 2026-10-11

`v0.4.1` is `ea65347`; this machine, rustc 1.98.1. The release: [v0.4.1](https://github.com/PixDevsApps/sealwax/releases/tag/v0.4.1).

| Check | Result | Verdict |
| --- | --- | --- |
| CI on `ea65347`, `SEALWAX_CI_DISPLAY=73 bash tools/ci.sh --ui` | 1,483 passed, 0 failed, 98 ignored; 75 scripts, 7,507 of 7,507 steps; the hostile suite clean (60 cases, 274 s) | Pass |
| `python3 tools/release.py build` on `v0.4.1` (tagged locally first, pushed only after the build) | makepkg's `check()` 1,068 passed, 0 failed, 90 ignored; only "Skipping dependency checks"; `pacman -Qp` reads `sealwax 0.4.1-1`; the binary says `sealwax 0.4.1`, carries the client ID (compared, never printed) and no path of the home folder; `SHA256SUMS` checks | Pass |
| The release `v0.4.1` (`tools/release.py github`) | Four files, not a draft or prerelease, GitHub's latest; INSTALL.md's download URL (`releases/latest/download/sealwax-x86_64.pkg.tar.zst`) downloaded with curl: the same SHA-256 as the build, `sealwax 0.4.1-1`. The notes' relative link to GMAIL.md didn't resolve on the release page; the notes were edited to an absolute link | Pass |
| Installing 0.4.1 on this machine | Not done (not asked) | Not verified |

### Gmail without the "Turn on IMAP" note — 2026-10-11

[ADR-008 amendment](DECISIONS.md#adr-008-amendment--gmail-with-an-app-password-documented-2026-10-11), its later paragraph. This machine.

| Check | Result | Verdict |
| --- | --- | --- |
| `cargo test -p sealwax-domain` (the new `gmail_gets_no_turn_on_imap_link`: no link for `imap.gmail.com` and `imap.googlemail.com`, the link kept for `imap.example.test` and `imap.notgmail.com`) | 484 passed, 0 failed; the user guide's keys test 3 passed | Pass |
| `cargo test -p sealwax-engine autoconfig` | 28 passed, 1 ignored | Pass |
| `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` | Clean | Pass |
| `setup-detect`, `setup` self-tests; `preview-gmail-guide` with the `gmail` fixture now without the link | 205 of 205, 140 of 140, 39 of 39 steps; the password step shows no note (the picture kept in `.local/guide/`, not in the guide until the next release) | Pass |
| A live lookup of a Gmail address | Not run (no network lookup with a real provider's address in this change) | Not verified |

### The Gmail guide's pictures — 2026-10-11

[ADR-008 amendment](DECISIONS.md#adr-008-amendment--gmail-with-an-app-password-documented-2026-10-11). This machine; a fake engine, no network.

| Check | Result | Verdict |
| --- | --- | --- |
| `tools/headless.sh 41 target/debug/sealwax --self-test tests/selftest/preview-gmail-guide.sealwax-test` (the new `gmail` detection fixture) | 39 of 39 steps; the assistant's lookup rows, Gmail's card and the "Turn on IMAP" note as on the owner's own 0.4.0 screenshot of the same step | Pass |
| `setup-detect` and `setup` self-tests after the fixture | 205 of 205 and 140 of 140 steps | Pass |
| `cargo fmt --check`, Clippy (`-p sealwax --all-targets -D warnings`) | Clean | Pass |
| The owner's Gmail account added with an app password, spaces taken out (the owner, 2026-10-11) | "That gmail sign in on Sealwax worked perfect" | Pass, sign-in only |
| A second Gmail account of the owner's, the app password pasted with its spaces (the owner, 2026-10-11) | "users can paste the password with spaces, no need to remove the spaces" | Pass, sign-in only |

### 0.4.0, the first public release — 2026-10-09

[ADR-021](DECISIONS.md#adr-021--a-public-repository-releases-on-github-and-the-aur-2026-10-09). `v0.4.0` is `b4fc4fe`; this machine, rustc 1.98.1, WebKitGTK 2.52.6.

| Check | Result | Verdict |
| --- | --- | --- |
| CI on the 0.4.0 tree, `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui` (21:34–21:43) | Stopped at `triage-frames`: Sealwax crashed in GTK's icon lookup 0.09 s after the first frame (`object_ref: assertion '!object_already_finalized'`, SIGSEGV in `gtk_icon_theme_lookup_icon`), the open intermittent crash; 1,482 tests and 67 scripts (6,687 steps) had passed | The known crash |
| CI again, on `7836445` (21:44–21:57) | 1,482 passed, 0 failed, 98 ignored; 75 scripts, 7,507 of 7,507 steps; the hostile suite clean (60 cases, 273 s) | Pass |
| The package build's `check()` on `7836445` (`tools/release.py build`, 21:57–22:01) | 760 passed, **1 failed**: `rules_run_on_new_mail_before_it_is_announced` read the Inbox before the rules' moves had finished (the fake server has no MOVE: UID COPY, then `\Deleted` and UID EXPUNGE; the test waited only for the copies) | A test race, fixed in `b4fc4fe` |
| That test, the old and the fixed binary, each run 24 at a time with a busy loop on each of the 24 cores, five rounds | Old: 20 of 120 failed; fixed: 0 of 120. Without the load, 12 at a time: 0 of 60 each. A capture of 24 old runs under load: 6 failed on the race, 1 on a later assertion (`RulesFailed` reported twice where once was expected; cause not found, a follow-up) | Pass |
| `cargo fmt --check`, Clippy (`-p sealwax-engine --all-targets -D warnings`), `cargo test -p sealwax-engine --test newmail` on `b4fc4fe` | Clean; 13 passed | Pass |
| `python3 tools/release.py build` on `v0.4.0` (22:22–22:26) | makepkg's `check()` 1,067 passed, 0 failed, 90 ignored; only "Skipping dependency checks"; the binary says `sealwax 0.4.0`, carries the client ID (compared, never printed) and no path of the home folder; the AUR tarball holds exactly the package's `usr/` entries, all owned by root | Pass |
| `packaging/aur/sealwax-bin/PKGBUILD.in` built with makepkg against the local tarball (a scratch folder, not installed) | A package of the same files, owned by root, its binary identical to the release package's; `provides = sealwax=0.4.0` | Pass |
| The public repository | `main` and the tags pushed to the new `PixDevsApps/sealwax`; GitHub answers "No commit found" for the old `main` (`cef353c`) and 404 for `docs/PLAN.md`; `sealwax-archive` private | Pass |
| The release `v0.4.0` (`tools/release.py github`) | Four files; INSTALL.md's download URL (`releases/latest/download/sealwax-x86_64.pkg.tar.zst`) downloaded with curl: the same SHA-256 as the build, read by `pacman -Qp` as `sealwax 0.4.0-1` | Pass |

- **Installed with INSTALL.md's command** (the owner's word, 22:32): the download, then `sudo pacman -U` with the owner typing the password: pacman's log `upgraded sealwax (0.3.0-1 -> 0.4.0-1)` at 22:32:16; `/usr/bin/sealwax` byte for byte the release package's, `sealwax --version` says 0.4.0.
- **Not verified:** the AUR package on the AUR (not published: the AUR closed new accounts); uninstalling.

### M9 a narrow design sits on the theme — 2026-10-09

The user's request (a newsletter's column on a light grey wrapper across a dark reader). On `main` at `1425cf9` with the change in the working tree; this machine, WebKitGTK 2.52.6, headless (Broadway, display 72).

| Check | Result | Verdict |
| --- | --- | --- |
| Unit tests: the paper stylesheet carries `--sealwax-margins-color` (`theme::webview`, the `webview_paper` snapshot), the margins script loads nothing and takes that colour (`reader::view`) | Pass | Pass |
| `reader` self-test: the newsletter sample sits on the theme (frame edgeless, the root painted the theme's background), the dark promotion keeps its card, the newsletter again in the narrow layout | 93/93 | Pass |
| `live-theme` headless: the newsletter sits on catppuccin-latte's background, then on matte-black's after the switch, in place | 29/29 | Pass |
| `reader_corpus --bodies` (release), 144 messages in matte-black and catppuccin-latte, every body's whole document looked at | Heights 276/276 exact and stable, 0 blank frames, 0 overflow, 0 canary connections, 0 criticals, PASS. Paper bodies on the theme, in each theme: 65 of the 69 templates, 55 of S2's 55; the full-width card kept for email-boilerplate-full and -lite, sendwithus-airmail-invoice and -oxygen-upsell, and the Outlook web reply. A first version that recoloured each wrapper's background left wrappers' borders across the margins (sendwithus-narrative-progress), and centred text alone hugged a dark promotion's headline; both fixed, the clip and the rule that centred text must fit the column | Pass |
| CI, `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui`, 20:39–20:58 (a first run stopped on the `webview_paper` snapshot, which pins the paper stylesheet; the snapshot updated) | 1,482 passed, 0 failed, 98 ignored; 75 scripts, 7,507 of 7,507 steps; the hostile suite clean (60 cases, 273 s) | Pass |

- **Not verified:** the user's own mail (Synsam's newsletter in their screenshot); a live look.

### M9 the build paths mapped — 2026-10-09

ML-095's P3 follow-up. On `main` at `62252fb` with the change in the working tree; this machine, rustc 1.98.1.

| Check | Result | Verdict |
| --- | --- | --- |
| The installed binary of `62252fb` (`strings`) | 1,195 paths into `~/.cargo/registry/src`, one into the build folder (`…/src/sealwax-0.3.0/app` + `../tests/theme`, from `CARGO_MANIFEST_DIR`) | The finding |
| The PKGBUILD's `prepare()` and `build()` run with cargo on a copy of the tracked working tree, `srcdir` a scratch folder, makepkg's `RUSTFLAGS` (`-C force-frame-pointers=yes`), 16:32–16:34 | Release build in 1 min 45 s; the binary: 0 matches of the build folder, 0 of `/home/fredrick`, 772 strings under `/usr/src/cargo/registry` | Pass |
| `live-theme` headless (display 85), from the repository and from `/tmp` | 27/27 both | Pass |
| CI, `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui`, until 16:47 | 1,481 passed, 0 failed, 98 ignored; 75 scripts, 7,503 of 7,503 steps; the hostile suite clean (60 cases, 272 s) | Pass |

- **Not verified:** makepkg's own lint (`Package contains reference to $srcdir`) on a package build.

### M9 a move the store can't place syncs its target — 2026-10-09

[ADR-015 amendment](DECISIONS.md#adr-015-amendment--a-move-the-store-cant-place-syncs-its-target-at-once-2026-10-09). On `main` at `91e2fec` with the change in the working tree; this machine, rustc 1.98.1.

| Check | Result | Verdict |
| --- | --- | --- |
| The user's store, read-only (`sqlite3 -readonly`, counts and ids only), 14:5x | The rule's folder (id 17, created that day): 6 rows, 4 unread; its 6 moves recorded without a new UID (`ops.new_uid` empty, the server sends no COPYUID); the journal: the folder's first sync at 14:44:09 fetched 6, 71 s after the moves were due | The cause |
| Engine, `a_move_without_copyuid_shows_in_its_target_at_once` (new; fake server with MOVE, no COPYUID, a target never synced) | Before the fix: failed (nothing in the target after 5 s; the account pass is 60 s). After: 2 messages shown and counted in the target with no ops pending, the Inbox at 18, nothing announced as new mail there; 0.56 s | Pass |
| Store, `a_move_without_a_new_uid_…` and `a_move_the_targets_sync_stored_first_…` | `Recorded::unplaced` names the target without a new UID, and nothing when the new UID places it | Pass |
| `cargo test -p sealwax-engine -p sealwax-infrastructure` | Every suite passed, none failed | Pass |
| CI, `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui`, 14:54–15:08 | Formatting and Clippy with `-D warnings` clean; 1,481 passed, 0 failed, 98 ignored; 75 scripts, 7,503 of 7,503 steps; the hostile suite clean (60 cases, 273 s) | Pass |

- **Not verified:** the user's server itself (their next try); a copy's target pass on a real server; a very large never-synced target (its first sync is its window, as when opened; not timed).

### M9 a rule saved runs on the Inbox's mail — 2026-10-09

[ADR-020 amendment](DECISIONS.md#adr-020-amendment--a-rule-saved-runs-on-the-inboxs-mail-2026-10-09). On `main` at `70e4915` with the change in the working tree; this machine (Linux 7.2.5, 24 threads), rustc 1.98.1.

| Check | Result | Verdict |
| --- | --- | --- |
| Engine, `cargo test -p sealwax-engine --test newmail rules_run_on_new_mail` (fake IMAP server) | Extended: Apply Rules gives one batch; one rule's text runs alone over a rules file that doesn't parse; with a 60 s undo window its 2 moves leave the Inbox at once (the overlay: 22 → 20), Undo restores both locally and nothing reaches the server; without the window the moves reach the server; a rule matching nothing answers 0 and no batch; a rule that doesn't compile answers `RulesError::Script` and no `RulesFailed`; passed (1.5 s) | Pass |
| Unit test `rules::text::a_rule_just_saved_says_where_it_moved_the_inbox_mail` | Where a rule moves mail (Junk before a folder before Trash, a folder by its leaf name) and the run's toasts | Pass |
| Self-tests by hand, display 85 | `rules-dialog` 162/162 (a new rule runs alone on the Inbox once saved: 2 messages leave the list, "Moved 2 messages from Inbox to Kunder" in the dialog, its Undo brings them back; a second rule matching nothing; a switched-off rule edited sends nothing), `rules-menu` 151/151 (Create Rule from Message: both of the sender's messages leave the Inbox, `z` in the list brings them back), `rules-events` 119/119 (Apply Rules' toast with Undo) | Pass |
| CI, `SEALWAX_CI_DISPLAY=72 bash tools/ci.sh --ui`, 13:47–14:00 | Formatting and Clippy with `-D warnings` clean; 1,480 passed, 0 failed, 98 ignored; 75 scripts, 7,503 of 7,503 steps; the hostile suite clean (60 cases, 273 s); after it one condition in the dialog simplified with the same behaviour, then Clippy clean and `rules-dialog` 162/162 on the rebuilt binary | Pass |

- **Not verified:** a real account (Outlook.com, the user's other server); a large Inbox (one batch per run, not timed); the self-tests' runs are the fake engine's, with the store's real rules queue and Undo; a live look.

### M9 rules in the app (B2) — 2026-10-09

Branch `m9-rules-app` (on `main` with A1 merged); machine as M5's; rustc 1.98.1.

| Check | Result | Verdict |
| --- | --- | --- |
| Unit tests: `cargo test -p sealwax --lib rules` and `triage` | 13 new (`rules::draft` 5, `rules::text` 3, `rules::file` 3, `rules` 1, `triage` 1), all passing; the registry's, the menu's and the keymap tables' tests updated for the five commands | Pass |
| Self-tests by hand, display 85 | `rules-dialog` 140/140, `rules-menu` 140/140, `rules-events` 109/109; `a11y-window` and `a11y-dialogs` with their new sections, all steps | Pass |
| Branch CI, `SEALWAX_CI_DISPLAY=86 bash tools/ci.sh --ui` on `09c4a16`, 09:36–09:50 | Formatting and Clippy with `-D warnings` clean; 1,477 passed, 0 failed, 98 ignored (75 test results); 75 scripts, 7,455 of 7,455 steps (`a11y-window` 177, `a11y-dialogs` 168); the hostile suite clean (60 cases, 0 DNS lookups in production, 274 s) | Pass |

- **Not verified:** a real account and the real engine reading a file the dialog saved (the engine's side is B1's tests); a pointer right click reaching the row's gesture (Broadway's doesn't headless; `row-menu` opens the menu as the gesture does, Menu and Shift+F10 were pressed for real); Open File on the desktop; what Orca says; a live look.

### M9 folders (A1): create, rename and delete, the folder tree — 2026-10-09

Branch `m9-folders`; machine as M5's; rustc 1.98.1.

| Check | Result | Verdict |
| --- | --- | --- |
| Engine against the fake IMAP server, `cargo test -p sealwax-engine --test folders` | 7 passed, five runs in a row: CREATE + SUBSCRIBE on three flavours, RENAME with a subfolder (subscriptions moved, messages and sync state kept, new mail after), DELETE deepest first with a folder the store never saw, a top folder under `INBOX.`, refusals (the engine's send nothing; the server's in its words), a lost reply, offline and unknown accounts, a POP3 account's local folders | Pass |
| Folder testbed suite, `cargo test --release -p sealwax-engine --test folders_testbed -- --ignored --test-threads=1` (user `spike`, its folders deleted at the end), 08:35 on `de5aed3` | 1 passed (0.28 s). Dovecot: create at the top 5.8 ms, under it 6.3 ms, rename with two subfolders 7.1 ms, delete of four 21.4 ms; Dovecot IMAP4rev1: 15.3, 6.7, 6.5, 18.8 ms; Stalwart: 1.8, 1.3, 1.6, 2.8 ms; a sibling's name refused before sending (under 0.1 ms) and a name only the server knew refused with `ALREADYEXISTS` on all three; a top folder went to the top on all three (delimiter `/`). An earlier run at 07:42 (before the last lint fixes) passed the same | Pass |
| `testbed` (M2's login, folder list and first sync) on `de5aed3`, 08:36 | 5 passed (33.9 s) | Pass |
| `sync_testbed` (the passes, whose folder removal `replace_folders` shares with the new delete) on `de5aed3`, 08:37–08:41 | 9 passed (183.4 s): the randomized two-client runs on four server variants with 0 divergences, the mid-FETCH cuts on three servers, the control that must diverge, the resync timings | Pass |
| Self-tests by hand, display 81 | `folders-tree` 101/101; `folders-dialogs` 152/152 three runs in a row; `a11y-window` 167/167; `a11y-dialogs` 150/150 | Pass |
| Branch CI, `SEALWAX_CI_DISPLAY=82 bash tools/ci.sh --ui` on `508c142` | 1,392 passed, 0 failed, 97 ignored; 72 scripts, 7,038 steps; the hostile suite clean (60 cases, 275 s) | Pass |
| Branch CI again on `4b28754` (the code of `de5aed3`), 08:41–08:54 | 1,392 passed, 0 failed, 97 ignored; 72 scripts, 7,038 steps; the hostile suite clean (60 cases, 274 s) | Pass |

- **Not verified:** Outlook.com, Microsoft 365 and a server with an `INBOX.` personal namespace (the fake server only); NAMESPACE (not sent: imap-proto 0.17 can't parse its reply); the dialogs on a POP3 account (the engine's path only); what Orca says; a live look. `tools/ci.sh --testbed` wasn't run (it asks Docker which containers run, and Docker commands weren't this work's to run); the suites above were run directly.

### M9 phase A: the user guide, the security review and fuzzing, the accessibility pass — 2026-10-07

Machine as M5's; rustc 1.98.1.

| Check | Result | Verdict |
| --- | --- | --- |
| The guide's shortcut table against the registry (`domain/tests/user_guide_keys.rs`, ML-094) | 2 tests; both failed when one key in the guide was changed by hand, both passed after | Pass |
| Fuzzing (ML-093), 12 targets, stable libFuzzer without sanitizers, 10 s per input, at most four jobs at `nice 10`, 10:16–11:31 | About 13,700 s in all. Final runs: `text` 1,215,890 executions; `pop3` 16,812,278; `attachment` 26,409,419; `autoconfig` 30,807,505; `search` 1,256,698; `mailto` 4,755,663; `theme` 7,974,939; `imap` 4,254,853; `config` 5,416,754; `links` 14,711,870; `html` 219,814 (2,401 s); `message` 105,883 (1,801 s); no crash, hang or input over 1 s. Earlier runs found six faults in Sealwax's code (F3, F5, F6, F7, F8), all fixed with tests, and two beneath it (imap-proto F15, mail-parser F20) | Pass, with F15 open |
| Deeply nested HTML (F14), release `sealwax-domain`, one run each beside four fuzz jobs | `message::parse` of 55,000 nested `<ul><li>` (990,000 bytes): 37.9 s; of 14,563 (262,134 bytes): 2.56 s; 55,000 nested `<div>`: 9.0 s | A proposal for M9's gate (a 256 KiB cap) |
| Advisories: `python3 tools/advisories.py -v target/advisory-db Cargo.lock` (RustSec `ef6173c`) | 464 locked packages, 1 match (RUSTSEC-2025-0007, `ring`, withdrawn), 0 vulnerabilities; `fuzz/Cargo.lock` the same | Pass |
| The accessibility audit (ML-092), five scripts in `tools/ci.sh --ui` | `a11y-window` 121 steps, `a11y-reader` 68, `a11y-composer` 80, `a11y-setup` 102, `a11y-dialogs` 125: every control named with a role and its states, no Tab trap, the planned Tab chains, plain-text links by Tab, Go to and Back to List | Pass (Orca not run) |
| SMTP STARTTLS fix (F1) and every testbed suite on `main`: `bash tools/ci.sh --testbed` on `45ec99b`, 12:35–13:15 | All 16 suites passed (`actor_testbed` 9, `append_testbed` 5, `bodies_testbed` 1, `log_grep` 1, `newmail_testbed` 5, `ops_testbed` 12, `outbox_testbed` 6, `parts_testbed` 1, `password` 1, `pop3_accounts_testbed` 2, `pop3_testbed` 7, `send_testbed` 6, `sync_testbed` 9, `testbed` 5, `write_testbed` 9, S1's 8); login and submission over STARTTLS on Stalwart 587 and Mailpit | Pass |
| The user's three security changes (phase B): `domain/tests/html_cap.rs` and probes, release, one run each | 990,000 bytes of nested lists at sync 19.8 s → 7.4 s; open `<ul>` 175 s → 6.3 s; open `<s>` over 300 s → 36.7 s; an actor restarted 105 ms after imap-proto's panic and received new mail; the web processes under bubblewrap with the variable set. Branch CI: 1,319 passed, 0 failed, 96 ignored; 67 scripts, 6,333 steps; the hostile suite clean (after one `triage-frames` flake) | Pass; a P1 stack overflow found (html2text, nested lists) |
| The depth limit and the crash-loop guard (phase B) | `domain/tests/html_depth.rs` on 2 MiB threads (every step at most 120 ms on the measured inputs); `html_depth::tests` (the count against html5ever's tree; 20,000 random soups at most 3 levels short; the same output on 20,061 inputs within the limit); deep regression inputs in `fuzz/corpus/`; the store's v11 → v12 migration and build marks; the at-most-once property test with "stopped while building" (3,000 cases); an engine start reporting `SendFailed(Interrupted)`; a POP3 reply end to end. Each new test group overflowed on the base commit's code (control). Branch CI: 1,334 passed, 0 failed, 96 ignored; 67 scripts, 6,333 steps; the hostile suite clean. On `main` (`tools/ci.sh --ui --testbed`, 16:59–17:52): 1,335 passed, 0 failed, 96 ignored; 67 scripts, 6,333 steps; the hostile suite clean; all 16 testbed suites passed | Pass |
| CI on `main` after the three merges: `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui`, 12:23–12:35 | 1,306 passed, 0 failed, 93 ignored; 67 scripts, 6,331 steps; the hostile suite clean. Three earlier runs stopped: the S7 spike's Bcc check (a random id spelled "bcc"; fixed) and twice `triage-frames`' junk step (a load-sensitive stall that `bb23c4e`, before M9, shows as often; a follow-up) | Pass |
| CI on the branches | Guide: `tools/ci.sh` 1,280 passed, 0 failed, 93 ignored. Security: `tools/ci.sh --ui` on `abfdd99` 1,303 passed, 0 failed, 93 ignored, 62 scripts (5,818 steps), the hostile suite clean. Accessibility: `tools/ci.sh --ui` on `9d2f583` 1,281 passed, 0 failed, 93 ignored, 67 scripts (6,331 steps), the hostile suite clean | Pass |

- **Not verified:** AddressSanitizer (nightly only); the Referer fix over a real redirect; the keyring-password steps in the guide; a clean-system install (ML-095); what Orca says; typing through fcitx5 in the composer after the focus changes around WebKit's key-binding field (live).

### The corpus harness's GPU memory — 2026-10-07

M5's P1 follow-up: `reader_corpus`'s GPU memory grew from 118 to 1,350 MiB. `--no-snapshots` skips its blank-frame checks; GPU memory sampled from `nvidia-smi`'s process table every 0.5 s (a scratch script; the app's own process; WebKit's processes never appeared). Release build of `5260e92` plus the option; machine as M5's (NVIDIA driver 610.57.04, WebKitGTK 2.52.6, GTK 4.22.4); live runs at the compositor's 1709×1400 under the tests authorization of 2026-10-07. Logs in `.local/reader-corpus-*.log` (not committed).

| Check | Result | Verdict |
| --- | --- | --- |
| Live with the snapshots, `reader_corpus` (05:32–05:36, 260 s) | The app's GPU memory 47 MiB at 1.4 s, then rising steadily (232 at 42 s, 701 at 130 s, 1,131 at 217 s) to 1,352 MiB. Heights 276/276, 0 blank, 0 criticals, PASS; laid out p50 18.9 / p95 65.1 ms | Reproduces M5's growth |
| Live without them, `reader_corpus --no-snapshots`, three runs (05:36–06:00) | The app's GPU memory 47 MiB from 1.4 s to the end in all three. Laid out p50 59.2 / 59.2 / 59.4 ms, p95 72.5 / 72.8 / 73.4; scrolling p95 14.16 / 14.23 / 14.29 ms | **The growth is the harness's snapshots** |
| The rows of those runs | Run 1: the first message after the switch to the light theme skipped silently (no view; `failures=[]`, still PASS). Run 2, with a first version of the new check: 12 false failures (plain-text messages have no view; the check corrected). Run 3, with the check: `VIEW_LATE theme=light id=antwort-single-column after=10.6ms`, then 276/276 heights, PASS | Harness fixed; P3 follow-up |
| Headless, `tools/headless.sh 33 … reader_corpus --no-snapshots` (05:51–05:55) | Heights 276/276, stable 276/276, 0 blank, 0 overflow, 0 canary connections, 0 criticals, PASS | Pass |

### Reader memory over 1,000 and 3,000 opens — 2026-10-07

M5's P1 follow-up: does the growth after 200 opens level off? `reader_memory --opens=N` opens N messages after the warm-up and prints the process tree's PSS every 200; the checks still judge the first 200. Release build of `6fe4321` plus the option; machine as M5's (NVIDIA driver 610.57.04, WebKitGTK 2.52.6, GTK 4.22.4); the live runs under the tests authorization of 2026-10-07, self-closing, no input, Hyprland tiling the window to 1709×1400. Logs in `.local/reader-memory-{1000,3000}*.log` (not committed).

| Check | Result | Verdict |
| --- | --- | --- |
| Headless: `SEALWAX_HEADLESS_TIMEOUT=600 tools/headless.sh 31 target/release/examples/reader_memory --opens=1000`, three runs, 05:16–05:19 (load 1.6–2.4) | Tree PSS after the warm-up 269.7 / 274.0 / 272.3 MiB; after 200 opens 290.8 / 294.8 / 295.1; after 1,000 287.5 / 293.1 / 288.3, with a swing to 303–313 at 800. WebKit's web process 108.0–109.2 MiB at 1,000 (105.4–107.7 at 200) | Levels off at 200 |
| Live: `reader_memory --live --opens=1000`, three runs, 05:19–05:22 | Tree after the warm-up 363.3 / 367.9 / 372.5 MiB; after 200 380.6 / 380.0 / 395.5; after 1,000 399.4 / 388.3 / 396.4. The app's process 225.9–241.8 MiB from 200 on, no trend; WebKit's web process 106.6 → 129.7, 106.1 → 119.6, 116.4 → 124.4 MiB from 200 to 1,000 (its anonymous memory; the driver mappings in it stay 18.0) | Still rising in WebKit at 1,000: the longer runs below |
| Live: `reader_memory --live --opens=3000`, two runs, 05:23–05:26 | Tree after 1,000 397.8 / 393.1 MiB, between 1,000 and 3,000 395.6–422.4 / 394.2–410.5, after 3,000 399.0 / 399.2. The app's process 238.2–248.9 / 232.8–243.6 MiB throughout; WebKit's web process 116.4–133.2 / 119.6–136.4 MiB from 1,000 on, falling back without a trend (133.2 at 2,000, 116.6 at 2,200) | **Levels off** |
| Growth over the first 200 opens in these runs (`growth_tree_mib`) | Live 17.3 / 12.1 / 23.0 / 20.0 / 11.7 MiB (2 of 5 over 20 MB); headless 21.1 / 20.8 / 22.8 | As at M5 |

Not measured: GPU memory by `nvidia-smi` (not sampled in these runs), real mail with remote images, sessions of hours (the M9 soak).

### M8 phase A integrated: POP3 accounts — 2026-10-07

Machine as M7's; the testbed with POP3 on Dovecot (127.0.0.1:14995, 14110) and Stalwart (16995, 16110). The soak (ML-089) and the gate are the user's.

| Check | Result | Verdict |
| --- | --- | --- |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui --testbed` on `6c74cc8` (the three branches merged, the lead's integration), 2026-10-07 04:21–05:11 | 1,278 passed, 0 failed, 93 ignored; all 62 scripts (new: `setup-pop3` 159, `setup-pop3-folders` 118, `refresh` 13 steps); the hostile suite clean (273 s); every testbed suite: `actor_testbed` 9, `append_testbed` 5, `bodies_testbed` 1, `log_grep` 1, `newmail_testbed` 5, `ops_testbed` 12, `outbox_testbed` 6, `parts_testbed` 1, `password` 1, `pop3_accounts_testbed` 2, `pop3_testbed` 7, `send_testbed` 6, `sync_testbed` 9, `testbed` 5, `write_testbed` 9, S1's 8 | Pass |
| The client on the testbed (`pop3_testbed`, release, the client agent's two runs) | 7 of 7 each: Dovecot and Stalwart over implicit TLS and STLS; alice's messages byte for byte with IMAP's copies and no flag changed (a `\Seen` message only); DELE taking effect only at a successful QUIT (a dropped session and RSET deleted nothing); 30 fault runs per server: a cut mid-RETR left the message whole on the server, a QUIT cut before it arrived kept it, a lost QUIT reply had always deleted it; connect 11–13 ms, a listing of 10,000 66 ms (Dovecot) and 4 ms (Stalwart), RETR of 1 MiB about 5 ms | Pass |
| An account's actor on the testbed (`pop3_accounts_testbed`, release, the accounts agent's three runs) | 2 of 2 each on `popclient`, Dovecot and Stalwart: 200 messages byte for byte with no `NewMail`; `CheckMail` → `NewMail` for 2 in about 20 ms; a reply through Stalwart's submission with its Sent copy filed locally; removal keeping the server's mail and leaving no blobs; with `leave_on_server = false`, the server's INBOX emptied | Pass |
| The assistant end to end (`testbed-pop3-setup`, debug, display 21, Stalwart 16995, 04:2x) | 34 of 34: the POP3 login checked, a wrong password refused naming "The incoming mail server (POP3)", alice added with `[account.pop3]` and `leave_on_server = true` in `accounts.toml`, the six local folders, 30 messages in the local Inbox. The first run (on Dovecot's 14995, before the move) also passed 34 of 34 but left alice's 10 unread messages read on Dovecot (its POP3 marks downloaded mail read); restored by Message-ID | Pass |
| CI after the setup merge (`645cd6a`) and after the client merge (`da27562`) | 1,197 and 1,239 passed, 0 failed; every script but `triage-keys` once (one step, load about 9; 3 of 3 alone); the hostile suite clean both times | Pass |

- **Flaky:** `triage-keys` once; the accounts agent saw `engine/tests/bodies.rs` fail 2 of 33 runs at line 381 (follow-ups).
- **Not verified:** real POP3 providers (the soak); LOGIN-DELAY, EXPIRE, busy maildrops and oversized messages on a real server; first downloads beyond 200 messages; XOAUTH2 against a server; POP3 with a Microsoft account.

### M7 integrated: search, notifications and background mode — 2026-10-07

Machine: AMD Ryzen 9 5900X, 62 GiB, NVIDIA RTX 3070 Ti, Linux 7.2.5-3-omarchy, WebKitGTK 2.52.6, system SQLite 3.53.4; the testbed's pinned images on 127.0.0.1; omarchy-shell (Quickshell 0.3.1) on the live session.

| Check | Result | Verdict |
| --- | --- | --- |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui` on `83052ee` (every phase merged), 00:55–01:05 | 1,188 passed, 0 failed, 84 ignored; all 59 self-test scripts (new: `search-suggestions` 158, `search-scope` 89, `search-rebuild` 39, `search-timing` 74, `background-activation` 19, `background-login` 50, `background-memory` 48, `background-start` 63, `background-window` 61, `notify-*` 281 steps in four); the hostile suite `production_failures=[] cases=60 dns=production=0 control=10 glib_criticals=0`, 273 s | Pass |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui --testbed` on `e3343e2` (the docs of the gate in the working tree), 2026-10-07 01:27–02:15, load average 1.3–2.7 at the start | 1,188 passed, 0 failed, 84 ignored; all 59 scripts; the hostile suite clean (274 s); every testbed suite: `actor_testbed` 9, `append_testbed` 5, `bodies_testbed` 1, `log_grep` 1, `newmail_testbed` 5, `ops_testbed` 12, `outbox_testbed` 6, `parts_testbed` 1, `password` 1, `send_testbed` 6, `sync_testbed` 9, `testbed` 5, `write_testbed` 9, S1's 8. Two earlier runs (01:11, 01:17) stopped at `composer-editing` 156/158 | Pass |
| Search at 100,000 messages (`sealwax-fixtures bench-search`, release, a copy of the search agent's v9 baseline store, 2026-10-07 01:06–01:08 and 01:08–01:09, load average 1.4–3.2) | The first 50 results of 38 shapes: worst p95 82.29 ms cold and 24.38 ms warm, then 88.51 and 24.54 ms; the worst cold shapes `the` in INBOX and the five-condition combination; a word typed a key at a time p95 ≤ 18.6 ms warm. Schema v10's pass 24.7 and 25.0 s, a flag change meanwhile p95 10.4 and 9.9 ms, a search p95 14.7 and 15.1 ms. Opening and migrating the v9 store 1,859 ms (right after an unsynced 1.1 GB copy) and 84 ms (after `sync`). Rebuild Search Index 19.3 s (5,181 messages/s): a flag change meanwhile p50 4.89, p95 11.08, p99 21.59, max 273.25 ms; a search p95 22.38 ms; consistency check ok | Pass (bar 100 ms) |
| Typing at 100,000 messages (`perf-search`, release, headless display 53, 01:09–01:10) | A key → results painted with no delay: p50 43.6 / p95 66.8 ms from the Inbox (n=32), p50 47.0 / p95 64.7 ms from All Inboxes (n=18); with GTK's old 150 ms: p95 215.4 and 217.9 ms | Pass; no delay |
| The live notification click (`notify_live --live`, debug, authorized at M7's start, the user clicking; 2026-10-07 00:55) | Step 1, the window open but unfocused: ActionInvoked → selected 6.2 ms → painted 14.9 ms, the right message in the list and the reader, the window active (Hyprland's active class the test app-id). Step 2, background mode, no window: a new window, selected 35.3 ms, painted 73.4 ms, the right message, active. GLib criticals 0. The first attempt (00:51) timed out: the popup left the screen after 8 s unclicked | Pass (M7 exit) |
| New mail in background mode, end to end (`python3 tools/testbed/notify_e2e.py 21`, debug, Stalwart's `newmail`, 00:39) | 5 of 5 runs, 33 of 33 steps each: the real engine synced, the window closed (the one-time notice first), another client's APPEND → the notification "Ada Example" / "Testbed end-to-end check" at the stand-in, a click → a new window with the message selected and in the reader; APPEND OK → the notification seen by the script 10, 23, 23, 20 and 23 ms | Pass |
| `NewMail` on the testbed (the notifications agent's `newmail_testbed`, release) | APPEND OK → `NewMail` median 1.7 ms, p95 1.8 ms on Stalwart, 497.8 / 499.3 ms on Dovecot (its IDLE delay); 20 each, each exactly its message; none for a first sync, older mail or a restart on three servers | Pass |
| Background mode (the background agent's runs, release, headless) | Activation → first frame p50 63.8 ms, p95 67.1 ms (10 runs, load 6.2); start → engine running without a window p50 47.4 ms; PSS flat over 30 close/reopen cycles after the leak fix (Sealwax 123.9 → 125.9 MiB closed; before the fix, debug, 155.7 → 381.2 MiB) | Pass |
| `composer-editing` (32 runs, four at a time, displays 19–25, 01:2x) | 4 of 32 failed step 16 (Undo unavailable after the first edit) on M7's build `e3343e2` and 4 of 32 on the pre-M7 build `11c8802`; the step failed in 3 of M7's CI runs | Flaky, older than M7 (P2 follow-up) |

- **Flaky:** `composer-editing` as above; `triage-frames` failed one timing step once under load after the first merge, and passes alone.
- **Not verified:** real accounts and real mailboxes; servers that show notification buttons; the background notice's click, Start Sealwax at Login at a real login and the INTEGRATIONS.md snippets on the live desktop; typing on after clicking a search suggestion on Hyprland; schema v10's pass on the user's own store; screen readers.

### M6 integrated: compose rules, the drafts store, sending, the composer, reply and the outbox — 2026-10-06

Machine: AMD Ryzen 9 5900X, 62 GiB, NVIDIA RTX 3070 Ti, Linux 7.2.5-3-omarchy, WebKitGTK 2.52.6; the testbed's pinned images (Dovecot 2.4.5, Stalwart 0.16.24, Mailpit 1.31.4) on 127.0.0.1; the live session at 3440×1440 and 144 Hz.

| Check | Result | Verdict |
| --- | --- | --- |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui --testbed` on `c07ece9` (every phase merged), 19:15–19:58 | 1,112 passed, 0 failed, 79 ignored; all 47 self-test scripts (new: `composer-editing` 158, `composer-recipients` 108, `composer-attachments` 73, `composer-safety` 72, `composer-drafts` 113, `composer-store` 103, `reply-commands` 129, `reply-quick` 95, `reply-quick-drafts` 60, `reply-outbox` 98, `reply-mailto-alone` 13, `reply-default-app` 30 steps); the hostile suite `production_failures=[] cases=60 dns=production=0 control=10 glib_criticals=0`, 273 s; testbed suites `actor_testbed` 9, `append_testbed` 5, `bodies_testbed` 1, `log_grep` 1, `ops_testbed` 12, `outbox_testbed` 6, `parts_testbed` 1, `password` 1, `send_testbed` 6, `sync_testbed` 9, `testbed` 5, then `write_testbed` 8 of 9 (Stalwart answered NO to moving two messages back from Junk), which stopped the run | Fail, flaky (below) |
| The rest by hand on the same build (20:00–20:06) | `write_testbed` 9/9 three times; S1's suites 8/8 | Pass |
| At most once (`outbox_testbed`, Stalwart through the fault proxy, a cut and a hard stop of the engine at five steps, 2 seeds; the agent's 4-seed run: 40 runs, the same pattern) | 20 runs: before MAIL, after RCPT and after the store's start with the connection cut, 10 queued and sent exactly once; a stop after the start, during the data and after the final dot, 10 uncertain, 0 sent again before the user's answer; "send again" exactly one more transmission 5 of 5, "it was sent" nothing more 5 of 5 (transmissions counted from the proxy's record: Stalwart drops a repeated delivery); Undo inside the window: MAIL FROM 0 times, the message's data 0 times | Pass (M6 exit) |
| Interop (`outbox_testbed`, M6's amended exit criterion: [ADR-007](DECISIONS.md#adr-007-amendment--interop-checked-on-the-testbed-m6-start-2026-10-06)) | Five S7 cases (Norwegian and emoji names and subject, a long URL, Bcc only, an inline image with a PDF and a UTF-8 text attachment, a reply with a quote) sent through the engine to Stalwart (bob, carol and dave) and Mailpit: every received copy round-trips through mail-parser to the draft's fields, attachments by name and bytes; each delivered or caught once with one Sent copy; Sealwax's transmitted bytes and Stalwart's copies have no Bcc field and no host name (Mailpit adds its own `Bcc:` and a `Received:` naming this machine, so its raw message isn't scanned) | Pass (M6 exit) |
| The Sent copy and drafts under faults (`outbox_testbed`, Dovecot, IMAP4rev1 Dovecot, Stalwart) | One Sent copy (`\Seen`) each, also with the APPEND cut mid-literal; the source `\Answered`; a draft upload whose reply was lost leaves one live copy; after the send no live draft copy | Pass |
| `kill -9` while composing (`tools/kill9-compose.sh`, debug, headless; a letter every 200 ms, SIGKILL 4–20 s in) | The composer agent's 12 runs (17:59–18:02): lost 0.97–3.51 s, mean 2.17 s; the lead's 6 (18:15, load average about 19): 1.41–3.34 s, mean 2.83 s; every reopened draft exactly a prefix of what was typed | Pass (M6 exit: ≤ 5 s) |
| The composer live (`composer_live --live`, release, 18:42:02–18:42:19, load average 1.4; authorized at M6's start) | Window tiled 1709×1400, Vulkan, the editor without acceleration. Edit → painted frame: small document p50 8.15, p95 9.94, max 16.88 ms (n=80, 1 over 16.7 ms); above a 199 KB quoted thread p50 10.15, p95 12.35, max 15.42 ms (n=40). Scrolling the quote at 1,500 px/s for 4 s: frame interval p50 13.90, p95 14.62, max 21.22 ms over 282 frames (7 over 16.7 ms) | Pass (S7's bar < 16 ms; S2's scroll bar) |
| Mid-APPEND cuts (`append_testbed`, the sending agent's 20 seeds) | 240 runs on three servers: 0 duplicates, 0 lost, 0 old draft copies left live; a blind replay made 2 copies on all three | Pass (S1's scenario) |
| SMTP cuts (`send_testbed`, Stalwart, 20 seeds) | Before DATA, retried: delivered once 20 of 20; the reply after the final dot lost or held: uncertain, delivered 40 of 40; a cut inside the message: uncertain, not delivered 20 of 20 | Pass |
| Trace-level log grep (`log_grep`, the send, APPEND and outbox flows) | No password, token, address, subject, body or folder name; it caught html5ever's debug output carrying a draft's body, now capped ([ADR-018](DECISIONS.md#adr-018-amendment--the-html-parser-capped-m6-2026-10-06)) | Pass |
| The drafts store (100,000 messages, release) | A 200 KB draft saves in p99 1.24 ms idle (p95 25 ms during a 20,000-message ingest); suggestions p95 ≤ 13.2 ms with 50,000 extra contacts; the at-most-once property test 3,000 cases | Pass |

- **Flaky:** `write_testbed` failed once in the gate run (Stalwart's NO to a move back from Junk) and passed 3 of 3 on re-runs; `list-older` failed one step once under load (a late "folder opened" after All Inboxes) and passed 4 of 4 alone.
- **Not verified:** rendering in Gmail web, Outlook web and Thunderbird (the user's own test, ADR-007); Microsoft's SMTP with a token and its own Sent copy; faults on a STARTTLS submission port or on Mailpit; the portal's file chooser and a real drag from a file manager into the composer; the 60 Hz re-measure (the user's); screen readers.

### M5 integrated: conversations, files and remote content, rendering and safety — 2026-10-06

Machine: AMD Ryzen 9 5900X, 62 GiB, NVIDIA RTX 3070 Ti (driver 610.57.04), Linux 7.2.5-3-omarchy, WebKitGTK 2.52.6; the live session at 3440×1440 and 144 Hz, Hyprland tiling the measured windows to 1709×1400.

| Check | Result | Verdict |
| --- | --- | --- |
| CI with all three branches (`aded70a`), `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui --testbed`, 13:42–14:14 | 1,001 passed, 1 failed, 62 ignored; all 35 self-test scripts; hostile suite `production_failures=[] cases=60 dns=production=0 control=10 glib_criticals=0`, 275 s. Testbed: `actor_testbed` 9, `bodies_testbed` 1, `log_grep` 1 passed; `ops_testbed` 11 of 12 (`crash_replay_stalwart`: one password check after a simulated crash answered "failed", 15 of 16 scenarios passed), which stopped the run | Fail, flaky (below) |
| The testbed suites again, one at a time, release (14:14–14:23) | `ops_testbed` 12/12, `parts_testbed` 1, `password` 1, `sync_testbed` 9, `testbed` 5, `write_testbed` 9, S1's 8 | Pass |
| `parts_testbed` (13:13): a 12,042,932-byte message stored as its text parts | Its 4.4 MB attachment by section in 42.2 / 42.7 / 28.3 ms and the original in 63.3 / 58.4 / 25.0 ms (Dovecot / IMAP4rev1 Dovecot / Stalwart), byte for byte; nothing marked `\Seen`; 36 trace-level log lines with no secret, address, folder name, subject or body text | Pass |
| Final `main` (`d24c79c`), `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui` | 990 passed, 0 failed, 62 ignored; 33 of 35 self-test scripts passed in the run (14:31–14:35); `triage-undo` failed once (the `z` after a deleted-for-good message's toast had gone undid nothing: 92 of 95 steps) and stopped the run; it then passed 5 of 5 on the same build, `window-controls` 24/24 and `window` 107/107 ran by hand, and the hostile suite with the DNS canary as `ci.sh` runs it: `production_failures=[] cases=60 dns=production=0 control=10 glib_criticals=0`, 273 s | Pass, with one flaky self-test (follow-up) |
| S2's bars on the real corpus, live: `SEALWAX_OMARCHY_STATE=<scratch> target/release/examples/reader_corpus` (14:27–14:31) | 144 messages (69 open-licensed templates, 69 of S2's, 6 personal-style replies) in matte-black and catppuccin-latte: heights 275/275 exact and stable; 0 blank frames (WebKit's snapshot and GTK's render); 0 overflow; 0 canary connections and TLS hellos; 0 criticals. Open: laid out p50 18.1 / p95 59.0 ms, painted p50 25.4 / p95 70.6 ms. Scrolling the three tallest bodies (9,828–12,997 px) in each theme: 2,408 frames, p95 14.60 ms, 6–16 frames over 16.7 ms per scroll, max 66.5 ms | Pass (open ≤ 150 ms p95, scrolling ≤ 16.7 ms p95, 0 blank, heights ±1 px) |
| Memory, live: `target/release/examples/reader_memory --live --judge`, five runs (14:25–14:27); GPU memory sampled from `nvidia-smi` every ~0.5 s | Process-tree PSS with a 10-message conversation open: 365.7 / 378.0 / 376.4 / 379.4 / 378.2 MiB (383.5–397.8 MB). NVIDIA's libraries and device mappings in it: 96.9–97.1 MiB (app 71.3–71.5, web process 16.6, network process 9.0); without them 268.8–282.3 MiB (281.9–296.0 MB). Before the conversation: 323.6–331.7 MiB. Live views at most 3 (cap 4). Growth after 200 opens: 15.0 / 22.9 / 15.2 / 22.2 / 25.4 MiB (15.7–26.6 MB; the app 9.5–10.2 MiB each run, the web process 4.8–15.9). GPU memory: the app 47 MiB steady, 55 MiB peak; WebKit's processes none | Over 300 MB counted with the driver's mappings, under it without them: **the user's decision**; growth over 20 MB in 3 of 5 runs |
| GPU memory during `reader_corpus` (`nvidia-smi`) | The app's process grew from 118 to 1,350 MiB over the 288 renders; likely the harness's full-height snapshots (the memory harness's 200 opens stayed flat) | Not established (follow-up) |
| Headless, the agents' runs (not judged) | `Store::conversation` at 100,000 messages: ≤ 1.66 ms cold, ≤ 0.42 ms warm for 2 and 150 messages. `reader_memory` under Broadway: tree PSS 231.9 MiB open, growth 15.4 MiB. `reader_corpus` under Broadway: heights 276/276, 0 blank | Recorded |
| The remote-content canary (`app/examples/remote_senders.rs`, three runs, display 35) | 30 checks each: 0 connections until allowed; Show images and "Always for this sender" bring TLS hellos only, never a request; another sender's card stays at 0; Hide returns to 0 | Pass |
| After the gate's keys (Ctrl+U, Ctrl+S, `n`/`p`), `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui` (ended 15:32) | 990 passed, 0 failed, 62 ignored; all 35 self-test scripts (`triage-keys` 221/221, `reader-conversation` 118/118 with the `n`/`p` steps); hostile suite clean (60 cases, 0 DNS lookups), 273 s | Pass |
| The user's checks on 2026-10-06 | Live keyboard pass approved; fcitx5 typing approved (above); the notification click passed (above) | Pass |

Not verified: the portals' file chooser, the default application and the print dialog on the live session; screen readers; real providers; whether memory growth levels off past 200 opens.

### M4 integrated: the op runner, the app on the queue, end to end — 2026-10-06

On `main` after every M4 merge (`e33be76`), development machine, the Docker testbed up since the lead seeded it.

| Check | Result |
| --- | --- |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui --testbed`, 09:00–09:33 CEST | 878 passed, 0 failed, 61 ignored; all 32 self-test scripts (`triage-actions` 175, `triage-counts` 55, `triage-frames` 39, `triage-keys` 221, `triage-read` 61, `triage-select` 152, `triage-undo` 95 steps); every testbed suite passed: `actor_testbed` 9 (1,242 s), `ops_testbed` 12 (72 s), `write_testbed` 9, `sync_testbed` 9, `testbed` 5, `bodies_testbed` 1, `password` 1, `log_grep` 1, S1's 8 |
| `testbed-triage` (lead, 08:59, display 25; user `triage` after `triage_mailbox.py fill dovecot stalwart`) | 229/229; `triage_mailbox.py show`: on both servers INBOX Triage 1, 2, 5, 7 seen and 8 flagged and seen (1 back after an Undo that followed its archive, 2 kept by an Undo inside the window), Archive Triage 6, Prosjekter Triage 3, the Trash empty (Triage 4 deleted for good) |
| Crash-during-queue replay (`ops_testbed`, the runner's agent): MOVE, COPY, STORE, EXPUNGE; the connection cut or the engine stopped hard after the server got the command, then a new engine on the same store and blobs; three servers, 5 seeds each | 110 runs, 506 messages: 0 duplicates, 0 lost, the store equal to the server every time |
| Undo window end to end (`ops_testbed`) | Inside the window: no command for those messages on the wire (the proxy recorded every client line), three servers. After the archive ran: the message back in the Inbox, the same row with its downloaded body |
| Randomized two-client suite with local triage (`ops_testbed`) | 15 runs (5 seeds per server), a UIDVALIDITY reset in each: 0 differences against a model of every action not undone, 0 refused, nothing pending |
| Runner timing | A move queued with a 1.5 s window reached the server 1.508–1.509 s later |
| Same frame with the store's queue (`triage-frames`, headless, the app's agent) | Handler → list changed p50 0.33–0.76 ms; one message's p95 ≤ 4.86 ms; three selected archived at once p95 11.15 ms (5 samples); an account with a key p95 ≤ 3.15 ms. Broadway's frame clock is unpaced, so paints aren't judged |
| Gate previews | `preview-m4`: 17 images in `tools/progress/public/m4-*-2026-10-06.png` |
| Not verified | The live session and the US and Norwegian layouts live; real providers; a real crash or suspend; the runner with thousands of pending ops |

### M4 phase A: the op store, the IMAP writes, the app's triage — 2026-10-06

Development machine (Ryzen 9 5900X, Omarchy, Linux 7.2.5-3-omarchy), debug builds for CI and the self-tests, release for measurements and the testbed suites, the Docker testbed up since the lead seeded it.

| Check | Result |
| --- | --- |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui` on `main` after the last merge (`42bcc70`) | 864 passed, 0 failed, 49 ignored; all 31 self-test scripts (new: `triage-actions` 164, `triage-select` 152, `triage-undo` 82, `triage-read` 61, `triage-keys` 221, `triage-frames` 16 steps) |
| `bash tools/ci.sh --testbed` on `main` with the store merged (`ab938a2`) | 824 passed, 0 failed, 40 ignored; every testbed suite passed: `actor_testbed` 9 (1,265 s), `sync_testbed` 9, `testbed` 5, `bodies_testbed` 1, `password` 1, `log_grep` 1, S1's 8 |
| `write_testbed` on `main` after the writes' merge (`8df2ac7`), release | 9 passed (19.0 s) |
| Store: triage → change event, release (the store's agent) | One message p99 ≤ 3.85 ms at 100,000 messages, ≤ 0.67 ms at 10,000 (target 6.9 ms, a frame at 144 Hz); 50 adjacent messages p99 3.1 ms; Undo about the same. Up to 64 messages spread across a 100,000-message folder: 41–107 ms (M3's per-row position diff; open issue) |
| Store: S3's bars at 100,000 with schema v7 | Folder open ≤ 12.8 ms cold; one-insert diff p99 4.66 ms; pages as on `1d8dd96`; flag changes p99 1.69 → 2.13 ms; the migration about 1 s, the file about 7 MiB larger |
| Store: property test (queue, undo, server changes, UIDVALIDITY resets, outcomes, reopening, against a model) | 32 cases a CI run; 5,000 once (255.5 s), passed |
| Writes: S1's lost replies and cuts through the proxy, three servers | 250 runs (20 seeds per lost reply of MOVE, COPY, the `\Deleted` STORE and UID EXPUNGE, 10 per cut on send), 2,325 messages moved: 0 duplicates, 0 lost, nothing left in the source or `\Deleted` in the target. The blind-replay control duplicated 5 of 5 on both Dovecots |
| Writes: timings on the testbed, release (p50, 500 messages) | Flags 12.3 ms (Dovecot), 11.9 ms (IMAP4rev1 Dovecot), 14.0 ms (Stalwart); moves 60.9 ms (UID MOVE), 68.4 ms (the COPY path), 32.1 ms (Stalwart) |
| App: the same-frame measurement (`triage-frames`, headless, display 23) | The list changed 0.26–0.92 ms (p50) after the command's handler with the phase A stand-in backend; the slowest step's p95 5.3 ms; no p95 over 16.7 ms in any run (one archive once took 20.4 ms on a loaded machine) |
| Gate previews | `preview-m4` 93/93: 17 images in `tools/progress/public/m4-*-2026-10-06.png`, dark and light |
| Not verified | The op runner and the queue end to end (phase B); real clicks on buttons through Broadway (the scripts activate them); the live session and the US and Norwegian layouts live; real providers |

### M3 gate: the server's counts for windowed folders — 2026-10-06

Development machine, debug builds for CI and the self-test, release for the testbed suites, the Docker testbed up.

| Check | Result |
| --- | --- |
| `SEALWAX_CI_DISPLAY=31 bash tools/ci.sh --ui` on the branch | 783 passed, 0 failed, 40 ignored; all 25 self-test scripts |
| `testbed-setup.sealwax-test` against the testbed (headless, display 31) | 73/73: `perf`'s Inbox shows 3,333 unread (66 before) with 200 rows held; its heading "3333 unread · Perf"; All Inboxes "3343 unread · 2 accounts" with 230 rows |
| `sync_testbed`, `actor_testbed`, `testbed` | 9, 9 and 5 passed; 0 divergences and 0 count mismatches in 40 of 40 randomized runs; the counts equal the server's at the actor's first check every time |
| `a_refused_login_halts_until_reconnect` | Flaky before the change (1 failure in 30 runs on `29473f0`); now waits 300 ms before counting logins: 40 of 40 |

### M3 6-hour soak (ML-049) — 2026-10-05 to 2026-10-06

`SEALWAX_TESTBED_CA=$PWD/tools/testbed/certs/ca.pem SEALWAX_SOAK_MINUTES=360 cargo test -p sealwax-engine --release --test soak -- --ignored --nocapture` on `9a2f449`, development machine, 2026-10-05 19:25:08 CEST to 01:26, under `systemd-inhibit --what=sleep`. Log: `.local/soak-m3-6h-2026-10-05.log`.

| Check | Result |
| --- | --- |
| Duration, verdict | 361.2 min, **0 problems**; `test soak ... ok` (21,670 s) |
| Load | Four accounts through the fault proxy (Dovecot, IMAP4rev1 Dovecot, Stalwart, Stalwart's 10,000-message `perf` read only). The other client: 7,082 appends, 10,578 flag changes, 2,164 flag storms, 4,554 moves, 4,585 expunges, 2,527 copies, 316 UIDVALIDITY resets. Faults: 25 cuts, 19 black holes with `Reconnect`, 17 unannounced, 17 suspends. The user's side: 1,420 folders opened, 1,436 older-mail requests, 1,399 body requests |
| Hourly checks (61, 120, 181, 242, 301 min) and the end | Every account equal to its server; the store's check 0 problems. Time to converge once paused: account 1 (Dovecot, IDLE) 18.3–88.8 s, the others 0–29.7 s (limit 600 s) |
| Resources | File descriptors 47–62; resident memory 44 → 47 MiB |
| Not shown | async-imap's tag wrap at 10,000 commands (connections cut every few minutes); a real suspend or network; real providers |

### M3 integrated: sync core, the per-account actor, account status, threading — 2026-10-05

On `main` after every M3 merge (`60b7054`), development machine (Ryzen 9 5900X, Omarchy, Linux 7.2.5-3-omarchy, rustc 1.98.1), the Docker testbed up since the lead seeded it.

| Check | Result |
| --- | --- |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui --testbed`, 18:30–19:01 | 774 passed, 0 failed, 40 ignored; all 25 self-test scripts; every testbed suite passed: `actor_testbed` 9 (1,241 s), `sync_testbed` 9, `testbed` 5, `bodies_testbed` 1, `password` 1, `log_grep` 1, S1's spike suites 8 |
| Randomized two-client test, seeds 1–10 × 1,000 changes, one UIDVALIDITY change per seed (the agents' runs and each `--testbed` run) | Sync core with a polling driver: 0 divergences in 40 of 40 runs. Through the engine's actor (IDLE, account passes, the STATUS backstop): 0 in 80 of 80 (bursts and paced), then 40 of 40 paced in each of four `--testbed` runs. Dovecot, Dovecot without QRESYNC, IMAP4rev1 Dovecot, Stalwart. The control that leaves INBOX unsynced diverges (103–129 differences) |
| New mail ≤ 5 s through IDLE (APPEND's OK → in the store, 20 samples a run) | Dovecot p50 500–501 ms, max 512 ms (its own notification delay); Stalwart p50 6 ms |
| Faults through the proxy | Cuts mid-FETCH (first sync, flag fetch, new mail) and while idling, and a 5 s black hole lifted with `Reconnect` (everything stored 17–24 ms after it): nothing lost or duplicated |
| No-change account pass, `perf` (10,000 messages), release | p50 5.9–7.3 ms on Dovecot, 0.3–0.7 ms on Stalwart, 8.8–35.1 ms with IMAP4rev1's full flag scans (worst 41.3 ms); S1's bars 200 ms and 2 s |
| Soak harness, 30 minutes (the actor's agent) | Four accounts, 2,548 changes, six faults (a cut, black holes with and without `Reconnect`, two suspends): four checks equal to the servers, the store's check clean. The 6-hour run waits for the user |
| Account status (ML-048) | `status-banners` 257/257, `status-password` 182/182, `status-offline` 118/118; engine `password.rs` and `keyring_fallback.rs`; `UpdatePassword` on the testbed as `bob` |
| Threading and All Inboxes (ML-046, ML-047), release measurements on 3–5 fixture accounts of 90,000–100,000 messages | Property test 64 cases a CI run, 5,000 once (passed); All Inboxes opens ≤ 32.3 ms cold; one new message's diff p99 ≤ 5.2 ms (one maximum 24.9 ms on a cold cache under load); deep filtered pages 0.3–1.7 ms warm, ≤ 17.7 ms cold |
| Gate previews | `preview-m3-status` 180/180 (28 images), `preview-m3` 54/54 (8 images): `tools/progress/public/m3-*-2026-10-05.png` |
| Not verified | Real providers and real accounts (the user's manual test once the app is finished); a real suspend and a real network change; slow or lossy links; mailboxes far beyond 10,000; Thread-Index on real Outlook mail; the live session |

### M3 bodies (ML-044) — 2026-10-05

Debug builds unless noted.

| Check | Result |
| --- | --- |
| Store (`body_tests`, 6 + a child) | Upgrade in place with one exact change event, full text past the preview; orphaned blobs of gone messages; the `bodies` table follows expunge, reset and account removal; eviction in last-used order sparing `keep`, without an event; SIGKILL between a blob and its commit, 4 rounds: integrity and FTS integrity pass, the killed row unchanged, one orphan, no missing blob |
| Scripted IMAP (`imap::bodies`, 7) and the service offline (`engine/tests/bodies.rs`) | Whole messages in one FETCH; a 12 MB message as text parts with its attachment fetched on demand; UIDVALIDITY changed and vanished UIDs `Gone`; a cut connection leaves the row unchanged; only `BODY.PEEK`; `FetchBodies` coalesced, one answer per message, `NeedsSignIn` on a refused login, `Command::Storage` evicting |
| Testbed, release (`cargo test -p sealwax-engine --release --test bodies_testbed -- --ignored`, 14:40:44–14:41:01) | 1 KB, 5 MB and 12 MB fixtures on Dovecot (both) and Stalwart: stored within 81.7–108.6 ms of connecting; the 12 MB one as 810 B; an attachment of 4,400,000 B on demand in 29.5–51.8 ms; still unread afterwards; 288 trace-level log lines with no secret, address, folder name, subject or body text |
| `reader-bodies` self-test | 86/86 (downloading, success through the change event, offline, needs sign-in, gone, Retry, coalescing, no request loop) |
| `SEALWAX_CI_DISPLAY=33 bash tools/ci.sh --ui` (14:57:49–14:59:05) | 715 passed, 0 failed, 20 ignored; all 21 self-test scripts pass |
| Not verified | Real providers; slow links (the 60 s read timeout covers a whole 10 MiB literal); the cap at tens of thousands of bodies; the reader's states on the live session |

### ML-032 Microsoft sign-in in the app — 2026-10-05

Debug builds.

| Check | Result |
| --- | --- |
| Engine, offline (a fake Microsoft, a simulated browser, scripted IMAP and SMTP servers) | 15 OAuth unit tests, 7 service tests in `engine/tests/sign_in.rs`, a trace-level no-secret test (no code, verifier, token or ID token in any file or log line; no username, address or sign-in URL in the logs) |
| App, headless with a scripted engine | `setup-microsoft` 147/147, `setup-microsoft-failures` 104/104, `setup-microsoft-banner` 164/164 in CI; `preview-ml032` 80/80 wrote 10 previews (`tools/progress/public/ml032-*-2026-10-05.png`) |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui` after integration | 691 passed, 0 failed, 18 ignored; all 20 self-test scripts pass |
| **Live**, on the S5 account (authorized): `cargo run -p sealwax-engine --example oauth_live -- <address> --resign-in`, 13:32–13:33 CEST (a run at 13:18 timed out unanswered; the owner missed the tab) | Signed in after 33.9 s, the owner's sign-in included; IMAP and SMTP took the token as the ID token's user, which differs from the address typed. Added; first sync: 12 folders, 0 INBOX headers (the INBOX is empty). After a restart: a refresh from the keyring, synced again, and the rotated refresh token replaced the stored one. Re-sign-in after 11.6 s replaced the refresh token and synced. Removed (12 folders); 0 keyring items under the test app-id; scratch folders deleted |
| Not verified | The app's own sign-in on the live session (UI covered headless only); a revoked token live (scripted only); a Microsoft 365 work account; header sync of real messages on Outlook.com (the INBOX was empty) |

### S5 OAuth and secrets, Microsoft (ML-013) — 2026-10-05

Debug build of `spikes/s5-oauth` on the development machine, against the owner's unused Outlook.com account (authorized 2026-10-05).

| Check | Result |
| --- | --- |
| PKCE loopback sign-in in the system browser | Completed in 26.6 s, the owner's sign-in included; access token 3,599 s and a refresh token; no `login_hint` |
| IMAP XOAUTH2 (`outlook.office365.com:993`) | Login 629 and 640 ms in two processes; 12 folders listed |
| SMTP XOAUTH2 (`smtp-mail.outlook.com:587`, `smtp.office365.com:587`) | `235 2.7.0` with the account's sign-in name as `user=`; `535 5.7.3` with an alias or the generated primary address, in both AUTH forms |
| Refresh after a forced expiry, and after a restart | Six refreshes in new processes from the keyring's refresh token, each rotating it; the test token deleted afterwards (`secret-tool` finds none) |
| No keyring | Memory fallback confirmed in the CI smoke run; the banner isn't built (ML-048) |

### M2 integrated: the engine service, the app and the testbed end to end — 2026-10-05

Lead, after merging the four phase-A branches; debug builds, headless on Broadway.

| Check | Result |
| --- | --- |
| Engine service offline (`cargo test -p sealwax-engine --test service`) | 2/2: a preset address is detected with no network request; an account whose server refuses connections is added, reports `SyncFailed { Unreachable }`, and is removed with its `accounts.toml` entry |
| End to end on the testbed (`tools/headless.sh 33 target/debug/sealwax --self-test tests/selftest/testbed-setup.sealwax-test`, 10:55 local; Dovecot 2.4.5, Mailpit 1.31.4 for SMTP, Stalwart 0.16.24; 127.0.0.1 only, the testbed CA, secrets in memory) | 68/68 steps. alice added through the manual settings, its first sync 30 headers in 13.6 ms (engine log); role folders with the server's counts (INBOX 10 unread, Sent 1); a wrong password refused with "Wrong username or password" before anything was saved; perf added and 10,000 headers in 2,580.5 ms (a debug build; 2,167.1 ms in the run before); All Inboxes 10,030 rows; Remove Account deleted alice (10 folders, 30 messages) and left perf's 10,000. Previews: `tools/progress/public/m2-testbed-*-2026-10-05.png` |
| Remove Account on a scripted engine (`setup-remove`, in CI) | 64/64: unavailable without a listed account; Cancel sends nothing; Remove sends the key; with two accounts the dialog asks which |
| The window's layout (`window-layout`, in CI, 2026-10-08) | 91/91: folding an account by click and keys, the folded count, `layout.toml`, real pointer drags within the ranges and the reader's room, double clicks, the rail and one pane, a new window keeping widths and folds, the reader's column at 712 and 1,200 px |
| Edit Account on a scripted engine (`setup-edit`, in CI, 2026-10-08) | 102/102: unavailable without a listed account; fields from `accounts.toml`; Cancel and an unchanged Save send nothing; Save sends key, label and colour and the heading follows; the choice starts at the shown account; the heading's right click; Enter in the name and on a colour; a refusal's toast |
| `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui` (10:56) | 664 passed, 0 failed, 18 ignored; all 17 self-test scripts pass. Smoke run: first frame 100.0 ms, the engine started 32 ms later and stopped in 0.33 ms at quit; on the wrapper's private bus no keyring answers, and the engine fell back to memory 8 ms after it started |

### M2 IMAP, login check, folders and first sync (ML-035, ML-036, ML-037) — 2026-10-05

Release builds; AMD Ryzen 9 5900X, Docker 29.7.2; the testbed's pinned images.

| Check | Result |
| --- | --- |
| Testbed suites (`cargo test -p sealwax-engine --release --test testbed -- --ignored --test-threads=1`) | 5 groups pass on both Dovecots and Stalwart over TLS and STARTTLS; an interrupted 10,000-message first sync resumes to exactly 10,000 on every server |
| 1,000 headers ≤ 5 s | **Pass**: 84.1–228.6 ms after the start of connect, 27 runs in three sessions; all 10,000 in 0.70–1.25 s |
| Log grep at trace level (`engine/tests/log_grep.rs`) | No password, AUTH line, address, username or folder name; the control without the caps catches async-imap's AUTHENTICATE line |
| Not verified | Real providers (no test accounts yet); LOGIN and XOAUTH2 against real servers (scripted only); the provider texts behind `AppPasswordRequired`, `ImapDisabled` and `OAuthRequired` |

### M2 accounts (ML-034, ML-038) — 2026-10-05

Debug builds; `tools/headless.sh 52`. `accounts_file` 14 tests, `infrastructure::accounts` 5, `store::account_tests` 3, `engine::accounts` 7, `preferences` 1 new, and the no-secret test: the dummy password is in no file (store and WAL included) and no trace-level log line, and no address is in the logs; a planted copy is found. Removal at scale (`sealwax-fixtures remove-account`, release, a 110k-message store): 100k messages in 3.8–4.0 s from a cold cache, readers' p99 under 0.61 ms cold. Not verified: the real keyring (only the in-memory store), a crash at each step (simulated through the journal).

### M2 setup assistant (ML-033) — 2026-10-05

Debug builds; `tools/headless.sh 51`; a scripted engine, no network. Self-tests setup 140/140, setup-failures 156/156, setup-detect 205/205 in CI; `preview-m2-setup` 165/165 wrote 28 previews (`tools/progress/public/m2-setup-*-2026-10-05.png`). The app's own log lines from those runs hold none of the typed passwords, addresses or names. Not verified: real key presses, the live session, screen-reader output, the bottom-sheet layout of a small window.

### M2 autodetect (ML-030, ML-031, ML-097) — 2026-10-05

Release build of `devtools/autodetect-measure`, dummy addresses only, cold DNS to 1.1.1.1.

| Check | Result |
| --- | --- |
| S4's list | 44/50 strict, 45/50 with the second login attempt (as S4b); p50 59 ms, p95 4,599 ms, max 6,508 ms |
| ML-097: a fresh held-out list, 42 domains (37 with documented truth), frozen 08:24:27 UTC (sha256 `ffa7f686…7e1e`), measured once | **20/37 strict (54 %), 21/37 with the second attempt (57 %), 22/37 with verified aliases (59 %)**; S4b's held-out list: 13/26 strict (50 %), 16/26 extended (62 %). p50 295 ms, p95 5,110 ms, max 5,318 ms |
| Privacy | 2,711 requests in five runs, 0 violations; one redirect that would have carried the address to plain HTTP on another host was refused |
| Tests | 59 domain, 27 engine (scripted network, paused time) and 11 harness tests; `cargo test` makes no network request |

### M1 live checks — 2026-10-04

Release builds on the live Hyprland session: 3440×1440 at 144 Hz, NVIDIA RTX 3070 Ti, GTK's Vulkan renderer. Scratch config, data and theme folders; no input injected. Logs are in `.local/live/` (not committed). `perf-memory` reads the app's own process; the WebKit processes aren't counted, and none runs until a message is shown.

| Check | Result | Verdict |
| --- | --- | --- |
| 100k list (`perf-list`): three runs, at 21:17 (as of `e4e7a31`) and at 21:27 and 21:32 (reader deferred, below) | Every phase in every run: frame interval p99 7.94–7.95 ms; at most 2 frames over 16.7 ms in a phase (max 25.6 ms). Jump to end p50 59.8–60.3 ms; jump to start 69.7–71.4 ms. Folder open p50 38.0–38.7 ms. Insert → row on screen p50 2.6–2.7 ms, max 9.0 ms | **Pass** (S3's frame, jump, open and insert bars) |
| UI memory: Pss of the app process at the first painted rows | 174.0 MiB as of `e4e7a31`; 153.2 and 151.7 MiB with the reader deferred (one run before, two after; the cause of the drop isn't established). End of script: 219.7, then 195.5 and 194.0 MiB | **Miss** against UI PSS ≤ 150 MB (143.1 MiB); how the driver's share counts was decided in M5 (user, 2026-10-05). *M5 gate, 2026-10-06: the driver's mappings count toward the GPU budget, so the bar judges 114.3 MiB: pass* |
| Where the 151.7 MiB goes (`perf-memory`'s Pss-by-mapping lines, 21:32) | Heap and anonymous 61.7 MiB. NVIDIA, GL and Vulkan libraries 25.8 MiB, plus 11.6 MiB of GPU device mappings (`nvidiactl`). Other libraries 21.7 MiB (GTK 8.1). WebKit libraries 14.2 MiB. Fonts 11.7 MiB (Noto Sans CJK 7.0). The binary 4.7 MiB. **Without the GPU stack: 114.3 MiB (119.9 MB)** | Gate decision: does the driver's share count toward the 150 MB, or toward the GPU budget? |
| Theme switch (`live-theme`): a scratch theme dark → light → dark with an open message | 27 of 27 steps pass; the WebView's own colours follow each switch in place; screenshots in `.local/live/` | Pass |
| Startup with a cached 10k INBOX (`tools/live/startup.py`, 21 runs, the first discarded), as of `e4e7a31` | Process start → first painted frame: p50 414.2 ms, p95 436.5 ms. `main()` → first frame: p50 346.9 ms. Rows present in every run | **Miss** (≤ 400 ms warm). These runs went beyond the M1 live-window authorization, which names only the list measurement and the theme switch; a live re-run waits for the user |
| Startup re-timed live, 2026-10-05 00:49 (build `d0fa158`, reader created after the first frame; authorized that day) | Process start → first painted frame: p50 358.7 ms, p95 379.9 ms (20 kept runs, 334.4–379.9). `main()` → first frame: p50 292.2 ms. Rows present in every run. With S6's ~12 ms from painted to on screen: about 371 ms p50, 392 ms p95 | **Pass** (≤ 400 ms warm; cold is measured in M9) |
| Reader created after the first frame: headless A/B, 21 runs each (Broadway, not the bar) | `main()` → first frame: p50 122.6 → 90.9 ms, p95 125.7 → 95.1 ms. Process start, including the headless wrapper: p50 201.0 → 168.6 ms. In the live `perf-list` runs, `main()` → first frame (100k store, self-test runner, one run each) was 362.9 ms before and 352.6 and 355.6 ms after | Live re-run: see the 2026-10-05 row |
| After the change (lead) | `tools/ci.sh --ui`: 473 passed, 0 failed, 11 ignored; all 13 self-test scripts pass, reader 83/83 | Pass |

### M1 reader (ML-027) and the integrated app — 2026-10-04

Headless on Broadway.

| Check | Result |
| --- | --- |
| Hostile suite, 47 cases, against the real reader (`app/examples/hostile.rs`) | Three runs: as shipped (DOM clicks), with real pointer clicks (8 cases), and in pooled views after Show images. Every run: 0 connections, 0 requests, 0 TLS hellos, 0 script traces, 0 GLib criticals; after Show images, TLS hellos only, in 13 image cases. No-defence control: network in 37 cases, scripts in 11 |
| Live theme switch with an open themed and paper HTML message (`app/examples/reader_theme.rs`, headless) | Passes both ways; restyled in place ~24 ms after the theme applied; the paper card keeps its design |
| Cached open, dry run, release (`app/examples/reader_perf.rs`) | show → on screen p95 13.7 ms settled, 105.9 ms back to back; show → first painted frame p95 102.1 ms settled (paper newsletters 212.7 ms), 146.9 ms back to back. Broadway, software rendering; **not judged** |
| Keys and focus (`app/examples/reader_keys.rs`, real input through Broadway) | Keys reach the Reader scope from a focused message; a text field in a message keeps its letters; the next message's body takes the focus |
| Integrated app (lead: the list's selection drives the reader through `MessageSource`) | `tools/ci.sh --ui`: 473 passed, 0 failed, 11 ignored; all 13 self-test scripts pass, reader 83/83; no Gtk-CRITICAL or Gtk-WARNING in the log |

### M1 message list (ML-026) — 2026-10-04

Debug builds for tests and self-tests, release builds for the dry runs; headless on Broadway; fictional data only.

| Part | Evidence | Verdict |
| --- | --- | --- |
| Store in the app, list, keys | 18 unit tests; self-tests list-sections 57/57, list-chips 71/71, list-search 55/55, list-select 56/56, list-insert 42/42, list-first-frame 21/21, list-churn 32/32 (change events checked against the store's own view) | Pass headless; real key presses not verified |
| 100k list (`perf-list`, headless dry run) | work per frame p99 0.23 ms at 1,500 px/s, 4.21 ms at 12,000 px/s, 4.90 ms in a 5,000 rows/s sweep; jump to end p50 89.6 ms, to start 105.2 ms; folder open p50 63.8 ms; insert → row on screen p50 8.2 ms | **Not judged**: Broadway; the live run is pending |
| Startup (headless dry run) | 10k store: first-frame p50 137.0 ms (11 runs); empty store 91.9 ms | **Not judged** |
| After the merge (lead) | `tools/ci.sh --ui`: 447 passed, 0 failed, 11 ignored; all 12 self-test scripts pass | Pass |

### M1 store, blob store and fixtures (ML-023, ML-024, ML-025) — 2026-10-04

Release builds, headless; 100,000 synthetic messages in one folder (`sealwax-fixtures import --count 100000 --seed 1`), then 50,000 more in a second account; system SQLite 3.53.4; other agents building in parallel (load 2–10).

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

Built on three branches in parallel and merged by the lead. Debug builds, headless on Broadway.

| Part | Evidence | Verdict |
| --- | --- | --- |
| Theme follower and mail roles (ML-018, ML-019) | Domain 48 unit + 16 integration tests (insta snapshots of five themes, contrast rules over all 22 stock themes: secondary text and selected chip ≥ 4.5:1, initials ≥ 4.5:1 for any accent); infrastructure 31 tests (the cascade matches `omarchy-theme-color` for all 22 themes; helper deadlines); `theme_switch` headless: dark → light → dark → built-in, every check passed, applied ~178 ms after the swap (150 ms debounce); live read-only `theme_dump`: 8.8–9.0 ms | Pass headless; the live dark↔light switch with WebViews waits for the reader |
| Actions, palette, settings (ML-022) | Domain 43, infrastructure 5, app 15 tests (keymap audit for US and Norwegian); `palette` example 36 of 36 checks, five runs, and again after the lead moved preferences onto the shared watcher | Pass; real key presses not verified (GTK can't synthesize them) |
| Shell, window, packaging (ML-020, ML-021, ML-028) | Self-tests: window 107/107, window-controls 24/24, activation 62/62 (second launch, `mailto:` from a second instance and D-Bus `Open` with `attach` dropped, background hold, D-Bus activation on a private bus), signals 17/17; `desktop-file-validate` clean; `package-source.py` deterministic | Pass; `makepkg` not run yet |
| `makepkg` build (lead, authorized, no install) | `python3 tools/package-source.py` (commit `72ab02d`), then `makepkg --cleanbuild --force --nodeps` in `.local/package/`: release build 48.6 s; `check()` ran the GTK-free crates' tests, 153 passed, 0 failed, 2 ignored; `sealwax-0.0.1-1-x86_64.pkg.tar.zst` (1.25 MB) holds the binary, desktop file, D-Bus service, the icon at six sizes, docs and the license. Without `--nodeps` makepkg stopped at `rust` and `cargo` (rustup's toolchain isn't a pacman package) | Pass (ML-028) |
| Shell visual pass (ML-021) | One `sw-` vocabulary, `style.css` free of literals; `theme.sealwax-test` 84/84 in CI (colours by role, contrast, corners measured 0.0/6.1/8.1 px for 0/6/8, fonts, tooltips with keys); compose contrast 5.57:1 (matte-black) and 5.90:1 (catppuccin-latte), was 2.63:1; eight themed previews in `tools/progress/public/` | Pass headless; tooltips, Yaru icons and Hyprland rounding not checked live |
| After merging and wiring (lead) | `tools/ci.sh --ui`: 371 tests passed, 0 failed, 10 ignored; all four self-tests pass; the app reads and applies the theme at startup (9.2 ms read, 20 ms applied), then `first-frame` | Pass |

### S6 desktop integration (ML-014) — 2026-10-04

Release builds, 16:00–16:18 CEST: 18 self-closing live runs on Hyprland under the live-window authorization (127 windows, most under 1 s; 3 fictional low-urgency notifications), headless runs on Broadway display 26, Dovecot and Stalwart from the testbed (15:57–16:21, removed afterwards). Logs in `spikes/s6-desktop/evidence/`.

| Bar | Result | Verdict |
| --- | --- | --- |
| First frame with a cached 10k INBOX ≤ 400 ms | Process start → first frame with rows on screen, fullscreen 3440×1440, Vulkan: warm p50 467 / p95 490 ms (20 runs); cold-ish p50 570 / p95 580 ms (12 runs); with the first frame limited to 40 rows, warm p50 326–330 / p95 341–361 ms (two batches of 6) | **Fail as built**; passes warm with the first-frame trim (cold with the trim not measured) |
| Second-instance `mailto:` ≤ 100 ms | p50 50.8 / p95 52.7 ms (20 runs); `attach` and `attachment` dropped in 5 of 5 runs; D-Bus `Open` 0.5 ms | Pass |
| A notification click focuses the window and opens the message | omarchy-shell accepts a notification in 0.8 ms; headless, the click path works (default action → `app.open-message` in 0.05 ms); live, `present()` refocuses an unfocused window in 2.2–3.9 ms (3 of 3) | **Pass** in the user's run (2026-10-06): message painted 16.4 ms after the action with the window open but unfocused, 194.3 ms from background mode (window active at 197.8 ms); focused both times, no server activation token |
| Background mode keeps IDLE alive | APPEND → notification sent p50 1.3 ms (Stalwart), 492 ms (Dovecot's own IDLE delay); 16 cut connections back in IDLE in ~14 ms; a 330 s hold crossed one IDLE restart without a disconnect | Pass (minutes on the testbed; hours not verified) |
| Reconnect after resume ≤ 10 s | A silently frozen connection went unnoticed for 10 s; after a resume signal, idling again in p50 14.0 ms (Stalwart) / 14.5 ms (Dovecot) on localhost | Pending the user's suspend (`bash spikes/s6-desktop/manual-resume.sh`) |
| GDK_SCALE=2 doesn't change the scale | Same scale factors, sizes and timing with it set or unset | Pass — GTK 4 on Wayland ignores it |

`cargo test -p email-spike-s6`: 14 tests pass; fmt and clippy clean (re-run by the lead).

### S1 IMAP library and sync model (ML-009) and the testbed (ML-005) — 2026-10-04

Release build, localhost testbed (Dovecot 2.4.5 full, Dovecot IMAP4rev1-only, Stalwart 0.16.24), Ryzen 9 5900X; final run 14:54–15:03 on a freshly recreated and reseeded testbed. Evidence: `spikes/s1-imap/evidence/2026-10-04.log` (produced by `evidence/run.sh`).

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

Release builds; 13 short self-closing live runs on Hyprland (3440×1440 at 144 Hz, NVIDIA; one stopped by the harness's own 55 s watchdog and not used) and headless runs on display 21. Corpus: 55 synthetic newsletters and 8 simple messages. Raw logs in `spikes/s2-webkit/results/`.

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

Release builds; Ryzen 9 5900X, RTX 3070 Ti, 3440×1440 at 144 Hz, system SQLite 3.53.4, GTK 4.22.4; 100k synthetic messages over 5 years (29.4 % unread, 21.3 % with attachments, Norwegian/English plus about 10 % Chinese and Japanese, 40,548 conversations).

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

Release builds; four headless runs (display 23) and five self-closing live runs on Hyprland (DP-2, 3440×1440 at 144 Hz, NVIDIA); no synthetic input in the live session.

| Bar | Result | Verdict |
| --- | --- | --- |
| Formatting commands | 98 checks live, 102 headless | Pass |
| Toolbar follows the selection | caret move → toolbar update, live p50 0.77–1.57 ms, p95 1.46–13.83 ms | Pass |
| Undo | every format; a typed run and a paste each undo in one step | Pass (the app must bind Ctrl+Z, Ctrl+Shift+Z/Ctrl+Y, Ctrl+U) |
| Paste cleaned | hostile HTML stripped to the allowlist, 0 canary requests | Pass |
| Typing < 16 ms | accelerated p95 16.0–24.1 ms; `hardware-acceleration-policy = NEVER` p95 9.9–11.9 ms | Pass without acceleration only |
| fcitx5 and Norwegian dead keys | `bash spikes/s7-composer/manual-check.sh`, the user, 2026-10-06 | Approved by the user: p95 10.51 ms (æ/ø/å 11.24 ms) with fcitx5; 1 of 5 lines typed exactly, € ô ü not exercised |
| Round trip through mail-parser | 12 `.eml` files | Pass |
| No Bcc in the transmitted copy; no hostname | 0 hits | Pass |
| Gmail web / Outlook web / Thunderbird | samples ready | Pending test accounts |

**Method:** an editing command sent by the app, timed to the end of the first frame that shows it; a one-child probe widget around the WebView marks frames where it drew; the frame clock's after-paint ends the frame; caret blink off; 50 ms without frames before each edit; a pixel pass checks that the first frame shows the edit (19–20 of 20 per run). Headless Broadway frame time grows with WebView area (about 18 ms at 1100×595, 3.3 ms at 535×195), so paint latency is judged live only. `cargo test -p email-spike-s7`: 10 tests pass; fmt and clippy clean (re-run by the lead).

**Harness change:** the S7 probe's first headless run wrote WebKit data to `~/.cache/email-spike-s7` and `~/.local/share/email-spike-s7`, because `tools/headless.sh` passed the user's XDG folders through. Since 2026-10-04 the wrapper gives every run scratch XDG folders under `.local/headless-<display>/`.

### S4 autodetect (ML-012) — 2026-10-04

Release builds, 12:01–12:25 UTC, residential network in Norway.

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

Release builds, 14:23–14:45 UTC, residential network in Norway, cold DNS at 1.1.1.1. Rubric frozen 14:10 UTC before any code, rules frozen 14:28:35, held-out list frozen 14:42:23 (sha256 `a8d32d12…6281c`, re-checked by the lead) before any request to its domains.

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
