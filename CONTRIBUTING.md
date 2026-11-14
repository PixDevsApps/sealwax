# Contributing to Sealwax

Thank you for helping. Sealwax is a small project with one maintainer, so a little care up front saves everyone time.

## Ways to help

- **Found a bug?** [Open an issue](https://github.com/PixDevsApps/sealwax/issues/new/choose) with the bug form. The lines under **Help → About Sealwax → Troubleshooting → Debugging Information** tell us your version, how Sealwax was installed and your system, without anything about your mail.
- **Have an idea?** Use the idea form, or start a thread under [Discussions → Ideas](https://github.com/PixDevsApps/sealwax/discussions/categories/ideas).
- **Have a question?** Ask in [Discussions → Q&A](https://github.com/PixDevsApps/sealwax/discussions/categories/q-a), or read the [user guide](https://pixdevs.com/guides/sealwax/) first.
- **No GitHub account?** Write to **support@pixdevs.com**. **Help → Contact Support…** in Sealwax gives you the address and the lines that help.
- **A security problem?** Never in a public issue, pull request or discussion: follow the [security policy](SECURITY.md).
- **Tried Sealwax with a provider, distribution or architecture we don't list?** Say how it went in an issue or a discussion. Reports like that have shaped several releases.

## Before a pull request

- **Small fixes** (a bug, a typo, a wrong word in the guide): open the pull request directly.
- **Anything larger** (a feature, a new setting, a dependency, a change in how something looks or works): open an issue first and wait for a yes. Sealwax keeps its scope lean on purpose ([ADR-013](docs/DECISIONS.md#adr-013--lean-10-scope-2026-10-04)), and it is a pity to turn away work that is already done.
- **Keyboard shortcuts** are the maintainer's decision: they follow Omarchy's rules ([ADR-017](docs/DECISIONS.md#adr-017--keymap-scoped-one-key-shortcuts-under-omarchy-rules-2026-10-04)), and a new command gets no default key unless agreed.
- **Look and feel** starts from [DESIGN.md](docs/DESIGN.md), and Sealwax follows the live Omarchy theme and font while still running outside Omarchy.

## Building

[BUILD.md](docs/BUILD.md) lists the toolchain (Rust stable, MSRV 1.92) and the system libraries (GTK 4, libadwaita, WebKitGTK 6.0, SQLite). Then:

```sh
cargo build
cargo run -p sealwax
```

A development build keeps its own folders (`sealwax-devel`) and app-id, so it starts with no accounts and never touches an installed Sealwax's mail.

## Checks

Every pull request must pass:

```sh
bash tools/ci.sh
```

It runs formatting, Clippy with `-D warnings`, the MSRV check, rustdoc, `cargo deny`, the tests and the build. Add the others when your change calls for them:

- `SEALWAX_CI_DISPLAY=19 bash tools/ci.sh --ui` for anything in the window: a headless run, the self-test scripts and the hostile-HTML suite. It never opens a window on your desktop: GTK programs run headless through `tools/headless.sh` ([TESTING.md](docs/TESTING.md#headless-runs)).
- `bash tools/ci.sh --testbed` for IMAP, POP3 or SMTP changes: the suites against real mail servers in Docker ([BUILD.md](docs/BUILD.md#mail-testbed)).

In the pull request, say which checks you ran and on what system. "Not run" is a fine answer; we would rather know.

## How the code is laid out

- The crates depend in one direction: `sealwax-domain` ← `sealwax-ui-model` ← `sealwax-infrastructure` ← `sealwax-engine` ← `sealwax` (the app). `domain` and `ui-model` do no I/O; `infrastructure` does local I/O only; `engine` is the only crate with tokio and the network; `app` is the only one that links GTK ([ADR-003](docs/DECISIONS.md#adr-003--layering-with-an-engine-crate-tokio-only-there-2026-10-04)).
- Nothing blocks the GTK thread, and the GTK thread never runs SQL.
- Every command a user can run is an `ActionId`; the menus, the command palette and the shortcuts are made from that registry.
- Mail content is hostile: no script in a message ever runs, and nothing remote loads until the user allows it ([docs/SECURITY.md](docs/SECURITY.md)).
- Never log message bodies, subjects, addresses, folder names, tokens or protocol traces.

## Style

- Rust 2024, `clippy -D warnings` clean under the workspace's lint policy ([ADR-025](docs/DECISIONS.md#adr-025--workspace-hygiene-a-lint-policy-the-msrv-and-dependency-checks-in-ci-the-spikes-archived-2026-11-04)): every `#[allow]` carries a `reason`, every `unsafe` a `SAFETY:` comment, and shipped code has no `unwrap`.
- Match the code around yours, and keep comments to what the code can't say.
- Test data is fictional: never put real mail, addresses or account details in a test, a screenshot or a log you attach.
- Update the documents your change affects in the same pull request: the [user guide](docs/USER_GUIDE.md) for anything a user sees, and [DECISIONS.md](docs/DECISIONS.md) for a choice with a long-term cost. The maintainer writes the [CHANGELOG](CHANGELOG.md) at release time.

## After you open it

The maintainer reviews, may ask for changes, and may finish a pull request on your branch with your commits kept. Contributors are thanked in the release notes and in the README.

By contributing, you agree that your work is licensed under the project's [MIT License](LICENSE). Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).
