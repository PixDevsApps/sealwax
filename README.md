# Sealwax

Mail, sealed. A fast, minimal email client for [Omarchy](https://omarchy.org): native GTK 4 and libadwaita, written in Rust, following your live Omarchy theme and font, quick to drive from the keyboard, and careful with every message: no script in one ever runs, and nothing from the internet loads until you allow it.

![Sealwax in the matte-black theme, showing a conversation of fictional mail](tools/progress/public/m6-reply-conversation-matte-black-square-2026-10-06.png)

## Install

Open a terminal (**Super + Return** on Omarchy), paste this line and press **Enter**:

```sh
curl -L -o /tmp/sealwax.pkg.tar.zst https://github.com/PixDevsApps/sealwax/releases/latest/download/sealwax-x86_64.pkg.tar.zst && sudo pacman -U /tmp/sealwax.pkg.tar.zst
```

Run the same line again to update; **Check for Updates…** in Sealwax's main menu says when there is a new version and gives you the line. To uninstall: `sudo pacman -R sealwax`.

[INSTALL.md](INSTALL.md) explains every step in plain words: installing, starting, updating and uninstalling, and what to do if something goes wrong. [What's new](CHANGELOG.md) lists each release.

## Status

**Version 0.5.1.** The features below are built and tested, and its author reads their own mail with it every day; 1.0 follows a longer period of daily use with real accounts. Testing so far is on local test mail servers (Dovecot, Stalwart, Mailpit), with Microsoft sign-in checked live on an Outlook.com account and the author's own accounts in daily use; other providers are welcome to report how it goes. The [1.0 release notes](docs/RELEASE_NOTES.md) are drafted.

## Features

- **Accounts without server settings:** type your address, and Sealwax finds the servers (its provider list, your domain's autoconfig, Mozilla's ISPDB, DNS). **Sign in with Microsoft** for Outlook.com and Microsoft 365; app passwords for iCloud, Fastmail, Yahoo and others, with a link to the provider's page; manual settings for the rest. Several accounts and All Inboxes, each account with a name and a muted colour of your choosing, folding away in the sidebar.
- **IMAP** with IDLE push and an offline copy of your mail, and **POP3** into local folders (mail left on the server by default).
- **Reading:** whole conversations; safe HTML (no message script ever runs; nothing remote loads until you allow it, per message or per sender); attachments that open safely, with a warning for programs and disguised files; link warnings; print, Show Original, Save as `.eml`; graceful display of encrypted and signed mail.
- **Triage:** archive, delete, junk, move, star, read and unread at once, on one message or many, with Undo, also offline; a right-click menu on messages.
- **Folders and rules:** new subfolders, rename and delete, shown as a tree you can fold; rules in Sieve, the standard mail-filter language, made in a Rules dialog or by hand in `rules/<account>.sieve`, run by Sealwax on new mail (move, copy, mark read, star, junk, delete), with always-junk and never-junk senders and Create Rule from Message.
- **Writing:** a rich-text composer docked in the main window (minimize, expand, several at once), reply, reply all, forward and a quick reply under the conversation; identities and signatures; attachments with the server's size limit checked; drafts saved as you type and kept on the server; an outbox with Undo send that never sends a message twice; `mailto:` links, and becoming the default mail app when you choose.
- **Search** on this computer with `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `before:`, `after:` and suggestions as you type.
- **New-mail notifications** that open the message, and **background mode**: closing the window keeps mail syncing; Start Sealwax at Login.
- **Omarchy:** the live theme and font, light and dark; corners that follow Hyprland; no title bar on Hyprland; layouts for wide, half-screen and narrow tiled windows, with panes you can drag wider or narrower.
- **Keyboard first:** one-key shortcuts in the list and reader, a command palette (F1), and your own keys in `keys.toml`. Settings live in plain files: `config.toml`, `keys.toml`, `accounts.toml`.
- **Private:** passwords and tokens only in your keyring; no telemetry; logs without mail content; outgoing mail without your computer's name or a Bcc header.

**Gmail** works with an app password, as [this step-by-step guide](docs/GMAIL.md) shows; there is no Sign in with Google. Not in 1.0: server-side search, rules uploaded to the server (ManageSieve), PGP and S/MIME, calendars and contacts beyond autocomplete ([release notes](docs/RELEASE_NOTES.md#known-limitations)).

## Requirements

Omarchy, or Arch Linux, on x86_64, with GTK 4.18+, libadwaita 1.9+, WebKitGTK 6.0 2.52+, SQLite 3.43+ and a Secret Service such as gnome-keyring; optionally `hunspell` and a hunspell dictionary for spell checking. Omarchy 4 is the desktop it is made for. Building needs Rust 1.92+.

## Documentation

- [User guide](docs/USER_GUIDE.md): setup (Microsoft sign-in, app passwords, POP3), reading, writing, search, notifications, every keyboard shortcut, theming, the settings files, privacy and troubleshooting
- [Install](INSTALL.md): installing, updating and uninstalling, in plain words; building from source
- [What's new](CHANGELOG.md): each release
- [Release notes for 1.0.0](docs/RELEASE_NOTES.md) (a draft until the sign-off)
- [Security](docs/SECURITY.md): the threat model, the rules and the known limitations
- [Integrations](docs/INTEGRATIONS.md): Omarchy, Hyprland key bindings and menu entries, providers
- [Microsoft sign-in setup](docs/OAUTH-SETUP.md): registering a client ID
- [Building](docs/BUILD.md) and [testing](docs/TESTING.md)
- [Decisions](docs/DECISIONS.md) and [design](docs/DESIGN.md)

## Thanks

Thanks to everyone who reports bugs and suggests improvements in the [issues](https://github.com/PixDevsApps/sealwax/issues), and especially to [@ocewers](https://github.com/ocewers) for detailed reports and ideas (issues #2 to #8).

License: MIT.
