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
```

## Workspace

| Crate | Path | Role |
| --- | --- | --- |
| `sealwax-domain` | `domain/` | Pure logic |
| `sealwax-infrastructure` | `infrastructure/` | Local std I/O, logging |
| `sealwax-engine` | `engine/` | tokio: network, sync, op queue, secrets |
| `sealwax` | `app/` | GTK app, binary `sealwax` |
| `email-spike-s1` … `email-spike-s7` | `spikes/<id>-<topic>/` | M0 measurement code, one crate per spike (workspace glob `spikes/*`) |

Release profile: thin LTO, one codegen unit. The M0 spike crates keep their `email-spike-*` names (ADR-010).

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

The users are alice, bob, perf, rand, fault, idle and spike, password `testbed-password`. Clients trust the generated CA `tools/testbed/certs/ca.pem` explicitly (the S1 harness takes an override path from `MAIL_TESTBED_CA`), never through the system store or a disabled verifier. The scripts need Python 3 (standard library only), OpenSSL and Docker Compose.

## Packaging

M1 adds `packaging/PKGBUILD.in`, the desktop file (`DBusActivatable=true`, `MimeType=x-scheme-handler/mailto;`), the D-Bus service file and `tools/package-source.py`, following Stet. `makepkg` and installs need authorization.
