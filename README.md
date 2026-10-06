# Sealwax

A fast, beautiful, minimal email client for [Omarchy](https://omarchy.org) — native GTK4 and libadwaita, written in Rust, following your live Omarchy theme and font.

![Sealwax in the matte-black theme, showing fictional mail](tools/progress/public/m1-gate-wide-matte-black-square-2026-10-04.png)

**Status:** version 0.2.0 (M4, triage and offline changes, approved on 2026-10-06); M5, the reading experience, is built and waits for its gate: whole conversations in the reader, safe links and remote images per sender, attachments that open safely, print and the message's source. Accounts are added without typing server settings (autodetect, passwords and app passwords, or Sign in with Microsoft), new mail arrives through IDLE, message bodies download when opened, and every account's state shows in the sidebar and a banner. Archive, delete, junk, move, star and read/unread work at once from the keyboard or the mouse, on one message or a selection, with Undo, and reach the server in the background, also after being made offline. Sending comes in M6, so it isn't ready for daily use yet.

- Product requirements: [docs/PRD.md](docs/PRD.md)
- Plan and current state: [docs/PLAN.md](docs/PLAN.md)
- Decisions: [docs/DECISIONS.md](docs/DECISIONS.md)
- Building: [docs/BUILD.md](docs/BUILD.md); testing: [docs/TESTING.md](docs/TESTING.md)
- Working on it with an agent: [AGENTS.md](AGENTS.md)

License: MIT.
