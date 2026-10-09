# Sealwax

A fast, minimal email client for [Omarchy](https://omarchy.org): native GTK 4 and libadwaita, written in Rust, following your live Omarchy theme and font, quick to drive from the keyboard, and treating every message as hostile.

![Sealwax in the matte-black theme, showing a conversation of fictional mail](tools/progress/public/m6-reply-conversation-matte-black-square-2026-10-06.png)

## Status

**Pre-1.0: version 0.3.0, with milestones M0 to M8 built.** M0–M7 are approved; M8 (POP3 accounts) is built and works end to end on the local test mail servers, and its gate follows the owner's 1–2 weeks of daily use with real accounts. M9, the work toward 1.0, is under way: accessibility, the security review and fuzzing and this documentation are done; UI polish is in progress (account names and colours, Edit Account, folding accounts, resizable panes, the composer docked in the window, folders and rules so far), then performance, packaging and the sign-off. Testing so far is on local test servers (Dovecot, Stalwart, Mailpit), plus Microsoft sign-in checked live on one Outlook.com account; real providers come with the owner's daily use. [docs/PLAN.md](docs/PLAN.md) has the current state.

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

Not supported: Gmail and Sign in with Google (use Gmail's own app or web version). Not in 1.0: server-side search, rules uploaded to the server (ManageSieve), PGP and S/MIME, calendars and contacts beyond autocomplete ([scope](docs/PLAN.md#scope-control)).

## Requirements

Arch Linux on x86_64 with GTK 4.18+, libadwaita 1.9+, WebKitGTK 6.0 2.52+, SQLite 3.43+ and a Secret Service such as gnome-keyring; optionally hunspell dictionaries for spell checking. Omarchy 4 is the desktop it is made for. Building needs Rust 1.92+.

## Install

Build the Arch package from this repository with its PKGBUILD: [INSTALL.md](INSTALL.md).

## Documentation

- [User guide](docs/USER_GUIDE.md): setup (Microsoft sign-in, app passwords, POP3), reading, writing, search, notifications, every keyboard shortcut, theming, the settings files, privacy and troubleshooting
- [Install](INSTALL.md): the package, building from source, uninstalling
- [Security](docs/SECURITY.md): the threat model, the rules and the known limitations
- [Integrations](docs/INTEGRATIONS.md): Omarchy, Hyprland key bindings and menu entries, providers
- [Microsoft sign-in setup](docs/OAUTH-SETUP.md): registering a client ID
- [Building](docs/BUILD.md) and [testing](docs/TESTING.md)
- [Product requirements](docs/PRD.md), [plan and current state](docs/PLAN.md), [decisions](docs/DECISIONS.md), [design](docs/DESIGN.md)
- Working on it with an agent: [AGENTS.md](AGENTS.md)

License: MIT.
