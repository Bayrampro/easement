# Contributing

Thank you for helping with easement. The conventions live in [AGENTS.md](AGENTS.md): read it before you start, and point your coding agent at it (most agents read it on their own).

## Start with an issue

Open an issue and agree on the design before writing code for:

- a new language;
- a new rule, rule kind or config key;
- a change to the output that agents read (text, `--format json`, exit codes).

easement treats every language the same way: a rule is defined once and works for all of them, and a language plugs in through the registry and an indexer. The issue is where a proposal finds that shape. A pull request for one of the changes above that has no agreed issue will be asked to start there.

Bug fixes and documentation fixes can go straight to a pull request.

## Pull requests

- One pull request, one change: a new language and new rules are separate pull requests.
- `mise run check` and `cargo test` pass; snapshot changes are reviewed with `cargo insta review`.
- Commits follow [Conventional Commits](https://www.conventionalcommits.org/). Rebase on `main` instead of merging it into your branch.
