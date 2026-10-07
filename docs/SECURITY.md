# Security

Synced: 2026-10-06

Sealwax handles hostile content (every incoming message), long-lived credentials and private data. This page is the threat model and the rules every change must keep. M9 reviews it (ML-093).

## Threat model

| Threat | Example | Defence |
| --- | --- | --- |
| Hostile HTML | Scripts, tracking pixels, CSS `url()` beacons, `@import`, meta refresh, forms, iframes, SVG scripts, `<a ping>`, DNS prefetch, `<base>` tricks | Layered WebKit hardening ([below](#message-rendering)); the hostile-HTML suite in CI (ML-065) |
| Tracking | Remote images reveal when and where a message was opened | Nothing remote loads until allowed per message (for the session) or per sender (a list the user keeps); a forged From address can borrow a listed sender's permission, still https only and without cookies |
| Phishing | Link text shows one domain, the target is another | Real URL on hover; mismatch warning; links open in the browser, never in the app |
| Dangerous attachments | `.desktop`, `.sh`, `.AppImage`, `.jar` launched by a double-click; names that hide their type | Never opened directly; warning and Save only; safe names; content checked at Open |
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
- POP3 (M8): only the greeting and STLS cross in plaintext, and anything the server sends after STLS's `+OK` refuses the account (STARTTLS injection); CAPA is read over TLS only; USER and PASS go only over TLS; credentials with CR, LF or NUL are never sent. POP3 logs counts, sizes, timings and the mechanism, never UIDLs.

### Credentials

- Passwords and refresh tokens are stored only in the Secret Service keyring (oo7). Access tokens stay in memory.
- `config.toml`, `keys.toml`, `accounts.toml`, the database, the blob store and the logs never contain a secret: `engine/tests/no_secret_in_files.rs` adds accounts with a known dummy password and searches every file and trace-level log line for it.
- Removing an account deletes its secrets, database rows and blobs, in that order after its `accounts.toml` entry; a removal that fails or crashes after the entry is gone is journalled and finished at the next start. An entry deleted by hand only detaches the account: its data and secret stay until it is removed in Sealwax.
- Without a keyring, secrets are kept in memory for the session and a banner says so (ML-048: the window's banner, until dismissed for the session).
- A password typed again is checked on IMAP and SMTP before it replaces the stored one; a refused one is never stored.
- **Development builds only** (`cfg!(debug_assertions)`; release builds never read them): `SEALWAX_TEST_CA=<file>` replaces the system's trust with exactly that CA (the mail testbed's), and `SEALWAX_SECRETS=memory` keeps secrets out of the keyring. Self-tests that start the real engine (`engine-start`) use both.

### Message rendering

Every layer stays even when another would suffice ([ADR-011](DECISIONS.md#adr-011--reading-pane-approach-a-with-hardened-webkit-2026-10-04) and its S2 and S7 amendments). S2 checked them against 47 hostile cases: 0 connections, 0 TLS hellos and 0 script traces at a local canary, while a no-defence control showed the attacks were live. M5's suite (ML-065) runs 60 cases in `tools/ci.sh --ui`, S2's 47 plus M5's surfaces and a conversation of hostile bodies, with a DNS canary that records and refuses every name the app or WebKit looks up: production passes show 0 connections, 0 requests, 0 TLS hellos, 0 DNS lookups and 0 script traces. M1's reader (ML-027) passed the same 47 cases in three runs: as shipped, with real pointer clicks, and in pooled views after Show images. Every run shows 0 connections, 0 requests, 0 TLS hellos and 0 script traces; after Show images the canary saw only TLS hellos.

1. `enable-javascript-markup = false`: no script in a message ever runs; WebKit also strips `javascript:` URLs and `srcdoc` attributes. It makes `<noscript>` content live, so the other layers must stay. Sealwax's own helper scripts run in an isolated world.
2. `default-content-security-policy` set when each view is built: `default-src 'none'; img-src cid: data: https:; style-src 'unsafe-inline'; font-src cid: data:; form-action 'none'; base-uri 'none'; frame-src 'none'; upgrade-insecure-requests` (so remote images the user allows load over TLS).
3. A WebKit content filter blocks every URL except `cid:`, `data:` and `about:` until the user allows remote content for that message or sender.
4. Link clicks and form submits are intercepted in Sealwax's isolated world, because WebKit connects to a clicked link's host before `decide-policy` answers. Sealwax opens only absolute http(s) links with a host, and mailto links, after the link-mismatch check, in the default browser or mail handler (`gtk::UriLauncher`). Relative, `cid:`, `data:`, `file:` and `javascript:` targets are dropped, and a headless run never opens a link. `decide-policy` blocks every navigation as the backstop. A link whose visible text names another registrable domain than its target, or whose address puts a name before an `@` (`https://bank.example@evil.example/`), shows a warning on hover and asks before opening, with Cancel as the default; the real target shows in a strip on hover and focus, hosts that mix alphabets in punycode. Plain-text bodies link only http(s), `www.` and mailto targets (M5, ML-062).
5. An ephemeral network session (no cookies accepted, so allowed images can't link messages through a cookie; no cache on disk; TLS errors fail; downloads are cancelled). The runtime features `LinkDNSPrefetch`, `LinkPreconnect`, `LinkPrefetch`, `LinkPreload` and `SpeculationRulesPrefetch` are off (`Settings::set_feature_enabled`): the older `enable-dns-prefetching` and `enable-hyperlink-auditing` settings do nothing in WebKitGTK 2.52, and `<a ping>` is otherwise sent. Page cache, local storage, databases, WebGL, WebAudio, media and (in release builds) developer extras are off.
6. `cid:` parts come from a per-view URI scheme backed by the local blob store; a part belonging to another message never loads. Answers come from the main loop, never inside WebKit's call, and only for the load that asked: a view released, blanked or loaded again gets its request ended unanswered. A large message's inline parts come from the server by their section (`FetchPart`) and are served as the type the stored message declares (M5, ML-060).
7. View heights are capped at 16,000 px, and growth driven by viewport units is frozen (a hostile `150vh` would otherwise inflate the view).
8. WebKitGTK 6's web-process sandbox (bubblewrap) stays on. Only web processes run inside it; WebKit's network process is a plain child of the app.
9. **Show images** lifts the content filter for that message, in that reader, until Sealwax quits; nothing is stored. **Always for this sender** puts the sender's address (lower case) on a list in the store (schema v8); a message whose From address is on it loads its images with the filter off, under the same CSP and the same cookieless session. A forged From address borrows that permission: the images tell the forger that the message was opened and from where, which is what remote images always tell; nothing else loads. Hide, or the toggle turned off, forgets the sender. In a conversation each message has its own banner, and a view that comes back for a body far off screen gets the filter again unless that message or its sender is allowed (M5, ML-058). The CSP still admits only https images, upgrading http ones. Every other load, in any pooled view, keeps the filter: a released view gets it back before it loads anything else (M1).
10. Views refuse new windows, script dialogs, permission requests, file and colour choosers, fullscreen, page-initiated printing, notifications and HTTP authentication. The context menu keeps Copy, Copy Link, Copy Image and Select All, plus Inspect Element in debug builds. Console messages never reach stdout (M1).
11. If a web process ends, the message is rendered once more; after a second crash it is shown as text (M1).

Isolated-world helper scripts are **not** bound by the page's CSP for `fetch()`: in S7 a helper `fetch()` reached a local test server with the CSP alone, and only the content filter stopped it. So helper scripts fetch nothing but `blob:` URLs, and every view that runs helpers keeps the content filter. With script markup off, no markup script or event handler ran even without the CSP (S2, S7).

### Composer

The composer view uses the same layers as the reader, without `https:` for images, in a web context and process of its own: script markup off, the CSP at construction (`img-src data: cid:`), a content filter that blocks everything but `cid:`, `data:`, `about:` and `blob:`, an ephemeral session, every navigation and new window refused, `hardware-acceleration-policy` NEVER. Paste and drop go through the allowlist cleaner; links are limited to http(s) and mailto; pasted or dropped images become `cid:` parts stored in the blob store and served only to the composer view that holds them (M6: a hostile paste with S7's cases made 0 requests to a local canary).

HTML that enters a draft from elsewhere (a quoted or forwarded original, a draft read back from the Drafts folder) goes through the same allowlist in Rust (`domain::compose::clean`: an html5ever pre-pass, then ammonia), keeping `https:`, `cid:` and typed base64 `data:` images; the composer's CSP still never loads remote images. The quick reply is plain text in a text view: no HTML, nothing loads.

### Attachments

- Opened from a new private folder (0700) under `$XDG_RUNTIME_DIR/<app-id>/attachments` (the cache folder without a runtime folder), as a file created 0600 with a safe name, never executable, with the desktop's default application (`gtk::FileLauncher`); the folder is emptied when Sealwax next starts, and the runtime folder at logout.
- **Safe names** (`domain::attachment`): the last path component, without control or invisible characters, look-alike dots made real, no leading dot, dash or space, no trailing dot or space, no `..`, at most 200 bytes, `attachment.<ext>` from the type when nothing is left; Download all never overwrites (`name (2).pdf`). The card shows that name.
- **Never opened directly**, with a warning and Save only: programs, launchers, scripts, installers and packages by their last extension (case-insensitive, trailing dots and spaces ignored: `.desktop`, `.sh` and other shells, `.AppImage`, `.jar`, `.jnlp`, `.exe`, `.msi`, `.bat`, `.cmd`, `.com`, `.scr`, `.ps1`, `.vbs`, `.hta`, `.lnk`, `.run`, `.bin`, `.deb`, `.rpm`, `.apk`, `.snap`, `.flatpak` and `.flatpakref`, and the rest in the code) or MIME type; names that hide their type (direction overrides, a look-alike dot before what reads as an extension); and files whose content GIO identifies as one of those types when they are opened.
- Saved files (Save, Download all, Save as .eml) go through the file chooser (the portal's where there is one) and are never made executable.

### Print and Show Original

- **Print** renders the message in a view from the reader's pool (every layer above; remote images only where the user allowed them), held by a window that is never shown; Sealwax's isolated world adds the header as text. Page-initiated printing stays refused.
- **Show Original** shows the raw source as text in a read-only view, never rendered. For a large message stored as its text parts, Show Original and Save as .eml fetch the message whole first (`FetchOriginal`); the copy rebuilt from its text parts is never shown or saved.

### Outgoing mail

- Message-ID is generated by Sealwax with the sender's domain; mail-builder's `gethostname` feature and lettre's `hostname` feature are off; `hello_name` is 127.0.0.1.
- The transmitted copy never carries a Bcc header; the Sent copy is the transmitted bytes plus one `Bcc:` field (S7).
- A message is sent at most once (M6, [ADR-015](DECISIONS.md#adr-015-amendment--the-outbox-as-built-m6-2026-10-06)): the store records the send, with its built bytes, before SMTP DATA in a transaction committed with a full sync; a send left in that state when Sealwax stopped, or whose connection was lost while its data went out, is "may have been sent" and goes again only when the user says so, with the same bytes and Message-ID.
- No User-Agent or X-Mailer field. The Message-ID is always on the From address's domain (the builder refuses another); a forward carries no In-Reply-To or References. Names and subjects lose control characters before they are written. Recipients' addresses appear in logs only as counts, and refused ones never.
- `mailto:` links keep only their safe fields; `attach` / `attachment` stay ignored, and nothing is sent without the user's Send.

### Privacy

- Microsoft sign-in: the address never goes into the sign-in URL (no `login_hint`); the sign-in URL, code, tokens and the username are never logged (ML-032).
- Autodetect: the full address goes only to the domain's own HTTPS autoconfig; ISPDB, MX and SRV lookups use the domain. Plain-HTTP autoconfig is never used. A redirect that would carry the address to any other host is refused: S4b found `autoconfig.msn.com` redirecting to `www.msn.com` with the address kept, which S4's audit had missed. Hosts that resolve to non-public addresses are neither fetched nor probed.
- No avatars, favicons, BIMI or Gravatar lookups; no read receipts; no telemetry.
- New-mail notifications (M7) give the desktop's notification server the sender and the subject, the user's choice at M7's start; the text is mail content, so control and direction characters are dropped, it is capped (80 and 200 characters), and the body is markup-escaped where the server reads markup. omarchy-shell writes toasts under `~/.local/state/omarchy/notifications/`; `[notifications] show = "count"` sends only a count, and `enabled = false` nothing.
- Development and spikes never put the user's address or personal data in network requests ([ADR-018](DECISIONS.md#adr-018--logging-and-privacy-2026-10-04)).

### Logging

Logs never contain message bodies, subjects, addresses, folder names, search text, tokens or protocol traces; AUTH lines are always redacted. Logs use account ids and counts. The search query's types (`Query`, `Term`, the store's FTS expression) and anything holding a notification's text print counts or lengths in `Debug` (M7).

Third-party protocol crates are capped at `warn` whatever `RUST_LOG` says: S1 found that async-imap's trace logging prints the LOGIN password ([ADR-018 amendment](DECISIONS.md#adr-018-amendment--third-party-trace-logs-2026-10-04)). A test runs the testbed suites at trace level and greps the captured logs for credentials, tokens, addresses and folder names.

### Local data

- Private folders are created 0700 and files 0600 (database, blobs, drafts, temporary attachment copies under `$XDG_RUNTIME_DIR/<app-id>/attachments`).
- The app never edits `~/.config` outside its own folder, `~/.local/state/omarchy` or system files, except through opt-in commands the user confirms: the default mail handler (Make Sealwax the Default Mail App…, which asks first; M6) and Start Sealwax at Login (M7), which asks before it writes `$XDG_CONFIG_HOME/autostart/<app-id>.desktop` and, turned off, deletes only that file.
- A POP3 account's mail lives only in the store and the blob store once the server deletes it (M8): the reader, attachments and Show Original take it from there, never from a server again, and the disk cap never evicts it.
- Drafts, their parts and the outbox's built messages live in the store and the blob store (0600); a part's bytes stay until the draft is discarded or sent, even after the part is removed in the editor, and the disk cap never evicts them (M6).

## Known limitations

- **Plaintext keyring on this machine:** gnome-keyring's "Default keyring" is passwordless, so its file is stored unencrypted and protected only by full-disk encryption. Sealwax can't fix this; USER_GUIDE will explain how to set a keyring password.
- **Self-signed servers** (Proton Bridge, some self-hosted servers) can't be added until pinned exceptions arrive in 1.x.
- **Encrypted and signed mail** (PGP, S/MIME) is displayed gracefully but not decrypted or verified in 1.0: encrypted mail shows a notice in place of its body (the ciphertext is never shown, and the encrypted part can be saved); signed mail shows its content with "Signed · not checked"; signature parts stay out of the attachment cards (M5, ML-064).
- **Google verification** (only once Sign in with Google returns in 1.x): until the OAuth app is verified, Google shows an "unverified app" screen and limits use to test users ([OAUTH-SETUP.md](OAUTH-SETUP.md)).
