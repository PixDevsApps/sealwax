# What's new in Sealwax

Each release, newest first, in plain words. [INSTALL.md](INSTALL.md) says how to install, update and uninstall Sealwax.

## 0.4.1 — 2026-10-11

A small update for Gmail users.

- **Gmail:** Sealwax no longer asks you to turn on IMAP in Gmail's settings, as Gmail always has it on. [GMAIL.md](docs/GMAIL.md) shows, step by step, how to add a Gmail account with an app password.

## 0.4.0 — 2026-10-09

The first public release. Since 0.3.0:

- **Search your mail** on your own computer, with suggestions as you type. Narrow it down with words like `from:`, `to:`, `subject:`, `has:attachment` and `is:unread`.
- **Notifications** for new mail; click one to open the message.
- **Keeps working in the background:** closing the window doesn't stop new mail from arriving. You can also have Sealwax start when you log in.
- **POP3 accounts**, for mail providers that don't offer IMAP. The mail is kept in folders on your computer, and left on the server unless you choose otherwise.
- **Folders:** make new folders and subfolders, rename them and delete them. Subfolders show as a tree you can fold away.
- **Rules** sort your mail for you: move, copy, star, mark as read, move to Junk or delete messages that match what you choose. A new rule tidies the Inbox right away, and you can undo it. Rules use Sieve, the standard language for mail filters.
- **Your accounts, your way:** give each account a name and a colour, change them later with Edit Account, and fold an account away in the sidebar.
- **Drag the borders** between the sidebar, the message list and the message to the widths you like. The reading column is wider than before.
- **Writing a message** happens in the main window: minimise it, expand it, or keep several messages open at once.
- **Easier on the eyes:** a newsletter narrower than the reading pane now sits on your theme's background instead of a wide white or grey page.
- **Tidier folder list:** housekeeping folders made by other mail programs are hidden (Outlook's sync logs, Notes and Outbox on Microsoft accounts, an empty Mailspring folder).
- **A new icon:** the wax seal.
- **Safer:** a security review fixed three problems, and very deeply nested or very large messages can no longer slow Sealwax down or crash it.
- **Easier to use without a mouse or with a screen reader:** every control has a name, and Go to and Back to List help you move around.
- **A user guide** comes with Sealwax, in `/usr/share/doc/sealwax/USER_GUIDE.md`.
- **Gmail is no longer a listed provider**, and Sign in with Google isn't offered. Use Gmail's own app or website.

**Updating from 0.3.0:** the first start converts your mail store to the new format by itself. The change is one way: 0.3.0 can't open the store afterwards.

## 0.3.0 — 2026-10-06

The first version good enough for daily use: add an account, read whole conversations safely, archive, delete, move and star messages (with Undo, also offline), and write, reply to and send mail with drafts and an outbox you can undo. Not released publicly.

## 0.2.0 — 2026-10-06

Triage and offline changes, on the way to 0.3.0. Not released publicly.
