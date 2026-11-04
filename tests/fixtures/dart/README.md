# Dart and Flutter fixtures

One directory groups the Dart language fixtures. Each child remains a separate
Pub package so pure Dart tests do not inherit Flutter dependencies.

- `basic/`: Dart directives, dependency extraction and recorded SCIP evidence.
- `flutter-basic/`: minimal real Flutter SDK dependency-indexing fixture.

Other scenarios (caching, external dependencies, malformed sources, generated
files and Pub workspaces) use temporary packages created by `tests/cli.rs`.
They need no empty placeholder directories here. Runtime edits stay in scratch
copies; committed fixture sources retain their deliberate violations.

From repository root, prepare real Flutter dependencies with:

```sh
mise run prepare-dart-fixtures
```

See each fixture's README for expected results and extraction evidence. The
minimal `flutter-basic` package is also exercised directly by Rust unit and CLI
tests. Generated `.dart_tool` directories are ignored; lockfiles are committed
where Flutter dependency resolution needs reproducibility.
