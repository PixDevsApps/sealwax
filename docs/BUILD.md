# Build

Synced: 2026-10-04

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

Arch packages needed to build: `gtk4`, `libadwaita`, `webkitgtk-6.0`, `sqlite`, `pkgconf`, `rust` (or rustup). Optional at runtime: `gnome-keyring` (or another Secret Service provider), `hunspell` dictionaries for spell check.

Not installed here (no action needed for M0): mold/lld, rust-analyzer, blueprint-compiler, meson.

## Commands

```bash
cargo build                       # the app (default member)
cargo build --workspace           # every crate
cargo test --workspace
bash tools/ci.sh                  # fmt, clippy -D warnings, tests, build
SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui   # plus a headless smoke run
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
| `sealwax-fixtures` | `devtools/fixtures/` | Synthetic `.eml` corpora, import and store benchmarks (workspace glob `devtools/*`, `publish = false`) |
| `email-spike-s1` … `email-spike-s7` | `spikes/<id>-<topic>/` | M0 measurement code, one crate per spike (workspace glob `spikes/*`) |

Release profile: thin LTO, one codegen unit. The M0 spike crates keep their `email-spike-*` names (ADR-010).

## Fixture data

```bash
cargo build --release -p sealwax-fixtures
target/release/sealwax-fixtures generate --count 2000 --seed 7 --out target/m1-store/eml
target/release/sealwax-fixtures import --count 100000 --seed 1 --store target/m1-store/100k/store.sqlite
target/release/sealwax-fixtures bench --store target/m1-store/100k/store.sqlite
target/release/sealwax-fixtures check --store target/m1-store/100k/store.sqlite
```

Always give `--store` a scratch path; the app's own store is `SealwaxDirs::from_env().store_file()`. The store links the system SQLite and refuses one without FTS5 or `contentless_delete` (≥ 3.43).

## Headless GTK runs

`tools/headless.sh <display> <cmd...>` starts `gtk4-broadwayd` on `127.0.0.1:<8080 + display>`, runs the command with `GDK_BACKEND=broadway`, a private D-Bus bus from `tools/headless-dbus.conf` (no service directories, so no portal, gvfs or at-spi can start and reach the live session), `GSETTINGS_BACKEND=memory` and `ADW_DISABLE_PORTAL=1`, and cleans up the daemon and its socket. It refuses a display that is already in use. Every run gets scratch XDG folders under `.local/headless-<display>/` (config, cache, data, state), so nothing it writes — WebKit's data included — lands in the user's home. Never replace it with a plain `dbus-run-session`.

## Mail testbed

`tools/testbed/` (ML-005) is the Docker Compose project `mail-testbed`. Every port is bound to 127.0.0.1 and all data is synthetic ([S1 report](spikes/S1-imap.md#testbed-ml-005)). Docker use is authorized for the testbed only (PLAN.md authorization log).

| Service | Image | Ports |
| --- | --- | --- |
| Dovecot, full extensions | `dovecot/dovecot:2.4.5` | 14993 IMAPS, 14143 IMAP+STARTTLS |
| Dovecot, IMAP4rev1 only | `dovecot/dovecot:2.4.5` | 15993, 15143 |
| Stalwart | `stalwartlabs/stalwart:v0.16.24` | 16993, 16143, SMTP 16025, submission 16587, submissions 16465, HTTP/JMAP 16080 |
| Mailpit | `axllent/mailpit:v1.31.4` | SMTP 17025 (STARTTLS), API 17080 |

```bash
tools/testbed/up.sh                 # start (generates the testbed CA on first use)
tools/testbed/seed.sh               # users, folders and 10k messages; about 3 min the first time
python3 tools/testbed/check.py      # health check
tools/testbed/down.sh               # stop and remove; --keep-volumes keeps the mail
```

The users are alice, bob, perf, rand, fault, idle, spike, ops, opsfault and triage (the last three added for M4), password `testbed-password`. Clients trust the generated CA `tools/testbed/certs/ca.pem` explicitly (the S1 harness takes an override path from `MAIL_TESTBED_CA`), never through the system store or a disabled verifier. The scripts need Python 3 (standard library only), OpenSSL and Docker Compose.

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

**Sign in with Microsoft** needs the client ID in `config.toml` or at build time ([OAUTH-SETUP.md](OAUTH-SETUP.md#where-the-id-goes)). Two runners check it against a real account, which needs the user's authorization each time (AGENTS.md): `spikes/s5-oauth` (S5) and `cargo run -p sealwax-engine --example oauth_live -- <address> [--resign-in]` (ML-032), which uses scratch folders and a test app-id in the keyring and removes everything at the end. The user signs in in the browser, so tell them before starting a run.

## Packaging

`packaging/` contains:
- `PKGBUILD.in`;
- the desktop file `io.github.pixdevsapps.Sealwax.desktop` (`DBusActivatable=true`, `MimeType=x-scheme-handler/mailto;`, `Exec=sealwax %u`);
- the D-Bus service file (`sealwax --gapplication-service`);
- the brand icon under `packaging/icons/hicolor/`. `python3 tools/make-icons.py packaging/icons` regenerates it from the user's brand canvas geometry: the detailed seal for the scalable icon, the simplified seal at 64 px and below.

`python3 tools/package-source.py` writes a deterministic tarball of HEAD and a filled-in PKGBUILD to `.local/package/` (version, SHA-256, `SOURCE_DATE_EPOCH` from the commit). Building it (`cd .local/package && makepkg --cleanbuild --force`) is authorized for M1 as a build without install; installing needs its own authorization. Where Rust comes from rustup (as on the development machine, in `~/.cargo`), pacman can't see `rust` and `cargo`, so add `--nodeps`, as Stet does; pacman checks the runtime dependencies again at install time.

A command line with no Sealwax running asks the session bus to start the installed service and hands its `mailto:` links to it. Development builds (`.Devel`) have no service file and start directly; run `target/debug/sealwax` for logs.
