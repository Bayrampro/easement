# Dart semantic extraction evidence

Historical Stage 2 extraction artifacts below were validated with Flutter 3.47.9, bundled Dart 3.13.7, `scip_dart` 1.8.1,
analyzer 14.4.0, companion 1.0.0, and semantic fact format 1. Current companion 1.1.0 emits format 2 and adds
Flutter composition facts; regenerate runtime facts with the current launcher.

## Extraction decision

`evidence.scip` comes from the released Stage 1 indexer over these sources.
`scip-summary.json` records its symbol kinds, signature text and relationships.
SCIP provides resolved identities, source occurrences, declaration kinds, textual
signatures and unparameterized implementation relationships. Its `load` signature
is `Future<List<WireOrder?>> load()`: alias spelling and provenance are lost there.
These records do not provide the structured recursive types, operation context,
substituted inherited API or modifier fields needed by all five semantic rules.
Parsing displayed signatures would not recover that missing evidence reliably.

The companion therefore owns semantic facts; Stage 1 retains SCIP dependency
analysis. `evidence.json` records real resolved modifier, alias, signature,
supertype, constructor/reference and Flutter facts with project-relative source
locations. SDK/package types retain their defining library URIs. Source positions
are one-based and use Dart UTF-16 columns.

## Verified capabilities

| Capability | Evidence |
| --- | --- |
| Modifiers and identity | `OrderRepository` is an abstract interface in `package:example/orders.dart`. |
| Alias expansion | Its `load` return contains `Orders`, `List<WireOrder?>` and the resolved `WireOrder` identity. |
| Interface relationship | `Storage` resolves to `OrderRepository`; CLI cases also cover indirect inheritance, wrong-library namesakes and generic arguments. |
| Inferred types and operations | Companion tests verify inferred locals; bootstrap facts distinguish construction from other references. |
| API shape and substitutions | CLI and companion tests cover fields, getters/setters, constructors, parameters, records, callbacks and inherited generic members. |
| Real Flutter | A temporary package using `flutter-basic` dependencies resolves `Context = BuildContext` through `flutter/widgets.dart` to `package:flutter/src/widgets/framework.dart`. |
| Analysis errors | Companion tests introduce an unresolved type and require error facts. CLI tests preserve proven findings with partial confidence. |
| Resolution and caching | CLI cases exercise imported local aliases, role-only changes, missing tools, incompatible facts, baseline/diff/suppression and mixed-language scope. |

Raw tool timings are recorded in `evidence.json`. They are single local process
measurements, under concurrent verification load, without semantic cache reuse.
The companion adds another analyzer process to Stage 1; these measurements are
observations rather than a performance guarantee. No emulator or Flutter build
is involved.

## Reproduce

From the repository root:

```sh
mise run install-dart-indexer
mise run install-dart-semantics
mise run prepare-dart-fixtures
mise run check-dart-semantics
cargo test dart_semantic_ -- --test-threads=4
cd tests/fixtures/dart/semantic
dart pub get
../../../../target/dart-indexer/scip_dart --output /tmp/dart-semantic.scip .
../../../../target/debug/easement doctor
../../../../target/debug/easement check --no-cache --format json
../../../../target/debug/easement check --config clean.yml --no-cache
../../../../target/debug/easement check --config mvvm.yml --no-cache
```

The default and Clean Architecture examples deliberately fail because `Orders`
exposes `WireOrder`. Change only `List<WireOrder?>` to `List<Order>` and both pass.
The independent MVVM example checks the ViewModel API and passes in either case.

Role defaults use existing components or project-relative globs. Explicit
selectors override defaults; same-precedence conflicts fail configuration.
Generic contract associations match named type arguments and nullability exactly.
Unresolved types and missing requested files produce partial analysis. Windows
launchers, record/function association-argument syntax and runtime
business-completeness checks are not provided. Flutter composition rules are
covered by the separate `flutter-ui` fixture.
