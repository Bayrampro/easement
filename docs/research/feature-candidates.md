# Candidate features for easement

Prepared on 2026-11-04 after the [capability comparison](easement-capability-assessment.md), inspection of the current source, and a live read of the repository's open issues and PRs. These are proposals, not approved specifications. Names and command examples are illustrative; they do not establish new CLI or YAML contracts. No implementation changes or comparative runtime tests were made.

The selection favors concrete developer/agent questions, a single language-independent rule model, reuse of normalized index facts, and explicit limits on unsupported evidence. Effort labels are relative to this codebase, not calendar estimates: **small** needs existing facts and a bounded command/report change; **medium** changes shared evaluation or a stored contract; **large/spike** needs a new semantic model or evidence from real indexer outputs.

## Prioritized list

| Order | Capability | First useful result | Effort / evidence |
| --- | --- | --- | --- |
| 1 | Validate a proposed dependency before editing code | A source/target query returns applicable rule decisions and, with an indexed graph, cycle/reach consequences | Medium; existing component/evaluator/graph facts |
| 2 | Review architecture-policy changes | Explain permissions expanded/restricted and observed edges legalized/broken by a config change | Medium; config and evaluator reuse, with indexing-scope caveats |
| 3 | Test the architecture policy itself | Synthetic path-edge examples assert intended allow/deny outcomes without adding bad source code | Medium; reuse direct edge evaluation |
| 4 | Make incomplete evidence fail CI when requested | An opt-in mode rejects unread/unsupported evidence while allowing deliberate diff/scope restriction | Small–medium; report already distinguishes those reasons |
| 5 | Provide structured query answers and evidence | JSON for `explain` and `why`, typed witness chains and rule provenance | Small–medium; facts exist, output contract needs agreement |
| 6 | Prevent growth inside an accepted symbol-baseline key | An optional budget rejects another reported prohibited-symbol use when its accepted count is exhausted | Medium for symbol reports; baseline migration semantics |
| 7 | Query impact and paths at definition granularity | Reverse reach and witness paths from an explicitly selected method/function | Medium; existing Flow needs a reusable query surface |
| 8 | Expose different APIs to different consumers | One provider owns several named path interfaces, granted to selected consumers | Medium–large; shared configuration/evaluation extension |
| 9 | Extend existing symbol rules to more indexed languages | Existing `forbid_symbol`/`public_symbols` gain evidenced coverage for Java, C# and PHP | Spike, then medium per indexer; no new rule family |
| 10 | Stop infrastructure types leaking through public APIs | Distinguish implementation-only type use from public return/parameter exposure | Large/spike; shared signature/visibility facts are not established |

The first five can advance without inventing new language semantics. The baseline and query extensions are useful second-stage work. Consumer-specific interfaces add policy expressiveness. The final two should start by examining actual indexer evidence, not by choosing configuration syntax.

## 1. Proposed-dependency validation

**User problem:** an agent has selected the source and target of a future import and wants an answer before generating the code. Existing `explain` describes a component; `why` explains observed dependencies. Neither is a full verdict for a specific hypothetical edge. [Current explain](../../src/explain.rs), [current why](../../src/impact.rs).

**Bounded first version:** accept a source path and an already resolved target path, optionally an identified symbol. Reuse the real checks for ownership, allowances, explicit deny, public paths, incoming restrictions and slice identity. Return the applicable rule, reason, ownership and limitations. Paths for proposed files can be classified without creating those files, but a path pair does not establish that a language's compiler will resolve the future source that way.

**Graph extension:** overlay the edge on the cached/current graph and report a cycle or newly forbidden transitive chain. A following increment can validate a batch of proposed edges together: individually safe changes can jointly close a cycle. Return a witness path and which checks were actually applied. No symbol-flow verdict without a known caller/target definition; no complete verdict from a partial graph.

**Acceptance example:** `checkout -> billing/internal/admin` fails before editing; access through the exposed API passes local rules but reports a cycle if the existing graph has a return path. The predicted decision agrees with a real check after the corresponding import is added.

**Implementation:** extract a reusable query from [check](../../src/check.rs), [components](../../src/components.rs) and graph traversal; keep one evaluator. A later legal-target query can enumerate policy-visible alternatives, labelled as legal candidates rather than semantically interchangeable functions. Zügel's `check_proposed_dependency` and legal-target workflow are a concrete [precedent](https://blog.hello2morrow.com/2026/07/sonargraph-mcp-2/).

## 2. Architecture-policy review

**User problem:** a PR can become green by weakening a rule, widening `public`, accepting debt or excluding code. Conversely, making a rule stricter affects imports whose lines did not change. The existing changed-line `--diff` deliberately does not answer either question. [Diff filtering](../../src/main.rs), [documented semantics](../../README.md#diff-mode-judging-a-pull-request).

**Bounded first version:** compare previous and proposed effective policy, including `extends` and diagram inputs, on the same current graph. Report observed dependencies newly permitted/forbidden, changed component ownership and explicit permission changes even if no code currently exercises them. Separately inventory baseline additions and new exclusions/accepted coverage gaps. Where glob or symbol-surface changes cannot be exhaustively compared, report the changed selectors and bounded witnesses rather than claiming a complete permission-set difference.

**Acceptance example:** removing a deny reports an expanded permission even if no import uses it; adding a deny reports unchanged offending imports. This report can direct CI to run a full check for relevant config/manifest changes instead of relying solely on changed import lines.

**Boundary:** evaluating two policies on one fixed graph is not the full effect of a PR. If config changes affect what gets indexed (`exclude`, build tags, project coverage), rescan conservatively or mark the comparison incomplete. Full base/head comparison requires two graphs and reproducible environments and should be a separate increment.

**Implementation:** configuration loading, component mapping, evaluator, baseline parsing and diff provenance. The report must identify the compared policy revisions and graph. Existing baseline entries are not erased or regenerated by inspection.

## 3. Tests for the user's architecture policy

**User problem:** CI currently observes real dependencies. A forbidden dependency that nobody has written yet supplies no regression test for the prohibition: a future edit may accidentally allow it while real-code checks remain green.

**Bounded first version:** a project supplies positive and negative examples such as `domain/order.go -> infra/db.go must be denied by dependency`, and `domain/order.go -> domain/value.go must be allowed`. Evaluate synthetic path edges through the same production rule engine, without adding imports, invoking indexers or altering the application. Test deny precedence, public paths, incoming permissions and slice captures.

**Acceptance example:** removing a domain/storage prohibition leaves the source check green but makes the negative policy example fail. Reusing ordinary `check` decisions avoids building a second architecture-policy interpreter.

**Boundary:** initial examples test declared policy, not compiler resolution. Symbol cases require resolved symbol fixtures; cycles/reach require a graph fixture. Do not advertise arbitrary extractor or runtime tests. A separate test artifact is preferable to choosing new YAML keys before agreeing the issue's model.

**Implementation:** [Config](../../src/config.rs), [ComponentMap](../../src/components.rs), a reusable direct dependency evaluator and a small test-runner/report layer. This can reuse the same query interface as proposed-dependency validation.

## 4. Fail CI on missing evidence

**User problem:** a structured `confidence: partial` warning can be overlooked while the command exits successfully. Some teams want unread files or unsupported configured rules to block the build.

**Bounded first version:** an opt-in CLI requirement for adequate evidence, with reasons and affected files in text/JSON. Keep analysis error, proven violation, and evidence incompleteness distinct in the report. Exit behavior must be agreed as an agent-facing contract.

**Critical distinction:** `--diff` and `--scope` intentionally narrow the verdict; rejecting every `Confidence::Partial` would make those workflows unusable. Gate on unread/unsupported evidence, not deliberate narrowing. [report.rs](../../src/report.rs) already separates `narrowed_reasons` and `unread_reasons`.

**Acceptance example:** a clean diff run over adequately indexed files passes; the same run with an uncovered source file fails the evidence requirement. Deliberately accepted `uncovered_ok` remains a visible policy choice, not a claim that its code was indexed. Missing/failed indexers remain existing hard errors.

## 5. Structured query answers and witnesses

**User problem:** `check` and `impact` offer JSON, but `explain` and `why` currently produce text. An agent must parse prose to understand matching policy, scope and dependency chains. Some violation evidence already exists internally but is skipped during serialization. [CLI variants](../../src/main.rs), [Violation evidence](../../src/check.rs), [query renderers](../../src/impact.rs).

**Bounded first version:** stable JSON models for ownership, matched selectors, decision/rule references, reasons, owners, witness hops and provenance. Query renderers consume those models for both text and JSON. Do not expose an internal debug structure as a permanent external schema; distinguish a shortest explanatory path from all edges contributing to a violation.

**Acceptance example:** an agent can identify the exact rule and every hop of a forbidden reach without splitting a human message. Partial coverage, cache provenance and whether the query used files or definitions remain machine-readable.

**Implementation:** output models and renderers; no new parser or rule kind. This is useful groundwork for preflight and policy review, not a claim that JSON itself differentiates easement.

## 6. Budgets for baselined symbol uses

**User problem:** baseline keys omit line numbers. One accepted use currently legalizes another use with the same rule/file/target/components, and that behavior is explicitly tested. [Baseline implementation](../../src/baseline.rs), [key canonicalization](../../src/check.rs).

**Bounded first version:** an optional budget for reported `symbol` violations per key: accepting one reported use permits at most one current reported use. Fixed uses shrink the budget; new accepted uses require an explicit debt decision. Keep aggregate rules such as cycles, component reach and metric thresholds on their existing identities rather than counting their reported witness lines. Import dependencies are currently collapsed to their first file/module occurrence, so counting all import occurrences is a separate, larger extraction change.

**Acceptance example:** moving the accepted call down ten lines passes; adding a second same-key prohibited call on another line fails; deleting one call lowers the recorded budget when regenerated.

**Boundary:** counts prevent aggregate growth, not replacing an old prohibited occurrence with a new one while retaining the same count. Today's deduplication also does not distinguish multiple same-key uses on one line. Stable occurrence fingerprints would be a separate, harder design. Existing baseline files need explicit backwards-compatible semantics or a migration; silently strengthening old files is inappropriate.

**Implementation:** baseline schema, admission/judgment and grouping semantics. This needs an agreed issue because it changes a persisted agent-facing contract.

## 7. Definition-level impact and path queries

**User problem:** `reach: symbols` already separates functions for forbidden-reach checking, but `impact` and `why` still operate on files. Changing one pure helper can therefore implicate callers of unrelated methods in the same file.

**Bounded first version:** explicitly select an indexed definition and query reverse dependencies or an explanatory path through [Flow](../../src/flow.rs). Report fully qualified symbol identity, file/line witnesses and fallback reasons. Begin with an explicit symbol, not automatic inference of every changed definition from Git.

**Acceptance example:** querying `Document.host` includes its callers without assigning it every caller of `Document.fetch`; an opaque receiver retains the conservative fallback rather than disappearing from the answer.

**Boundary:** this is static reference reach, not proof that a test is sufficient or a change cannot break runtime behavior. Automatic diff-to-definition mapping and deleted-definition handling require a separate base/head design.

## 8. Named interfaces per consumer

**User problem:** checkout needs the customer-facing billing API, while administrative jobs need a different API belonging to the same billing component. Current `public`/`public_symbols` defines one external surface; `allow_importers` restricts consumers but cannot choose a distinct surface for each. [Current component fields](../../src/config.rs).

**Bounded first version:** a provider owns named views selected by paths; selected consuming components are granted selected interfaces. Interfaces do not create overlapping ownership. Preserve deny precedence and settle same-component access, default surfaces, tests and slice captures explicitly. Symbol interfaces can follow existing symbol capability.

**Acceptance example:** checkout importing billing's admin API fails; an administrative job passes; both APIs continue to belong to billing.

**Implementation:** shared config/component/evaluator changes. No new index facts are needed for path-selected interfaces. Sonargraph provides the richer [interface/connector precedent](https://eclipse.hello2morrow.com/doc/standalone/content/architecture_interface_connectors.html); easement need not adopt the whole DSL.

## 9. Close existing symbol-coverage gaps

**User problem:** teams see Java/C#/PHP in the supported-language list, but existing symbol-use rules do not judge those languages. Supporting another language is a separate concern from making established rule coverage more consistent. [Current symbol scope](../../README.md#symbols).

**First step:** inspect real SCIP outputs for class references, aliases, namespaces, nested types, methods and inherited/receiver uses. Identify how much the current symbol normalization already supports and which definitions cannot be attributed reliably. Add fixtures before promising parity with the best-supported languages.

**Acceptance example:** the existing `forbid_symbol` catches a prohibited type through an alias and a supported typed use, with the same generic rule id and explicit gaps where extraction is insufficient.

**Implementation:** index-reader normalization and language capabilities, not language-named branches in rules or a new language-specific config key. Effort depends on actual indexer facts; this is not a guaranteed small adapter task.

## 10. Public API type boundaries

**User problem:** a service may use an ORM entity internally but should return a domain DTO. A whole-component dependency ban cannot allow internal use while rejecting an exposed return or parameter type.

**First step:** validate a shared model of signature relationships and visibility across real indexer outputs. Include aliases, generic arguments, inferred returns, re-exports and inherited signatures; unknown facts must remain unknown. SCIP has optional signature/relationship data, but it does not guarantee universally structured public API edges. [SCIP protocol](https://github.com/sourcegraph/scip/blob/main/scip.proto).

**Acceptance example after the spike:** internal use of `storage.Entity` passes; exporting it in a public API fails at the declaration with the leaked type and evidence path. An indexer without adequate signature evidence reports an unsupported/partial result rather than passing silently.

**Implementation:** shared normalized semantic facts and one general rule shape agreed first. This belongs in the existing [semantic architecture discussion #85](https://github.com/mokevnin/easement/issues/85), which also covers contracts, implementations and generic relationships. Dependency-kind filtering can be a later consumer of validated facts; it should not be inferred from symbol names or the existence of an import.

## Existing work and scope choices

Live repository reads found [Dart support #62](https://github.com/mokevnin/easement/issues/62) and [PR #71](https://github.com/mokevnin/easement/pull/71) open, plus the separate [semantic-model discussion #85](https://github.com/mokevnin/easement/issues/85). These are current planning context, not evidence of implementation or release. Previously completed issues include definition-specific reach (#58), Cargo/test parts (#65) and broader Rust test classification (#72); those are existing features rather than new candidates.

Do not prioritize a thin MCP wrapper, a general architecture dashboard, automatic application refactoring, framework-style lint rules or universal HTTP-edge inference ahead of the concrete capabilities above. A transport adds no policy on its own; a runtime service graph needs evidence beyond imports; source-style checks need a demonstrated architecture property and a cross-language shape.

The proposed first tranche is preflight evaluation, policy-change review and contract tests, with structured output and the evidence gate as bounded supporting work. This improves what agents can verify before and during changes while preserving the project's current focus. Under [CONTRIBUTING.md](../../CONTRIBUTING.md), rules, configuration keys and output contracts are settled in issues before implementation; this document is the candidate list for that discussion.
