# Feasible feature additions

Assessed on 2026-11-04 from the current source. This is a proposal, not an implementation plan or a claim about runtime accuracy. Competitor evidence is in [the capability assessment](easement-capability-assessment.md); the estimates below are engineering judgments. **S** changes an existing report or narrowly scoped decision; **M** extends an existing mechanism across CLI, configuration and fixtures; **L** needs new extraction facts or difficult migration semantics. Sizes are relative, not delivery dates.

Every addition keeps one language-agnostic model. New rules/configuration require an issue agreeing their general shape first, as [AGENTS.md](../../AGENTS.md) requires. CLI examples below illustrate possible interfaces; none exists yet.

## Recommended order

| Priority | Addition | Size | What it enables |
| --- | --- | --- | --- |
| 1 | Capability inventory for the actual project | S/M | Know which requested checks can judge which files before trusting a result. |
| 2 | Structured dependency explanations | S/M | Let an agent inspect the exact policy and dependency evidence without parsing prose. |
| 3 | Full-check escalation for policy/build-input changes | M | Catch consequences of PRs that change no importing line. |
| 4 | Proposed-dependency preflight | M | Reject a bad dependency, including graph consequences, before editing code. |
| 5 | Policy and debt change review | M | Review newly permitted boundaries and legalized debt even when code stays green. |
| 6 | Occurrence budgets for baselined symbol violations | M | Prevent more uses of an already legalized forbidden symbol in one file. |
| 7 | Consumer-specific API surfaces | M | Give different clients different entry points without splitting component ownership. |
| 8 | Definition-aware `why` and `impact` | M | Ask which referenced methods are affected, with conservative file fallback. |
| 9 | Dependency roles and public-signature isolation | L | Distinguish implementation dependencies from API leakage and inheritance. |
| 10 | Executable policy contracts | M | Pin intended allowed/forbidden boundaries even when no live code uses them. |

The first three improve the reliability of existing workflows. Preflight is the strongest bounded new agent workflow. Semantic roles are the largest new enforcement surface, with the greatest extraction risk.

## 1. Capability inventory for the actual project

**Existing:** `doctor` checks installed indexers and prerequisites; check reports already carry partial-analysis reasons. It is not missing basic diagnostics. [Doctor](../../src/doctor.rs), [diagnosis](../../src/indexer.rs#L815), [confidence](../../src/report.rs#L79).

**Gap and bounded change:** expose a stable matrix of import resolution, resolved-name policy, definition ranges, type ownership, manifest policy and unused-file eligibility, plus why each is unsupported/partial for the current project. Put language-specific declarations in the registry; keep observed run coverage separate from a declared capability. Include tree-sitter languages, which do not need an indexer and therefore are absent from the current indexer-oriented report. A config-aware result can say that `public_symbols` exists but cannot judge a project's Java files. [Registry](../../src/lang/registry.rs#L29), [current coverage](easement-capability-assessment.md#capability-coverage-is-uneven).

**Example:** an agent considering `unused: true` learns that Ruby files are conservatively retained and that an uncovered TS file invalidates a confident unused-file judgment.

**Modules:** `lang/registry.rs`, `doctor.rs`, `indexer.rs`, `report.rs`, `main.rs`; optionally `scan.rs` for observed coverage. **Risk:** an installed executable is not proof of complete references or usable scope ranges; static inventory is S, project/config-aware applicability is M. No new extraction is needed for the initial inventory.

**Independent quick win:** add an opt-in CI mode that fails when required evidence is unread/unsupported. `report.rs` already separates `unread_reasons` from `narrowed_reasons`, so intentional `--diff`/`--scope` runs must not fail merely because their aggregate confidence is partial. Missing indexers already error; the gap is successful-but-incomplete analysis. This is S/M across `main.rs`, `report.rs` and verdict/exit handling. An issue must define how accepted `uncovered_ok`, unsupported rules and cache provenance interact; a cache hit alone is not missing evidence. Example: a green diff run remains valid, but the same run fails because a TS file lies outside every tsconfig.

## 2. Structured dependency explanations

**Existing:** `why` gives policy, direct imports or a shortest chain in text; JSON check already includes violation identities and reasons. Internal `Violation.evidence` is deliberately skipped during serialization. [Why](../../src/impact.rs#L161), [violation](../../src/check.rs#L137), [CLI](../../src/main.rs#L142).

**Gap and bounded change:** `why --format json` with source/target components, policy origin, reason, typed hops and source locations. Add structured evidence paths to reach/cycle violations instead of requiring clients to parse their message. Start with normalized field paths such as `rules.domain.must_not_reach`; exact YAML spans through `extends` need separate provenance work.

`explain --format json` is the complementary small addition for prospective files: expose the already computed ownership, captures, allowances and reasons. [Explain](../../src/explain.rs). It should share structured policy decisions with preflight rather than create another policy interpretation.

**Example:** `domain -> shared -> http` returns two source locations and the field that forbids the reach; an agent opens the relevant lines and proposes a boundary fix.

**Modules:** `impact.rs`, `check.rs`, `report.rs`, `main.rs`; `config.rs` only if richer policy provenance is added. **Risk:** evidence must identify fallback/ambiguity and propagate existing coverage/cache warnings. Merely serializing today's evidence is S; complete typed explanations and policy provenance are M. No MCP server is needed to provide this value.

## 3. Full-check escalation for policy/build-input changes

**Existing:** `check --diff` analyzes the graph, then reports violations when any evidence location is on an added/changed line. Some config-invalidity rules bypass the filter; ordinary dependencies, manifest validation and unused-file consequences do not. The README explicitly recommends a full check for rule changes. [Filter](../../src/main.rs#L389), [diff](../../src/diff.rs), [documented limit](../../README.md#diff-mode-judging-a-pull-request).

**Gap and bounded change:** an explicit review mode escalates to full judgment when a relevant config, inherited config, baseline, supported manifest or indexer input changes. Start conservatively: full judgment, with an explicit reason, rather than inventing a precise impacted-policy slice. Source-only deletions that can orphan previously live files also need full judgment if this mode promises PR consequences.

**Example:** removing `zod` from `package.json` raises `undeclared` on unchanged imports; making `billing.public` narrower raises privacy violations in unchanged consumers.

**Modules:** `diff.rs`, `main.rs`, `config.rs`, `manifests.rs`, `indexer.rs`, `report.rs`. **Risk:** changing default diff behavior would surprise users adopting only a changed-line gate; make scope explicit. The changed input closure must include `extends` and external config paths. Full judgment uses the existing baseline and can reveal old unbaselined debt: state that plainly rather than calling every report item newly introduced.

## 4. Proposed-dependency preflight

**Existing:** `explain` answers what a file may do; `why` inspects already observed component edges. Neither evaluates a specific proposed target or its new graph consequences. [CLI](../../src/main.rs#L102), [policy evaluation](../../src/check.rs#L287), [cycles](../../src/check.rs#L512), [reach](../../src/check.rs#L576).

**Gap and bounded change:** accept a source path and resolved target path or external module; evaluate a synthetic `Dep` with the same rule engine. Overlay it on the current graph for cycle, forbidden reach and coupling-limit impact. Return direct policy failures separately from additional graph failures. Run without editing files and do not legalize a proposed violation just because an existing baseline key matches it.

**Example:** `propose src/domain/order.ts src/infra/db.ts` explains a privacy failure; proposing an otherwise allowed `shared -> domain` edge reports the newly closed `domain -> shared -> domain` cycle.

**Modules:** `main.rs`, `check.rs`, `components.rs`, `scan.rs`, `report.rs`; reuse graph algorithms. **Risk:** a component pair alone cannot judge path privacy, captures or test parts. A target path cannot prove a language import will resolve. Keep initial inputs explicit; symbol-aware preflight needs symbol identity and source context, and unknown context must not return a complete green result. No source or build files are changed.

## 5. Policy and debt change review

**Existing:** `stats.new_dependencies` shows observed new edges; `suggest` removes unused allowances. Baselines ratchet when regenerated normally, but a PR can intentionally change config or use `--accept-new`. These are different review surfaces. [New edges](../../src/check.rs#L499), [suggest](../../src/suggest.rs), [baseline](../../src/baseline.rs).

**Gap and bounded change:** compare the merge-base and working-tree policy/baseline, listing added allowances, removed denies, warning demotions, expanded public surfaces, new exclusions/uncovered exceptions, and baseline entries added/removed. Make the first version a classified semantic config diff, not a claim to prove arbitrary glob policy inclusion. Optionally evaluate both effective configurations over the current facts to show currently observed violations legalized by the change.

**Example:** a PR adds `domain -> storage` and ten baseline keys. Review output shows this even if every current check passes, with the attached `because` reasons.

**Modules:** `diff.rs`, `config.rs`, `baseline.rs`, `report.rs`, `main.rs`. **Risk:** current facts under the old policy do not reconstruct the old source graph. Exclude/test/part changes can change fact collection, so their consequences require separate scans or an explicit incomplete comparison. Raw policy edits and observed legalization must remain distinct. This complements full-check escalation: one audits policy changes; the other judges consequences.

## 6. Occurrence budgets for baselined symbol violations

**Existing:** one baseline `Key` legalizes every occurrence with that key. A unit test pins this behavior; line numbers are omitted intentionally to survive unrelated edits. [Key](../../src/check.rs#L156), [test](../../src/baseline.rs#L146), [documented behavior](../../README.md#baseline-adopting-on-an-existing-project).

**Gap and bounded change:** opt-in, versioned count budgets per key for occurrence-reporting rules, initially `symbol`. Allow no more occurrences than recorded; regenerating without acceptance may only lower budgets. Do not use line numbers as identity. If a budget of 2 sees 3 uses, report budget growth without arbitrarily claiming which indistinguishable occurrence is new.

**Example:** a file has two approved uses of `db.RawClient`; a third fails even though its rule/file/target/components match existing debt.

**Modules:** `baseline.rs`, `check.rs`, `main.rs`, `report.rs`; serialization migration and fixtures. **Risk:** the current `Dep` extraction collapses a file/module pair to its first line, while symbol uses survive separately and deduplication is per key/line. A generic occurrence-sensitive baseline across every rule would need richer extraction and rule-specific semantics: **L**, not a small baseline-map edit. Counts also do not distinguish replacing one use with another, or two uses on one line under today's deduplication. Preserve legacy entries explicitly as unbounded rather than silently reinterpreting them. [Extraction](../../src/scip.rs#L655), [dedupe](../../src/check.rs#L1512).

## 7. Consumer-specific API surfaces

**Existing:** a target component has one global `public` path set, one `public_symbols` name set and a component-wide `allow_importers`. Splitting API directories into separate components already expresses many simple cases. [Component](../../src/config.rs#L295), [privacy](../../src/components.rs#L327), [symbol visibility](../../src/check.rs#L1085).

**Gap and bounded change:** named, target-owned API surfaces selected by consumer component. Start with path surfaces using existing `Dep` targets; add resolved-name surfaces only where today's symbol policy works. Preserve one owner and one set of coupling/instance rules for the provider. Default to existing global visibility unless the design issue deliberately changes it.

**Example:** `checkout` may use billing's charge API; `reporting` may use its read API; neither gets the maintenance API merely because another consumer needs it.

**Modules:** `config.rs`, `components.rs`, `check.rs`, `explain.rs`, `impact.rs`, schema/report fixtures. **Risk:** define how global `public`, overlapping surfaces, denies, self-use and captures compose before implementation. This is most valuable when separate components would distort ownership/metrics; otherwise existing component splits may be sufficient. Competitor precedent: [Sonargraph interfaces/connectors](https://eclipse.hello2morrow.com/doc/standalone/content/architecture_interface_connectors.html), documented in the [comparison](polyglot-architecture-capabilities.md).

## 8. Definition-aware `why` and `impact`

**Existing:** `reach: symbols` already builds a `Flow` between ranged definitions with conservative whole-file fallbacks. `why` and `impact` use file graphs, so proposing method-aware queries is a real gap, not a new name for forbidden reach. [Flow](../../src/flow.rs), [symbol graph](../../src/check.rs#L734), [query implementation](../../src/impact.rs).

**Gap and bounded change:** opt-in symbol selectors and definition-level changed ranges, reusing the same reference graph semantics as forbidden reach. Expose normalized nodes and fallback hops. Retain ordinary file mode for broad impact and unknown symbols.

**Example:** changing `Document.fetch` identifies users of that method without automatically blaming callers of pure `Document.host`; a dependency from shared class initialization is still retained.

**Modules:** `flow.rs`, `check.rs`, `impact.rs`, `scan.rs`, `main.rs`, `report.rs`. **Risk:** `Analysis` currently does not retain `Flow`, and scan builds it only for `reach: symbols`. Queries need a reusable graph builder rather than a second interpretation. Definition references are not runtime dispatch proof; absent/overlapping ranges and opaque receivers retain whole-file effects. Initial symbol selectors are M; precise old/new definition matching across Git revisions is a separate larger feature.

## 9. Dependency roles and public-signature isolation

**Existing:** `Reference` retains line, symbol and a special spelled module, while `parse` discards occurrence metadata beyond its definition role. `Dep` has no dependency kind. `defines` owns matching type names, not inheritance or API-signature policy. [Reference/parser](../../src/scip.rs#L29), [Dep](../../src/scan.rs#L19), [type-name policy](../../README.md#names-of-types).

**Gap and bounded change:** first agree a general vocabulary such as type use, inheritance, call/reference and unknown, plus signature versus implementation context. Normalize only evidence actually available at extraction; unknown remains explicit. Then a general rule can forbid infrastructure types in a domain public signature while allowing an implementation dependency. Do not introduce `typescript_type_only` or language-named core keys.

**Example:** a public `createOrder` returning `infra.SqlRow` fails, while constructing the same type inside a permitted adapter body may pass. A separate use could allow a type/interface dependency but reject implementation calls.

**Modules:** `scip.rs`, `lang/registry.rs`, adapters/indexers as needed, `scan.rs`, `config.rs`, `check.rs`, cache/schema/report/tests. **Risk:** SCIP symbols and function ranges alone cannot reliably distinguish all signatures, inheritance and implementation references. Indexers expose uneven facts; several contexts require new syntax extraction. A small language-neutral schema spike with pinned cross-language fixtures must precede rule claims. This is L and should extend the existing semantic-rule discussion rather than start a parallel incompatible policy system.

## 10. Executable policy contracts

**Existing:** repository fixtures test easement itself, and `check` judges observed project facts. There is no project-level mechanism to pin a dependency expectation when no code currently exercises that edge. [CLI surface](../../src/main.rs#L61), [common evaluator](../../src/check.rs#L287).

**Gap and bounded change:** users declare synthetic source/target paths, optional normalized symbol facts, and an expected rule outcome. Run these against effective config without creating language source files or invoking indexers. Reuse the proposed-dependency evaluator. Initial contracts cover ownership, layers, public paths, captures, external restrictions and supported symbol decisions. Keep general cycle/reach claims out unless the contract supplies a complete synthetic graph.

**Example:** a contract says `src/domain/order.ts -> src/infra/db.ts` must fail `dependency`; moving `infra` into `common` unexpectedly makes it pass and fails the contract, even if no source currently imports it.

**Modules:** `config.rs` or a dedicated contract-file parser, `main.rs`, `check.rs`, `components.rs`, `report.rs`; schema/fixtures. **Risk:** agree whether contracts assert a specific rule or any rejection, and distinguish an invalid synthetic source/target from a successful rejection. Symbol expectations test policy on supplied facts, not indexer resolution. Avoid duplicating rule semantics inside a test engine. Size M; adoption can start with a small sidecar contract file after an issue agrees its shape.

## Scope boundaries

Do not prioritize language count, a general MCP wrapper, more output formats or another diagram exporter as standalone product value: existing mechanisms already cover much of that. Do not turn static references into an HTTP/service-call graph or imply runtime dispatch/taint precision. General lint, refactoring, data flow and vulnerability scanning would change the product's scope. Richer package/build-model support can be useful, but is distinct from claiming that every build configuration is verified.

No comparative tools were executed for this feasibility pass, no code was changed and no GitHub mutation was performed. The attempted read of issue #85 could not reach api.github.com in this environment; its current text/status was not used as evidence here.
