# Integrations

Synced: 2026-10-04

How Mail fits Omarchy 4, Hyprland and the mail providers. Facts about this machine were read on 2026-10-04 (Omarchy 4.0.4, Hyprland 0.56.2); anything marked *to verify* is checked in the named spike before code depends on it.

## Omarchy theme

- **Active palette:** `~/.local/state/omarchy/current/theme/colors.toml`, a flat `key = "#rrggbb"` file with a `mode` key (`dark` or `light`). Omarchy hardcodes `$HOME/.local/state` and ignores `XDG_STATE_HOME`. `~/.config/omarchy/current` no longer exists in Omarchy 4.
- **Resolver:** `/usr/share/omarchy/bin/omarchy-theme-color --file <colors.toml> --all` prints `key<TAB>value` lines with every alias and fallback applied (`bg`, `color0`–`color15`, `purple`, derived `dark_background` and so on). Stet's Rust port of the cascade is the fallback when the script is missing.
- **Light or dark:** first match wins: `mode`, legacy `theme_type`, a `light.mode` file, then background luminance (R+G+B > 382 is light). Five stock themes are light: catppuccin-latte, flexoki-light, white, rose-pine, lupine.
- **What changes on a switch** (`omarchy-theme-set`): `current/next-theme/` is staged, then `rm -rf current/theme; mv current/next-theme current/theme`, then `theme.name` is rewritten. The reload trigger (from Stet) watches `current/` and its parent: `theme.name` ChangesDoneHint, or `theme` Renamed; debounce 150 ms; retry if a read hits the gap while `theme/` is replaced. Never watch files inside `theme/`; they are replaced.
- **gsettings lag behind:** `omarchy-theme-set-gnome` sets `color-scheme` and `gtk-theme` after the swap, so Mail takes `mode` from `colors.toml` and forces `adw::StyleManager`.
- **No hook needed:** the directory watch sees every switch. `~/.config/omarchy/hooks/theme-set.d/` exists, but writing there would edit the user's config.
- **Theme-provided CSS (P2):** if a theme ships `<app>.css`, Omarchy copies it to `current/theme/`; Mail loads it after its generated CSS, giving theme authors an override point without a template.

## Fonts

- `omarchy font set` writes `~/.config/fontconfig/fonts.conf`, putting the font first for `monospace`; `fc-match monospace -f '%{family[0]}'` returns it (JetBrainsMono Nerd Font here). Mail watches `~/.config/fontconfig/` as Stet does.
- `/etc/fonts/conf.d/50-omarchy.conf` maps sans-serif to Liberation Sans. WebKit's default fonts are set explicitly from these families.
- `omarchy display text size` writes `[font] base-size` to `~/.config/omarchy/shell.toml` (12 px default); Mail's chrome scale follows it.
- gsettings `font-name` is not updated by Omarchy and is not read.

## Hyprland

- Configured in Lua (`~/.config/hypr/hyprland.lua` loads Omarchy's defaults, then the user's modules); `hyprctl reload` applies, `hyprctl configerrors` checks.
- **Corner radius:** `hyprctl -j getoption decoration:rounding` (8 here, 0 on stock Omarchy), read at start and after theme changes, as omarchy-shell does ([ADR-019](DECISIONS.md#adr-019--corner-radius-follows-hyprland-2026-10-04)).
- **Window class:** GTK4 apps use the app-id as their Wayland app_id, so rules match `^io\.github\.pixdevsapps\.<Name>$` (working: `Mail`).
- **Default opacity:** Omarchy tags every window `default-opacity` (0.985 / 0.96); an opt-out snippet is `o.window(<match>, { tag = "-default-opacity" })`. This machine's `looknfeel.lua` already forces windows opaque.
- **Key binding:** the user's `~/.config/hypr/bindings.lua` binds SUPER+SHIFT+E to BlueMail. The snippet the user can paste once Mail is installed (ML-085), with the final name:
  ```lua
  hl.unbind("SUPER + SHIFT + E")
  o.bind("SUPER + SHIFT + E", "Mail", { launch = "<binary>", focus = "^io\\.github\\.pixdevsapps\\.<Name>$" })
  ```
  Mail never edits Hyprland files itself.

## omarchy-shell (notifications, launcher)

- omarchy-shell (Quickshell 0.3.1) owns `org.freedesktop.Notifications`; it supports actions, body markup, hyperlinks and `image-path`.
- **Click behaviour** (read in `/usr/share/omarchy/shell/plugins/notifications/Service.qml`): the shell invokes the notification's `default` action if there is one; otherwise it focuses a window whose class matches `app_name` as a case-insensitive regex. Mail always sends a `default` action. Whether the activation token reaches `present()` is checked live in S6 (*to verify*).
- Low-urgency notifications stay ≥ 5 s, normal ≥ 8 s, none longer than 30 s; Do Not Disturb stores them silently.
- **Launcher:** the `.desktop` file is found through Quickshell `DesktopEntries` and started with `uwsm-app -- gtk-launch <id>.desktop`.
- **Omarchy menu (snippet only):** entries can be added in `~/.config/omarchy/extensions/omarchy-menu.jsonc` (ids like `"mail.compose"`). Documented in M7, never written by the app.
- **Unread count in the bar:** a 1.x bar-widget plugin (manifest `kinds: ["bar-widget"]`, QML with `qs.Commons`), following the user's Hertz Radio and Scarlett Control plugins.

## Secret Service

- gnome-keyring 50 owns `org.freedesktop.secrets` on this machine; the "Default keyring" is unlocked and passwordless, so its file is stored unencrypted (protected by full-disk encryption only). See [SECURITY.md](SECURITY.md#known-limitations).
- Mail stores items through oo7 with attributes `{ app: <app-id>, account: <uuid>, kind: password | refresh-token }`. Headless runs use the in-memory store (the private bus starts no services).

## Default mail handler

- Current handler: `bluemail.desktop`. `~/.config/mimeapps.list` still names a removed Thunderbird for `message/rfc822`; Mail does not touch it.
- Mail's desktop file declares `MimeType=x-scheme-handler/mailto;`. "Make Mail the default email app" is an opt-in command that asks first and then runs `xdg-mime default <app-id>.desktop x-scheme-handler/mailto` (Stet's `set_default` pattern).
- `mailto:` URIs are parsed per RFC 6068; `attach` / `attachment` parameters are ignored.

## Account autodiscovery sources

Run in parallel with per-source timeouts; the first verified result wins (spike S4 in [PLAN.md](PLAN.md#milestone-plan) measures accuracy):

1. Built-in presets for the large providers (offline, instant).
2. The domain's own autoconfig over HTTPS: `https://autoconfig.<domain>/mail/config-v1.1.xml?emailaddress=<address>` and `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`. The only place the full address is sent.
3. The Mozilla ISPDB: `https://autoconfig.thunderbird.net/v1.1/<domain>` (domain only).
4. MX lookup → registrable domain of the MX host (public suffix list) → presets or ISPDB for that domain. This catches custom domains on Google Workspace, Microsoft 365, Fastmail, Zoho and others.
5. DNS SRV records (RFC 6186, RFC 8314): `_submissions._tcp`, `_submission._tcp`, `_imaps._tcp`, `_imap._tcp`, `_pop3s._tcp`, `_pop3._tcp`.
6. Guarded probing: `imap.`, `mail.` and `smtp.` hosts on 993, 143 (STARTTLS), 465 and 587 with short timeouts; TLS only.

Then IMAP login and SMTP AUTH are verified (≤ 2 attempts) before anything is saved.

## Providers

Initial notes; policies are re-checked in S4 and S5 and the quirks table grows in M8.

| Provider | Settings | Sign-in | Quirks |
| --- | --- | --- | --- |
| Gmail / Google Workspace | imap.gmail.com:993, smtp.gmail.com:465 | OAuth (scope `https://mail.google.com/`), or an app password with 2-Step Verification | Labels appear as folders; Archive = remove from INBOX; Delete = move to Trash; sent mail is saved by the server; X-GM-THRID / X-GM-MSGID available |
| Outlook.com / Microsoft 365 | outlook.office365.com:993, smtp.office365.com:587 (STARTTLS) | OAuth only for Outlook.com (since 2024); M365 depends on tenant policy | Sent mail is saved by the server; no CONDSTORE expected (*to verify*); `Thread-Index` threading |
| iCloud | imap.mail.me.com:993, smtp.mail.me.com:587 | App password | — |
| Yahoo / AOL | imap.mail.yahoo.com:993, smtp.mail.yahoo.com:465 | App password | — |
| Fastmail | imap.fastmail.com:993, smtp.fastmail.com:465 | App password | CONDSTORE/QRESYNC; JMAP later |
| GMX / web.de | imap.gmx.net:993, mail.gmx.net:587 | Password; IMAP may need enabling in web settings | — |
| Proton (Bridge) | 127.0.0.1:1143 (STARTTLS), 127.0.0.1:1025 | Bridge password | Self-signed certificate: pinned exceptions are 1.x |
| Domeneshop, Norwegian ISPs | via autoconfig, MX and SRV | Password | Checked in S4 |
