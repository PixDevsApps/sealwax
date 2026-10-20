# Installing Sealwax

Synced: 2026-10-18

Sealwax is an email app for [Omarchy](https://omarchy.org). This page shows how to install it, keep it up to date and remove it again. You don't need to know anything about programming: follow the steps in order.

- [Before you start](#before-you-start)
- [Install Sealwax](#install-sealwax)
- [Start Sealwax and add your email](#start-sealwax-and-add-your-email)
- [Updates](#updates)
- [Uninstall Sealwax](#uninstall-sealwax)
- [If something goes wrong](#if-something-goes-wrong)
- [For developers: build from source](#for-developers-build-from-source)
- [What has been verified](#what-has-been-verified)

## Before you start

You need:

- **A computer with Omarchy.** Sealwax also works on plain Arch Linux, on a normal 64-bit PC.
- **An internet connection.**
- **Your computer's password.** Installing asks for it once.

**How to use a command from this page.** Some steps show a command in a grey box. To run it:

1. Open a terminal: press **Super + Return**. (Super is the key with the Windows logo.)
2. Copy the command from this page, and paste it into the terminal with **Super + V**.
3. Press **Enter**.
4. If it asks for your password, type your computer's password and press **Enter**. Nothing appears on the screen while you type; that's normal.
5. If it asks a question such as `Proceed with installation? [Y/n]`, press **Enter** to say yes.

## Install Sealwax

### With one command (easiest)

This downloads the newest Sealwax from GitHub and installs it. Run this command in a terminal (see [how](#before-you-start)); it's one long line:

```sh
curl -L -o /tmp/sealwax.pkg.tar.zst https://github.com/PixDevsApps/sealwax/releases/latest/download/sealwax-x86_64.pkg.tar.zst && sudo pacman -U /tmp/sealwax.pkg.tar.zst
```

When it's done, the terminal shows the prompt again, and you can close it.

### Or download the file yourself

1. Open the [releases page](https://github.com/PixDevsApps/sealwax/releases).
2. Under the newest release, open **Assets** and click the file whose name starts with `sealwax-` and ends with `-x86_64.pkg.tar.zst`. Your browser saves it in your Downloads folder.
3. In a terminal, type `sudo pacman -U ~/Downloads/` and the file's name (press **Tab** after typing `~/Downloads/sealwax` to have the rest filled in), then press **Enter**.

## Start Sealwax and add your email

1. Press **Super + Alt + Space** to open the app menu, type `Sealwax`, and press **Enter**.
2. Click **Add an account** and type your email address. Sealwax finds your mail provider's settings by itself.
3. Type your email password, or click **Sign in with Microsoft** for Outlook.com, Hotmail and Microsoft 365 addresses. For Gmail, you need an app password from Google first: [GMAIL.md](docs/GMAIL.md) shows how, with pictures.

The [user guide](docs/USER_GUIDE.md) explains everything else, from reading and writing mail to rules and keyboard shortcuts. A copy comes with Sealwax, in `/usr/share/doc/sealwax/USER_GUIDE.md`.

## Updates

**To see whether there is a new version,** open Sealwax's main menu (the button with three lines, ☰, at the top of the message list) and choose **Check for Updates…**. Sealwax asks GitHub and says "You have the latest version", or shows the command that installs the new one: click **Copy Command**, paste it into a terminal (see [how](#before-you-start)) and press **Enter**.

**To update, run the [install command](#with-one-command-easiest) again.** It always fetches the newest Sealwax and installs it over the one you have. Sealwax doesn't update by itself.

**After an update, restart Sealwax:** click its window and press **Ctrl + Q**, then start it again. Closing the window isn't enough, because Sealwax keeps running in the background to check for new mail.

Your mail, accounts and settings stay as they are. When a new version needs to change how your mail is stored on the computer, it does that by itself the first time it starts. [CHANGELOG.md](CHANGELOG.md) says what's new in each version.

Sealwax never checks for updates by itself: only when you choose Check for Updates… does it ask GitHub, and that request carries nothing about you. New versions come only when you install them.

## Uninstall Sealwax

### 1. Remove your accounts in Sealwax (recommended)

This deletes the passwords Sealwax saved and the copy of your mail on this computer. **Your mail stays on your mail provider's server**: you can still read it on the web or in another app.

1. In Sealwax, open the main menu: the button with three lines (☰) at the top of the message list.
2. Choose **Remove Account**, pick the account and confirm. Do this for each account.
3. In the same menu, if **Start Sealwax at Login** has a check mark, click it to turn it off.
4. Quit Sealwax: press **Ctrl + Q**.

**Careful with POP3 accounts** whose settings remove mail from the server: for those, the mail on this computer may be your only copy. Move what you want to keep somewhere else first.

### 2. Remove the app

**From the Omarchy menu:** press **Super + Space**, choose **Remove**, then **Package**. Type `sealwax`, select it, and press **Enter**. Type your password when asked.

**Or with a command:**

```sh
sudo pacman -R sealwax
```

### 3. Delete Sealwax's leftover files (optional)

Removing the app leaves your Sealwax settings and its copy of your mail on the computer, in case you install it again. To delete them too, run:

```sh
rm -rf ~/.config/sealwax ~/.local/share/sealwax ~/.local/state/sealwax ~/.cache/sealwax
```

This deletes only Sealwax's own files on this computer, never the mail on your provider's server. The [user guide](docs/USER_GUIDE.md#removing-sealwax-and-its-data) lists every file and setting Sealwax can leave behind.

## If something goes wrong

- **"error: could not open file" or "no such file":** the download didn't finish. Check your internet connection and run the command again.
- **Nothing appears when you type your password:** that's normal. Type it and press **Enter**.
- **Sealwax still looks like the old version after an update:** quit it with **Ctrl + Q** in its window and start it again.
- **Something else:** open an [issue on GitHub](https://github.com/PixDevsApps/sealwax/issues) and say what you did and what you saw. Leave out your email address and passwords.

## For developers: build from source

**What you need:** Arch Linux on x86_64; `git`, Python 3.11 or later, `base-devel` (makepkg), `pkgconf`, and Rust 1.92 or later with Cargo (Arch's `rust` package, or rustup). At runtime, pacman installs `gtk4` 4.18 or later, `libadwaita` 1.9 or later, `webkitgtk-6.0` 2.52 or later, `sqlite` 3.43 or later, `glib2`, `gcc-libs` and `glibc`. Optional: a Secret Service such as `gnome-keyring` for passwords and tokens (without one, Sealwax keeps them in memory until it quits, and says so); `hunspell` and a dictionary such as `hunspell-en_us` for spell checking (the dictionary alone isn't enough).

**Build and install the Arch package:**

```sh
git clone https://github.com/PixDevsApps/sealwax.git
cd sealwax
python3 tools/package-source.py
cd .local/package
makepkg --cleanbuild --force -si              # Rust from Arch's rust or rustup package
makepkg --cleanbuild --force --nodeps -si     # Rust from rustup's own installer, in ~/.cargo
```

`tools/package-source.py` writes `.local/package/sealwax-<version>.tar.gz`, a tarball of the committed tree (HEAD; uncommitted changes aren't packaged, and it warns about them), and `.local/package/PKGBUILD` from `packaging/PKGBUILD.in`, with the version, the tarball's SHA-256 and `SOURCE_DATE_EPOCH` (the commit's time) filled in. makepkg fetches the crates (`cargo fetch --locked`), builds Sealwax (`cargo build --release --locked --offline -p sealwax`), runs the tests of the crates without a user interface (the app's self-tests need a display) and makes the package; `-i` installs it with pacman, `-s` installs missing dependencies first. The package you build is called `sealwax`, like the release's; the coming AUR package `sealwax-bin` replaces it and the other way round.

- **Sign in with Microsoft** needs a Microsoft client ID. `tools/package-source.py` builds one in: `SEALWAX_MICROSOFT_CLIENT_ID` if you export it, else your `~/.config/sealwax/config.toml`'s `client_id`; it says which, and `--no-client-id` builds none. Without one, add it after installing ([OAUTH-SETUP.md](docs/OAUTH-SETUP.md)). The release packages carry the project's ID.
- **Why `--nodeps` with rustup's installer:** Rust installed by rustup's own script isn't a pacman package, so makepkg's check of the build dependencies fails. `--nodeps` skips only makepkg's checks; pacman still checks the runtime dependencies when it installs the package.
- **Why the PKGBUILD turns off LTO** (`options=('!debug' '!lto')`): with Arch's default LTO flags, makepkg compiles the C code inside the `ring` crate to GCC's LTO bytecode, which Rust's linker can't read, and the final link fails. Sealwax's release build still uses Rust's own thin LTO. `!debug` because the release build carries no debug information.
- **No paths of your machine:** the PKGBUILD maps the build folder and Cargo's folder in your home to fixed names (`/usr/src/sealwax`, `/usr/src/cargo`), so the binary carries no path of yours.

**What the package installs:**

| File | What |
| --- | --- |
| `/usr/bin/sealwax` | The program |
| `/usr/share/applications/io.github.pixdevsapps.Sealwax.desktop` | The launcher entry; it also declares Sealwax a `mailto:` handler, which becomes the default only when you choose it in Sealwax |
| `/usr/share/dbus-1/services/io.github.pixdevsapps.Sealwax.service` | D-Bus activation |
| `/usr/share/icons/hicolor/*/apps/io.github.pixdevsapps.Sealwax.png`, `…/scalable/apps/io.github.pixdevsapps.Sealwax.svg` | The icon |
| `/usr/share/licenses/sealwax/LICENSE` | The MIT license |
| `/usr/share/doc/sealwax/` | README, INSTALL, the user guide and INTEGRATIONS |

Nothing in your home folder changes until Sealwax runs. Its key binding and Omarchy menu entries are yours to add ([INTEGRATIONS.md](docs/INTEGRATIONS.md#hyprland)).

**Update a source build:** `git pull`, then the last three commands again, then quit Sealwax with Ctrl+Q and start it again.

**Run from the source tree:** `cargo build` makes a debug build (`target/debug/sealwax`, app-id `io.github.pixdevsapps.Sealwax.Devel`, which runs beside an installed Sealwax with its own keyring items), and `bash tools/ci.sh` runs formatting, Clippy, the tests and the build. Every build uses the same folders as the installed Sealwax (`~/.config/sealwax/`, `~/.local/share/sealwax/`), so a debug build started plainly finds your real accounts; give it scratch folders to keep it away from your mail:

```sh
scratch=$PWD/.local/dev-home
XDG_CONFIG_HOME=$scratch/config XDG_DATA_HOME=$scratch/data \
  XDG_STATE_HOME=$scratch/state XDG_CACHE_HOME=$scratch/cache \
  RUST_LOG=debug target/debug/sealwax
```

A release build from the source tree has the installed app-id: with the package installed, it hands over to the installed Sealwax instead of running itself. [docs/BUILD.md](docs/BUILD.md) has the toolchain, every command, the test suites, the mail testbed and how a release is made; [docs/TESTING.md](docs/TESTING.md) the tests.

## What has been verified

| Step | Verified? | How, and when |
| --- | --- | --- |
| Building the package (`tools/package-source.py`, then makepkg with Rust from rustup's installer) | **Yes** | The owner's builds; 0.4.0's on 2026-10-09, 0.4.1's to 0.4.3's on 2026-10-11, 0.4.4's to 0.4.6's on 2026-10-12, 0.4.7's and 0.4.8's on 2026-10-13, 0.4.9's on 2026-10-19, 0.5.0's and 0.5.1's on 2026-10-20 with `tools/release.py build` (makepkg's `check()`: 1,067 to 1,096 passed, 0 failed) |
| Installing a built package with `sudo pacman -U` | **Yes** | The owner's machine, on every package since 2026-10-07; the installed binary compared byte for byte with the build's |
| Updating over an earlier package | **Yes** | The same installs, each over the one before; the store's format changes by itself on the first start |
| The install command (the download from the latest release) | **Yes** | On the owner's machine, 2026-10-09, as written on this page: the download is the release's package (the same SHA-256), and pacman upgraded `sealwax` from 0.3.0-1 to 0.4.0-1; the installed binary is byte for byte the release's and says `sealwax 0.4.0`. For 0.4.1 to 0.4.8 (2026-10-11 to 13) and 0.5.1 (2026-10-20) the download was checked, not installed: each the same SHA-256 as its build, read by pacman as that version. On 2026-10-19 the owner ran the command for 0.4.9: pacman upgraded `sealwax` from 0.4.7-1 to 0.4.9-1, and the installed binary is byte for byte the release's. On 2026-10-20 the owner ran the command for 0.5.0: pacman upgraded `sealwax` from 0.4.9-1 to 0.5.0-1, and the installed binary is byte for byte the release's |
| Building with Arch's `rust` or `rustup` package (without `--nodeps`) | Not verified | |
| Installing on a clean system with only the listed dependencies | Not verified | The clean-container install test comes before 1.0 |
| Uninstalling with `sudo pacman -R` | Not verified | |
