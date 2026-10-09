# Installing Sealwax

Synced: 2026-10-07

Sealwax is pre-1.0 (version 0.3.0). There is no prebuilt package and no AUR package yet: you build an Arch package from the source with the repository's PKGBUILD, and pacman installs it. [Verified so far](#what-has-been-verified) says which of these steps have been run, and on what.

- [What you need](#what-you-need)
- [Install the Arch package](#install-the-arch-package)
- [Update](#update)
- [From source, for development](#from-source-for-development)
- [Uninstalling](#uninstalling)
- [What has been verified](#what-has-been-verified)

After installing, the [user guide](docs/USER_GUIDE.md) takes over: first start, adding accounts, and everything else.

## What you need

**A system:** Arch Linux on x86_64. Sealwax is made for Omarchy 4; on other desktops it uses its own light and dark palettes (that is tested headless only). The PKGBUILD builds for x86_64 only.

**At runtime** (pacman installs these with the package):

| Package | Why | Version the build needs |
| --- | --- | --- |
| `gtk4` | The interface | 4.18 or later |
| `libadwaita` | The interface | 1.9 or later |
| `webkitgtk-6.0` | Showing HTML mail and the composer | 2.52 or later |
| `sqlite` | The mail store (with FTS5 for search) | 3.43 or later |
| `glib2`, `gcc-libs`, `glibc` | Base libraries | |

The versions are the build's minimums (the GTK, libadwaita and WebKitGTK bindings' feature levels in `Cargo.toml`, and the store's check for SQLite).

**Optional:**

- **a Secret Service** that keeps passwords and sign-in tokens: `gnome-keyring` (Omarchy installs it). Without one, Sealwax keeps them in memory until it quits and says so in a banner;
- **hunspell dictionaries** for spell checking in the composer, such as `hunspell-en_us`.

**To build:** `git`, Python 3.11 or later (for `tools/package-source.py`), `base-devel` (makepkg), `pkgconf`, and Rust 1.92 or later with Cargo, from Arch's `rust` package or from rustup. The build downloads Sealwax's Rust dependencies (crates) from crates.io.

**For Sign in with Microsoft** (Outlook.com, Microsoft 365): a Microsoft client ID. `tools/package-source.py` builds one into the package: `SEALWAX_MICROSOFT_CLIENT_ID` if you export it, else your `~/.config/sealwax/config.toml`'s `client_id`; it says which, and `--no-client-id` builds none. Without one you can add it after installing ([user guide](docs/USER_GUIDE.md#outlookcom-hotmail-and-microsoft-365-sign-in-with-microsoft), [OAUTH-SETUP.md](docs/OAUTH-SETUP.md)).

## Install the Arch package

1. **Get the source** (the repository is private for now):
   ```sh
   git clone https://github.com/PixDevsApps/sealwax.git
   cd sealwax
   ```
2. **Make the package's source and PKGBUILD:**
   ```sh
   python3 tools/package-source.py
   ```
   It writes `.local/package/sealwax-<version>.tar.gz`, a tarball of the committed tree (HEAD), and `.local/package/PKGBUILD` from `packaging/PKGBUILD.in`, with the version, the tarball's SHA-256 and `SOURCE_DATE_EPOCH` (the commit's time) filled in. Uncommitted changes aren't packaged; the script warns about them.
3. **Build and install:**
   ```sh
   cd .local/package
   makepkg --cleanbuild --force --nodeps -si     # Rust from rustup's own installer, in ~/.cargo
   makepkg --cleanbuild --force -si              # Rust from Arch's rust or rustup package
   ```
   makepkg fetches the crates (`cargo fetch --locked`), builds Sealwax (`cargo build --release --locked --offline -p sealwax`), runs the tests of the crates without a user interface (`sealwax-domain`, `sealwax-infrastructure`, `sealwax-engine`; the app's own self-tests need a display, which a package build doesn't have) and makes the package. `-i` then installs it with pacman, which asks for your password through `sudo`; `-s` installs missing dependencies first (with `--nodeps` it has nothing to do).
4. **Start Sealwax** from the app launcher, or with `sealwax` in a terminal, and add an account ([user guide](docs/USER_GUIDE.md#first-start-and-adding-an-account)).

**Why `--nodeps` with rustup's installer.** The PKGBUILD lists `rust` and `cargo` as build dependencies. Rust installed by rustup's own script (in `~/.cargo`, as on the development machine) isn't a pacman package, so makepkg's check of the build dependencies fails, or `-s` would install Arch's `rust` beside it. Arch's `rust` package, and its `rustup` package (which provides `rust` and `cargo`, per `pacman -Si rustup`), need no `--nodeps`. `--nodeps` skips makepkg's dependency checks only: when `-i` installs the package, pacman still checks the runtime dependencies (makepkg doesn't pass `--nodeps` on to `pacman -U`; read in `/usr/bin/makepkg` on the development machine, 2026-10-07).

**Why the PKGBUILD turns off LTO** (`options=('!debug' '!lto')`). Arch's `/etc/makepkg.conf` turns on link-time optimisation for every package (`lto` in `OPTIONS`, `LTOFLAGS="-flto=auto"`). With it, makepkg compiled the C code inside the `ring` crate (TLS cryptography) to GCC's LTO bytecode, which Rust's linker on x86_64 Linux (rust-lld) can't read, and the build failed at the final link with `undefined symbol: ring_core_0_17_14__OPENSSL_cpuid_setup` and every other function of ring's C code. Sealwax's own release build still uses Rust's thin LTO (`[profile.release]` in `Cargo.toml`). `!debug` because the release build carries no debug information, so a `-debug` package would be empty.

**If the install step was missed** (makepkg built the package but `sudo` timed out, or you left out `-i`), install the package makepkg made:

```sh
cd .local/package
sudo pacman -U sealwax-0.3.0-1-x86_64.pkg.tar.zst    # the version in the name follows Cargo.toml
```

**What the package installs:**

| File | What |
| --- | --- |
| `/usr/bin/sealwax` | The program |
| `/usr/share/applications/io.github.pixdevsapps.Sealwax.desktop` | The launcher entry; it also declares Sealwax a `mailto:` handler, which becomes the default only when you choose it in Sealwax |
| `/usr/share/dbus-1/services/io.github.pixdevsapps.Sealwax.service` | D-Bus activation: launchers start Sealwax through the session bus, and so does `sealwax` typed with no Sealwax running |
| `/usr/share/icons/hicolor/*/apps/io.github.pixdevsapps.Sealwax.png`, `…/scalable/apps/io.github.pixdevsapps.Sealwax.svg` | The icon (16 to 512 px, and scalable) |
| `/usr/share/licenses/sealwax/LICENSE` | The MIT license |
| `/usr/share/doc/sealwax/README.md`, `INTEGRATIONS.md` | Docs |

The package adds only these files, and nothing in your home folder changes until Sealwax runs. Its key binding and Omarchy menu entries are yours to add ([INTEGRATIONS.md](docs/INTEGRATIONS.md#hyprland)).

**No paths of your machine:** the PKGBUILD maps the folder you build in and Cargo's folder in your home to fixed names (`/usr/src/sealwax`, `/usr/src/cargo`), so the binary's panic messages carry no path of yours. Packages built before 2026-10-09 made makepkg warn "Package contains reference to $srcdir" for this.

## Update

```sh
cd sealwax
git pull
python3 tools/package-source.py
cd .local/package && makepkg --cleanbuild --force --nodeps -si    # without --nodeps for Arch's rust or rustup
```

Then **quit the running Sealwax** with Ctrl+Q in its window (or `pkill -TERM -x sealwax`) and start it again. Closing the window isn't enough: Sealwax keeps running in the background, still the old version, until it quits.

Sealwax updates its store's format by itself when a new version opens it, so there is nothing to migrate by hand. An older version refuses a store that a newer one has updated, and leaves it untouched.

## From source, for development

```sh
cargo build                 # a debug build: target/debug/sealwax
cargo build --release       # a release build: target/release/sealwax
bash tools/ci.sh            # formatting, Clippy, the tests and the build
```

[docs/BUILD.md](docs/BUILD.md) has the toolchain, every command, the test suites and the mail testbed; [docs/TESTING.md](docs/TESTING.md) the tests.

What to know before you run a build from the source tree:

- **A debug build** uses the app-id `io.github.pixdevsapps.Sealwax.Devel`: it runs beside an installed Sealwax, has its own keyring items (so it has none of the installed one's passwords) and can't be the default mail app or use D-Bus activation. In debug builds only, `SEALWAX_TEST_CA` and `SEALWAX_SECRETS=memory` change trust and the keyring for testing ([BUILD.md](docs/BUILD.md#accounts-in-development-builds)).
- **Every build uses the same folders** as the installed Sealwax: `~/.config/sealwax/` with your `accounts.toml`, and your store under `~/.local/share/sealwax/`. A debug build started plainly finds your real accounts, without their passwords, and opens the store the installed Sealwax may have open at the same time in the background, which is not tested. To keep a development build away from your mail, give it scratch folders:
  ```sh
  scratch=$PWD/.local/dev-home
  XDG_CONFIG_HOME=$scratch/config XDG_DATA_HOME=$scratch/data \
    XDG_STATE_HOME=$scratch/state XDG_CACHE_HOME=$scratch/cache \
    RUST_LOG=debug target/debug/sealwax
  ```
  Agents and the self-tests run the app only through `tools/headless.sh`, which does this and more.
- **A release build** has the installed app-id. With the package installed, `target/release/sealwax` doesn't run itself: with no Sealwax running, it asks the session bus to start the installed one and hands over; with one running, it hands its command line to that one. Run release builds from the source tree on a machine without the package, or uninstall it first.
- **Sign in with Microsoft** needs a client ID: put it in `config.toml`, or build it in by exporting `SEALWAX_MICROSOFT_CLIENT_ID` before `cargo build` ([OAUTH-SETUP.md](docs/OAUTH-SETUP.md#where-the-id-goes)). Never commit it.

## Uninstalling

1. In Sealwax, remove your accounts first if you want their local mail and keyring items gone, and turn off Start Sealwax at Login if it is on ([user guide](docs/USER_GUIDE.md#removing-sealwax-and-its-data)).
2. Quit Sealwax with Ctrl+Q.
3. Remove the package:
   ```sh
   sudo pacman -R sealwax
   ```

pacman removes only the files the package installed. Your settings, your mail store and the keyring items stay until you delete them: the user guide's [Removing Sealwax and its data](docs/USER_GUIDE.md#removing-sealwax-and-its-data) lists them and how.

## What has been verified

| Step | Verified? | How, and when |
| --- | --- | --- |
| `python3 tools/package-source.py` | **Yes** | The owner's soak builds on 2026-10-07 built the packages it made for `d4d1d0e` and `c21c65f`; the installed package's build date is `c21c65f`'s commit time, which only the script's PKGBUILD sets |
| `makepkg --cleanbuild --force --nodeps -si`, Rust from rustup's installer | **Yes, after a fix** | On `d4d1d0e` the build failed to link (makepkg's LTO, above). On `c21c65f`, which turned LTO off, the owner's build linked, makepkg's `check()` passed (domain 372 and 17 tests, engine 175 plus its suites, infrastructure 195; none failed) and `sealwax-0.3.0-1-x86_64.pkg.tar.zst` was made (09:34–09:37). Its install step stopped because `sudo` timed out waiting for the password |
| Installing the built package | **Yes** | pacman's database on the development machine lists `sealwax 0.3.0-1`, installed 2026-10-07 09:49:59 CEST, its build date `c21c65f`'s commit time (the PKGBUILD's `SOURCE_DATE_EPOCH`); which command installed it (`sudo pacman -U` or makepkg again) isn't recorded. Read with `pacman -Qi sealwax` while writing this page |
| Starting the installed Sealwax through D-Bus activation | **Yes** | A bus-started Sealwax (`dbus-:1.1-io.github.pixdevsapps.Sealwax@0.service`, the release app-id) running since 09:50:17 CEST, and its log in the user journal (`journalctl --user -t sealwax`; lines counted, not read). Read with `systemctl --user` while writing this page |
| Building with Arch's `rust` or `rustup` package (without `--nodeps`) | Not verified | |
| Installing on a clean system with only the listed dependencies | Not verified | The clean-container install test is ML-095, after M8's gate |
| Updating over an earlier package | Not verified | |
| The client ID built in by `tools/package-source.py` (2026-10-09) | **Partly** | The owner's makepkg build of `f6727d8` (from `config.toml`'s ID) made a package whose binary carries it, installed with `pacman -U`; Sign in with Microsoft from an installed package without `config.toml`'s ID isn't verified yet |
| `sudo pacman -R sealwax` | Not verified | |
| `cargo build` and `cargo build --release` | **Yes** | Every CI run builds the workspace (`tools/ci.sh`); the release build is what makepkg runs |

Running a development build with scratch XDG folders as above is what `tools/headless.sh` does for every self-test; the command line above, run by hand on the live desktop, is not tried.
