# Build

Synced: 2026-10-09

## Toolchain and system libraries

| Item | Version on the development machine (2026-10-04) |
| --- | --- |
| Rust (rustup, stable) | 1.98.1; MSRV 1.92 |
| GTK 4 | 4.22.4 |
| libadwaita | 1.9.3 |
| WebKitGTK 6.0 | 2.52.6 |
| SQLite | 3.53.4 |
| GLib / GIO | 2.88.3 |
| libsecret | 0.21.7 (gnome-keyring 50 provides the Secret Service) |

Arch packages needed to build: `gtk4`, `libadwaita`, `webkitgtk-6.0`, `sqlite`, `pkgconf`, `rust` (or rustup). Optional at runtime: `gnome-keyring` (or another Secret Service provider), `hunspell` with a hunspell dictionary (such as `hunspell-en_us`) for spell check: a dictionary alone isn't enough.

Not installed here (no action needed for M0): mold/lld, rust-analyzer, blueprint-compiler, meson.

## Commands

```bash
cargo build                       # the app (default member)
cargo build --workspace           # every crate
cargo test --workspace
bash tools/ci.sh                  # fmt, clippy -D warnings, tests, build
SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui   # plus a headless smoke run, the self-test scripts and the hostile suite
bash tools/ci.sh --testbed        # plus every testbed suite (see Mail testbed)
tools/headless.sh 19 target/debug/sealwax --self-test tests/selftest/window.sealwax-test
cargo run -p sealwax -- --version
```

## Workspace

| Crate | Path | Role |
| --- | --- | --- |
| `sealwax-domain` | `domain/` | Pure logic |
| `sealwax-infrastructure` | `infrastructure/` | Local std I/O, logging |
| `sealwax-engine` | `engine/` | tokio: network, sync, op queue, secrets |
| `sealwax` | `app/` | GTK app, binary `sealwax` |
| `sealwax-fixtures` | `devtools/fixtures/` | Synthetic `.eml` corpora, import and store benchmarks, `bench-conversation` (workspace glob `devtools/*`, `publish = false`) |
| `sealwax-dns-canary` | `devtools/dns-canary/` | A preloadable `getaddrinfo` that records and refuses every name looked up, for the hostile suite (M5) |
| `email-spike-s1` … `email-spike-s7` | `spikes/<id>-<topic>/` | M0 measurement code, one crate per spike (workspace glob `spikes/*`) |
| `sealwax-fuzz` | `fuzz/` | libFuzzer targets (M9, ML-093); a workspace of its own, excluded from this one, built by `tools/fuzz.sh` |
| `lettre` (patched) | `vendor/lettre/` | lettre 0.11.23 with a 16 KiB reply-line limit, used through `[patch.crates-io]` here and in `fuzz/`; excluded from the workspace ([vendor/README.md](../vendor/README.md), ADR-015 amendment of 2026-10-13) |

Release profile: thin LTO, one codegen unit. The M0 spike crates keep their `email-spike-*` names (ADR-010).

## Fixture data

```bash
cargo build --release -p sealwax-fixtures
target/release/sealwax-fixtures generate --count 2000 --seed 7 --out target/m1-store/eml
target/release/sealwax-fixtures import --count 100000 --seed 1 --store target/m1-store/100k/store.sqlite
target/release/sealwax-fixtures bench --store target/m1-store/100k/store.sqlite
target/release/sealwax-fixtures check --store target/m1-store/100k/store.sqlite
target/release/sealwax-fixtures bench-search --store COPY/store.sqlite --blobs COPY/blobs --cold 10 --warm 30 --rebuild yes
```

`bench-search` (M7) times the first 50 results of 38 query shapes cold and warm, a word typed a key at a time, the index's size, schema v10's pass when the store comes from an earlier build, Rebuild Search Index with flag changes and searches beside it, and optionally (`--date-keyed yes`) S3's date-keyed table to compare; run it on a copy, since it migrates and rebuilds.

Always give `--store` a scratch path; the app's own store is `SealwaxDirs::from_env().store_file()`. The store links the system SQLite and refuses one without FTS5 or `contentless_delete` (≥ 3.43).

## Reader measurements (M5)

```bash
python3 tools/corpus/fetch.py                  # the real-world corpus into .local/corpus/real/ (open-licensed templates, checked by SHA-256)
cargo build --release -p sealwax --example reader_corpus --example reader_memory
SEALWAX_OMARCHY_STATE=$PWD/.local/reader-examples/corpus-theme SEALWAX_HEADLESS_TIMEOUT=900 \
    tools/headless.sh 33 target/release/examples/reader_corpus [--previews]
SEALWAX_HEADLESS_TIMEOUT=600 tools/headless.sh 31 target/release/examples/reader_memory
```

The fetch makes anonymous HTTPS GETs of public files with a generic User-Agent; the templates stay out of the repository (`tests/corpus/real/SOURCES.toml` lists them with their licences). Both harnesses write only under `.local/`. On the live session they run only under a live-window authorization (`reader_memory --live`), and measure at the window size the compositor gives.

## Composer measurements (M6)

```sh
tools/kill9-compose.sh 46 10        # kill -9 while composing: seconds of typing lost per run (bar 5 s)
cargo build --release -p sealwax --example composer_live
tools/headless.sh 45 target/release/examples/composer_live          # dry run (Broadway)
target/release/examples/composer_live --live                        # authorized live runs only
```

`kill9-compose.sh` (debug build first: `cargo build`) runs Sealwax headless on a scratch store under `.local/kill9/`, types into a composer every 200 ms, kills it with SIGKILL at a random moment, starts it again on the same folders and reads the reopened draft; about 12 s a run, so it isn't in `tools/ci.sh`. `composer_live` times an edit to the painted frame in the real composer's editor (S7's method: WebKit's `InsertText`, a paint probe, no key events) in a small document and above a 199 KB quoted thread, then scrolls the quote; it makes no input, uses scratch XDG folders, writes its report under `.local/reader/`, and ends by itself. The testbed's sending suites (`send_testbed`, `append_testbed`, `outbox_testbed`) run under `tools/ci.sh --testbed`; `SEALWAX_OUTBOX_SEEDS` sets the outbox suite's seeds (2 by default).

## Search, notification and background measurements (M7)

```sh
cargo build --release -p sealwax
SEALWAX_HEADLESS_TIMEOUT=900 tools/headless.sh 53 target/release/sealwax --self-test tests/selftest/perf-search.sealwax-test   # key → results at 100,000
python3 tools/testbed/notify_e2e.py 21          # new mail in background mode, end to end on the testbed (debug build first)
cargo build -p sealwax --example notify_live
tools/headless.sh 56 target/debug/examples/notify_live            # dry run (the stand-in clicks)
target/debug/examples/notify_live --live                          # authorized live runs only: the user clicks
```

`perf-search` uses perf-list's 100,000-message fixture store under `target/perf-list/`. `notify_e2e.py` runs `tests/selftest/testbed-notify.sealwax-test` (the real engine on Stalwart's `newmail` user, the window closed into background mode) and APPENDs one fictional message as another IMAP client once the window has closed, timing its OK to the notification at the stand-in server. `notify_live` sends two notifications over a scratch fictional mailbox (app-id `io.github.pixdevsapps.Sealwax.NotifyLive`) and records which message each click opened; it injects no input. `SEALWAX_CLOSE_WINDOW_AFTER_MS` (development builds only) closes the first main window that long after its first frame, a test hook like `SEALWAX_EXIT_AFTER_MS` (`background-*` self-tests).

## POP3 suites (M8)

```sh
SEALWAX_TESTBED_CA=$PWD/tools/testbed/certs/ca.pem cargo test -p sealwax-engine --release --test pop3_testbed -- --ignored --test-threads=1
SEALWAX_TESTBED_CA=$PWD/tools/testbed/certs/ca.pem cargo test -p sealwax-engine --release --test pop3_accounts_testbed -- --ignored --test-threads=1
tools/headless.sh 21 target/debug/sealwax --self-test tests/selftest/testbed-pop3-setup.sealwax-test
```

`pop3_testbed` (the client: Dovecot's and Stalwart's POP3 over both securities, alice read-only, `popclient`'s own INBOX for deletions, cuts through the fault proxy; `SEALWAX_POP3_SEEDS` sets its fault runs, 5 by default) and `pop3_accounts_testbed` (an account's actor end to end on `popclient`) also run under `tools/ci.sh --testbed`. `testbed-pop3-setup` adds a POP3 account through the assistant on Stalwart's POP3 (port 16995): never point it at Dovecot's alice, since Dovecot marks downloaded mail read when the POP3 session ends. `engine/tests/pop3.rs` (a scripted server), `pop3_log.rs` and `pop3_accounts.rs` (a fake maildrop) run in CI.

## Fuzzing and the advisory check (M9)

```sh
bash tools/fuzz.sh list                    # the 13 targets
bash tools/fuzz.sh message 900             # build, then fuzz one target for 900 s
bash tools/fuzz.sh all 600 4               # every target, 600 s each, 4 at a time
bash tools/fuzz.sh repro html target/fuzz-artifacts/html/crash-…   # one input again
SEALWAX_CORPUS_REAL=/path/to/.local/corpus/real bash tools/fuzz.sh html 900   # from a worktree
git clone --depth 1 https://github.com/rustsec/advisory-db target/advisory-db
python3 tools/advisories.py -v target/advisory-db Cargo.lock
```

The fuzz targets live in `fuzz/`, a workspace of its own (excluded from the main one), so `cargo build` and CI never compile them. They link libFuzzer through the `libfuzzer-sys` crate on the stable toolchain, instrumented with SanitizerCoverage flags (`-C passes=sancov-module`); no nightly, no `cargo-fuzz` and no AddressSanitizer. Debug assertions and overflow checks are on (`fuzz/Cargo.toml`). Each target calls an entry point in the crates (`sealwax_domain::fuzz`, `sealwax_engine::fuzz`, built for tests and with the `fuzz` feature) that also checks what the parser promises, so a property that fails is a crash. Working corpora, crashes and logs go to `target/fuzz-corpus/`, `target/fuzz-artifacts/` and `target/fuzz-logs/`; the checked-in seeds and every crash found are in `fuzz/corpus/<target>/`, which `cargo test -p sealwax-domain -p sealwax-engine` runs through the same entry points. The advisory check reads the public RustSec database (cloned into `target/`, never committed) instead of installing `cargo audit`.

## Headless GTK runs

`tools/headless.sh <display> <cmd...>` starts `gtk4-broadwayd` (from GTK 4.24, GTK's daemon patched by `tools/broadwayd/build.sh`, which needs a C compiler, `pkg-config`, `patch`, `curl` and Python, and downloads GTK's release tarball once; see TESTING.md) on `127.0.0.1:<8080 + display>`, runs the command with `GDK_BACKEND=broadway`, a private D-Bus bus from `tools/headless-dbus.conf` (no service directories, so no portal, gvfs or at-spi can start and reach the live session), `GSETTINGS_BACKEND=memory` and `ADW_DISABLE_PORTAL=1`, and cleans up the daemon and its socket. It refuses a display that is already in use. Every run gets scratch XDG folders under `.local/headless-<display>/` (config, cache, data, state), so nothing it writes — WebKit's data included — lands in the user's home. Never replace it with a plain `dbus-run-session`.

## Mail testbed

`tools/testbed/` (ML-005) is the Docker Compose project `mail-testbed`. Every port is bound to 127.0.0.1 and all data is synthetic. Docker use is authorized for the testbed only.

| Service | Image | Ports |
| --- | --- | --- |
| Dovecot, full extensions | `dovecot/dovecot:2.4.5` | 14993 IMAPS, 14143 IMAP+STARTTLS, 14995 POP3S, 14110 POP3+STLS (M8) |
| Dovecot, IMAP4rev1 only | `dovecot/dovecot:2.4.5` | 15993, 15143 |
| Stalwart | `stalwartlabs/stalwart:v0.16.24` | 16993, 16143, POP3S 16995, POP3+STLS 16110 (M8), SMTP 16025, submission 16587, submissions 16465, HTTP/JMAP 16080 |
| Mailpit | `axllent/mailpit:v1.31.4` | SMTP 17025 (STARTTLS), API 17080 |

```bash
tools/testbed/up.sh                 # start (generates the testbed CA on first use)
tools/testbed/seed.sh               # users, folders and 10k messages; about 3 min the first time
python3 tools/testbed/check.py      # health check
tools/testbed/down.sh               # stop and remove; --keep-volumes keeps the mail
```

The users are alice, bob, perf, rand, fault, idle, spike, ops, opsfault and triage (the last three added for M4), carol, dave, outbox and outboxfault (added for M6's sending suites) newmail (M7's new-mail suite and end-to-end check) and popclient (M8's POP3 client and accounts suites; on a running testbed, `python3 tools/testbed/stalwart_setup.py` and `tools/testbed/seed.sh` add them), password `testbed-password`. Clients trust the generated CA `tools/testbed/certs/ca.pem` explicitly (the S1 harness takes an override path from `MAIL_TESTBED_CA`), never through the system store or a disabled verifier. The scripts need Python 3 (standard library only), OpenSSL and Docker Compose.

`bash tools/ci.sh --testbed` (M3) runs, after the usual checks, every test file with tests ignored as "needs the testbed", in release and one test at a time, since the suites share test users; each file's output goes to `.local/testbed-<package>-<file>.log`. With no testbed container running it starts and seeds the testbed first and stops it at the end with `down.sh --keep-volumes`; a running testbed is used and left running, and a partly running one is refused. The suites need `tools/testbed/certs/` (the CA, and the server certificate for their proxies); a worktree has none, so link it there from the checkout that started the testbed. The sync soak (ML-049) is ignored for another reason and stays out; it runs by hand for as long as `SEALWAX_SOAK_MINUTES` says (30 unless set):

```bash
SEALWAX_TESTBED_CA=$PWD/tools/testbed/certs/ca.pem SEALWAX_SOAK_MINUTES=1440 SEALWAX_SOAK_CHECK_MINUTES=60 \
  cargo test -p sealwax-engine --release --test soak -- --ignored --nocapture
```

## Accounts in development builds

Development builds (`cfg!(debug_assertions)`) read two overrides that release builds ignore ([SECURITY.md](SECURITY.md#credentials)):

- `SEALWAX_TEST_CA=<file>` trusts exactly that CA instead of the system's (the testbed's `tools/testbed/certs/ca.pem`);
- `SEALWAX_SECRETS=memory` keeps passwords and tokens out of the keyring.

The testbed end to end through the real engine and the setup assistant (not in CI; start and seed the testbed first, and stop it after):

```bash
tools/headless.sh 33 target/debug/sealwax --self-test tests/selftest/testbed-setup.sealwax-test
bash tools/ci.sh --testbed
```

Triage end to end (M4): prepare the user `triage` (every folder emptied, eight synthetic messages in its INBOX), run the script, then print what the servers hold:

```bash
SEALWAX_TESTBED_CA=$PWD/tools/testbed/certs/ca.pem python3 tools/testbed/triage_mailbox.py fill dovecot stalwart
tools/headless.sh 25 target/debug/sealwax --self-test tests/selftest/testbed-triage.sealwax-test
SEALWAX_TESTBED_CA=$PWD/tools/testbed/certs/ca.pem python3 tools/testbed/triage_mailbox.py show dovecot stalwart
```

`tools/ci.sh --testbed` runs every engine testbed suite, M4's `write_testbed` (the IMAP writes and S1's lost replies) and `ops_testbed` (the op runner: crash replay, the undo window, the randomized triage suite) included.

**Sign in with Microsoft** needs the client ID in `config.toml` or at build time ([OAUTH-SETUP.md](OAUTH-SETUP.md#where-the-id-goes)); packages made with `tools/package-source.py` build it in (2026-10-09). Two runners check it against a real account, which needs the owner's authorization each time: `spikes/s5-oauth` (S5) and `cargo run -p sealwax-engine --example oauth_live -- <address> [--resign-in]` (ML-032), which uses scratch folders and a test app-id in the keyring and removes everything at the end. `RUST_LOG=info,sealwax_engine::smtp=debug cargo run -p sealwax-engine --example send_live -- <address>` (2026-10-13) sends through a Microsoft 365 account with the patched lettre: one message from the account to itself, and one whose From the mailbox doesn't own, which Microsoft refuses with a long SendAsDenied reply (issue #2); it keeps the tokens in memory only, and each run needs the owner's authorization. The user signs in in the browser, so tell them before starting a run.

## Packaging

`packaging/` contains:
- `PKGBUILD.in`, the package built from source (`sealwax`);
- `aur/sealwax-bin/PKGBUILD.in`, an AUR package that installs a release's prebuilt files (ADR-021; never published, and not planned since 2026-10-18);
- the desktop file `io.github.pixdevsapps.Sealwax.desktop` (`DBusActivatable=true`, `MimeType=x-scheme-handler/mailto;`, `Exec=sealwax %u`);
- the D-Bus service file (`sealwax --gapplication-service`);
- the icon under `packaging/icons/hicolor/`: PNGs from 16 to 512 px of the wax seal in `packaging/icons/source/sealwax-seal.png` (2026-10-08; the envelope tile's SVGs before). `python3 tools/make-icons.py packaging/icons` regenerates them with ImageMagick (`magick`), sharpening the sizes up to 64 px, and the scalable SVG that carries the 512 px PNG.

`python3 tools/package-source.py` writes a deterministic tarball of HEAD and a filled-in PKGBUILD to `.local/package/` (version, SHA-256, `SOURCE_DATE_EPOCH` from the commit). Building it (`cd .local/package && makepkg --cleanbuild --force`) is authorized for M1 as a build without install; installing needs its own authorization. Where Rust comes from rustup (as on the development machine, in `~/.cargo`), pacman can't see `rust` and `cargo`, so add `--nodeps`, as Stet does; pacman checks the runtime dependencies again at install time. The PKGBUILD turns off makepkg's `lto` option: with Arch's default `OPTIONS=(… lto)` makepkg compiles ring's C code to GCC LTO bytecode, which Rust's default linker (rust-lld) can't read, and the link fails with ring's `ring_core_*` symbols undefined (found by the user's soak build, 2026-10-07). Its `build()` and `check()` add `--remap-path-prefix` to `RUSTFLAGS`, mapping the build folder (`$srcdir`) to `/usr/src/sealwax` and Cargo's home (`$CARGO_HOME`, else `~/.cargo`) to `/usr/src/cargo`, so the panic locations in the binary name no path of the machine that built it (2026-10-09; makepkg had warned "Package contains reference to $srcdir"). The app keeps no `CARGO_MANIFEST_DIR` path either: the self-tests find `tests/theme/` beside the script they run. The user's install: `cd .local/package && makepkg --cleanbuild --force --nodeps -si`.

A command line with no Sealwax running asks the session bus to start the installed service and hands its `mailto:` links to it. Development builds (`.Devel`) have no service file and start directly; run `target/debug/sealwax` for logs.

**Packaging Sealwax elsewhere** (Nix, other distributions; 2026-10-19, [ADR-008 amendment](DECISIONS.md#adr-008-amendment--packagers-may-build-in-the-projects-client-id-2026-10-19)): set `SEALWAX_MICROSOFT_CLIENT_ID` when building, and Sign in with Microsoft works in your package as in the release packages. You may build in the project's own client ID: ask for it in an issue. It is a public client's ID, not a secret, but sign-ins through it count as the project's app, so say where the package is published. Without one, your users put their own registration's ID in `config.toml` ([OAUTH-SETUP.md](OAUTH-SETUP.md)).

## Making a release

Releases follow [ADR-021](DECISIONS.md#adr-021--a-public-repository-releases-on-github-and-the-aur-2026-10-09): a tag `vX.Y.Z` and a GitHub release with the package, which people install and update with INSTALL.md's command. There is no AUR package ([ADR-021 amendment, 2026-10-18](DECISIONS.md#adr-021-amendment--no-aur-package-2026-10-18)). A plain push reaches no one's computer.

1. **Version and notes:** set `version` in `Cargo.toml`'s `[workspace.package]` (then `cargo metadata --format-version 1 >/dev/null` and the same with `--manifest-path fuzz/Cargo.toml` update both lock files), add a `## X.Y.Z — date` section to `CHANGELOG.md` in plain words, and update the version in README, INSTALL and the user guide where they name it. Commit, run `bash tools/ci.sh --ui`, then tag: `git tag -a vX.Y.Z -m "Sealwax X.Y.Z"` and `git push origin main vX.Y.Z`.
2. **Build:** `python3 tools/release.py build` runs `tools/package-source.py` and makepkg on the tagged commit (adding `--nodeps` where Rust isn't a pacman package), then writes to `.local/release/X.Y.Z/`: the package `sealwax-X.Y.Z-1-x86_64.pkg.tar.zst`, the same file as `sealwax-x86_64.pkg.tar.zst` (the name INSTALL.md's download command asks for through `releases/latest`), `sealwax-X.Y.Z-x86_64.tar.gz` (the package's `usr/` files, owned by root, in name order, for `sealwax-bin`) and `SHA256SUMS`.
3. **GitHub:** `python3 tools/release.py github` creates the release `vX.Y.Z` with those files and `CHANGELOG.md`'s section as its notes (`gh`, the tag already on GitHub).
4. **AUR (not used since 2026-10-18):** `python3 tools/release.py aur` clones or pulls `ssh://aur@aur.archlinux.org/sealwax-bin.git` into `.local/aur/sealwax-bin/`, writes its PKGBUILD from the template with the version and the tarball's SHA-256 (checking that its `depends` and `optdepends` match `packaging/PKGBUILD.in`), downloads the release's tarball to verify the sum (`makepkg --verifysource`), writes `.SRCINFO` and pushes. It needs the owner's AUR account with their SSH key; `SEALWAX_AUR_SSH_KEY=<key file>` names the key when ssh doesn't find it. (Never run: the AUR closed new accounts on 2026-10-09, and the owner dropped the AUR package on 2026-10-18. The step and `packaging/aur/` are kept, unused.)

Step 3 publishes (and step 4 would): an agent runs them only on the owner's word.

