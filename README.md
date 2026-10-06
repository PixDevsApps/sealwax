# Sealwax

A fast, beautiful, minimal email client for [Omarchy](https://omarchy.org) — native GTK4 and libadwaita, written in Rust, following your live Omarchy theme and font.

![Sealwax in the matte-black theme, showing fictional mail](tools/progress/public/m1-gate-wide-matte-black-square-2026-10-04.png)

**Status:** version 0.3.0, the MVP (M6, compose and send, approved on 2026-10-06): a composer in a window of its own with formatting, recipient suggestions, attachments and autosaved drafts, reply, reply all, forward and a quick reply under the conversation, and sending through an outbox with Undo send that never sends a message twice. Before it: whole conversations in the reader with safe links, remote images per sender and attachments that open safely (M5); accounts added without typing server settings (autodetect, passwords and app passwords, or Sign in with Microsoft), new mail through IDLE, and archive, delete, junk, move, star and read/unread at once with Undo, also offline (M2–M4). Search, notifications and background mode come in M7; real accounts haven't been tried yet.

- Product requirements: [docs/PRD.md](docs/PRD.md)
- Plan and current state: [docs/PLAN.md](docs/PLAN.md)
- Decisions: [docs/DECISIONS.md](docs/DECISIONS.md)
- Building: [docs/BUILD.md](docs/BUILD.md); testing: [docs/TESTING.md](docs/TESTING.md)
- Working on it with an agent: [AGENTS.md](AGENTS.md)

License: MIT.
