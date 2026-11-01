---
name: easement
description: Keeps code inside the architecture declared in easement.yml. Use in a project that has an easement.yml before adding a file, a directory or an import, after changing code, when `easement check` fails, and when easement.yml or its baseline needs to change.
---

# easement

`easement` checks the dependencies between the components of a project against `easement.yml`. The config is the architecture the team chose: change the code to fit it, never the config to fit the code.

If the project has no `easement.yml`, easement is not set up yet: follow "Set up with a coding agent" in the README of <https://github.com/mokevnin/easement>.

## Before writing code

Run `easement explain <path>` for the file or directory you are about to add or change. It prints the component the path belongs to, what that component may depend on and why. Put new code where the imports it needs are allowed.

## After changing code

Run `easement check`. It exits 0 when the code matches the config, 1 on violations and 2 on an error.

- On a branch, `easement check --diff <base>` judges only the lines you added. Run the full `easement check` too when `easement.yml` or the baseline changed, because a rule change touches no import line.
- `--format json` gives the same report as data. When `confidence` is `partial`, a pass covers only the code an indexer read: say so, and name the files in `stats.uncovered`.
- On exit 2, read the message. For a missing indexer or missing dependencies, run `easement doctor`.

## When the check fails

A violation reads like `internal/handlers/books.go:6: handlers -> repository is not allowed (imports internal/repository/books.go) [dependency]`, followed by `because:` with the reason of the rule.

1. Read `because`: it says where the code should go instead.
2. Fix the code: go through the component the rule allows (call the service, not the repository), move the file into the component it belongs to, or turn the dependency around.
3. Do not edit `easement.yml`, regenerate the baseline or add an `easement:disable` comment to get the check through. If the rule itself looks wrong, stop and ask a human, with the violation and why you think the rule is wrong.

Two cases are cleanup, not exceptions:

- **Stale baseline entries**: a violation recorded in the baseline is gone. When the check reports stale entries and no new violations, run `easement baseline` to drop them; the baseline only shrinks. With new violations in the report, fix those first, or `easement baseline` would legalize them.
- **`unused-suppression`**: the excused import is gone, so delete the comment.

## Changing easement.yml

Change the config only when a human asks for it or agrees to it.

- A new directory that belongs to no component: add a component with `in`, and a rule with `may_depend_on` and `because`. With `strict: true` such a directory fails as `orphan` until it has a component or is listed in `unowned`.
- Every rule gets a `because` that tells the reader where the code should go instead.
- `easement suggest` lists the allowances no import uses and the imports nothing allows. `easement suggest --patch | git apply` drops the unused allowances and keeps comments and reasons. Leave alone the entries it lists apart as unreliable: they sit next to files no indexer covered.
- Run the full `easement check` after every change to the config.

`easement schema` prints the full format of `easement.yml`; the README describes every rule.
