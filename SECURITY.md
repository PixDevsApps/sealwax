# Security policy

## Reporting a vulnerability

Please report a security problem in Sealwax privately, through GitHub's private vulnerability reporting: **[Report a vulnerability](https://github.com/PixDevsApps/sealwax/security/advisories/new)** (also under the repository's Security tab). Don't open a public issue, pull request or discussion for it.

A report helps most when it says:

- which version of Sealwax (`sealwax --version`);
- what an attacker needs (a message they send, a mail server or DNS answer they control, a place on the network path) and what they get;
- how to reproduce it, or which code you read, and what you saw;
- whether you have told anyone else, such as the maintainers of a library Sealwax uses.

Please put everything in the report as text, and test only with accounts and servers that are yours.

## What happens next

The report stays private between you and the maintainer, and the answer comes in the report's own thread. Once the problem is confirmed, the fix comes in a release, and a GitHub security advisory is published with it, crediting you if you want. When the fault is in a library Sealwax uses, its maintainers are told privately before the fix is published.

## Supported versions

Security fixes go into the latest release only. **Check for Updates…** in Sealwax's main menu says whether you have it, and [INSTALL.md](INSTALL.md#updates) shows how to update.

## Scope

Sealwax's own code in this repository, including the copies of lettre, imap-proto and async-imap in `vendor/`. A problem in another library alone (WebKitGTK, GTK, a mail parser) belongs with that project; tell us as well when the way Sealwax uses it makes it worse.

How Sealwax protects you, and the limitations it already knows about, are in [docs/SECURITY.md](docs/SECURITY.md).
