# Feasible feature opportunities

Assessed on 2026-11-04 against the local README and selected implementation files, with first-party competitor documentation retrieved on that date. These are product proposals, not implementation estimates or claims of uniqueness. No competitor was executed. See the [capability assessment](easement-capability-assessment.md) for what easement already supplies; adding another language is separate from improving its policy model.

## Priority

| Order | Capability | Recommendation | Why |
| --- | --- | --- | --- |
| 1 | Proposed dependency and plan validation | Immediate, bounded first version | Prevent agent rework using the existing policy and graph. |
| 2 | Policy change review over unchanged code | Immediate, bounded first version | Closes a concrete gap in changed-line checking and makes rule changes reviewable. |
| 3 | Consumer-specific named interfaces | Immediate after an issue settles the model | Adds a real policy distinction; path selectors need no new index facts. |
| 4 | Legal targets and cycle remediation candidates | Immediate legal-target query; spike for graph optimization | Turns failures into actionable choices without weakening policy. |
| 5 | Dependency kinds and public-signature leakage | Capability spike first | Valuable, but the current normalized facts cannot honestly support it. |

“Immediate” means compatible with the current architecture, not an approved task. Under AGENTS.md, every new rule, rule kind or config key needs an issue settling its general shape before implementation.

## 1. Check an intended dependency before writing it

**Problem:** an agent knows the proposed source path and target but has not added the import. `explain <file>` describes component-level allowances; `why` explains existing graph evidence. Neither is a complete hypothetical-edge verdict, especially for a cycle or an indirect forbidden path created by an otherwise allowed edge. [Current commands](../../README.md#usage), [explain implementation](../../src/explain.rs).

**Smallest useful feature:** a read-only JSON query accepting source path and resolved target path, optionally a known symbol. Return applicable direct-policy decisions, reasons/owners, the hypothetical cycle and a witness chain for newly forbidden reach. Reuse the real evaluator, including public paths, deny precedence, slice equality and incoming restrictions. Distinguish policy legality from graph consequences; a missing file or ambiguous symbol is not a resolved dependency. The current [evaluator](../../src/check.rs) already has edge checks, cycle detection and reach traversal; the work is exposing reusable evaluation rather than adding language parsers.

**Before-code fitness:** allow a batch of intended edges and proposed file paths next. Evaluate the entire overlay: individually safe edges can jointly create a cycle. This turns an agent's implementation plan into a checkable architecture hypothesis. It does not prove the future code will follow that plan; a real check remains necessary afterward. Start with file-level analysis and explicitly mark cases that need definition-level evidence.

**Primary precedent:** Zügel documents `check_proposed_dependency`, including policy verdicts and the cycle impact of adding the edge, even without architecture rules. Its legal-target enumeration helps choose an accessible API. Easement should adopt the useful question, not imply this workflow is novel. [Zügel reference, use case 1](https://blog.hello2morrow.com/2026/07/sonargraph-mcp-2/).

**Acceptance example:** `handler -> billing/internal` is denied before an edit; `handler -> billing/api` is directly allowed but reports the existing return path that would close a cycle. Two proposed edges are assessed together, with coverage/cache qualifications inherited from the scanned graph.

## 2. Review what a policy change actually legalizes or breaks

**Problem:** deleting an allowance, expanding `public`, changing component ownership or weakening a deny may affect source lines that did not change. Easement explicitly documents changed-line `--diff`; that answers a different question. [Diff semantics](../../README.md#diff-mode-judging-a-pull-request), [implementation](../../src/diff.rs).

**Smallest useful feature:** evaluate the same current scanned graph under the previous and proposed effective configuration, including `extends` and diagram input. Report newly forbidden and newly legalized observed dependencies, moved/unowned files and disappeared component patterns. Separate this from a full base-revision-versus-head code comparison, which requires two graphs and reproducible indexing environments. Do not label a same-graph comparison as the entire PR's effects.

**Policy contract:** additionally summarize changed *permissions*, even where no current import exercises them: “web may now access storage.” Component-edge permissions can be enumerated; arbitrary path/symbol glob differences should be described or supplied with witnesses, rather than claiming a complete symbolic set difference. JSON should record both config identities and graph identity.

**Primary precedent:** Sonargraph-Build compares reports with a baseline report. That establishes the value of result-state differences; the narrower proposal above intentionally isolates policy changes on a fixed graph. [Reporting Changes](https://eclipse.hello2morrow.com/doc/build/content/reporting_changes.html).

**Acceptance example:** a PR changes only YAML to forbid `web -> storage`; unchanged offending imports appear as newly forbidden. Removing that ban reports a permission expansion even if storage currently has no web caller. No baseline regeneration is needed to explain the change.

## 3. Give different consumers different APIs of one component

**Problem:** `public` and `public_symbols` expose one surface to every permitted outside consumer. `allow_importers` limits who can enter the component, not which API each consumer may use. Teams may want checkout to use `billing/customer-api`, while administrative jobs may use `billing/admin-api`. Splitting every surface into independently owned components is possible but loses the single provider's grouping. [Current public API](../../README.md#public-api), [component fields](../../src/config.rs).

**Smallest useful feature:** target-owned named interfaces selected first by paths, connected to specific consuming components. Preserve existing direct-deny precedence and the current single ownership model; an interface is a view of its component, not a second owner. Settle interactions with `public`, same-component access, slice captures and default interfaces in an issue before choosing YAML syntax. Symbol selectors can follow only where symbol-use capability already exists.

**Primary precedent:** Sonargraph's named interfaces expose subsets of a provider; connectors select a consumer subset and connect it to those interfaces. Easement need not import its whole nested artifact DSL to solve consumer-specific visibility. [Interfaces and Connectors](https://eclipse.hello2morrow.com/doc/standalone/content/architecture_interface_connectors.html).

**Acceptance example:** checkout can call the customer API but importing the admin API fails, while the administrative job passes. Both API paths still belong to billing. This needs config/evaluator work, not richer SCIP semantics.

## 4. Suggest legal targets and useful cycle cuts

**Problem:** a violation identifies the forbidden edge, and `because` gives human guidance, but the agent must discover concrete allowed targets. A cycle witness also does not identify which removal buys the most structural progress. [Current guidance](../../README.md#usage), [cycle/reach functions](../../src/check.rs).

**Smallest useful feature:** enumerate visible candidate files/interfaces for a source, applying the same checks as preflight and including reasons. Label them architecturally legal candidates, not behaviorally interchangeable implementations. Zügel documents a legal-target query; it is a useful precedent rather than proof of semantic substitutability. [Zügel reference](https://blog.hello2morrow.com/2026/07/sonargraph-mcp-2/).

**Separate spike:** rank dependency-removal candidates within a strongly connected group by source-reference count and simulated reduction of the group. Prefer policy-violating edges; return source witnesses and what cycles remain. Sonargraph documents weighted feedback-arc analysis with user preferences, and virtual refactorings before source edits. [Vendor algorithm explanation](https://blog.hello2morrow.com/2015/02/identifying-dependencies-for-cycle-removal/), [product capabilities](https://www.hello2morrow.com/products/sonargraph/architect).

Do not promise a minimum-effort code repair: edge count is a graph cost, not semantic refactoring cost. A bounded heuristic is more defensible than unqualified “minimal remediation”; expose its objective and optimality limits. Removing a component edge means removing every underlying source reference, not merely the displayed representative import.

**Acceptance example:** a cycle report identifies two candidate cuts, shows their concrete references and resulting cycle groups, and checks each against the intended policy. It never edits rules or application code.

## 5. Distinguish using a dependency from leaking it through an API

**Problem:** a facade may legitimately use persistence internally while its public function must not return an ORM entity. A blanket `must_not_depend_on` cannot allow the former while forbidding the latter. Similar policies distinguish type-only references from runtime access, or implementation/inheritance from ordinary use. The normalized [Reference](../../src/scip.rs) currently retains line/symbol/spelling and uses occurrence roles only to recognize definitions; [Dep](../../src/scan.rs) has no dependency kind. Symbol relationships and signature metadata are not currently consumed there.

**Primary precedents:** Sonargraph's DSL supports filtering dependency types; ArchUnit supplies predicates on method return/parameter types. Those are concrete richer contracts, not merely better import matching. [Sonargraph grammar](https://eclipse.hello2morrow.com/doc/standalone/content/architecture_grammar.html), [ArchUnit CodeUnitsShould](https://github.com/TNG/ArchUnit/blob/main/archunit/src/main/java/com/tngtech/archunit/lang/syntax/elements/CodeUnitsShould.java).

**Why a spike:** SCIP supplies import/read/write occurrence roles, implementation relationships, and optional signature documentation with optional referenced-symbol occurrences. It does not supply a universal CALLS/EXTENDS/IMPLEMENTS enum, a universal public-visibility flag, or mandatory structured parameter/return edges. `ForwardDefinition` is a declaration role, not evidence that a dependency leaks through a public API. [SCIP specification](https://github.com/sourcegraph/scip/blob/main/scip.proto).

Inspect pinned indexer output on fixtures for Go, TS, Python, Rust and Ruby before designing a rule. Establish which facts exist for aliases, generic types, inferred returns, visibility, re-exports and interface implementations. Normalize supported relationships into language-agnostic facts at the index/adapters boundary; capability metadata must distinguish unknown from absent. Missing kind information must not silently pass a rule. Related semantic extraction work belongs with [issue #85](https://github.com/mokevnin/easement/issues/85); this note does not re-verify that issue's live status.

**Acceptance example after the spike:** a public facade returning `storage.Entity` fails with the API declaration and leaked type; using the same type solely inside the implementation passes. An indexer lacking signature evidence produces an explicit unsupported/partial result. Start with one general relationship model and one separately agreed rule, not language-specific YAML keys.

## Do not recommend as the next increment

- An MCP wrapper without a new capability: a transport may help adoption but does not itself close these policy gaps; a JSON CLI can support the first versions.
- A universal service-call graph inferred from imports: HTTP/RPC edges need explicit contracts or framework-specific evidence. Sharing a polyglot report cannot establish them. Sonargraph has a separate Spring/Feign plugin for this purpose. [Plugin mechanism](https://eclipse.hello2morrow.com/doc/standalone/content/spring_microservices_plugin.html).
- A general metrics dashboard or autonomous architecture repair: both broaden scope substantially before answering the existing agent's concrete preflight and remediation questions.
- Language-specific dependency rules that bypass the shared model, or adding syntax before validating index facts. This conflicts with the repository's architecture and produces misleading coverage guarantees.

The strongest sequence is proposed-edge validation, policy review, then consumer-specific interfaces. Legal-target discovery can reuse preflight. Rich dependency kinds should proceed only after evidence establishes the normalized facts available from actual indexers.
