# Sealwax 1.0.0 release notes (draft)

> **Draft, 2026-10-09.** Sealwax 1.0.0 isn't released: 0.4.0, the first public release, came out on 2026-10-09 ([CHANGELOG.md](../CHANGELOG.md)), and 1.0.0 follows the owner's sign-off (ML-096), after M8's gate and the rest of M9. The parts marked *(at the sign-off)* are filled in then, from what was actually run. Nothing here is a measurement made for this page.

Sealwax is a fast, minimal email client for [Omarchy](https://omarchy.org): native GTK 4 and libadwaita, written in Rust, following your live Omarchy theme and font, quick to drive from the keyboard, and careful with every message: no script in one ever runs, and nothing from the internet loads until you allow it. 1.0.0 is its first stable release.

## What's in 1.0

- **Accounts without server settings.** Type your address and Sealwax finds the servers: its provider list, your domain's autoconfig, Mozilla's ISPDB and DNS. **Sign in with Microsoft** for Outlook.com and Microsoft 365; app passwords for iCloud, Fastmail, Yahoo, AOL and others, with a link to the provider's page; Proton Mail through Proton Mail Bridge, whose own certificate you trust once; manual settings for the rest. Several accounts and All Inboxes, each account with a name and a colour of your choosing, folding away in the sidebar.
- **IMAP** with IDLE push, CONDSTORE and QRESYNC where the server has them, and an offline copy of your mail; **POP3** into local folders, leaving mail on the server by default.
- **Reading.** Whole conversations; safe HTML (no message script ever runs, and nothing remote loads until you allow it, for one message or for a sender); attachments that open safely, with a warning for programs and disguised files; link warnings; print, Show Original and Save as `.eml`; encrypted and signed mail shown with a notice rather than as garbage.
- **Triage.** Archive, delete, junk, move, star, read and unread, on one message or many, with Undo, also offline; a right-click menu on messages.
- **Folders and rules.** New subfolders, rename and delete, shown as a tree you can fold, as you left it. Rules in Sieve, the standard mail-filter language, made in the Rules dialog or by hand in `~/.config/sealwax/rules/<account>.sieve`: move, copy, mark read, star, junk and delete, on new mail in the Inbox and Junk; always-junk and never-junk senders; Create Rule from Message. A rule you save sorts the mail already in the Inbox at once, with Undo.
- **Writing.** A rich-text composer docked in the main window (minimize, expand, several at once); reply, reply all, forward and a quick reply under the conversation; identities and signatures; attachments with the server's size limit checked before sending; drafts saved as you type and kept on the server; an outbox with Undo send that never sends a message twice; `mailto:` links, and Sealwax as the default mail app when you choose.
- **Search** on this computer, with `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `before:` and `after:`, and suggestions as you type.
- **Notifications and background mode.** New-mail notifications that open the message; closing the window keeps mail syncing; Start Sealwax at Login.
- **Made for Omarchy.** The live theme and font, light and dark; corners that follow Hyprland; no title bar on Hyprland; layouts for wide, half-screen and narrow tiled windows, with panes you can drag. Outside Omarchy, Sealwax uses its own light and dark palettes.
- **Keyboard first.** One-key shortcuts in the list and the reader, a command palette (F1), and your own keys in `keys.toml`. Settings live in plain files: `config.toml`, `keys.toml` and `accounts.toml`.
- **Accessible.** Controls are named for screen readers, the folder tree reports its levels and folded states, and every command can be reached from the keyboard; an accessibility audit of the windows and dialogs runs in CI.
- **Private.** Passwords and tokens only in your keyring; no telemetry; logs without mail content, addresses or folder names; outgoing mail without your computer's name or a Bcc header.

## Changes since 0.3.0

0.3.0 (2026-10-06) was the first usable version: accounts, sync, triage, reading, writing and sending. Since then:

- **Search, notifications and background mode** (M7).
- **POP3 accounts** with local folders (M8).
- **The user guide, README and INSTALL**; the guide is installed in `/usr/share/doc/sealwax/`.
- **A security review and fuzzing:** three fixes found by the review (an SMTP STARTTLS injection, a Referer leak, and POP3 mail that the disk limit could remove when an IMAP account held the same message); 13 fuzz targets; search text, previews and replies read at most 256 KiB of a message's HTML; deeply nested HTML flattened so it can't exhaust the stack; a sync that crashes starts again after a pause.
- **The accessibility pass:** the audit in CI, Go to and Back to List.
- **Interface:** account names and colours and Edit Account; folding accounts; resizable panes and a wider reader; the composer docked in the main window; the wax-seal icon; styled mail narrower than the reader sitting on your theme's background instead of a wide white page; housekeeping folders other mail programs create (Outlook's Sync Issues, Notes and Outbox on Microsoft's servers, an empty Mailspring folder) left out.
- **Folders and rules** (above).
- **Google dropped:** Gmail isn't a listed provider and Sign in with Google isn't offered. Gmail works with an app password: [GMAIL.md](GMAIL.md) shows how, step by step.
- **Proton Mail through Proton Mail Bridge** (0.4.8, [issue #9](https://github.com/PixDevsApps/sealwax/issues/9)): Bridge's own certificate is trusted once, for that server on this computer only.

## Requirements

- Arch Linux on x86_64, made for Omarchy 4.
- At runtime: `gtk4` 4.18 or later, `libadwaita` 1.9 or later, `webkitgtk-6.0` 2.52 or later, `sqlite` 3.43 or later, `glib2`, `gcc-libs`, `glibc` (pacman installs them with the package).
- Optional: a Secret Service such as `gnome-keyring` (without one, passwords and tokens are kept in memory until Sealwax quits, and a banner says so); hunspell dictionaries for spell checking.
- To build: Rust 1.92 or later, `base-devel`, `pkgconf`, `git` and Python 3.11 or later.
- Sign in with Microsoft works as installed: the package carries Sealwax's Microsoft client ID. A build from source needs one ([OAUTH-SETUP.md](OAUTH-SETUP.md#where-the-id-goes)).

## Installing and upgrading

Install the release's package with one command, or build the Arch package from the repository ([INSTALL.md](../INSTALL.md)). Coming from 0.3.0, the first start moves the mail store to its current format (schema v12) automatically; the move is one way, so an older Sealwax can't open the store afterwards.

## Known limitations

- **Gmail and Google Workspace** have no Sign in with Google: they work with an app password ([GMAIL.md](GMAIL.md)).
- **Microsoft work or school accounts** may need their organization's admin to approve Sealwax before it can sign in, as Sealwax's Microsoft registration isn't publisher-verified yet (planned after 1.0). After a minute of waiting, the setup assistant says so, and the [user guide](USER_GUIDE.md) explains what to ask the admin.
- **Rules run in Sealwax**, not on the server: only while Sealwax runs (background mode is on by default), and on headers, not message bodies. Rules aren't uploaded to servers (ManageSieve).
- **Servers with self-signed certificates elsewhere** (some self-hosted servers) can't be added: only a server on this computer, such as Proton Mail Bridge, can be trusted with its own certificate.
- **PGP and S/MIME** mail is shown with a notice but not decrypted or verified.
- **A passwordless keyring:** gnome-keyring's default keyring often has no password, so its file is protected only by disk encryption; the [user guide](USER_GUIDE.md#giving-your-keyring-a-password) shows how to give it one.
- **A hostile IMAP server can keep an account failing** by sending a reply that crashes the IMAP parser Sealwax uses (reported upstream as djc/imap-proto#210); Sealwax restarts the account's sync after a pause and shows it offline meanwhile.
- **WebKit's sandbox** needs bubblewrap and unprivileged user namespaces.
- **Sleep and wake** were tested on test servers but not through a real suspend: if mail doesn't arrive after the computer wakes, Refresh (F5) or a restart brings it.
- **English only**; no translations in 1.0.
- Not in 1.0: server-side search, schedule send, snooze, templates, calendars, and contacts beyond address suggestions.

The full list, with what was checked and how, is in [SECURITY.md](SECURITY.md#known-limitations).

## How 1.0.0 was checked *(at the sign-off)*

- The CI run on the release commit: tests, self-tests, the accessibility audit and the hostile-mail suite.
- The owner's daily use with real accounts from 7 to 19 October 2026 (M8's soak): Outlook.com, Microsoft 365 and IMAP, Gmail among them through an app password. POP3 was tested on the test servers only.
- The clean-container install test (ML-095).
- The owner's live checks: typing through fcitx5, Orca, the desktop's file chooser and print dialog, the live theme and font, search suggestions, background mode, starting at login, mailto: links, Check for Updates, and how Sealwax's mail looks in Gmail web, Outlook web and Thunderbird.

## License

MIT. The licenses of the Rust crates Sealwax is built with are in the package, at `/usr/share/licenses/sealwax/THIRD-PARTY-LICENSES.txt` beside Sealwax's own `LICENSE`, and attached to the release on GitHub. `tools/licenses.py` makes the file from `Cargo.lock` when the package is made ([BUILD.md](BUILD.md#packaging)).
