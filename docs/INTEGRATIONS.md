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
- **Focus and scale (S6):** Omarchy sets `misc.focus_on_activate = true`, so `present()` focuses the window (2–4 ms). `GDK_SCALE=2` in the session environment has no effect on GTK 4 Wayland apps; it still applies to X11 and XWayland programs.
- **Default opacity:** Omarchy tags every window `default-opacity` (0.985 / 0.96); an opt-out snippet is `o.window(<match>, { tag = "-default-opacity" })`. This machine's `looknfeel.lua` already forces windows opaque.
- **Key binding:** the user's `~/.config/hypr/bindings.lua` binds SUPER+SHIFT+E to BlueMail. The snippet the user can paste once Mail is installed (ML-085), with the final name:
  ```lua
  hl.unbind("SUPER + SHIFT + E")
  o.bind("SUPER + SHIFT + E", "Mail", { launch = "<binary>", focus = "^io\\.github\\.pixdevsapps\\.<Name>$" })
  ```
  Mail never edits Hyprland files itself.

## omarchy-shell (notifications, launcher)

- omarchy-shell (Quickshell 0.3.1) owns `org.freedesktop.Notifications`. Measured in S6: name and vendor `quickshell`, spec 1.2, capabilities `persistence`, `body`, `body-markup`, `body-hyperlinks`, `actions`, `icon-static`; a notification is accepted in 0.8 ms.
- **Click behaviour** (read in `/usr/share/omarchy/shell/plugins/notifications/Service.qml`): the shell invokes the notification's `default` action if there is one; otherwise it focuses a window whose class matches `app_name` as a case-insensitive regex. Mail always sends a `default` action. **Cards show no action buttons:** a left click runs `default`, a right click dismisses. GLib 2.88 ignores the `ActivationToken` signal; GTK's `present()` requests its own token and Omarchy's `focus_on_activate = true` focuses the window (2–4 ms, S6). A real click is the user's manual check.
- Low-urgency notifications stay ≥ 5 s, normal ≥ 8 s, none longer than 30 s; Do Not Disturb stores them silently.
- **History:** toasts are mirrored under `~/.local/state/omarchy/notifications/` and moved to `history/` (newest 10) when they leave the screen. Restored and history entries have no live actions, so a click reaches the app only while it runs (background mode, D6).
- **What GLib sends** (S6): `app_name` is the process name, so the app sets it to the product name (`g_set_prgname`) before its first notification; the app-id goes in the `desktop-entry` hint, the themed icon name in `image-path`, and actions as detailed action names.
- **Launcher:** the `.desktop` file is found through Quickshell `DesktopEntries` and started with `uwsm-app -- gtk-launch <id>.desktop`.
- **Omarchy menu (snippet only):** entries can be added in `~/.config/omarchy/extensions/omarchy-menu.jsonc` (ids like `"mail.compose"`). Documented in M7, never written by the app.
- **Unread count in the bar:** a 1.x bar-widget plugin (manifest `kinds: ["bar-widget"]`, QML with `qs.Commons`), following the user's Hertz Radio and Scarlett Control plugins.

## Secret Service

- gnome-keyring 50 owns `org.freedesktop.secrets` on this machine; the "Default keyring" is unlocked and passwordless, so its file is stored unencrypted (protected by full-disk encryption only). See [SECURITY.md](SECURITY.md#known-limitations).
- Mail stores items through oo7 with attributes `{ app: <app-id>, account: <uuid>, kind: password | refresh-token }`. Headless runs use the in-memory store (the private bus starts no services).

## Default mail handler

- Current handler: `bluemail.desktop`. `~/.config/mimeapps.list` still names a removed Thunderbird for `message/rfc822`; Mail does not touch it.
- Mail's desktop file declares `MimeType=x-scheme-handler/mailto;`. "Make Mail the default email app" is an opt-in command that asks first and then runs `xdg-mime default <app-id>.desktop x-scheme-handler/mailto` (Stet's `set_default` pattern).
- `mailto:` URIs are parsed per RFC 6068; `attach` / `attachment` parameters are ignored. Only `to`, `cc`, `bcc`, `subject`, `body` and `in-reply-to` are kept, and control characters are stripped from header values (S6).
- **Forwarding (S6):** a second process forwards a `mailto:` to the running instance in ~51 ms, 49 of them loading libraries; a D-Bus `org.freedesktop.Application.Open` call, which launchers use for a `DBusActivatable` app, takes 0.5 ms. The desktop file keeps `DBusActivatable=true`, and the app handles both `open` and `command-line`.

## Network and sleep

- GIO's network monitor uses NetworkManager here (`GNetworkMonitorNM`); logind's `PrepareForSleep` signal is on the system bus (read-only subscription).
- IMAP IDLE never notices a silently dead connection: in S6 one went unnoticed for 10 s, and it would stay unnoticed until the IDLE restart plus the read timeout. On `PrepareForSleep(false)` and when the network monitor reports the network back, the engine drops and reconnects every connection at once ([ADR-003](DECISIONS.md#adr-003-amendment--desktop-integration-measured-in-s6-2026-10-04)); the reconnect itself took ~14 ms on localhost.
- Real suspend, Wi-Fi reassociation and NAT timeouts are not verified yet: the user's manual check first, then the M3 soak.


## Account autodiscovery sources

Measured in S4 on 2026-10-04 ([report](spikes/S4-autodetect.md)). The sources run in parallel. For each protocol, the highest-priority source that answers wins once every higher-priority source has answered, failed or timed out. The first usable answer is shown at once; only the settled one is used.

1. Built-in presets (offline, no request), including MX host suffixes for hosted platforms: Google, Microsoft 365 (`*.mail.protection.outlook.com`, `*.mx.microsoft`), Fastmail (`messagingengine.com`), Zoho by region, Altibox (`altibox.net`), Domeneshop.
2. The domain's own autoconfig over HTTPS (4 s): `https://autoconfig.<domain>/mail/config-v1.1.xml?emailaddress=<address>`. The only place the full address is sent; redirects are followed only over HTTPS inside the domain.
3. DNS SRV records (RFC 6186, RFC 8314).
4. The Mozilla ISPDB (3 s): `https://autoconfig.thunderbird.net/v1.1/<domain>` (domain only).
5. MX lookup → the preset suffix table, else the ISPDB for up to two registrable MX domains (public suffix list). A null MX means the domain has no mail.
6. `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`, capped at 1.5 s (slow and rarely useful in S4).
7. Guarded probing: `imap.`, `mail.`, `smtp.` and the bare domain on 993, 143 (STARTTLS), 465 and 587 (STARTTLS). It starts when the other sources are done or after 2 s, is TLS only, and stops when the preferred host answers.

Hosts from network sources that are loopback, private or IP literals are never used (Proton's own autoconfig returns the Bridge on 127.0.0.1; that maps to the Bridge notice). SRV- and MX-derived hosts outside the user's domain or a known provider are shown for confirmation before the password is sent. One resolver (hickory) with fully qualified names serves HTTP, DNS and probes, so a search domain such as `home` never turns `autoconfig.<domain>` into `autoconfig.<domain>.home`.

IMAP login and SMTP AUTH are verified before anything is saved, with **at most 2 attempts**: the engine holds a budget of 2 and spends one before any authentication command; the first attempt uses the top-ranked IMAP host and the detected username form; the second comes only after an authentication failure and uses the other form (full address ↔ local part, as RFC 6186 recommends); the password never goes to a lower-ranked host; SMTP reuses the form that worked; where the username can't be derived (Domeneshop account names, some iCloud custom domains) the user is asked first.

## Providers

Settings and policies checked on the providers' help pages on 2026-10-04 (S4). Anything marked unverified needs a real login.

| Provider | Settings | Sign-in | Notes |
| --- | --- | --- | --- |
| Gmail / Google Workspace | imap.gmail.com:993; smtp.gmail.com:465 (587 STARTTLS); user = address | OAuth (XOAUTH2/OAUTHBEARER offered), or an app password with 2-Step Verification | Workspace found by MX (`aspmx.l.google.com`, `smtp.google.com`); admins can turn IMAP off; consumer IMAP always on since 2025-01. Labels appear as folders; Archive = remove from INBOX; Delete = move to Trash; the server saves sent mail |
| Outlook.com | outlook.office365.com:993; smtp-mail.outlook.com:587 STARTTLS (no 465); user = address | OAuth only (basic auth ended 2024-09-16; IMAP offers only XOAUTH2, LOGINDISABLED) | "Let devices and apps use IMAP" toggle; MX `*.olc.protection.outlook.com`; the server saves sent mail |
| Microsoft 365 | outlook.office365.com:993; smtp.office365.com:587 STARTTLS (465 unsupported) | OAuth (IMAP/POP basic auth off since 2022-10) | MX `*.mail.protection.outlook.com` or `*.mx.microsoft`; the ISPDB route wrongly gives the consumer SMTP host |
| iCloud | imap.mail.me.com:993 (user = local part, full address as fallback); smtp.mail.me.com:587 STARTTLS (user = address) | App-specific password (two-factor login required) | — |
| Yahoo / AOL | imap.mail.yahoo.com:993, smtp.mail.yahoo.com:465 (587 STARTTLS works) / imap.aol.com:993, smtp.aol.com:465 | App password | AOL help pages didn't load; AOL settings from search-engine text (unverified) |
| Fastmail | imap.fastmail.com:993; smtp.fastmail.com:465 (587 STARTTLS) | App password (mandatory) | No IMAP on the Basic plan; custom domains by MX `messagingengine.com` |
| GMX / WEB.DE / mail.com | imap.gmx.net / imap.gmx.com / imap.web.de / imap.mail.com:993; mail.gmx.net / mail.gmx.com / smtp.web.de / smtp.mail.com 465 or 587 STARTTLS; user = address | Password; app password with two-factor login or outside the EU/US/Canada | IMAP must be enabled and switches itself off after long non-use; mail.com IMAP is Premium only |
| Zoho | Personal and free organizations: imap.zoho.com / smtp.zoho.com; paid organizations: imappro / smtppro; regional .eu/.in/.com.au from MX | App password with two-factor login | IMAP must be enabled; the EU hostnames are not in Zoho's docs (TLS-checked only) |
| Yandex | imap.yandex.com (imap.ya.ru outside Russia):993; smtp.yandex.com:465; user = local part (docs) | App password | Its own autoconfig says the full address (unverified which works); press reports of paid IMAP from 2026-06-29 unconfirmed |
| Mail.ru | imap.mail.ru:993; smtp.mail.ru:465 (TLS 1.2) | Password for external applications | Paid-tier press reports unconfirmed |
| mailbox.org / Posteo | imap/smtp.mailbox.org 993, 465/587 / posteo.de 993 (143 STARTTLS), 465/587 | App password with two-factor login (mailbox.org) | Found by their own autoconfig |
| Proton | Bridge only: 127.0.0.1:1143/1025 | Bridge password | Its autoconfig returns the loopback Bridge config; pinned certificates are 1.x |
| Tuta | No IMAP or SMTP | — | "Not supported" |
| Telenor (online.no) | mail.online.no:993/465; user = the part before @online.no | Password | "Tredjeparts e-postprogrammer" toggle in Mine Sider; port 587 presents an untrusted certificate |
| Altibox (altibox.no, lyse.net, partners) | imap.altibox.no:993; smtp.altibox.no:465; user = address | Password | altibox.no's MX is Microsoft 365 (staff mail), so MX detection misleads; partner domains by MX `altibox.net`; lyse.net inferred (unverified) |
| Domeneshop | imap.domeneshop.no:993; smtp.domeneshop.no 587 STARTTLS or 465 | Password | The username is the generated account name (e.g. `mittnavn1`), not the address; ask the user |
| one.com | imap.one.com:993; send.one.com 465 or 587; user = address | Password | In the ISPDB |
| NextGenTel (broadpark.no) / Tussa | imap.broadpark.no:993, smtp.broadpark.no:587 STARTTLS / imap.tussa.com:993, smtp.tussa.com:587 STARTTLS | Password | Broadpark's username form is undocumented |
| Discontinued | c2i.net and tele2.no (closed 2013; c2i.net now held by an unknown party), chello.no (2008, null MX); getmail.no → Wemail, start.no → Mailia (paid third parties) | — | Never autoconfigure c2i.net |
