# OAuth setup: Microsoft sign-in

Synced: 2026-10-07

"Sign in with Microsoft" needs an OAuth client registered by the app's publisher ([ADR-008](DECISIONS.md#adr-008--oauth2-and-secrets-2026-10-04)). Only you can create the registration: it belongs to your Microsoft account. This page walks through it.

**2026-10-07: Google dropped.** Sign in with Google is never built and Gmail is no longer a listed provider (user decision, [ADR-008 amendment](DECISIONS.md#adr-008-amendment--google-dropped-2026-10-07)); this page's Google section, kept for 1.x since 2026-10-04, is gone (it is in git history).

The app is named **Sealwax** ([ADR-010](DECISIONS.md#adr-010-accepted--sealwax-2026-10-04)); name the development registrations "Sealwax (dev)". Use a dedicated test account for the first sign-ins.

Facts below were checked against Microsoft's documentation on 2026-10-04; provider consoles change, so S5 records anything that differs.

## Microsoft (Outlook.com, Hotmail and Microsoft 365)

Outlook.com accepts only OAuth for IMAP and SMTP, so without this registration those accounts can't be added.

1. Open the [Microsoft Entra admin center](https://entra.microsoft.com/) → **Applications → App registrations → New registration**. If your personal Microsoft account can't register apps there, create a free Azure account first; it comes with an Entra tenant.
2. Register:
   - **Name:** "Sealwax (dev)".
   - **Supported account types:** "Accounts in any organizational directory and personal Microsoft accounts" (sign-in audience `AzureADandPersonalMicrosoftAccount`), so both Outlook.com and work accounts can sign in.
   - **Redirect URI:** platform **Mobile and desktop applications**, value `http://localhost`.
3. **Authentication:** confirm the `http://localhost` redirect is listed under Mobile and desktop applications. Microsoft ignores the port on localhost redirects, so the app can use any free port. (Microsoft prefers `127.0.0.1`, but the portal doesn't accept an `http://127.0.0.1` redirect without editing the manifest; `http://localhost` is fine. Sealwax listens on both 127.0.0.1 and ::1 for it.)
4. **API permissions → Add a permission:**
   - **APIs my organization uses →** search "Office 365 Exchange Online" → **Delegated permissions:** `IMAP.AccessAsUser.All`, `SMTP.Send`, `POP.AccessAsUser.All`.
   - **Microsoft Graph → Delegated:** `offline_access`, `openid`, `email` (usually present; `offline_access` gives refresh tokens).
   - No admin consent is needed for your own personal account. Work tenants may require their admin to consent.
5. Copy the **Application (client) ID** from the Overview page. There is no client secret: the app is a public client and uses PKCE.

Scopes the app requests: `https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid profile` (`engine::oauth::MICROSOFT_SCOPES`; `openid profile` bring the ID token whose `preferred_username` is the login name, ML-032). IMAP and SMTP then authenticate with SASL XOAUTH2. POP3 accounts don't use Microsoft sign-in in 1.0: POP would need `https://outlook.office.com/POP.AccessAsUser.All`, which isn't requested (M8; corrected 2026-10-07, this line said it was requested for POP3 accounts).

### As registered on 2026-10-05

The development registration "Sealwax (dev)" was made on 2026-10-05 in the owner's Azure tenant ("Default Directory"), with the agent driving the portal in the owner's Chrome and the owner confirming Register. The portal differed from the steps above, and one step proved unnecessary:

- **Supported account types** is a drop-down in the new form: choose "Any Entra ID Tenant + Personal Microsoft accounts". The Overview then reads "All Microsoft account users".
- **Redirect URI:** platform "Public client/native (mobile & desktop)", value `http://localhost`. Authentication (Preview) lists it under "Mobile and desktop applications". "Allow public client flows" stays Disabled: the authorization-code flow with PKCE doesn't need it.
- **API permissions (step 4): nothing to add.** Microsoft's guide ["Authenticate an IMAP, POP or SMTP connection using OAuth"](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth) (updated 2025-10-17) asks a delegated app only to register and then to request the full scopes at sign-in (above). Its "APIs my organization uses → Office 365 Exchange Online" step is for application permissions (client credentials), and that entry exists only in tenants with Exchange Online; this tenant has none, so searching for it found nothing. The default Microsoft Graph `User.Read` stays.
- **Unverified publisher:** the portal warns "End users cannot grant consent to newly registered multitenant apps without verified publishers". Per Microsoft's [publisher verification overview](https://learn.microsoft.com/en-us/entra/identity-platform/publisher-verification-overview) (updated 2026-06-15), when risk-based step-up consent is on, users in tenants other than the app's can't consent to an unverified multitenant app registered after November 2020 that asks for more than sign-in and profile. Verification needs a Microsoft AI Cloud Partner Program account and a publisher domain that isn't `*.onmicrosoft.com`. Personal Outlook.com accounts aren't another organization's tenant; S5 confirms their consent. Work accounts in other organizations may need their admin's consent until the publisher is verified (PLAN.md follow-up).
- The Application (client) ID is on the app's Overview page; it isn't recorded in this repository (below).

## Where the ID goes

As built in ML-032 (2026-10-05). Put the Microsoft client ID in `~/.config/sealwax/config.toml`; Sealwax applies it when you save the file, and it wins over one built in:

```toml
[oauth.microsoft]
client_id = "00000000-0000-0000-0000-000000000000"
```

Or build it in by exporting it before `cargo build`:

```bash
export SEALWAX_MICROSOFT_CLIENT_ID="00000000-0000-0000-0000-000000000000"
```

Never commit it to the repository. `config.toml` has no Google section (there is no Sign in with Google), and no settings file holds a client secret ([ADR-009 amendment](DECISIONS.md#adr-009-amendment--configtoml-and-keystoml-as-built-in-m1-2026-10-04)).
