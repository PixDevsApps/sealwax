# What's new in Sealwax

Each release, newest first, in plain words. [INSTALL.md](INSTALL.md) says how to install, update and uninstall Sealwax.

## Not released yet

- **Large messages open and answer faster:** a message too large to download whole (Sealwax keeps its text and leaves its attachments on the server) showed its images only after downloading them again each time you opened it. Now they're kept in memory once downloaded, so the message opens at once the next time. Replying to such a message no longer downloads the whole message, attachments and all, before the address and "Re:" appear: the reply is written from the text Sealwax keeps, and only the images the quote shows are downloaded, or taken from memory when you have just read the message. A forward still downloads the whole message, since it carries the attachments.

## 0.4.8 — 2026-10-13

- **Proton Mail through Proton Mail Bridge:** add your Proton address and Sealwax fills in Bridge's settings and asks for the Bridge password. Bridge makes its own certificate, which your system doesn't trust; the setup assistant shows its fingerprint and asks once whether to trust it, and Sealwax then accepts that one certificate from Bridge and no other. Only a server on your own computer can be trusted this way ([issue #9](https://github.com/PixDevsApps/sealwax/issues/9)).

## 0.4.7 — 2026-10-13

- **A clear answer when Microsoft 365 refuses a message:** some Microsoft 365 servers answer a sent message with a very long line, for example when the From address isn't one the mailbox may send as. Sealwax couldn't read such an answer, so it warned that it didn't know whether the message was sent. Now it reads answers up to 16 times longer and says plainly that the server refused the message, or that it was sent ([issue #2](https://github.com/PixDevsApps/sealwax/issues/2)).

## 0.4.6 — 2026-10-12

Two requests from a user ([issue #8](https://github.com/PixDevsApps/sealwax/issues/8)).

- **See who else got a message:** a message sent to you and others now says so in its header, for example "to me, Tomas Berg and 2 others", instead of just "to me".
- **Reply All by default, if you like:** set `reply = "all"` under `[reader]` in `config.toml` to have Reply (`r`), the first reply button and the quick reply answer everyone the message went to. A new **Reply to Sender** command answers the sender alone.

Thanks to [@ocewers](https://github.com/ocewers) for both ideas.

## 0.4.5 — 2026-10-12

- **A deleted message leaves at once:** when you delete, archive or move the message you're reading, it now leaves the reading pane right away, also on slower computers. Before, it could stay there for a second or two while the next message was prepared; now the next message shows at once and its content fills in as it loads.

## 0.4.4 — 2026-10-12

- **Send from any account:** the From menu of a new message now lists every account's addresses, not only one account's; pick another to send through that account ([issue #7](https://github.com/PixDevsApps/sealwax/issues/7)).

## 0.4.3 — 2026-10-11

Two requests from users.

- **Sealwax opens where you left it:** a new window shows the view you last had, All Inboxes or a folder, also after a restart ([issue #5](https://github.com/PixDevsApps/sealwax/issues/5)).
- **Newest message first, if you like:** set `conversation_order = "newest-first"` under `[reader]` in `config.toml` to see a conversation's latest message at the top, with the reply box above it ([issue #3](https://github.com/PixDevsApps/sealwax/issues/3)).

## 0.4.2 — 2026-10-11

Fixes from the first bug report ([issue #2](https://github.com/PixDevsApps/sealwax/issues/2)), about sending with Microsoft 365.

- **Clearer when Sealwax can't tell whether a message was sent:** if the server's answer doesn't arrive or can't be read (as can happen with Microsoft 365), Sealwax now says so, instead of saying that it stopped while sending. As before, it never sends such a message again without asking you. The warning now also names the message: its subject, account and time.
- **Microsoft accounts: a check on the sending address.** If the Microsoft account you sign in with isn't the address you typed, Sealwax asks which one to send from, instead of saving an account whose mail Microsoft would refuse.

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
