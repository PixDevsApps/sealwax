# Security

Synced: 2026-10-04

Mail handles hostile content (every incoming message), long-lived credentials and private data. This page is the threat model and the rules every change must keep. M9 reviews it (ML-093).

## Threat model

| Threat | Example | Defence |
| --- | --- | --- |
| Hostile HTML | Scripts, tracking pixels, CSS `url()` beacons, `@import`, meta refresh, forms, iframes, SVG scripts, `<a ping>`, DNS prefetch, `<base>` tricks | Layered WebKit hardening ([below](#message-rendering)); the hostile-HTML suite in CI (ML-065) |
| Tracking | Remote images reveal when and where a message was opened | Nothing remote loads until allowed per message or per sender |
| Phishing | Link text shows one domain, the target is another | Real URL on hover; mismatch warning; links open in the browser, never in the app |
| Dangerous attachments | `.desktop`, `.sh`, `.AppImage`, `.jar` launched by a double-click | Never opened directly; warning and Save only |
| `mailto:` abuse | `attach=` parameters that pull local files into a message | `attach` / `attachment` parameters ignored; nothing is sent without the user |
| Network attackers | TLS stripping, STARTTLS downgrade, forged certificates | TLS everywhere, STARTTLS required when used, platform certificate verification, plaintext authentication refused |
| Credential theft | Passwords or tokens in files, logs or crash output | Secrets only in the Secret Service; grep tests over config, database and logs |
| Metadata leaks | Hostname in Message-ID, EHLO and Received headers; Bcc recipients revealed | Neutral `hello_name`; Message-ID with the sender's domain; no Bcc header in the transmitted copy |
| Privacy of setup | The address sent to third parties during autodetect | Full address only to the domain's own HTTPS autoconfig; the ISPDB gets only the domain |
| Malformed mail | Parser crashes or hangs on hostile MIME | mail-parser plus fuzzing of parsing and HTML handling (M9) |
| Data loss | Crashes while writing drafts or sending | WAL store; draft autosave; idempotent op replay; at-most-once send ([ADR-015](DECISIONS.md#adr-015--op-queue-optimistic-overlay-undo-at-most-once-send-2026-10-04)) |

## Rules

### Transport

- IMAP on 993 (implicit TLS) or 143 with STARTTLS required; SMTP on 465 (implicit TLS, preferred per RFC 8314) or 587 with STARTTLS required; POP3 on 995 or 110 with STLS required.
- If a server offers no TLS, the account is refused with an explanation; there is no plaintext mode.
- Certificates are verified with rustls and the platform verifier (the system trust store). Pinned exceptions for self-signed servers (Proton Bridge) are 1.x ([ADR-007](DECISIONS.md#adr-007--smtp-mime-and-privacy-defaults-2026-10-04)).
- Authentication prefers OAuth (XOAUTH2 / OAUTHBEARER) and AUTHENTICATE PLAIN over LOGIN.

### Credentials

- Passwords and refresh tokens are stored only in the Secret Service keyring (oo7). Access tokens stay in memory.
- `config.toml`, `keys.toml`, `accounts.toml`, the database, the blob store and the logs never contain a secret; tests grep for this.
- Removing an account deletes its secrets, database rows and blobs.
- Without a keyring, secrets are kept in memory for the session and a banner says so.

### Message rendering

Every layer stays even when another would suffice ([ADR-011](DECISIONS.md#adr-011--reading-pane-approach-a-with-hardened-webkit-2026-10-04) and its S2 and S7 amendments). S2 checked them against 47 hostile cases: 0 connections, 0 TLS hellos and 0 script traces at a local canary, while a no-defence control showed the attacks were live.

1. `enable-javascript-markup = false`: no script in a message ever runs; WebKit also strips `javascript:` URLs and `srcdoc` attributes. It makes `<noscript>` content live, so the other layers must stay. Mail's own helper scripts run in an isolated world.
2. `default-content-security-policy` set when each view is built: `default-src 'none'; img-src cid: data: https:; style-src 'unsafe-inline'; font-src cid: data:; form-action 'none'; base-uri 'none'; frame-src 'none'; upgrade-insecure-requests` (so remote images the user allows load over TLS).
3. A WebKit content filter blocks every URL except `cid:`, `data:` and `about:` until the user allows remote content for that message or sender.
4. Link clicks and form submits are intercepted in Mail's isolated world, because WebKit connects to a clicked link's host before `decide-policy` answers. Mail opens only absolute http(s) and mailto links, after the link-mismatch check, in the default browser (`gtk::UriLauncher`). `decide-policy` blocks every navigation as the backstop.
5. An ephemeral network session (no cookies, no cache on disk). The runtime features `LinkDNSPrefetch`, `LinkPreconnect`, `LinkPrefetch`, `LinkPreload` and `SpeculationRulesPrefetch` are off (`Settings::set_feature_enabled`): the older `enable-dns-prefetching` and `enable-hyperlink-auditing` settings do nothing in WebKitGTK 2.52, and `<a ping>` is otherwise sent. Page cache, local storage, databases, WebGL, WebAudio, media and (in release builds) developer extras are off.
6. `cid:` parts come from a per-view URI scheme backed by the local blob store; a part belonging to another message never loads.
7. View heights are capped at 16,000 px, and growth driven by viewport units is frozen (a hostile `150vh` would otherwise inflate the view).
8. WebKitGTK 6's web-process sandbox (bubblewrap) stays on. Only web processes run inside it; WebKit's network process is a plain child of the app.

Isolated-world helper scripts are **not** bound by the page's CSP for `fetch()`: in S7 a helper `fetch()` reached a local test server with the CSP alone, and only the content filter stopped it. So helper scripts fetch nothing but `blob:` URLs, and every view that runs helpers keeps the content filter. With script markup off, no markup script or event handler ran even without the CSP (S2, S7).

### Composer

The composer view uses the same layers as the reader, without `https:` for images. Paste and drop go through the allowlist cleaner; links are limited to http(s) and mailto; pasted or dropped images go out as `cid:` parts.

### Attachments

- Opened through a private temporary folder (0700) with a sanitised file name, using the desktop's default application.
- Executable and launcher types (`.desktop`, `.sh`, `.AppImage`, `.jar`, `.exe`, `.bat`, `.ps1`, `.run`, files with the executable bit) are never opened directly; Save is offered with a warning.

### Outgoing mail

- Message-ID is generated by Mail with the sender's domain; mail-builder's `gethostname` feature and lettre's `hostname` feature are off; `hello_name` is 127.0.0.1.
- The transmitted copy never carries a Bcc header; the Sent copy is the transmitted bytes plus one `Bcc:` field (S7).
- A message is sent at most once; after a crash during sending, the user decides.

### Privacy

- Autodetect: the full address goes only to the domain's own HTTPS autoconfig; ISPDB, MX and SRV lookups use the domain. Plain-HTTP autoconfig is never used.
- No avatars, favicons, BIMI or Gravatar lookups; no read receipts; no telemetry.
- Development and spikes never put the user's address or personal data in network requests ([ADR-018](DECISIONS.md#adr-018--logging-and-privacy-2026-10-04)).

### Logging

Logs never contain message bodies, subjects, addresses, folder names, tokens or protocol traces; AUTH lines are always redacted. Logs use account ids and counts.

### Local data

- Private folders are created 0700 and files 0600 (database, blobs, drafts, temporary attachment copies).
- The app never edits `~/.config` outside its own folder, `~/.local/state/omarchy` or system files, except through opt-in commands the user confirms (the default mail handler).

## Known limitations

- **Plaintext keyring on this machine:** gnome-keyring's "Default keyring" is passwordless, so its file is stored unencrypted and protected only by full-disk encryption. Mail can't fix this; USER_GUIDE will explain how to set a keyring password.
- **Self-signed servers** (Proton Bridge, some self-hosted servers) can't be added until pinned exceptions arrive in 1.x.
- **Encrypted and signed mail** (PGP, S/MIME) is displayed gracefully but not decrypted or verified in 1.0.
- **Google verification:** until the OAuth app is verified, Google shows an "unverified app" screen and limits use to test users ([OAUTH-SETUP.md](OAUTH-SETUP.md)).
