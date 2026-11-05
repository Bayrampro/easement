# TypeScript and Go architecture gate comparison

Checked on 2026-11-04 against first-party documentation and schemas, without installing or executing competitors. Easement's reference is the current local [README](../../README.md). Online references follow moving default branches; JS Boundaries currently identifies its docs as 7.4.1 (one dependency metadata page still says 7.4.0). These are documentation-level findings, not verified runtime equivalence.

**N** means a documented built-in capability; **E** means the same constraint can be expressed through documented declarative selectors, without writing a checker; **C** requires user code; **NV** means no built-in equivalent was verified in the reviewed references. NV does not prove impossibility. A configuration written in JavaScript is still declarative when it only supplies data or a shipped helper.

## Easement reference behavior

The README documents these distinct contracts; collapsing them into one feature such as “boundaries” hides important differences.

| Contract | Easement semantics |
| --- | --- |
| Instances and named slices | `isolated` separates instances of a glob. Matching `<feature>` placeholders on both ends require equal captures; plain `*` deliberately permits any slice. |
| Public interface | `public` restricts target paths; `public_symbols` restricts resolved names. Symbol bans follow resolved definitions, aliases and typed receiver uses. Symbol rules cover Go, TS, Python, Rust and Ruby, with documented language limits. |
| Directed policy | Default deny between components, outgoing allow/deny, `allow_importers` on a target, ordered/sibling/closed layers. Explicit deny wins. |
| Graph policy | Component cycles, transitive `must_not_reach`, and entry-reachability `unused`. `strict` means component ownership, not graph connectivity. |
| Structure | Per-directory required internal/external import, file glob and comment directive; forbidden files; `defines` claims matching type names for one or more components. |
| Packages | External import allow/deny; nearest-manifest declaration checking for TS/JS, Go and Rust; separate `packages` edges from manifest declarations, including unused declarations. Cargo feature ownership is a further contract. |
| Policy maintenance | Mermaid arrows as an input policy; stale arrows and unused allowances in `suggest`; comment-preserving pruning patch and CI mode. |
| Adoption and queries | Existing-violation baseline, stale failure, shrink-only default; changed-import-line diff; `explain`, direct/shortest-chain `why`, transitive impact. |
| Trust | Structured coverage/partial-analysis fields and cache disclosure; unreliable suggestions separated and excluded from automatic pruning. A clean exit is not a proof of full coverage. |

## Dependency-cruiser

The closest broad counterexample in this set. Do not describe feature slices, reachability, required imports, baselines or impact analysis as easement-only.

| Contract | Documented equivalent and limit |
| --- | --- |
| Named slices / isolation | **E:** regex captures in `from.path`, referenced by `$1`, `$2`, etc. in `to.path`/`pathNot`; the docs give cross-component isolation explicitly. |
| Transitive forbidden / unused | **N:** `reachable: true` forbids transitive paths; `reachable: false` detects files unreachable from chosen roots. Reachability selectors are limited to paths. |
| Disconnected files | **N:** `orphan` means no incoming **and** no outgoing edges. This differs from easement's unowned-file `orphan`; unreachable subgraphs require `reachable: false`. Scan directories/globs to discover disconnected files. |

These are native rules, not custom JavaScript predicates. [Rule reference](https://github.com/sverweij/dependency-cruiser/blob/main/doc/rules-reference.md#group-matching)

| Contract | Documented equivalent and limit |
| --- | --- |
| Allow/deny, incoming, closed layers | **E:** `allowed`, `forbidden`, `from` and `to` selectors express outgoing restrictions, target-side importer restrictions and allowed adjacent layer pairs. No dedicated ordered/closed-layer syntax was verified. Explicit forbidden rules remain independent. |
| Public paths / symbols | **E** for path boundaries via source/target selectors. **NV** for resolved symbol-use bans or symbol allowlists. The rule schema describes modules/paths/dependency attributes, not a symbol identity model. |
| Required imports | **N:** `required` uses `module` and `to`; checks each selected module, with optional transitive reachability. This is not easement's per-directory existential requirement. |
| File/directive/naming ownership | **NV** for arbitrary required files/comment patterns and type-definition ownership. Path matching alone does not inspect definitions or prove a sibling file exists. |

The distinction between built-in selectors and new analyses follows the [configuration schema](https://github.com/sverweij/dependency-cruiser/blob/main/src/schema/configuration.schema.json). Custom config generation is possible but is not a built-in semantic checker. [Rule-writing tutorial](https://github.com/sverweij/dependency-cruiser/blob/main/doc/rules-tutorial.md)

| Contract | Documented equivalent and limit |
| --- | --- |
| Baseline / stale | **N:** `--baseline`, `--ignore-known`; `baseline.mode: shrink-only` and `staleEntriesSeverity: error` reproduce the core ratchet. Defaults differ: full replacement and stale warning. |
| Impact / diff | **N:** `--reaches` selects transitive dependents; `--affected <revision>` selects changed modules and their transitive dependents. This does not establish easement's changed-import-line violation semantics. |
| Why / explain | **N:** `err-long` prints rule comments; focused graph/text views expose dependencies. **NV** for easement's exact prospective-file explanation and shortest-chain query commands. |

See [CLI](https://github.com/sverweij/dependency-cruiser/blob/main/doc/cli.md#--baseline-create-or-update-a-known-violations-baseline) and [baseline options](https://github.com/sverweij/dependency-cruiser/blob/main/doc/options-reference.md#baseline).

External dependencies have explicit npm classifications (`npm-dev`, `npm-peer`, `npm-no-pkg`, etc.); rules can reject undeclared imports. Nearest `package.json` is the default; `combinedDependencies` can combine ancestor declarations. This overlaps `manifests`, not verified declaration-only manifest edges (`packages`). Mermaid is an output reporter; Mermaid-as-policy and unused-policy pruning were **NV**. [Options reference](https://github.com/sverweij/dependency-cruiser/blob/main/doc/options-reference.md#mono-repo-behaviour---combineddependencies), [dependency classification](https://github.com/sverweij/dependency-cruiser/blob/main/src/schema/cruise-result.schema.json)

JSON includes graph, violations, options and diagnostics such as `couldNotResolve`, `followable`, and `matchesDoNotFollow`. It is inaccurate to claim competitors lack machine-readable uncertainty. An inventory of all expected source files versus visited files, an overall easement-style partial-confidence contract, and conservative suggestion patching were **NV**. [Output format](https://github.com/sverweij/dependency-cruiser/blob/main/doc/output-format.md)

## ESLint Plugin Boundaries / JS Boundaries

Current documentation uses `boundaries/dependencies`; older `element-types`, `external` and `entry-point` rules are deprecated wrappers, not absent functionality. [Rule overview](https://www.jsboundaries.dev/docs/rules/)

| Contract | Documented equivalent and limit |
| --- | --- |
| Slices / instances | **E:** descriptor captures plus templates can compare the destination family/domain with the importer's capture; docs show rejecting a different family. |
| Incoming and outgoing | **E:** policies support `from`, `to`, `allow`, `disallow`; default deny is supported. Unlike easement's global deny precedence, the last matching policy wins. |
| Layers / closed layers | **E:** permitted type pairs express layer order, siblings and no-skipping constraints. A native ordered-layer declaration was **NV**. |
| Public paths | **N/E:** entry-point restrictions use element-relative `fileInternalPath`; generic dependency selectors now express the same policy. |

See [selectors and captures](https://www.jsboundaries.dev/docs/selectors/), [policy ordering](https://www.jsboundaries.dev/docs/rules/dependencies/), [entry-point migration](https://www.jsboundaries.dev/docs/rules/entry-point/).

| Contract | Documented equivalent and limit |
| --- | --- |
| Symbol/import names | **N:** `dependency.specifiers` matches imported/exported names. **Not equivalent** to definition-resolved `public_symbols` or typed receiver-use bans: metadata is derived from import/export syntax. Namespace-member and downstream typed uses were not established. |
| External imports | **N:** policies can target module origin/source, including external/core modules; enable the documented broader origin checks. |
| Unknown / ignored files | **N:** no-unknown-files, no-unknown-dependencies and no-ignored-dependencies establish architectural coverage of the linted files. They do not establish coverage of files ESLint never linted. |

See [dependency selector](https://www.jsboundaries.dev/docs/selectors/dependency/), [metadata derivation](https://www.jsboundaries.dev/docs/classification/dependency/), [module policies](https://www.jsboundaries.dev/docs/policies/), [coverage rules](https://www.jsboundaries.dev/docs/rules/).

**NV in this plugin's documented rule inventory:** transitive graph reachability and unused subgraphs; per-directory required imports/files/directives; prohibited-file existence and definition-name ownership; declaration-only manifest graph, nearest-manifest declaration validation, diagram input, built-in baseline/diff, policy pruning, impact and shortest-chain queries. ESLint facilities or another plugin may supply some of these; that is composition, not a verified Boundaries-native feature. ESLint JSON formatting likewise must not be presented as an easement-style explicit partial-coverage contract. The [active rule inventory](https://www.jsboundaries.dev/docs/rules/) is dependency-oriented, so a new custom ESLint rule would be **C** rather than declarative equivalence.

## Sheriff

| Contract | Documented equivalent and limit |
| --- | --- |
| Feature slices | **N:** `<domain>/<type>` path placeholders populate tags. Shipped `sameTag` with wildcard tag rules enforces same-domain dependencies; a custom predicate is optional, not required. |
| Directed boundaries / layers | **N/E:** tag allowlists express outgoing allowed edges, inverse importer constraints, adjacent-only layers and siblings. Explicit deny precedence and a dedicated closed-layer construct were **NV**. |
| Public API | **N:** barrel modules expose exported files; barrel-less modules hide an `internal` folder (configurable). The docs permit direct imports of an exported file: it is inaccurate to describe Sheriff as requiring every external import to go through `index.ts`. |
| Symbol identity policy | **NV:** arbitrary definition-resolved symbol allowlists and typed receiver-use bans. Barrel export visibility is a different contract. |

See [tag placeholders and sameTag](https://sheriff.softarc.io/docs/dependency-rules), [module visibility](https://sheriff.softarc.io/docs/module_boundaries), [configuration](https://sheriff.softarc.io/docs/configuration).

The CLI initializes config, verifies, lists tagged modules and exports JSON for reachable files. An export is not an inventory proving all source files were checked. [CLI reference](https://sheriff.softarc.io/docs/cli)

**NV in the reviewed full configuration/CLI references:** transitive forbidden contracts, dead-code detection, external-package/manifest policies, required imports/files/directives, type-name ownership, diagram input, violation baseline, Git-line diff, allowance pruning, impact and shortest-chain/prospective-file explanation. Documentation recommends gradual module adoption and `excludeRoot`, not a verified known-violation ratchet. [Incremental integration](https://sheriff.softarc.io/docs/integration)

## go-arch-lint

Native YAML components, `mayDependOn`, common components, vendor groups, `canUse`, component-specific unrestricted escapes, and global/component `deepScan` overlap easement's component and external-boundary model. Missing components are errors by default (`ignoreNotFoundComponents: false`). The documented schema does not include capture equality, public paths/symbols, incoming rules, explicit deny overrides, ordered/closed layers or transitive forbidden contracts. Enumerated positive edges can express a finite layer policy and restrict incoming edges globally, but do not create those missing abstractions. [Complete config syntax](https://github.com/fe3dback/go-arch-lint/blob/master/docs/syntax/README.md)

`deepScan` is documented as advanced AST analysis: do not reduce this competitor to textual import scanning, and do not infer symbol policy merely from deeper extraction. The README documents JSON output. Adoption guidance legalizes current edges in config with TODOs; a separate baseline ratchet, stale failure and changed-line diff were **NV**. [README](https://github.com/fe3dback/go-arch-lint/blob/master/README.md)

Graph output has focus, recursive dependencies, vendors and flow/DI direction. This is useful exploration, not a verified shortest-path explanation, reverse impact query or input diagram policy. [Graph reference](https://github.com/fe3dback/go-arch-lint/blob/master/docs/graph/README.md)

Required file/import/directive policies, type-name ownership, manifest declaration validation/edges, unused-code reachability, policy pruning and an expected-source partial-coverage contract were **NV** in those references.

## arch-go

Native YAML rules allow/deny internal, standard-library and third-party imports. Explicit package patterns can express finite layers and restrict all callers, but dynamic slice capture equality, a target-owned incoming rule, public path/symbol allowlists and transitive reachability were **NV**. It also has package-content restrictions and function limits beyond easement's boundary rules. Naming rules constrain implementers of matching interfaces; that differs from easement's “all matching type definitions belong in this component.” `describe` explains configured architecture; JSON reports are native. Coverage thresholds measure packages matched by a rule, not completeness of source indexing. [README and rule inventory](https://github.com/arch-go/arch-go/blob/main/README.md)

Required imports/files/comment directives, Mermaid input, manifest declaration edges/validation, baseline/stale/diff, allowance pruning and impact/shortest-chain queries were **NV** in that inventory. Programmatic architecture tests are supported; new assertions are **C**, not evidence that an arbitrary check is a shipped declarative rule.

## Consequences for positioning

Inference: defend the **shared model across languages, fine-grained semantic policies where supported, maintenance workflow and explicit trust contract**. Do not claim that slices, public interfaces, closed dependency sets, transitive bans, unused-code detection, baselines, stale checking, JSON or change-impact analysis are individually unique. In particular, dependency-cruiser's configurable ratchet substantially overlaps easement's baseline; the defensible distinction is defaults and exact behavior. The strongest unverified gaps in this comparator set are comment/file requirements, semantic name ownership, manifests-as-policy edges, input Mermaid policy and conservative automatic allowance pruning. A documentation survey cannot prove those gaps across all implementations or extensions.
