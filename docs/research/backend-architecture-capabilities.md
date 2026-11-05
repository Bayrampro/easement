# Easement, ArchUnit, Import Linter and Deptrac: concrete tasks

Survey date: 2026-11-04. Easement statements describe the current local [README](../../README.md), not a separately tested release. Competitor statements use first-party documentation and source, retrieved on this date. ArchUnit's guide identifies version 1.5.1; Deptrac source links use its `4.x` branch. This is a documentation/source survey; no tools were installed or exercised.

**Native** means a supplied contract, configuration option or fluent rule operator; ArchUnit's normal Java DSL counts as native even though it is code. **Custom** means implementing a predicate, condition, graph traversal or external wrapper beyond those operators. **Not verified** means no corresponding built-in capability was established, not proof that it is impossible.

## Policy tasks

| Concrete task | Easement | ArchUnit | Import Linter | Deptrac |
| --- | --- | --- | --- | --- |
| Only handlers may use services; services may use repositories | Native component allow-list, incoming allow-list, layers, explicit bans | Native class/package dependency rules, layer/onion DSL, module allowed dependencies [A1][A2] | Native forbidden/layers/protected contracts; shape the forbidden sets for an outgoing allow-list [I1][I2][I3] | Native layers plus default-deny `ruleset` [D1] |
| Permit `billing/api`, hide `billing/internal` even when billing itself is allowed | Native `public` path globs | Native exposed packages in modularization rules or package dependency predicates [A1][A2] | Native `protected` contract on internal modules with billing's modules as allowed importers; module granularity [I2] | Native `private: true` collectors, or `@deptrac-internal`; privacy overrides allowances [D2][D3] |
| Permit only named API types/functions, or ban one dangerous method | Native `public_symbols`/`forbid_symbol`, **only Go, TS, Python, Rust, Ruby** | Native class/member/access/call predicates; can target methods, constructors, fields and annotations, substantially deeper on JVM [A2][A3][A4] | No verified symbol-level contract; graph is modules, not API members [I4] | Can isolate individual class-like/function tokens in layers; collectors are richer than paths. No verified arbitrary receiver-method call allow-list equivalent [D2][D4] |
| Reject `web -> facade -> storage` although web does not import storage directly | Native `must_not_reach`; file graph by default, optional `reach: symbols` follows referenced definitions with conservative whole-file fallback | Native `transitivelyDependOnClassesThat`; class dependency traversal, not arbitrary runtime/data-flow tracing [A2] | Native forbidden, independence and layers check indirect paths by default [I1][I3][I5] | No verified general forbidden-transitive-reach contract. `+Layer` expands **allowed policy**, not a forbidden runtime reach assertion [D5] |
| Automatically isolate each feature, while permitting its own UI to call its own data layer | Native `<feature>` equality across different component patterns; `*` opts out; `isolated` handles siblings | Native captured package slices and independence/cycles. Correlating selected sublayers while allowing other cross-feature APIs may need additional rules/custom predicates [A1] | Native independence with wildcards isolates siblings. Containers repeat layers per feature, but do **not** forbid cross-container dependencies; combine contracts. Named cross-pattern equality is not verified [I3][I5] | Native explicitly enumerated feature layers/rulesets; no verified named-capture equality policy [D1][D2] |
| Every resource directory must import shared code, contain a test/README and a generator directive | Native per-directory `must_depend_on`, `must_use_external`, `requires_file`, `requires_directive` | Positive dependency conditions are native per selected class. Equivalent directory/file/comment requirements need custom filesystem/source checks; bytecode does not retain arbitrary comments [A2][A4] | Custom contract can inspect the graph for required imports; extra file/comment checks require source/filesystem code [I4] | No verified built-in per-directory required import/file/directive contract [D5] |
| Every `*Repository` type belongs in repository | Native `defines` claims names; indexed languages only | Native name predicates plus package placement; also inheritance, annotations, modifiers, signatures [A2][A4] | No verified built-in type-name placement rule [I6] | Native name-based collection, but selecting by name alone assigns ownership; checking physical placement additionally needs a composed collector plus unassigned/violation gate. Not identical to `defines` [D2][D6] |
| Architecture picture is the allowed dependency policy | Native Mermaid arrows in Markdown or `.mmd` | Native executable PlantUML component diagrams; **not unique to easement** [A5] | Not verified among supplied contracts [I6] | Mermaid/Graphviz are documented output formatters; no verified diagram-as-input policy [D7] |
| Find unreachable source files from entries | Native `entry`/`unused`, conservative language and coverage limits | No verified native entry-rooted dead-source-file gate; custom traversal is possible from class/access graph [A4] | Custom traversal could use graph; no native dead-file contract verified [I4][I6] | No verified entry-rooted dead-file gate. `debug:unused` finds unused **ruleset allowances**, not dead code [D6] |
| Reject architectural code outside all declared components | Native `strict`, plus optional `unowned`; fail disappeared patterns unless optional | Rule selection/scope is explicit; empty selections fail by default, but exhaustive source ownership needs an additional rule [A6] | Native `exhaustive = true` catches unlisted modules **inside layer containers**, with explicit ignores; not a universal file-ownership gate [I3] | Native `debug:unassigned` can fail CI; uncovered dependencies can separately fail analysis [D3][D6] |

These are architecture constraints, not evidence of complete runtime dependency discovery. In particular, easement's definition-level reach is not general interprocedural data flow, and compiled/reference-based tools cannot infer arbitrary reflection or runtime-computed imports. ArchUnit distinguishes recorded accesses, class dependencies and member resolution; missing class bytecode can leave stubs without annotation or superclass information [A2][A7].

For the precise task “this API method must not transitively reach that dangerous method,” ArchUnit supplies native **direct** method-call conditions and native **class-level** transitive dependency conditions. A method-to-method transitive traversal requires custom code over its code-unit/access model; these two supplied operators do not establish a native method-specific transitive rule [A2][A3][A7]. Import Linter has no method nodes in its contract graph [I4]. Deptrac's function-call analyzer does not, by itself, establish a method-specific transitive policy [D4].

## Import Linter details that a short comparison must preserve

- **Forbidden** checks source-to-forbidden module reach, including indirect imports and descendants; `as_packages = false` changes descendant scope. It can include external modules [I1].
- **Protected** is an incoming **direct** importer allow-list; it is useful for facade/internal-module policies, and does not mean “no transitive access” [I2].
- **Independence** forbids paths in both directions, including indirect paths; wildcard module sets support recurring feature families [I5].
- **Layers** rejects upward reach including indirect paths. `a | b` makes siblings independent; `a : b` permits sibling dependencies. Containers repeat a layer template; cross-container imports remain allowed unless another contract forbids them. Required layers must exist; `(optional)` layers may be absent. `exhaustive` detects extra children only with containers [I3].
- **Acyclic siblings** is also supplied, so comparing only the four older contract types is incomplete [I7].
- `broken_contract_guidance` prints migration instructions; `unmatched_ignore_imports_alerting` defaults to `error`. Rationale and stale exceptions are consequently not easement-only ideas [I6].

## Adoption, review and agent workflow

| Task | ArchUnit | Import Linter | Deptrac | Easement distinction |
| --- | --- | --- | --- | --- |
| Freeze existing violations and prevent regression | Native `FreezingArchRule` ignores known debt and automatically removes fixed violations. `freeze.refreeze=true` deliberately accepts current violations. Line numbers are ignored by default [A8] | Native `ignore_imports`, with unmatched ignores failing by default; no generated freeze store verified [I6] | Native baseline/`skip_violations`; unmatched skips are reported as errors [D3][D7] | A baseline itself is not unusual. Easement explicitly fails on stale entries; normal `baseline` only shrinks, `--accept-new` expands |
| Can updating the debt file accidentally accept new debt? | Normal frozen evaluation only removes obsolete entries; explicit refreeze accepts debt. Store creation/update can be separately controlled [A8] | Editing ignore lists can accept debt; no verified separate shrink command [I6] | Baseline formatter combines current violations and skipped violations and overwrites its output: regeneration can accept new debt [D8] | The explicit shrink-versus-accept command interface is more distinctive than “ratchet” alone |
| Gate only new/changed import lines relative to merge-base | No built-in equivalent verified; wrapper/custom result filtering | No equivalent in documented CLI [I8] | No equivalent in analysis command [D9]; `changed-files` is impact inspection, not changed-line gating [D6] | Native `--diff` keeps full graph but filters report by changed lines; config-only policy changes need full check |
| Explain component ownership and the violating dependency | Native failure locations and graph API; an easement-style general explain command needs custom tooling [A4] | Native broken-contract reports and guidance; custom reporting available [I4][I6] | Native `debug:token`, `debug:layer`, `debug:dependencies` [D6] | `explain` combines ownership, allowances and reasons; `why` reports direct imports or shortest chain |
| Show impact of changed files | Custom use of dependency graph [A7] | Custom use of graph [I4] | Experimental `changed-files --with-dependencies` lists affected/dependent **layers** [D6] | Native `impact --diff` returns dependent **files**, grouped by component, with partial-confidence metadata |
| Tighten unused allowances | Custom analysis of graph/policy | No verified native suggestion command [I8] | Native `debug:unused` and configurable usage threshold [D6] | `suggest --patch/--check`, unused diagram arrows, candidate isolation; refuses unsafe removals beside unindexed files |
| Explain reliability of a passing result | Scope/import options, missing-class stubs and cached imported classes are documented [A7] | Graph caching and `--no-cache`; no matching structured confidence contract verified [I9] | Uncovered/unassigned token diagnostics; these describe layer assignment, **not all files an indexer failed to read** [D3][D6] | Uniform `confidence`, reasons, uncovered paths and cache provenance in machine output; partial scope also explicitly labelled |

ArchUnit has native `.because(reason)` and customizable descriptions, so a rationale printed with a violation is not unique either [A6]. Its modularization, class/member rules and custom predicates let JVM teams express constraints substantially deeper than easement's Java support. Deptrac likewise provides PHP-specific class/trait/interface/attribute/Composer collectors and optional function-call analysis that easement's PHP class-only dependencies do not match [D2][D4].

## Assessment

The strongest evidenced differentiation in this subset is the **combination** of one language-independent policy and debt model, changed-import-line review, explicit trust metadata, graph exploration and safe policy tightening. Named slice equality and per-directory import/file/comment obligations are also unusually direct configuration tasks here. This does not establish global uniqueness against the full ecosystem.

Do not claim baseline ratchets, rationale messages, slices, public APIs, unused-allowance discovery, impact inspection or executable architecture diagrams as inventions of easement: these three competitors supply clear precedents or close equivalents. Do not compare easement's best supported language to competitors while implying all easement features work in every listed language. Java/PHP/C# currently lack its symbol-use policies; Kotlin/Swift lack indexed naming; unused-file checking deliberately becomes conservative with missing coverage.

The current README comparison's “no naming or call chains” statement is stale relative to its own `defines` and `reach: symbols` sections. A more precise distinction is that easement has type-name ownership and selected definition reach, while ArchUnit retains much richer JVM class/member/signature/annotation rules.

## Primary sources

[A1]: https://www.archunit.org/userguide/html/000_Index.html#_the_library_api
[A2]: https://github.com/TNG/ArchUnit/blob/main/archunit/src/main/java/com/tngtech/archunit/lang/syntax/elements/ClassesShould.java
[A3]: https://github.com/TNG/ArchUnit/blob/main/archunit/src/main/java/com/tngtech/archunit/lang/conditions/ArchConditions.java
[A4]: https://github.com/TNG/ArchUnit/blob/main/docs/userguide/007_The_Lang_API.adoc
[A5]: https://github.com/TNG/ArchUnit/blob/main/archunit-example/example-plain/src/test/java/com/tngtech/archunit/exampletest/PlantUmlArchitectureTest.java
[A6]: https://github.com/TNG/ArchUnit/blob/main/archunit/src/main/java/com/tngtech/archunit/lang/ArchRule.java
[A7]: https://github.com/TNG/ArchUnit/blob/main/docs/userguide/006_The_Core_API.adoc
[A8]: https://github.com/TNG/ArchUnit/blob/main/archunit/src/main/java/com/tngtech/archunit/library/freeze/FreezingArchRule.java
[I1]: https://import-linter.readthedocs.io/en/stable/contract_types/forbidden/
[I2]: https://import-linter.readthedocs.io/en/stable/contract_types/protected/
[I3]: https://import-linter.readthedocs.io/en/stable/contract_types/layers/
[I4]: https://import-linter.readthedocs.io/en/stable/custom_contract_types/
[I5]: https://import-linter.readthedocs.io/en/stable/contract_types/independence/
[I6]: https://import-linter.readthedocs.io/en/stable/contract_types/
[I7]: https://import-linter.readthedocs.io/en/stable/contract_types/acyclic_siblings/
[I8]: https://import-linter.readthedocs.io/en/stable/get_started/run/
[I9]: https://import-linter.readthedocs.io/en/stable/caching/
[D1]: https://github.com/deptrac/deptrac/blob/4.x/docs/concepts.md
[D2]: https://deptrac.github.io/deptrac/collectors/
[D3]: https://deptrac.github.io/deptrac/concepts/
[D4]: https://deptrac.github.io/deptrac/configuration/
[D5]: https://github.com/deptrac/deptrac/blob/4.x/docs/configuration.md
[D6]: https://deptrac.github.io/deptrac/debugging/
[D7]: https://deptrac.github.io/deptrac/formatters/
[D8]: https://github.com/deptrac/deptrac/blob/4.x/src/DefaultBehavior/OutputFormatter/BaselineOutputFormatter.php
[D9]: https://github.com/deptrac/deptrac/blob/4.x/src/Supportive/Console/Command/AnalyseCommand.php
