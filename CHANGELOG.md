# What's new in Sealwax

Each release, newest first, in plain words. [INSTALL.md](INSTALL.md) says how to install, update and uninstall Sealwax.

## Not released yet

- **A mail server can't make Sealwax quit any more:** when Sealwax fetches new mail, the server describes how each message's parts are nested inside one another. A description nested a few hundred levels deep, under a kilobyte, made Sealwax run out of room and quit at once, and again at each start while the server kept sending it. Anyone on the network could also send one in place of the server's first line on port 143, before the connection is encrypted. Sealwax now stops at 20 levels: such a message shows without its attachment clip. The fix is imap-proto's own, the library Sealwax reads IMAP with, applied to Sealwax's copy until an imap-proto release has it.

## 1.0.3 — 2026-10-24

A security update. Please update.

- **Microsoft sign-in stays with Microsoft:** when you added a Microsoft 365 account on your own domain, Sealwax could save an outgoing mail server that only an unchecked DNS answer named. The first message you sent then gave that server your account's access token, which lets its holder read and send your mail until it expires, usually within about an hour. Your password and the long-lived sign-in were never sent. Sealwax now sends a Microsoft token only to Microsoft's own servers, and a new Microsoft account always uses the servers its sign-in was checked on. If sending from a Microsoft account stops with an error after this update, remove the account and add it again. Reported privately by FieldsFury Labs ([GHSA-vhrm-52f2-v9v3](https://github.com/PixDevsApps/sealwax/security/advisories/GHSA-vhrm-52f2-v9v3)).
- **A hostile mail server can't tie up Sealwax any more:** an outgoing mail server, or anyone on the network before the connection is encrypted on port 587, could make Sealwax fill its memory with a reply line that never ends, or keep a processor busy for seconds with a reply of many short lines. Sealwax now stops reading a reply line at 16 KiB and reads each reply once. The never-ending line was reported privately by FieldsFury Labs ([GHSA-chx5-p5h3-52m5](https://github.com/PixDevsApps/sealwax/security/advisories/GHSA-chx5-p5h3-52m5)); the fix is in Sealwax's copy of lettre, the library it sends mail with, and has been reported to lettre.

## 1.0.2 — 2026-10-24

- **Links open exactly as written:** a link in a message used to reach the browser slightly rewritten, with its encoded characters undone. Most links didn't mind, but a link that carries another link inside it, as sign-in links from a click tracker do, broke and failed to sign in, while the same link pasted into a browser worked ([issue #22](https://github.com/PixDevsApps/sealwax/issues/22)). Sealwax now hands the browser the link unchanged.

## 1.0.1 — 2026-10-23

- **A clear word when Microsoft 365 won't let mail apps send:** many Microsoft 365 organizations have SMTP AUTH, the way mail apps send mail, turned off, for everyone or for some mailboxes. Adding such an account used to end in "The sign-in didn't work" after a sign-in that had worked ([issue #21](https://github.com/PixDevsApps/sealwax/issues/21)). The setup assistant now says "Sending is turned off for this account" and that your organization's admin can turn SMTP AUTH on for your mailbox; then sign in again. The user guide's "Common problems" says the same.

## 1.0.0 — 2026-10-22

Sealwax 1.0: everything in 0.5.1, checked in daily use with real accounts, in a fresh install and on a live desktop ([the 1.0 release notes](docs/RELEASE_NOTES.md)), and these changes:

- **Hide the folder list:** Toggle Sidebar (Ctrl+Shift+S or F9, or the main menu) now works. It hides the folders so the messages get the room, and brings them back; Sealwax remembers it. Before, the command was listed but did nothing.
- **A clearer icon at small sizes:** at 16 and 24 pixels, as in a bar or a file list, the wax seal's @ turned into a blur; those two sizes now show a bold @ on the seal. The larger sizes are unchanged.
- **The other projects' licenses come with Sealwax:** the package now carries the license texts of the libraries Sealwax is built from, in `/usr/share/licenses/sealwax/THIRD-PARTY-LICENSES.txt`, and each release on GitHub has the same file.
- **One message the server describes oddly no longer stops a folder:** when the server's reply about a message is one Sealwax can't read, Sealwax used to drop the connection and try again forever, so no new mail came ([issue #20](https://github.com/PixDevsApps/sealwax/issues/20), a Zoho account). It now asks for less about that message, and if it still can't read it, leaves that one message out and carries on; the log says which command's reply couldn't be read instead of "i/o error".
- **Sealwax names itself when it looks up your provider:** while you add an account, the lookups of your provider's settings now say they come from "Sealwax" instead of a placeholder left from development. They still carry no version and, apart from your domain's own settings server, not your address.

## 0.5.1 — 2026-10-20

- **Check for Updates:** a new item in the main menu, **Check for Updates…**, asks GitHub which Sealwax is the latest and says "You have the latest version", or shows the command that installs the new one, with a **Copy Command** button: paste it into a terminal and press Enter. Sealwax still never checks by itself, only when you choose it.

## 0.5.0 — 2026-10-20

- **Unread mail in folded subfolders shows:** a folder whose subfolders are folded away now counts their unread mail with its own, as Thunderbird does, so you can see there is new mail inside. Open the folder again and each one shows its own count.
- **Read and move around from the keyboard:** `d` and Shift+D scroll the open message half a screen down and up from the message list, so you can read on without leaving the list ([issue #10](https://github.com/PixDevsApps/sealwax/issues/10)); `l` opens the message, as Enter and `o` do. `g` then `a` goes to All Inboxes, and `g` then `1` to `9` to that account's Inbox, in the sidebar's order ([issue #12](https://github.com/PixDevsApps/sealwax/issues/12)). `keys.toml` can change them (`scroll-message-down`, `scroll-message-up`, `go-to-all-inboxes`, `go-to-account-1` …).
- **Ctrl+W closes the composer:** a new command, **Close Composer**, saves the draft and closes the composer as its Close button does, from anywhere in it. `keys.toml` can give it another key (`close-composer`).
- **Moving between messages keeps up on a slow computer:** when you move with `j`/`k`, a click or Previous/Next, the message you left goes from the reader within 150 ms, as a deleted one already did; the next one shows as far as it is ready and fills in. Before, the old message could stay for up to 3 seconds while the next one was prepared.
- **The first message after starting opens sooner:** Sealwax now starts the part that shows messages as soon as its window is up, instead of when you open your first message, so the first one no longer waits for it.
- **Large messages show at once:** a message too large to download whole no longer waits for its pictures from the server before it shows. Its text is there at once and the pictures appear as they arrive.
- **Spell checking needs `hunspell`:** the package now suggests `hunspell` itself as well as a dictionary such as `hunspell-en_us`; a dictionary alone gave no spell checking. If spelling mistakes aren't underlined in the composer, install both: `sudo pacman -S hunspell hunspell-en_us`.
- **A quieter log:** Sealwax no longer sets a WebKit setting that does nothing and logged a warning each time a message was shown.
- **A word when Microsoft sign-in seems stuck:** many work and school organizations must approve Sealwax before their people can sign in, and Microsoft then shows "Need admin approval" and never sends the browser back. After a minute of waiting, the setup assistant now says so and points to the user guide, which explains what to ask your IT admin. Personal Outlook.com accounts aren't affected.

## 0.4.9 — 2026-10-19

- **Images in mail no longer make your computer look up their links:** WebKitGTK 2.54, the engine Sealwax shows mail with, dropped the switch that turned DNS prefetch off. A message whose images you had allowed could then make your computer look up the names it lists, which tells the sender's name servers that the message was opened. Sealwax now blocks those lookups in every message, images allowed or not.
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
