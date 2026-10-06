# Sealwax

A fast, beautiful, minimal email client for [Omarchy](https://omarchy.org) — native GTK4 and libadwaita, written in Rust, following your live Omarchy theme and font.

![Sealwax in the matte-black theme, showing fictional mail](tools/progress/public/m1-gate-wide-matte-black-square-2026-10-04.png)

**Status:** M3, the sync engine's read path, was approved on 2026-10-06. Accounts are added without typing server settings (autodetect, passwords and app passwords, or Sign in with Microsoft), new mail arrives through IDLE, changes made elsewhere follow within minutes, message bodies download when opened, and every account's state shows in the sidebar and a banner. Triage (archive, delete, flags) and sending come in M4 and M6, so it isn't ready for daily use yet.

- Product requirements: [docs/PRD.md](docs/PRD.md)
- Plan and current state: [docs/PLAN.md](docs/PLAN.md)
- Decisions: [docs/DECISIONS.md](docs/DECISIONS.md)
- Building: [docs/BUILD.md](docs/BUILD.md); testing: [docs/TESTING.md](docs/TESTING.md)
- Working on it with an agent: [AGENTS.md](AGENTS.md)

License: MIT.
