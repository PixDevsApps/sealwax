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
MAIL_CI_DISPLAY=19 bash tools/ci.sh --ui   # plus a headless smoke run
```

## Workspace

| Crate | Path | Role |
| --- | --- | --- |
| `email-domain` | `domain/` | Pure logic |
| `email-infrastructure` | `infrastructure/` | Local std I/O, logging |
| `email-engine` | `engine/` | tokio: network, sync, op queue, secrets |
| `email` | `app/` | GTK app, binary `email` |
| `email-spike-s1` … `email-spike-s7` | `spikes/<id>-<topic>/` | M0 measurement code, one crate per spike (workspace glob `spikes/*`) |

Release profile: thin LTO, one codegen unit. Names are working names until ADR-010 is accepted.

## Headless GTK runs

`tools/headless.sh <display> <cmd...>` starts `gtk4-broadwayd` on `127.0.0.1:<8080 + display>`, runs the command with `GDK_BACKEND=broadway`, a private D-Bus bus from `tools/headless-dbus.conf` (no service directories, so no portal, gvfs or at-spi can start and reach the live session), `GSETTINGS_BACKEND=memory` and `ADW_DISABLE_PORTAL=1`, and cleans up the daemon and its socket. It refuses a display that is already in use. Never replace it with a plain `dbus-run-session`.

## Mail testbed

`tools/testbed/` (ML-005, after Docker authorization) will hold a compose file for Dovecot 2.4, Stalwart, an IMAP4rev1-only server and a mail catcher, bound to localhost only, with seeded test mailboxes and scripts to start, seed and stop them.

## Packaging

M1 adds `packaging/PKGBUILD.in`, the desktop file (`DBusActivatable=true`, `MimeType=x-scheme-handler/mailto;`), the D-Bus service file and `tools/package-source.py`, following Stet. `makepkg` and installs need authorization.
