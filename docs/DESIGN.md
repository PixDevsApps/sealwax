# Design & Interaction System

Synced: 2026-10-04

The repository docs are the canonical project memory ([ADR-012](DECISIONS.md#adr-012--repository-docs-canonical-notion-as-a-mirror-2026-10-04)). This page is a **proposal until the M1 gate**: the interactive DESIGN mock (ML-008) is approved by the user before any UI work, and anything the user changes there is recorded here as a dated entry above the affected section.

## Principles

1. **The theme is the only style.** Every colour comes from the live Omarchy palette; the app ships no palette of its own except the fallbacks for running outside Omarchy ([ADR-004](DECISIONS.md#adr-004--follow-the-live-omarchy-theme-2026-10-04)).
2. **Content first, chrome quiet.** Messages get the space and the contrast; toolbars, borders and counts recede.
3. **Keyboard first, mouse complete.** Everything is reachable from the keyboard and the palette, and everything a mouse user expects is visible and clickable.
4. **Calm and fast.** No spinners where cached data exists, no layout jumps, no animations that delay input.
5. **Never steal focus**; prefer banners and toasts to dialogs; ask only when a decision is needed.
6. **Plain files.** Settings live in `config.toml`, `keys.toml` and `accounts.toml`; the palette opens them.

## What we take from the inspiration designs

The six reference designs the user shared agree on a layout and a set of row details; Mail keeps those and drops the decoration that would fight the theme.

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
| ≥ 1280 px | Sidebar (≈ 220 px) · list (≈ 380 px) · reader (rest) |
| 900–1280 px | Rail (icons and counts, ≈ 56 px) · list · reader |
| < 900 px | One pane at a time: folders → list → reader, with back navigation |

- On wide screens (the 3440 px ultrawide here) the reader centres its text column at a readable maximum width (≈ 760 px of body text); headers and toolbars stretch.
- No title bar on Hyprland (detected as in Stet: `HYPRLAND_INSTANCE_SIGNATURE` or `XDG_CURRENT_DESKTOP`); window controls appear elsewhere.
- The compositor owns the outer window corners and border.

## Components

**Sidebar**
- Compose button at the top (accent fill, `c`).
- "All Inboxes", then each account with its role folders (Inbox, Drafts, Sent, Archive, Junk, Trash) and unread counts; other folders under a collapsible "More".
- Account status as a small dot: synced, syncing, offline, needs sign-in.

**Message list**
- Search field and filter chips on top, then sections (Today, Yesterday, This week, This month, Older).
- Row anatomy: unread dot · avatar (32 px) · sender (bold when unread) and time · subject · one- or two-line snippet · indicators (attachment, star, thread count).
- Selection mode with checkboxes for batch actions; hover shows quick actions (archive, delete, star).

**Reader**
- Toolbar: archive, delete, junk, move, mark unread, star, more; position "n of N" with previous and next.
- Subject as a heading; per message: avatar, name and address, "to me" or recipients, date with relative time; collapsed older messages as one-line rows.
- Body in a WebView (HTML) or a text view (plain text); attachment cards (type icon, name, size; Open, Save, Download All); remote-content banner ("Images are hidden — Show / Always for this sender").
- Quick reply under the conversation, expanding into the full composer.

**Composer**
- From (identity), To / Cc / Bcc as chips with autocomplete, Subject.
- Formatting toolbar (bold, italic, underline, strike, lists, link, quote, indent, clear formatting) driven by the editor state.
- Body, attachments tray (drag and drop), Send (Ctrl+Enter) with "Undo" in the toast that follows.

**Setup assistant**
- One column: address → "Looking up your provider…" with the sources it checks → password, or "Sign in with Google / Microsoft" → checking login → done.
- "Set up manually" expands host, port, security and username fields. Errors say what to do next (for example: "Gmail needs an app password — open the Google account page").

**Feedback**
- Toasts for Undo (archive, delete, move, send) and transient results.
- Banners for persistent states: offline, needs sign-in, certificate changed, quota full, no keyring.
- Empty states: no accounts (starts the assistant), empty folder, no search results.

## Theme tokens

All colours come from the resolved palette (`omarchy-theme-color --all`), mapped once in the theme layer. Widgets never reference palette keys directly. The UI roles are Stet's ([its DESIGN.md](../../notes/docs/DESIGN.md)); the mail roles are new.

| Role | Applied through | Omarchy key |
| --- | --- | --- |
| Window, reader background | `--window-bg-color`, `--view-bg-color` | `background` |
| Text | `--window-fg-color`, `--view-fg-color` and the other `*-fg-color` variables | `foreground` |
| Sidebar, list | `--sidebar-bg-color`, `--headerbar-bg-color` | `dark_background` |
| Popovers, palette, dialogs | `--popover-bg-color`, `--dialog-bg-color` | `darker_background` |
| Cards, hovered rows, attachment cards | `--card-bg-color` | `lighter_background` |
| Borders, separators | `--border-color` | `lighter_background` |
| Secondary text (snippets, times, addresses) | mail role | `foreground` mixed 35 % toward `background` |
| Accent: focus ring, compose button, unread dot, selected chip | `--accent-bg-color`, `--accent-color` | `accent` |
| Text on accent | `--accent-fg-color` | `readable_on(accent)` |
| Star | mail role | `yellow` (falls back through `orange`) |
| Error / warning / success | `--error-*`, `--warning-*`, `--success-*` | `red` / `yellow` / `green` |
| Avatar tints | mail role | hues from `red, green, yellow, blue, magenta, cyan, orange`, separated by Stet's `distinct`, chosen by a hash of the address |
| Light or dark | AdwStyleManager FORCE_DARK / FORCE_LIGHT | `mode` |
| Corners of rows, chips, cards, buttons | `--window-radius` and CSS | `corner_radius` ([ADR-019](DECISIONS.md#adr-019--corner-radius-follows-hyprland-2026-10-04)) |

Roles map to palette positions, never hue names: matte-black's `green` is amber and its `blue` is orange. Contrast is checked with `readable_on`, and Stet's `distinct` rotates hues when a theme can't tell its colours apart.

**Control states** follow omarchy-shell's `shell.toml`: normal = foreground at 4 % with a 1 px border at 40 %; hover and focus 8 %; selected 18 %; pressed 22 %; text selection 35 %.

**Spacing** follows omarchy-shell's scale: 2, 3, 4, 6, 8, 10, 12, 14, 18 px. Controls are 28 px tall with 10 × 6 px padding; rows breathe at 10–12 px vertical padding.

**Surfaces** are flat: no drop shadows; elevation comes from the background roles and 1 px borders.

## Typography

- **Chrome** (sidebar, list, toolbars, chips): the Omarchy font, `fc-match monospace` (JetBrainsMono Nerd Font here), re-resolved when `~/.config/fontconfig/` changes, as in Stet and omarchy-shell.
- **Scale** follows omarchy-shell (base 12 px, or `[font] base-size` from `~/.config/omarchy/shell.toml`): caption 10, small 11, body 12, subtitle 13, title 14, heading 16, display 24.
- **Message text**: sans-serif (`fc-match sans-serif`, Liberation Sans here) at a comfortable reading size (≈ 15 px, line height 1.55). WebKit's default fonts are set explicitly, because it would otherwise pick its own.
- `config.toml` can switch the chrome to sans or the reading font to mono. The mock decides the exact sizes.

## HTML mail inside the theme

- **Plain text and simple HTML** (no colours of their own) take the theme: background, foreground, accent links, muted quotes.
- **Styled HTML** (newsletters, receipts) keeps its design on a "paper" card: a light surface with the theme's corner radius and a 1 px border, so dark themes never invert someone else's layout.
- Quotes are collapsed behind "…" when they repeat earlier messages in the conversation.
- Nothing remote loads until allowed ([SECURITY.md](SECURITY.md)); hidden images keep their layout boxes so the page doesn't jump when shown.

## Avatars

Initials from the display name (or the address), on a tint chosen by hashing the address. Never fetched from Gravatar, BIMI or favicons (privacy).

## Motion

Short fades (≤ 150 ms) for toasts and banners; no sliding lists, no bounce. Animations follow `gtk-enable-animations`.

## Keyboard

Draft keymap ([ADR-017](DECISIONS.md#adr-017--keymap-scoped-one-key-shortcuts-under-omarchy-rules-2026-10-04)). One-key shortcuts work only while the list or reader has focus.

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
| Search | `/`, Ctrl+F |
| Go to Inbox / Starred / Sent / Drafts | `g` then `i` / `s` / `t` / `d` |
| Send | Ctrl+Enter |
| Undo | `z`, Ctrl+Z |
| Command palette | F1, Ctrl+Shift+P |
| Keyboard shortcuts | `?`, Ctrl+? |

Every entry is audited for US and Norwegian layouts (no AltGr; `#`, `/`, `!` and `?` have unshifted alternatives), nothing uses SUPER, and every F-key has an alternative.

## Accessibility

- Every control has an accessible label; icon-only buttons have tooltips with their shortcut.
- A visible accent focus ring; focus order follows the panes left to right.
- Text contrast is checked per theme (`readable_on`); state is never shown by colour alone (unread is bold as well as dotted).
- An Orca smoke test of the list, reader and composer is part of M9.

## Mock and gate

The interactive DESIGN mock (ML-008) is a private web page rendered in the live palette plus a light theme. It shows the three widths, list rows, the reader, the composer, the setup assistant, and empty and error states, each square and rounded. Its link and the user's decisions will be recorded here.
