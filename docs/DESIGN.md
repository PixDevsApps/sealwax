# Design & Interaction System

Synced: 2026-10-09

The repository docs are the canonical project memory ([ADR-012](DECISIONS.md#adr-012--repository-docs-canonical-notion-as-a-mirror-2026-10-04)). Anything the user changes after approval is recorded here as a dated entry above the affected section.

## DESIGN gate approved — 2026-10-04

The user approved the [design mock](https://claude.ai/artifact/TU2jwqgJu1YxTXFLLdpuLS) on 2026-10-04 with **corners that follow Hyprland's rounding** ([ADR-019](DECISIONS.md#adr-019-accepted--corners-follow-hyprland-2026-10-04)): square on stock Omarchy, soft cards on this desktop (rounding 8). The layouts, components and roles below are the M1 target. The app is named **Sealwax** ([ADR-010](DECISIONS.md#adr-010-accepted--sealwax-2026-10-04)); its wordmark, seal and icon come from the user's brand canvas, while the chrome keeps following the Omarchy theme.

## Principles

1. **The theme is the only style.** Every colour comes from the live Omarchy palette; the app ships no palette of its own except the fallbacks for running outside Omarchy ([ADR-004](DECISIONS.md#adr-004--follow-the-live-omarchy-theme-2026-10-04)).
2. **Content first, chrome quiet.** Messages get the space and the contrast; toolbars, borders and counts recede.
3. **Keyboard first, mouse complete.** Everything is reachable from the keyboard and the palette, and everything a mouse user expects is visible and clickable.
4. **Calm and fast.** No spinners where cached data exists, no layout jumps, no animations that delay input.
5. **Never steal focus**; prefer banners and toasts to dialogs; ask only when a decision is needed.
6. **Plain files.** Settings live in `config.toml`, `keys.toml` and `accounts.toml`; the palette opens them.

## What we take from the inspiration designs

The six reference designs the user shared agree on a layout and a set of row details; Sealwax keeps those and drops the decoration that would fight the theme.

| Keep | Drop or change |
| --- | --- |
| Three panes: folders → list → reader | Photo avatars → initials tinted from the palette (no remote lookups) |
| Row: avatar, sender, time, bold subject, two-line snippet, star, attachment clip, unread dot | Gradients, glows, drop shadows → flat theme surfaces with 1 px borders |
| Today / Yesterday sections; counts on folders; filter chips (All, Unread, Starred, Attachments) | Marketing chrome (upsell banners, Log out, profile menus) |
| Reader: big subject, sender block with "to me" and relative time, attachment cards with Download All | Separate Reply / Forward button rows that repeat the toolbar |
| Inline quick reply under the conversation; Send with a secondary menu | Schedule send (1.x) |
| Conversation count badge; "1 of 440" position with previous/next | Fixed light or dark look → follows the Omarchy theme, light and dark |

## Layout

The window is often tiled, so the layout adapts to width (AdwBreakpoints), not to screen size.

| Width | Layout |
| --- | --- |
| ≥ 1280 px | Sidebar (≈ 220 px; dragged 160–360 since 2026-10-08) · list (≈ 380 px; dragged 280–640) · reader (rest) |
| 900–1280 px | Rail (icons and counts, ≈ 56 px) · list · reader |
| < 900 px | One pane at a time: folders → list → reader, with back navigation |

- On wide screens (the 3440 px ultrawide here) the reader centres its text column at a readable maximum width (≈ 760 px of body text); headers and toolbars stretch.
- *Changed after approval, 2026-10-08 (the user's UI tweaks: "the box in which the mail is shown is a bit narrow", and "drag to resize columns"):*
  - **The reader's column** fills the reader pane within its 24 px margins up to **1,200 px** (760 before), centred beyond that.
  - **The borders drag** in the wide layout: the one between the sidebar and the list, and the one between the list and the reader. Each has a 6 px handle on the left edge of the pane to its right, so neither the folder list's nor the message list's scrollbar is covered, with the column-resize pointer; the border doubles in the accent while the pointer is on it or drags it. The sidebar takes 160–360 px and the list 280–640 px, neither less than its own content needs (the list's chips: 342 px at the default text size); a drag stops where the reader would get less than 360 px or than its toolbar needs. A double click on a handle puts that pane's default back (220 or 380 px). The rail keeps its 56 px and has no sidebar handle; the list's handle works beside it; one pane at a time has none. There is no keyboard way to resize.
  - **Remembered:** the dragged widths and the folded accounts (Sidebar below) are kept in `$XDG_STATE_HOME/sealwax/layout.toml` ([ADR-009 amendment](DECISIONS.md#adr-009-amendment--the-windows-layout-in-layouttoml-2026-10-08)), read before the first window shows (it waits at most 150 ms, as for the settings) and written as they change.
- No title bar on Hyprland (detected as in Stet: `HYPRLAND_INSTANCE_SIGNATURE` or `XDG_CURRENT_DESKTOP`); window controls appear elsewhere.
- The compositor owns the outer window corners and border.

## Components

**Sidebar**
- Compose button at the top (accent fill, accent text colour, full radius, `c`). While a command doesn't exist yet its button stays readable: an insensitive compose or "Add an account" takes the selected chip's tint and text (M1; kept by the user at the M1 gate, 2026-10-05).
- "All Inboxes", then each account with its role folders (Inbox, Drafts, Sent, Archive, Junk, Trash) and unread counts; other folders under a collapsible "More".
  - *Changed after approval, 2026-10-09 (the user's request: "hide Microsoft's housekeeping folders and the mailspring folders"):* other mail programs' housekeeping folders are **left out**: Outlook's sync logs ("Sync Issues", "Synkroniseringsfeil" and the folders under them), Notes and Outbox on Microsoft's servers, and Mailspring's folder ("Mailspring", at the top or under INBOX, and its "Snoozed") while it holds no message ([ADR-006 amendment](DECISIONS.md#adr-006-amendment--other-mail-programs-housekeeping-folders-left-out-2026-10-09)). They are not synced, searched or offered by Move to….
  - The counts are the server's, in the sidebar, the rail and the list's heading: a folder that keeps only its newest mail (ML-043) adds what STATUS leaves below its window, so `perf`'s Inbox shows 3,333 unread while it holds 200 messages (the user's choice at the M3 gate, 2026-10-06; [m3/counts.md](m3/counts.md)). The list shows the messages held, older ones loading at its end.
- Account status as a small round dot: accent synced, plain syncing, warning offline, error needs sign-in; account headings are upper-cased, letter-spaced captions.
- *Changed after approval, 2026-10-08 (the user's UI tweak: two accounts with the same sender name had the same heading):* the heading names the account by its **label** (`accounts.toml`'s `label`, typed as "Account name" in the setup assistant), else its **address**, never the sender name. An account with a **colour** (`color`) shows the heading's name and its synced dot in that colour, where they were secondary text and the accent; syncing keeps the plain dot, and the warning and error dots keep their colours, so the dot still says the state. The rail's initial for another account takes the colour too. The label also names the account in banners, toasts, Remove Account and the Sign in again title; the composer's From chip shows the sender name it sends with. A right click on an account's heading opens a menu with **Edit Account** for that account (the palette and the main menu have the command).
- *Changed after approval, 2026-10-08 (the user's UI tweak: "possibility to collapse and expand the mail accounts"):* an account's heading **folds its folders away** under it and opens them again: a click, Enter or Space toggles, Left folds and Right opens. The heading ends in an arrow (down while open, right while folded) and, folded, shows the account's Inbox unread count before its status word; the folder shown stays in the list. Headings are rows of the folder list now: Up and Down reach them (the list is still one Tab stop), screen readers hear them expanded or collapsed, and Menu or Shift+F10 opens the heading's menu. The space above a heading is a margin, so its hover is even round its text. Folded accounts are remembered by key across starts; the rail is unchanged.
- *Changed after approval, 2026-10-09 (the user's request for subfolders and folder commands, "Create, rename, delete (Recommended)"; [ADR-020](DECISIONS.md#adr-020--folders-and-rules-sieve-run-by-sealwax-2026-10-09), [m9/folders.md](m9/folders.md)):*
  - **The folder tree:** a folder whose parent the sidebar lists shows under it by its last level ("Acme" under "Kunder"), indented 16 px a level: the Inbox's subfolders under the Inbox among the role folders, a role folder's under it, the rest under "More folders" in name order (case-insensitively). A role folder always shows at the top of its account by its role's name, also where the server keeps it under the Inbox (`INBOX.Sent`); a folder whose parent isn't listed shows its whole name. The list pane's title is the folder's last level.
  - **Folding:** a folder with subfolders ends in an arrow (down while open, right while folded) that folds them away and opens them again; on its row Left folds and Right opens, as on account headings (the list's own keys, no registry command). Folds are remembered in `layout.toml` (`[folded]`, by account key and the folder's whole name) and come back with a new window; picking a folder under a folded one opens it. Screen readers hear each folder row's level (1 at the top) and a parent's expanded or collapsed state.
  - **The folder row's menu** (a right click, Menu or Shift+F10) holds Mark Folder as Read, then **New Subfolder…**, **Rename Folder…** and **Delete Folder…** (not on All Inboxes). An item that doesn't apply is insensitive with the reason as its tooltip: "Special folders keep their names and places" (Rename and Delete of Inbox, Drafts, Sent, Archive, Junk, Trash and All), "It holds a special folder", "This folder can't have subfolders" (`\Noinferiors`). The account heading's menu adds **New Folder…** (at the account's top) after Edit Account. The palette's New Folder, Rename Folder and Delete Folder act on the folder shown (none while All Inboxes shows). No keys (ADR-017).
  - **The name dialog** ("New Folder in Work", "New Subfolder in “Kunder”", "Rename “Kunder”"): a "Folder name" field (the folder's name to change, for Rename), Cancel and Create or Rename (the default, Enter). The name is checked as it is typed, and a line under the field says why one is refused, the field marked invalid: "Type a name for the folder.", "A folder name can't contain “/”: it separates folders on this account." (the account's delimiter), "A folder name can't contain “*” or “%”.", "A folder name can't contain a line break.", "There is a folder with this name here already." (a sibling's, whatever the case), "Sealwax hides folders with this name: other mail programs keep their own things in them." (Sync Issues, Notes and Outbox on Microsoft's servers, Mailspring; IMAP accounts only). Create and Rename can't be chosen while a name is refused or unchanged. Offline (an IMAP account offline, signed out or with a bad certificate) the body says "Work is offline. Its folders can change once it is connected again." and nothing can be sent; a POP3 account's local folders change at once, online or not.
  - **Delete asks first:** "Delete “Kunder” and everything in it?", "The server deletes “Kunder”, its 2 subfolders and the 14 messages in them, for good." (for a POP3 account "Sealwax deletes … from this computer"; "It holds no messages." for an empty one), Cancel the default, Delete in the destructive colour. A folder shown that goes leaves the list at its account's Inbox.
  - **The answer** is a toast: "Created “Reiser”", "Renamed “Kunder” to “Klienter”", "Deleted “Acme”", or "Couldn't create “Fjord”: the server said “[ALREADYEXISTS] Mailbox already exists”" (the server's words, 200 characters at most, never logged), "…: the account is offline", "…: the connection was lost; Sealwax reads the folders again when it reconnects".
- A footer with the accounts' summary and `config.toml`. The main menu sits at the end of the list's bar (the welcome page's while there is no account): in the footer it cut off the summary (M1, a change from the mock; kept by the user at the M1 gate, 2026-10-05).
- **Rail** (900–1279 px): compose, All Inboxes (an addition to the mock, kept by the user at the M1 gate, 2026-10-05), the shown account's role folders with an accent badge at the top-right of those with unread mail, the other accounts as initial and unread count (a dot only when offline or signed out), `config.toml` at the bottom.
- *As built in M4 (ML-052, [report](m4/triage-app.md)):* a right click on a folder, or Menu or Shift+F10 on a focused one, opens a menu with **Mark Folder as Read**, insensitive when nothing there is unread; on All Inboxes it marks every Inbox. The palette's Mark Folder as Read marks the folder the list shows, not while it shows search results.
- *As built in M3 (ML-048, [report](m3/status.md)); kept by the user at the M3 gate, 2026-10-06 ("keep the banners"):* every state the engine reports has a dot and words. Accent "synced", plain "syncing…", warning "offline" and "mailbox full" (Sealwax keeps trying, or still syncs), error "needs sign-in" and "bad certificate" (Sealwax stopped until the user acts). The rail marks every state but synced and syncing on the other accounts, and its tooltip and accessible label say the state in words. The footer's summary names the most important state in the banner's order.

**Message list**
- Search field and filter chips on top, then sections (Today, Yesterday, This week, This month, Older).
- Row anatomy: unread dot · avatar (32 px) · sender (bold when unread) and time · subject · one- or two-line snippet · indicators (attachment, star, thread count).
- Selection mode with checkboxes for batch actions; hover shows quick actions (archive, delete, star).
- Rules from S3 (2026-10-04, [report](spikes/S3-store.md)) — GtkListView keeps about 205 row widgets realized whatever the window size, so one row's layout cost sets the cost of every jump, first paint and fast drag:
  - no ligatures in list text (`font-feature-settings: "liga" 0, "calt" 0`): 30–36 % less layout with JetBrains Mono;
  - cut text to what fits before setting it (−15 %);
  - placeholder rows while the list moves faster than about 30,000 px/s (a scrollbar drag through 100k rows then stays at p99 7.94 ms instead of 48–57 ms);
  - GTK's CSS `line-height` multiplies the font's own line height, so the mock's 1.45 is about 1.1 in GTK;
  - when new mail arrives while the first row is visible, keep the list pinned to the top; otherwise GTK keeps the old row anchored and the new mail lands above the viewport;
  - keep GTK's default Vulkan renderer (GL costs about 95 MiB more on this NVIDIA machine).
- As built in M1 ([report](m1/list.md)):
  - **The row** is S3's: unread dot · avatar · sender (bold when unread) · thread count · time / subject / two-line snippet / star and attachment.
    - The time shows "10:32" for today and yesterday, the weekday this week, "4 Oct" this year, and "04.10.2025" before that.
    - Each row's accessible label says its state in words.
  - **Sections** start at local midnight; "This week" starts on Monday. The list regroups at midnight and checks the day at least every 10 minutes (after a suspend).
  - **Rows not loaded yet** show the row's shape without text, as does every row while the list moves faster than about 30,000 px/s.
  - **After a folder, chip or search change** the list starts at the top, with 40 rows in the first frame and 40 more each frame. A reload of the same view keeps the place, the selection and the focus.
  - **Search** replaces the list's title with "Search" and a count ("12 results", "50+ results", "No results"). The chips filter the results, and Escape ends the search. In a folder it searches that folder; in All Inboxes it searches every folder.
  - **Keys:** `j`/`k` and Down/Up move the selection, Enter/`o` open it, and `/` or Ctrl+F go to the search field.
- *As built in M4 (ML-052, ML-054, [report](m4/triage-app.md)); accepted by the user at the M4 gate (2026-10-06):*
  - **Batch selection:** Ctrl+click toggles a row, Shift+click selects from the row last clicked; Shift with Down/Up or with `j`/`k` extends from the keyboard. Two messages or more, or Select (`x`, Space), put the list in a **select mode**: each row's avatar becomes a checkbox of the same size (so the text doesn't move), `j`/`k` and Down/Up move a cursor without changing the selection, `x`/Space toggle the row under it, a click on a checkbox toggles that row, and a plain click or Escape leaves with one message selected (Escape keeps the one under the cursor); Escape anywhere in the window leaves it once text fields and dialogs have had it (phase B: a batch-page button that went insensitive under the focus left Escape nowhere). GtkListView's own Ctrl+A selects nothing.
  - **Every action applies to every selected message.** Star stars them all unless all are starred, then unstars them. Archive, Delete, Junk and Move to… apply to the messages they can; the toast counts any left as they were.
  - **After the selected messages leave the list** (archive, delete, junk, move, or a filter they no longer pass) the message that took the first one's place is selected, or the last when they were at the end, as Gmail does.
  - **Quick actions:** hovering a row shows archive, delete and star in the time's place, acting on that row alone; not in the select mode.
  - **A message the reader shows is marked read** (no toast, not undoable from the toast); in the Unread view once the selection leaves it, and in one pane at a time once the reader is on screen. Behind `triage::MARK_READ_ON_OPEN`, on in this build.

- *As built in M7 ([search-ui](m7/search-ui.md), [search](m7/search.md)); accepted at the M7 gate (the user's delegation, 2026-10-07):*
  - **The field** has the mock's placeholder, "Search — from: has:attachment is:unread", and the Search key (`/`) at its end while it is empty.
  - **Suggestions** open under the field as it is typed: the operator a word of two letters or more starts, with its description and example; the values of `has:` and `is:`; people after `from:` and `to:` from the contacts (the folder's account, or every account), inserted as their address; days after `before:` and `after:` ("Today", "The last 7 days", "The last month", "The last year"; "Before today", "Older than a week", …). Down with them closed lists every operator. Down and Up choose, Tab takes the chosen row or the first, Enter takes a chosen row and otherwise goes to the results, Escape closes them and then ends the search, a click takes a row; the field keeps the focus.
  - **Results follow every key** (no search delay; the last word typed is the start of a word until a space follows it). At 100,000 messages a key → results painted in p95 64.7–66.8 ms (release, headless).
  - **Scope:** from a folder, a row under the chips, "Search in [folder] [All folders]", widens the search to every folder of every account while it lasts ("5 results in all folders"); All Inboxes searches every folder. Every folder leaves out Trash, Junk and detached accounts; a search inside Trash or Junk still finds their mail.
  - **The query language:** `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `is:read`, `is:starred`, `before:` and `after:` (local days: `2026-10-01`, `2026/10/01`, `1.10.2026`, `2026-10`, `2026`), quoted values, `-word` and `-has:`/`-is:`/date negations; æ and ø are found whether typed or not (`blabaersyltetoy`); attachment names are searched as words.
  - **Rebuild Search Index** (the palette, no key): "Rebuilding the search index…" while it runs, then "Search index rebuilt: 100,000 messages in 19.3 s"; search keeps working meanwhile.

**Reader**
- *Decided at M5's start (user, 2026-10-06, [ADR-016](DECISIONS.md#adr-016-amendment--the-conversation-view-is-the-readers-m5-start-2026-10-06)):* the list keeps one row per message; selecting one shows its whole conversation here, as the mock's reader does.
- Toolbar: archive, delete, junk, move, mark unread, star, more; position "n of N" with previous and next.
- Subject as a heading; per message: avatar, name and address, "to me" or recipients, date with relative time; collapsed older messages as one-line rows.
- Body in a WebView (HTML) or a text view (plain text); attachment cards (type icon, name, size; Open, Save, Download All); remote-content banner ("Images are hidden — Show / Always for this sender").
- Quick reply under the conversation, expanding into the full composer.
- *As built in M1 (ML-027, [report](m1/reader.md)):*
  - **Toolbar:** archive, delete, junk, move | mark unread, star (insensitive until M4), then "n of N" with previous and next; tooltips name each command's keys.
  - **Showing a message:** it appears once its body is laid out at its height, replacing the previous one at the top of the column.
  - **Details:** the toggle shows From, Reply-To, To, Cc, the full date and the list.
  - **Not built yet:** "Always for this sender" is shown insensitive until M5; attachment cards show type, name and size only.
  - **Keys:** they work while a message has the focus. WebKitGTK passes back the keys a message doesn't use, and a text field inside a message keeps its letters.
- *As built in M4 (ML-052, ML-054, [report](m4/triage-app.md)):*
  - **Toolbar:** each command is sensitive where it applies to the selected messages, and its tooltip names its keys and, when it doesn't apply, why ("Archive (e, y) — already archived", "the account has no Junk folder", "the messages are in more than one account"). In a Junk folder Move to Junk is **Not Junk**, back to the Inbox (title, tooltip, icon and the palette's entry); Star or Unstar is "Unstar" when every selected message is starred.
  - **Several messages selected:** the reader shows a quiet page, "3 messages selected", "Every action here applies to all of them. Escape leaves the select mode.", with Archive, Delete, Junk, Move to…, Star, Mark as Read and Mark as Unread as buttons bound to the same commands. The reader's keys work there too.
  - **Keys:** WebKitGTK and the plain-text view keep Delete while a body has the focus, so the reader takes Delete before the body does; a text field inside an HTML message loses Delete.
  - **Move to…** (`v`, Ctrl+Shift+V): a picker over the window like the command palette, "Move 3 messages to…", listing the account's folders that take messages except the one they are in, role folders first under their role's name (the server's own name beside it). Typing filters, Up/Down move, Enter moves, Escape closes. Insensitive, saying why, when the selection spans accounts (1.0 doesn't move between accounts).
  - *Changed after approval, 2026-10-09 (folders, [m9/folders.md](m9/folders.md)):* a subfolder shows by its last level first, the folders above it beside it ("Acme" · "Kunder / 2026", a role folder above by its role's name: "Kvitteringer" · "Inbox"); a screen reader hears "Move to Acme, Kunder / 2026". Typing matches both.

- *As built in M5 ([conversation](m5/conversation.md), [files](m5/files.md), [rendering](m5/rendering.md)); the defaults and texts accepted at the M5 gate ("Accept all", 2026-10-06):*
  - **The conversation** (ML-057): the selected message's subject as the heading; every message oldest first; the selected one, the unread ones and the latest as cards, the others as one-line rows (initials, sender, the snippet's first line, a short time; unread ones dotted and bold). A click, Enter or Space on a row expands it; on a card's sender block collapses it, except the only expanded one. Expand All and Collapse All (More menu, palette) keep folds folded and the selected message expanded. Over 100 messages the first, the latest 10 and the expanded ones show; the rest fold into "N earlier messages", which reveals 50 a click. The column opens at the selected message once the bodies of its first screen are laid out; changes above what the column shows don't move it. A message arriving while the conversation is shown joins as an unread row.
  - **Toolbar:** Print (Ctrl+P, the mock's) and the More menu: Show Original, Save as .eml…, Expand and Collapse All Messages, Show Images, Always Show Images from This Sender. Reply, Reply All, Forward and the quick reply are M6's; the mock's Unsubscribe is 1.x.
  - **Remote content** (ML-058): each card has its own banner; "Always for this sender" works, and a listed sender's message shows a quiet line instead, "Images from this sender are always shown." with Hide (which forgets the sender). Always or Hide changes every card of that sender at once. The banner keeps M1's text; the mock's count ("7 images are hidden…") isn't built.
  - **Attachment cards** (ML-061): type badge, the name it is saved under, the size, Open and Save ("84 KB · Open · Save"); "N attachments" with a Download all chip. A program, launcher, script or installer, or a name that hides its type, has a warning sign in the badge, a warning and Save only. A large message's attachments show "about" sizes and download on demand ("Downloading…" with Cancel; a failure with Retry).
  - **Print** opens the print dialog on the message with a header (subject, From, To, Cc, Date); **Show Original** is a dialog with the source in the Omarchy font (the first 512 KiB) and Save as .eml…; **Save as .eml** suggests the subject as the name (ML-063).
  - **Links** (ML-062): the link under the pointer or the focus shows its target in a strip at the reader's bottom-left; a link whose text names another domain shows the mock's warning ("The link text and its target differ" / "Text says …" / "Opens …") and asks before opening ("Open this link?", Cancel the default).
  - **Encrypted and signed mail** (ML-064): "This message is encrypted. Sealwax can't read encrypted mail yet." in place of the body; signed mail shows "Signed · not checked".
- *As built in M6 ([reply](m6/reply.md)); the rules and texts accepted at the M6 gate ("Accept all", 2026-10-06):*
  - **A Drafts-folder message** opens in the composer with Enter, `o` or a double click (the user's choice at the M6 gate, 2026-10-06); a click selects it and the reader shows it, with Edit draft on its card.
  - **Reply, Reply all and Forward** end each expanded card's header (icon buttons, tooltips with the keys), acting on that card's message; a draft's card has Edit draft instead. The commands answer the message the user is on in the reader (the focused card, else the one `n`/`p` reached, else the selected one); they don't apply to several messages or to a draft. Compose writes from the account the list shows (in All Inboxes the selected message's, else the first).
  - **The quick reply** under the conversation answers the latest message that isn't the user's own: "Reply to <name>", a two-row plain-text area ("Write a reply…"), "Expand to the full composer with Ctrl+Shift+E" (a button) and Send (Ctrl+Enter; insensitive while empty); the box takes the accent border while focused. The reader's keys stop at its edge (Open Reply in Composer passes); Escape returns to the message it answers. Its text is saved as a draft 2 s after the typing rests and at most 5 s behind it, and comes back in the box after a restart; Send plans the reply with the quote and queues it like the composer's Send.

**Composer**
- *Changed after approval, 2026-10-08 (the user's UI tweak, with a screenshot of Gmail's composer: "open inline … not in a new tile"; [ADR-014 amendment](DECISIONS.md#adr-014-amendment--the-composer-docked-in-the-main-window-2026-10-08)):* the composer is a **card docked over the panes** at the main window's bottom right, no longer a window of its own; its content is unchanged.
  - **The card:** 640 × 600 px (shorter in a short window: it leaves 48 px above it), 16 px from the window's right edge and resting on its bottom; a border on the window's surface, the surfaces' radius on its top corners, no shadow. The header ends in **Minimize**, **Expand** and **Close** ("Save and close"), after Attach and Discard and a separator.
  - **Minimized:** the header alone, 300 px wide, with Restore, Expand and Close; it shows the subject once there is one (the request's title before), as the window's title did. A click on the header opens it.
  - **Expanded:** up to 1,000 × 820 px in the middle of the panes, 32 px from their edges, over a backdrop dimmed as under libadwaita's dialogs; the other cards hide meanwhile. Shrink, or a click on the backdrop, puts it back.
  - **Several drafts:** each newer card opens to the left of the older ones, 12 px apart; when they no longer fit, the oldest open ones minimize. While one pane fills the window (under 900 px) an open card fills the panes above the minimized headers, and opening one minimizes the other.
  - **Focus:** a card the user opens takes the focus (To while it is empty, else the body); drafts reopened after a crash open without taking it. Minimizing, closing or sending gives the focus back to where it was before the card took it, else to the message list. The window's accelerators work from a card (Ctrl+N opens another card; the palette lists and runs the card's commands too); Escape in a card stays there.
  - **Without a main window** (a `mailto:` link from the browser, `sealwax --compose`, a notification's Open in background mode) the main window shows with the card in it. Closing the main window saves each card's draft and closes it; the drafts stay.
- *Decided at M6's start (user, 2026-10-06, [ADR-014](DECISIONS.md#adr-014-amendment--the-composer-in-its-own-window-m6-start-2026-10-06)):* the full composer is a window of its own for each draft, as the mock's "New message" frame: its header has the From chip, Attach and Discard; Hyprland tiles it beside the main window. The quick reply stays under the conversation in the reader and expands into a composer window (the mock's Ctrl+Shift+E).
- From (identity), To / Cc / Bcc as chips with autocomplete, Subject.
- Formatting toolbar (bold, italic, underline, strike, lists, link, quote, indent, clear formatting) driven by the editor state.
- Body, attachments tray (drag and drop), Send (Ctrl+Enter) with "Undo" in the toast that follows.
- *As built in M6 ([report](m6/composer.md)); the defaults and texts accepted at the M6 gate ("Accept all", 2026-10-06):* the header (title, the From chip with the account's address and its identities, Attach, Discard), To with "Cc · Bcc", Subject, the toolbar (a paragraph-style menu: Paragraph, Heading 1–3, Preformatted; B I U S, lists, link, quote, indent, outdent, clear formatting, Undo), the body, and the footer (attachment cards, "Draft saved HH:MM", Send with Ctrl+Enter); a size warning on its own line over the footer, and Send refused with the reason when the message is larger than the account's server takes. Recipient chips take the address's tint; an invalid address is an error chip; suggestions (people the user wrote to first, "N messages") in a 380 px popover. Discard asks only when there is something to lose; a new draft closed before anything was typed is discarded. Replies and forwards show "Getting the message…" while the original downloads (30 s at most). The signature, after a `-- ` line, follows the sender.
- *M9 (the user's "HTML size cap", 2026-10-07; [SECURITY.md](SECURITY.md#hardening-m9)):* a reply or forward to a message with more than 256 KiB of HTML quotes its text (its plain part, else the text of its HTML's first 256 KiB) in the usual cite quote or forwarded block, without the original's inline images; a forward carries them as attachments. Smaller messages are quoted as before.
- *M9 ([depth limit](m9/depth.md)):* a quote, or a draft read back from the Drafts folder, keeps at most WebKit's 512 levels of nesting: deeper start tags are left out and their content kept, as WebKit's composer would show it. The text part of a message nested deeper than that comes from the simpler converter.

*Changed after approval, 2026-10-04 (user decision): Sign in with Google is deferred to 1.x. The mock's setup assistant shows Microsoft sign-in for an Outlook.com address, and Gmail's failure state offers only the app-password route.*

**Setup assistant**
- *Changed after approval, 2026-10-08 (the user's UI tweaks):*
  - **Account name and colour:** the first step asks under "Your name" for **"Account name"** (its placeholder the address typed, "Shown in the sidebar" before one is; left empty, or the address typed, the account has no label and the address names it) and **"Colour"**: a row of round swatches, the eight account colours round the wheel and "No colour" (a dashed ring), with the chosen one's name after them. The chosen swatch has a ring in the text colour; it is the row's one Tab stop, Left and Right (Up, Down, Home, End) move the choice and the focus, a click or Space chooses, Enter goes to the next step. A new account is offered the first colour no other account has. The manual settings show "Your name" but not these two: every way there passes the first step, which keeps them, and they would push the manual step's message out of view. Screen readers hear a radio group of radios ("Teal", checked). No key was added.
  - **The scrollbar:** the column scrolls under the sheet's right padding, so its overlay scrollbar sits in that gutter, 6 px from the edge and clear of the fields; it covered the fields' right edge before.
- One column: address → "Looking up your provider…" with the sources it checks → password (an app password where the provider needs one, with a link to its page), or "Sign in with Microsoft" → checking login → done.
- "Set up manually" expands host, port, security and username fields. Errors say what to do next (for example: "iCloud needs an app password — open the Apple account page"; the mock's example was Gmail's, which is no longer a listed provider since 2026-10-07).
- *As built in M8 ([pop3-setup](m8/pop3-setup.md)), for the M8 gate:* the manual step starts with **"Incoming mail"** (IMAP, POP3; IMAP whenever the step opens). POP3 calls the incoming server "POP3 server", moves a standard port (995 with TLS, 110 with STARTTLS; a typed port stays) and shows a note: "Sealwax downloads new mail to folders on this computer and leaves it on the server. To delete it there after some days, set delete_after_days in accounts.toml." Failures name "The incoming mail server (POP3)"; "POP3 is turned off for this account"; "This POP3 server can't keep track of messages" (no UIDL: use IMAP if the provider offers it); "The mailbox is busy" (another mail app, or a pause between sign-ins). A POP3 account is saved leaving mail on the server, never deleting it there. Its six local folders (Inbox, Sent, Drafts, Junk, Trash, Archive) never show "Loading older messages…". Remove Account warns that a POP3 account's sent mail, drafts and mail the server no longer has are gone for good. Refresh (F5, Ctrl+Alt+R) checks every account for new mail now.

*As built in M2 (ML-033, [report](m2/setup.md)). Approved with M2 on 2026-10-05 ("I approve M2"); the differences from the mock weren't answered one by one, so they stay as built:*
- One column in a dialog over the window (the mock's card, 428 × 740): address and sender name → "Looking up your provider…" with the sources as the engine starts them → the found settings and the password (an app password, with the provider's page, where one is needed; the username to confirm when only the domain's mail host gave the settings, or to type when the provider assigns it), or "Sign in with Microsoft" → checking login → done, with a toast. Microsoft sign-in waits for S5 and is shown unavailable.
- "Set up manually" opens host, port, security and username for IMAP and SMTP, filled from detection when there was one; host names and ports are checked before Check settings. A login failure shows why, what to do next and the settings tried.
- Keys: Enter advances, Escape goes back a step and closes on the first; each step puts the focus in its first field that needs typing. Add Account is in the palette and the main menu, without a default key.
- **Edit Account** (the user's UI tweak, 2026-10-08): an alert dialog, "Edit <account>" (with several accounts "Edit an account" and an **Account** choice, "Work — kari@example.test", starting at the account the list shows, or at the heading right-clicked), the body "Sealwax shows the account by this name and colour in the sidebar. The name people see on your mail stays as it is.", **Account name** (the address as its placeholder; empty, or the address typed, takes the label out) and **Colour** (the assistant's swatches), with Cancel and Save (suggested, the default; Enter in the name or on a colour saves). Save with nothing changed sends nothing; otherwise the engine writes the account's `label` and `color` lines and a toast says "Saved <account>" or "Couldn't save <account>: <why>". In the palette and the main menu, between Add Account and Remove Account, while `accounts.toml` lists an account; no default key.
- **Remove Account** (ML-038, built by the lead): a confirmation that names the account and what goes (its local mail, its keyring password and its `accounts.toml` entry; the mail on the server stays), with a choice when there are several accounts; a toast reports the result. In the palette and the main menu while `accounts.toml` lists an account; no default key. The mock has no such dialog.
- The `sw-setup-*` classes are listed in [m2/setup.md](m2/setup.md#classes-appsrcstylecss).

*Microsoft sign-in as built (ML-032, 2026-10-05, [report](m2/oauth-app.md)); the user kept every difference from the mock ("Keep them as built", 2026-10-05):*
- "Sign in with Microsoft" opens the page in the default browser and shows the mock's "Waiting for the browser…" box with Cancel, plus **"Open the sign-in page again"** (the mock has only Cancel; S5's first attempt stopped because the tab was closed). While the browser works, the card's button and the address are insensitive and Back is hidden; Cancel and Escape go back.
- Without a client ID the M2 note stays, with a line on setting `client_id` in `config.toml`.
- Failures show on the step as the M2 alert, and its button signs in again (the mock draws no Microsoft failure).
- **Sign in again:** a banner strip across the window's top, "<account> needs you to sign in again", with "Sign in". Several accounts: one banner naming the first and counting the rest. It opens the assistant captioned "SIGN IN AGAIN", with the account's name as title and the address read only. A toast follows ("<name> is signed in again"). The mock draws "Account needs attention" cards without placing them, and says "Mail keeps everything" where the build says "Sealwax keeps your mail".

**Feedback**
- Toasts for Undo (archive, delete, move, send) and transient results.
- Banners for persistent states: offline, needs sign-in, certificate changed, quota full, no keyring.
- Empty states: no accounts (starts the assistant), empty folder, no search results.

*As built in M3 (ML-048, [report](m3/status.md)); the order, the tints, the texts and the ten differences from the mock were kept by the user at the M3 gate, 2026-10-06 ("keep the banners"):*
- **One banner** across the window's top (ML-032's strip, generalised) shows the most important state and counts the rest: "1 more account needs it too" when the others are the same kind, else "2 more problems wait after this one". Resolving or dismissing the one shown brings the next.
- **Order:** no keyring; each account whose certificate no longer verifies; each that needs a sign-in; each whose mailbox is full (accounts in the sidebar's order); offline.
- **No keyring** (the card's tint, the lock in the accent, as the mock): "No keyring found", passwords and sign-ins stay in memory until Sealwax quits; Dismiss hides it for the session.
- **Certificate** (the error tint; the mock tints it like a warning): "Sealwax stopped connecting to <account>", what the certificate did and what to do; Details (why, and the account's servers) and Retry (reconnect). There is no way to trust the certificate: pinning is 1.x.
- **Needs sign-in** (the error tint): Microsoft accounts keep ML-032's banner and Sign in; password accounts get "Type the password again", which opens the setup assistant at the password with the address read only ("SIGN IN AGAIN"). The engine checks the password on IMAP and SMTP before it keeps it; a refusal shows why on the step, and success closes it with a toast ("Password accepted for <account>").
- **Mailbox full** (the warning tint): what bounces and what to do; no button.
- **Offline** (the warning tint, as the mock): "You're offline" while the desktop has no network; "Can't reach your mail server(s)" when every account's server is out of reach with the network up. A single unreachable account shows only in the sidebar.
  *As built in M4 phase B ([report](m4/triage-app.md#phase-b-the-stores-op-queue)):* with changes waiting for a server the strip ends in the mock's quiet count, "3 waiting" (the ops the accounts with a server haven't run, read again a moment after each action, Undo and change event, and every 3 s while any wait), and its text says Sealwax keeps the changes and sends them when the network is back.
- **Reconnecting:** when the network comes back and after a resume (logind's `PrepareForSleep(false)`), the app sends `Reconnect`; Retry does too, with a toast ("Connecting again…").

*As built in M4 (ML-053, [report](m4/triage-app.md)); accepted by the user at the M4 gate (2026-10-06):*
- **The Undo toast** follows an action that moves messages (archive, delete, junk, move) or changes several (star, read, unread, Mark Folder as Read): "Archived 3 messages", "Moved 2 messages to the Trash", "Deleted 1 message for good", "Moved 1 message to the Inbox as not junk", "Starred 3 messages", "Marked 12 messages in Inbox as read", with **Undo**. It shows for the undo window, **7 s** (ADR-015's 5–10 s). A new action replaces it, since only the last action is undoable from the toast; `z` and Ctrl+Z undo the last action while it can be undone, also after the toast is gone. After an Undo: "Undone".
- **A single message's read or star change** has no toast and doesn't replace the last action: it shows in place, and its key changes it back.
- **Deleting for good** (Delete in the Trash, or where an account has no Trash) waits for the undo window like a move. Undo asks the store, which puts back whatever hasn't reached the server and says what can't come back ("1 message was deleted for good and can't be brought back"). A local account (no server: fixtures) deletes at once, so its toast offers no Undo. *(Phase B; in phase A Undo was refused once the window was over.)*
- **What an action left alone** is named in its toast: "Archived 2 messages. 1 message was already archived", "… left where it was: no Junk folder", "… already gone"; and what Undo couldn't bring back: "… couldn't be found to bring back", "40 messages further back on the server stay read" (a windowed folder marked read). *(Phase B.)*
- **Mark Folder as Read** waits for the undo window too, since the server's unread below a windowed folder's floor can't be put back once it has run. *(Phase B, the op store's proposal 1.)*
- **A change the server refused** (`Event::OpsRefused`) shows a toast for 10 s that says what didn't happen and that the messages are back: "The server didn't move 3 messages; they're back where they were", with the account's name first when there are several.

*As built in M6 ([reply](m6/reply.md), [composer](m6/composer.md)); the texts and the banner's place accepted at the M6 gate ("Accept all", 2026-10-06):*
- **Sending:** Send closes the composer and the main window shows "Message queued" with **Undo** for the undo window (7 s, triage's); the send becomes the last action, so `z` and Ctrl+Z undo it too. Undo puts the draft back in a composer (a docked card since 2026-10-08), or the text back in the quick reply it came from; once the send has started: "Too late to undo: the message is being sent". Then "Message sent".
- **A send that failed for good** shows a toast for 10 s that says why ("Your message wasn't sent: the server refused 1 recipient. It's back in your drafts.") with **Open**; never the recipients or the subject.
- *M9 ([depth limit](m9/depth.md); the text a proposal for the user's gate):* a send whose message was being built when Sealwax stopped comes back the same way at the next start, "Your message wasn't sent: Sealwax stopped while preparing it. It's back in your drafts.", instead of being built again; it is never sent without the user sending it again.
- **A send that may have gone** (Sealwax stopped, or the connection broke, while its data went out) puts up the window's banner, "A message may have been sent", after the sign-ins and before the full mailboxes, with **Choose…**: "Was the message sent?" with Send Again (shown as the risky choice), It Was Sent, Back to Drafts and Later (the default).
- **The offline banner's count** ("3 waiting") includes sends waiting for a server.

*As built in M7 ([notifications](m7/notifications.md), [background](m7/background.md)); accepted at the M7 gate (the user's delegation, 2026-10-07):*
- **New-mail notifications** (desktop notifications, normal urgency, the app's icon): for unread mail in an Inbox, never while the focused window's list shows that folder or All Inboxes. Title the sender's name (else the address, else "Unknown sender"), body the subject (else "(no subject)"), at most 80 and 200 characters. With `[notifications] show = "count"`: "New mail" and "1 new message" / "N new messages". Up to three of their own within 30 s, then one "N new messages" naming up to three senders ("and N others"), which replaces itself. A click opens the message in the main window (a new one in background mode); "Archive" and "Mark Read" where the server shows buttons (omarchy-shell shows none). A notification goes when its message is read, moved or deleted anywhere; a message gone when clicked: "The message was moved or deleted".
- **Background mode's notice** (once per user, when the last window closes into background mode): "Sealwax keeps running" — "Mail keeps syncing and new mail is still notified while the window is closed. To quit, open Sealwax and press Ctrl+Q. To quit whenever the window closes, set enabled = false under [background] in config.toml." A click shows the main window.
- **Start Sealwax at Login** (the Settings menu's check item): "Start Sealwax at login?" — what it writes (`~/.config/autostart/io.github.pixdevsapps.Sealwax.desktop`, running `sealwax --background`) and how to undo it, with Cancel (the default) and Start at Login. Toasts: "Sealwax will start at login", "Sealwax won't start at login", "Sealwax already starts at login".

## Theme tokens

All colours come from the resolved palette (`omarchy-theme-color --all`), mapped once in the theme layer. Widgets never reference palette keys directly. The UI roles are Stet's ([its DESIGN.md](../../notes/docs/DESIGN.md)); the mail roles are new.

| Role | Applied through | Omarchy key |
| --- | --- | --- |
| Window, reader background | `--window-bg-color`, `--view-bg-color` | `background` |
| Text | `--window-fg-color`, `--view-fg-color` and the other `*-fg-color` variables | `foreground` |
| Sidebar, list | `--sidebar-bg-color`, `--headerbar-bg-color` | `dark_background` |
| Popovers, palette, dialogs | `--popover-bg-color`, `--dialog-bg-color` | `darker_background` |
| Cards, hovered rows, attachment cards | `--card-bg-color` | `lighter_background` |
| Borders, separators | `--border-color` | `lighter_background` mixed 8 % (dark) or 14 % (light) toward `foreground`, as the approved mock (M1) |
| Secondary text (snippets, times, addresses) | mail role | `foreground` mixed 35 % toward `background`, less where needed to keep 4.5:1 against the window and list backgrounds (M1). On a hovered or selected list row, mixed less again until it also keeps 4.5:1 on the row's fill, omarchy-shell's hover or selected fill over the list (`--sw-hover-secondary-fg-color`, `--sw-selected-secondary-fg-color`; 2026-10-04: plain secondary text was 3.6:1 on matte-black's selected row) |
| Accent: focus ring, compose button, unread dot, selected chip | `--accent-bg-color`, `--accent-color` | `accent` |
| Text on accent | `--accent-fg-color` | `readable_on(accent)` |
| Star | mail role | `accent` (several themes' `yellow` isn't yellow — matte-black's is red) |
| Error / warning / success | `--error-*`, `--warning-*`, `--success-*` | `red` / `yellow` / `green`, kept when every two are at least 20° of hue apart, else turned (matte-black's red and yellow share a hue) |
| Selected chip | mail role | accent text on the background mixed 18 % (dark) or 14 % (light) toward the accent; the more readable of accent and foreground where the accent stays under 4.5:1 (M1) |
| Account colours | mail role (`--sw-account-<name>-color`) | none: the user's choice of eight fixed OKLCH hues (rose 15°, clay 55°, olive 95°, sage 145°, teal 195°, slate 245°, iris 290°, mauve 335°) at lightness 0.74 and chroma 0.065 on dark themes, 0.52 and 0.075 on light ones, the chroma lowered where sRGB can't show it and the lightness moved away from the background until the colour reads at 4.5:1 on the window and the sidebar (2026-10-08, [ADR-004 amendment](DECISIONS.md#adr-004-amendment--account-colours-2026-10-08)) |
| Avatar tints | mail role | seven hues rotated from the `accent` hue in 51° steps at a fixed saturation and a mode-dependent lightness (the mock's approach; palette hues are often too alike, as in matte-black), picked by a hash of the address; initials take `readable_on(tint, #ffffff, #1b1b1f)`, at least 4.5:1 |
| Light or dark | AdwStyleManager FORCE_DARK / FORCE_LIGHT | `mode` |
| Corners of rows, chips, cards, buttons | `--window-radius` and CSS | `corner_radius` ([ADR-019](DECISIONS.md#adr-019--corner-radius-follows-hyprland-2026-10-04)); chips, folder rows, icon buttons and controls take three quarters of it; avatars are round while corners are rounded |

The generated CSS variables and the `sw-` classes are listed in [m1/theme.md](m1/theme.md#css-classes-appsrcstylecss) and, for the shell, [m1/polish.md](m1/polish.md); dots and badges stay round at any radius.

Roles map to palette positions, never hue names: matte-black's `green` is amber and its `blue` is orange. Contrast is checked with `readable_on`, and Stet's `distinct` rotates hues when a theme can't tell its colours apart.

**Control states** follow omarchy-shell's `shell.toml`: normal = foreground at 4 % with a 1 px border at 40 %; hover and focus 8 %; selected 18 %; pressed 22 %; text selection 35 %.

**Spacing** follows omarchy-shell's scale: 2, 3, 4, 6, 8, 10, 12, 14, 18 px. Controls are 28 px tall with 10 × 6 px padding; rows breathe at 10–12 px vertical padding.

**Surfaces** are flat: no drop shadows; elevation comes from the background roles and 1 px borders.

## Typography

- **Chrome** (sidebar, list, toolbars, chips): the Omarchy font, `fc-match monospace` (JetBrainsMono Nerd Font here), re-resolved when `~/.config/fontconfig/` changes, as in Stet and omarchy-shell.
- **Scale** follows omarchy-shell (base 12 px, or `[font] base-size` from `~/.config/omarchy/shell.toml`): caption 10, small 11, body 12, subtitle 13, title 14, heading 16, display 24.
- **Message text**: sans-serif (`fc-match sans-serif`, Liberation Sans here) at a comfortable reading size: 15 px at base 12, scaled with the base size, line height 1.55; code in messages at 13/15 of it. WebKit's default fonts are set explicitly, because it would otherwise pick its own.
- `config.toml` can switch the chrome to sans or the reading font to mono. The mock decides the exact sizes.

## HTML mail inside the theme

- **Plain text and simple HTML** (no colours of their own) take the theme: background, foreground, accent links, muted quotes.
- **Styled HTML** (newsletters, receipts) keeps its design on a "paper" card: a light surface with the theme's corner radius and a 1 px border, so dark themes never invert someone else's layout. S2 confirmed this on the live matte-black palette, dark newsletters included; the card's `lighter_background` border (#1e1e1e) is nearly invisible against #121212, so dark themes may drop it.
- Quotes are collapsed behind "…" when they repeat earlier messages in the conversation.
- Nothing remote loads until allowed ([SECURITY.md](SECURITY.md)); hidden images keep the box their HTML declares (WebKit outlines it and shows the alt text), so the page doesn't jump when shown.
- Themed bodies paint their root in the theme's background: with a transparent root and `color-scheme: dark`, WebKit would paint its own dark canvas. Plain-text bodies set `line-height: 1.35` in GTK, about the 1.55 of message text in a browser (M1).

*As built in M5 (ML-059, [report](m5/rendering.md); checked on 69 open-licensed real-world templates in a dark and a light theme):*
- A message with dark-mode rules (`@media (prefers-color-scheme: dark)`) shows its own dark design under a dark theme; its body fills the paper card, which paints no surface of its own under a message's background. The card keeps its 1 px border (accepted at the M5 gate).
- Themed bodies always give links the accent; colours set only for links (Outlook's replies) don't make a body paper.
- Quoted history collapses behind a "…" toggle, in HTML and plain text, for a message with an earlier message in the shown conversation.

## Avatars

Initials from the display name (or the address), on a tint chosen by hashing the address. Never fetched from Gravatar, BIMI or favicons (privacy).

## Motion

Short fades (≤ 150 ms) for toasts and banners; no sliding lists, no bounce. Animations follow `gtk-enable-animations`.

## Keyboard

The keymap ([ADR-017](DECISIONS.md#adr-017--keymap-scoped-one-key-shortcuts-under-omarchy-rules-2026-10-04)). One-key shortcuts work only while the list or reader has focus; `keys.toml` rebinds registry commands.

| Action | Keys |
| --- | --- |
| Next / previous message | `j` / `k`, Down / Up |
| Open / back to list | Enter, `o` / Escape, `u` |
| Compose | `c`, Ctrl+N |
| Reply / reply all / forward | `r`, Ctrl+R / `a`, Ctrl+Shift+R / `f`, Ctrl+L |
| Archive | `e`, `y` |
| Delete | `#`, Delete |
| Junk | `!`, Ctrl+J |
| Move to… | `v`, Ctrl+Shift+V |
| Star | `s` |
| Mark read / unread | Shift+I / Shift+U |
| Select | `x`, Space |
| Extend the selection (the list's own) | Shift+`j` / Shift+`k`, Shift+Down / Shift+Up |
| Leave the select mode (the list's own) | Escape |
| Mark folder as read | none (the palette, a folder's menu) |
| New Folder, Rename Folder, Delete Folder (2026-10-09) | none (the palette, a folder's menu, an account heading's menu) |
| Fold / open a folder's subfolders (the sidebar's own, 2026-10-09) | Left / Right on the folder |
| A sidebar folder's menu (the sidebar's own) | Menu, Shift+F10 |
| Search | `/`, Ctrl+F |
| Go to Inbox / Starred / Sent / Drafts | `g` then `i` / `s` / `t` / `d` |
| Send | Ctrl+Enter |
| Undo | `z`, Ctrl+Z |
| Command palette | F1, Ctrl+Shift+P |
| Keyboard shortcuts | `?`, `h`, Ctrl+? |
| Preferences / quit | Ctrl+, / Ctrl+Q |
| Refresh / toggle sidebar | Ctrl+Alt+R, F5 / Ctrl+Shift+S, F9 |

Every entry is audited for US and Norwegian layouts (no AltGr; `#`, `/`, `!` and `?` have unshifted alternatives), nothing uses SUPER, and every F-key has an alternative.

As built in M1 ([ADR-017 amendment](DECISIONS.md#adr-017-amendment--the-keymap-as-built-in-m1-2026-10-04)): keys with Ctrl or Alt, and F-keys, of window and application commands work anywhere; every key of the message commands, and every one-key shortcut, works only while the list or the reader has the focus, so text fields keep their letters, Delete and Ctrl+Z. `g` waits 1.5 s for its second key. **Accepted at the M1 gate (2026-10-05):** `h` for Keyboard Shortcuts (`?` and Ctrl+? need Shift), Ctrl+, Preferences, Ctrl+Q Quit, Ctrl+Alt+R and F5 Refresh, Ctrl+Shift+S and F9 Toggle Sidebar.

As built in M4 (ML-055, [report](m4/triage-app.md)): the message commands' keys act (archive, delete, junk, move to, star, read, unread, select, undo). No key was added to the registry; the list's own keys beside it, accepted at the M4 gate (2026-10-06, [ADR-017](DECISIONS.md#adr-017-amendment--the-lists-own-keys-accepted-m4-gate-2026-10-06)): **Shift with Next and Previous Message's keys** (Shift+`j`/`k`, as Shift+Down/Up) extends the selection; **Escape** leaves the select mode; **Menu** and **Shift+F10** open a sidebar folder's menu. Mark Folder as Read has no default key. The keyboard pass was approved headless; the user's own live pass followed on 2026-10-06 ("Keyboard pass approved").

M7 (accepted at the M7 gate, 2026-10-07, [ADR-017](DECISIONS.md#adr-017-amendment--m7-adds-no-keys-m7-gate-2026-10-07)) adds no key: the search field's own keys while its suggestions show (Down, Up, Tab, Enter, Escape) are the field's, as the composer's recipient suggestions are; Rebuild Search Index and Start Sealwax at Login have none.

Accepted at the M5 gate (2026-10-06, [ADR-017](DECISIONS.md#adr-017-amendment--m5s-keys-accepted-m5-gate-2026-10-06)): **Ctrl+P** Print (the mock's), **Ctrl+U** Show Original, **Ctrl+S** Save as .eml, and **`n`** and **`p`** for Next and Previous Message in Conversation, in the list and the reader (each expands the message it reaches and scrolls to it). Expand and Collapse All Messages, Show Images and Always Show Images from This Sender have no default key.

*As built in M9 (ML-092, [report](m9/accessibility.md)); proposed for the user at M9's gate, no key added:* **Go to Inbox, Sent and Drafts** (`g` then `i`, `t`, `d`) show that folder of the account the list shows (in All Inboxes the selected message's, else the first account's); Go to Inbox in All Inboxes stays there; **Go to Starred** (`g` then `s`) turns on the Starred chip, as there is no Starred folder. **Back to List** (Escape, `u`, in the reader) puts the focus in the list, showing it first in the one-pane layout, and leaves the select mode as Escape did there. Toggle Sidebar (Ctrl+Shift+S, F9) still has no handler. **Tab:** the folder list and the message list are one Tab stop each (Up and Down move inside them); Tab in a plain-text body goes from link to link (selected, its target in the strip, Enter opens it), as WebKit does in an HTML body; Tab in the composer's body moves on to Send and Shift+Tab back to the toolbar instead of typing a tab (a dialog keeps Tab inside it).

## Accessibility

- Every control has an accessible label; icon-only buttons have tooltips with their shortcut, taken from the keymap in use, so `keys.toml` changes show.
- A visible accent focus ring; focus order follows the panes left to right.
- Text contrast is checked per theme (`readable_on`); state is never shown by colour alone (unread is bold as well as dotted).
- An Orca smoke test of the list, reader and composer is part of M9.

*As built in M9 (ML-092, [report](m9/accessibility.md)):*
- **Names:** a button's visible label names it for screen readers (GTK's labelled-by relation, which wins over a label property); where the visible label alone is ambiguous the relation is replaced by a name (`accessibility::name_button`): each attachment card's "Open {file}", "Save {file}"; the setup assistant's "IMAP port", "SMTP security" and so on, whose captions say "Port" and "Security"; the plain-text "…" toggle, "Show quoted text" / "Hide quoted text". Icon-only buttons take their name from their tooltip's title. Lists are named ("Folders", "Messages", "Commands", the shortcut sections); every body is "Message body" (HTML) or "Message text" (plain).
- **States:** toggles say pressed, the conversation's rows and headers and the "…" toggles say expanded or collapsed, the current folder says selected, insensitive controls say disabled, a field the assistant refuses says invalid. Banners are alerts; the link strip is a status.
- **Spoken without moving the focus:** where Up and Down choose a row while the focus stays in a field (the command palette, Move to…, the search suggestions, the composer's recipient suggestions), the chosen row is announced; so is the link warning when the keyboard reaches a link whose text names another address.
- *Changed after approval, 2026-10-09 (folders):* folder rows carry their level in the tree (1 at the top) and, with subfolders, their expanded or collapsed state; the folder name dialog's field is named "Folder name", is marked invalid while its name is refused and has the reason as its description. `a11y-window` and `a11y-dialogs` audit the tree, the menus and the three dialogs.
- *Changed, 2026-10-08 (the docked composer):* composer cards come after the panes in the Tab order, the newest first: From, Attach, Discard, Minimize, Expand, Close, To, Cc · Bcc, Subject, the style menu and the formatting buttons, the body, Send; then round to the sidebar. The card no longer keeps Tab to itself as its window did.
- *Fixed, 2026-10-08:* the Tab order below holds with no message open too (an empty folder, or messages and none selected): Tab goes on from the reader's Next button to the sidebar, and Shift+Tab round from the sidebar reaches the reader's toolbar; after the list shows another folder, Tab from the sidebar goes through the list's bar again instead of past it. The setup assistant, while it checks a login, goes between Back and Close. Nothing in the order changed.
- **The Tab order** follows the panes left to right and top to bottom, and comes round: the sidebar (compose, the folder list as one stop, `config.toml`), the list's bar (menu, search, chips), the message list (one stop), the reader's toolbar, the conversation (rows, card headers, Details, Reply, Reply all, Forward, the body and its links, attachments, "…"), the quick reply. The composer: From, Attach, Discard, To, Cc · Bcc, Subject, the style menu and the formatting buttons, the body, Send. Dialogs keep Tab inside; Escape closes them and the focus goes back where it was. Panes and empty pages with nothing to operate are no Tab stops of their own, and the focus never stays in WebKit's hidden key-binding widget inside each web view (out of a page it goes on, coming in backwards it goes to the page).
- **Held by** `assert-accessible` and `tab-chain` in the `a11y-*` self-tests (`app/src/selftest/a11y.rs`), run by `tools/ci.sh --ui`.

## Mock and gate

The interactive DESIGN mock (ML-008), published 2026-10-04 as a private design canvas: [Mail — design mock](https://claude.ai/artifact/TU2jwqgJu1YxTXFLLdpuLS).

- **Artboards:** wide window (interactive: pick a message), medium (rail), narrow (one pane), reader with styled HTML on the paper card, hidden images and the link check, composer, setup assistant (detect, Microsoft sign-in, Gmail's app-password failure with manual settings; Google sign-in removed 2026-10-04), and empty and error states.
- **Tweaks on every artboard:** Theme (Matte Black, Tokyo Night, Catppuccin Latte — palettes resolved with `omarchy-theme-color --all`) and Corners (Square, Rounded).
- **Stand-ins:** JetBrains Mono for the Omarchy font; Arimo (metric-compatible with Liberation Sans) for message text. All names, addresses and content are fictional.

The user's decisions at the gate (layout, square or rounded, sizes) will be recorded here as a dated entry.
