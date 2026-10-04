# OAuth setup: Google and Microsoft sign-in

Synced: 2026-10-04

"Sign in with Google" and "Sign in with Microsoft" need an OAuth client registered by the app's publisher ([ADR-008](DECISIONS.md#adr-008--oauth2-and-secrets-2026-10-04)). Only you can create these registrations: they belong to your Google and Microsoft accounts. This page walks through both; it takes about 20 minutes. Spike S5 (ML-013) is blocked until the client IDs exist.

The app is named **Sealwax** ([ADR-010](DECISIONS.md#adr-010-accepted--sealwax-2026-10-04)); name the development registrations "Sealwax (dev)". Use a dedicated test account for the first sign-ins.

Facts below were checked against Google's and Microsoft's documentation on 2026-10-04; provider consoles change, so S5 records anything that differs.

## Google (Gmail and Google Workspace)

1. Open the [Google Cloud console](https://console.cloud.google.com/) and create a project, for example "sealwax-dev".
2. Open **Google Auth Platform** (APIs & Services → OAuth consent screen in older layouts):
   - **Branding:** app name "Sealwax (dev)", your support email, developer contact.
   - **Audience:** user type **External**. Leave the publishing status on **Testing** for now and add your test Google account(s) under **Test users**.
   - **Data access:** add the scope `https://mail.google.com/` (full IMAP/SMTP access; Google classes it as *restricted*).
3. **Clients → Create client:** application type **Desktop app**, name "Sealwax (dev) desktop". Copy the **client ID** (`…apps.googleusercontent.com`) and the client secret.

What to know:

- **Redirect:** desktop clients use a loopback address, `http://127.0.0.1:<port>` (or `http://[::1]:<port>`), on a port the app picks; nothing to register. Custom URI schemes and copy/paste (OOB) flows are no longer supported by Google.
- **PKCE** is supported and recommended; Sealwax always uses it.
- **Client secret:** Google treats desktop clients as unable to keep secrets, so the "secret" is not confidential. Keep it out of git anyway.
- **Testing status:** refresh tokens expire after **7 days** for an External app in Testing status (unless only name/email/profile scopes are requested), so you would sign in again weekly.
- **Production status:** publishing without verification shows Google's "unverified app" warning and caps the number of users (100 for unverified apps — to verify in S5). A public release needs Google's verification for the restricted scope, which includes a security assessment.
- **Alternative without OAuth:** with 2-Step Verification on, a Gmail **app password** works with normal IMAP/SMTP login; the setup assistant offers it.

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

Scopes the app requests: `https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access` (plus `https://outlook.office.com/POP.AccessAsUser.All` for POP3 accounts). IMAP and SMTP then authenticate with SASL XOAUTH2.

## Where the IDs go

For development builds, either export them before building:

```bash
export SEALWAX_GOOGLE_CLIENT_ID="…apps.googleusercontent.com"
export SEALWAX_GOOGLE_CLIENT_SECRET="…"
export SEALWAX_MICROSOFT_CLIENT_ID="00000000-0000-0000-0000-000000000000"
```

or put them in the app's `config.toml` (the section names are final once M2 builds them):

```toml
[oauth.google]
client_id = "…apps.googleusercontent.com"
client_secret = "…"

[oauth.microsoft]
client_id = "00000000-0000-0000-0000-000000000000"
```

Never commit them to the repository. When the IDs exist, tell the agent; S5 moves from Blocked to Ready.
