# Flutter UI composition fixture

Uses the real Flutter SDK and exact `flutter_riverpod: 3.4.2` dependency with a
committed lockfile. Dev dependencies resolve lint configuration includes used by
resolved packages, allowing conservative semantic cache tests. Repository tooling pins Flutter 3.47.9 / Dart 3.13.7 and
analyzer 14.4.0; companion 1.1.0 emits fact format 2.

From repository root:

```sh
mise run install-dart-indexer
mise run install-dart-semantics
mise run prepare-dart-fixtures
mise run check-dart-semantics
cargo test dart_ui_
cd tests/fixtures/dart/flutter-ui
dart analyze --fatal-infos
easement check --no-cache --format json
```

The committed source analyzes successfully and deliberately produces two easement
findings: `_header` at line 9 and `_ProfileHeader` at line 12. CLI tests also
exercise either rule alone and both disabled, then move header into a separate
file, rename it `ProfileHeader`, and verify Dart analysis plus a passing check.

Direct companion tests verify real resolved Flutter ancestry, private declaration
locations, return expressions and framework override targets. CLI cases cover
parts, aliases, indirect bases, Riverpod overrides, named locals, nullable/broad
returns, callbacks, coverage gaps, source selection, diff/baseline/suppression
behavior and cache invalidation. No emulator or application launch is needed.
