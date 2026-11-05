# Easement capability assessment

Assessed on 2026-11-04. Scope: the current local implementation and tests, compared with first-party competitor documentation and selected source files. No competitor was installed, no comparative fixtures were executed, and no performance or accuracy ranking is claimed. Missing documentation is not proof that a feature is absent.

Detailed evidence:

- [ArchUnit, Import Linter and Deptrac](backend-architecture-capabilities.md)
- [Dependency-cruiser, JS Boundaries, Sheriff and Go tools](ts-go-architecture-capabilities.md)
- [Sonargraph, Zügel and emerging polyglot tools](polyglot-architecture-capabilities.md)

## What the comparison establishes

Easement has a substantial policy language, beyond an import allow-list. Most individual mechanisms have precedents. Its strongest evidenced differentiation is how it combines definition-resolved restrictions, one component model for multiple ecosystems, source and manifest policy, and explicit qualifications on the resulting verdict.

The current README comparison understates its own depth: the statement that easement has no naming or call-chain rules conflicts with its `defines` and `reach: symbols` sections. These are narrower capabilities than a general class/member architecture-test framework, but they should be counted. [Current comparison](../../README.md#comparison-with-other-tools), [type ownership](../../README.md#names-of-types), [definition reach](../../README.md#reaching-through-others).

## Compare tasks, not feature names

| Concrete task | Easement mechanism | Closest verified precedent | Assessment |
| --- | --- | --- | --- |
| Reject UI access to storage, including layer skipping | Component allow-lists, explicit deny, incoming restrictions, closed layers | ArchUnit layers; Import Linter contracts; Deptrac rulesets; dependency-cruiser selectors; Sonargraph DSL | Established category. Syntax, defaults and usability differentiate implementations. |
| Keep each feature's UI with its own data layer | `<feature>` capture equality and `isolated` | Dependency-cruiser regex backreferences; JS Boundaries captures; Sheriff placeholders and `sameTag` | Compact expression is useful; slice isolation itself is not unique. |
| Allow a safe database API while banning a raw client, including aliases and typed receivers | `public_symbols` and `forbid_symbol` on resolved definitions | ArchUnit class/member/access rules; JS Boundaries imported-name restrictions are narrower | Meaningful depth versus module-only checkers, especially across several ecosystems. Not deeper than ArchUnit on JVM. |
| Reject indirect storage reach without blaming unrelated methods in the same helper file | `must_not_reach` plus `reach: symbols` | Import Linter and dependency-cruiser provide module-transitive checks; ArchUnit supplies class-transitive rules and direct member conditions | Definition-specific transitive policy is a stronger differentiator than transitivity alone. A matching built-in method-to-method transitive policy was not verified in this subset. |
| Enforce policy on declared package dependencies even when no source import uses them | `packages`, component ownership of manifests | Dependency-cruiser validates imported npm dependencies, but that is not the same declaration-only graph | A particularly interesting part of the combined model. Supports Cargo, npm and Go manifests. |
| Constrain who activates a Cargo feature | A component owns `part: feature/fake`, then ordinary boundary rules apply | No equivalent established in the reviewed architecture-tool sources | Specific and potentially distinctive, without a market-wide uniqueness claim. Not complete Cargo build verification. |
| Require a README, test file, generator directive and required imports per resource directory | `requires_file`, `requires_directive`, `must_depend_on` and `must_use_external` | Competitors have positive dependency rules or permit custom checks, but equivalent directory-level obligations were not established | Broader structural policy than import rules alone. Custom-code possibility is not a shipped equivalent. |
| Keep existing debt from growing | Shrink-only baseline by default, stale-entry failure, explicit `--accept-new` | ArchUnit freezing; dependency-cruiser shrink-only plus stale-error configuration | Strong defaults, not a unique mechanism. |
| Use the architecture drawing as policy | Mermaid input | ArchUnit PlantUML input | Useful integration, not an invention. Generating a diagram is a different feature. |
| Remove obsolete allowances safely | `suggest --patch` and `--check`, preserving reasons/comments and withholding unreliable removals | Deptrac `debug:unused` detects unused rules; automated safe patch equivalence was not established | The safety and update workflow matter more than unused-rule detection alone. |
| Tell an agent whether a clean answer can be trusted | Partial-analysis reasons, uncovered-file inventory, cache provenance, scoped-verdict labels | Dependency-cruiser unresolved/followability diagnostics; Zügel model freshness flags | Explicit coverage reporting remains valuable. Machine-readable uncertainty and freshness signaling themselves have precedents. |

Competitor evidence is linked task by task in the three detailed reports. General native rule references: [dependency-cruiser](https://github.com/sverweij/dependency-cruiser/blob/main/doc/rules-reference.md), [ArchUnit](https://www.archunit.org/userguide/html/000_Index.html), [Import Linter](https://import-linter.readthedocs.io/en/stable/contract_types/), [Deptrac](https://deptrac.github.io/deptrac/configuration/), [Sonargraph interfaces/connectors](https://eclipse.hello2morrow.com/doc/standalone/content/architecture_interface_connectors.html).

## Definition-specific reach: concrete evidence

The repository contains a Ruby fixture where `Document.host` only formats a value, while `Document.fetch` and `Document.both` use `Http`. A model calls `Document.host`:

```ruby
class Document
  def self.host(uid)
    uid.to_s
  end

  def self.fetch(uid)
    Http.get(uid)
  end
end

class Pure
  def run(uid)
    Document.host(uid)
  end
end
```

The configured model component must not reach the component holding `Http`. File-level traversal sees both methods together and reports the chain. With `reach: symbols`, easement follows the referenced definition and does not attribute `fetch`'s dependency to `host`. Further tests retain the violation when the called method actually reaches `Http` and conservatively retain it when the method cannot be resolved. [Fixture](../../tests/fixtures/ruby_reach), [CLI tests](../../tests/cli.rs#L2072), [reach implementation](../../src/check.rs).

This is static reference reach, not data-flow/taint analysis or proof of runtime behavior. Missing definition ranges, ambiguous definitions and unresolved method identity retain whole-file dependencies. `why` and `impact` still use files. [Documented semantics](../../README.md#reaching-through-others).

## Source and package policy are different contracts

Three independent questions fit the component model:

1. Does a source reference cross an allowed boundary?
2. Does its external package appear in the nearest supported manifest? (`manifests`)
3. Is a declared package or feature dependency itself allowed, even without a source reference? (`packages` and feature parts)

The Cargo feature CLI fixture rejects `bad` and `core` activating the component-owned fake feature, while accepting allowed consumers. This is a concrete additional policy surface, rather than another label for import scanning. [CLI test](../../tests/cli.rs#L1591), [manifest semantics](../../README.md#declared-dependencies).

Limits matter: the feature graph does not model `default-features = false`; workspace and manifest resolution have documented partial cases. The result cannot establish that an arbitrary release build contains no fake implementation under every build flag.

## Capability coverage is uneven

| Capability | Current documented coverage |
| --- | --- |
| Compiler/type-checker-derived dependency extraction | Go, TS/JS, Python, Rust, PHP, Java, C# and Ruby via their SCIP indexers |
| Import extraction without a SCIP index | Kotlin and Swift via tree-sitter, with resolution limits |
| Resolved symbol-use policy | Go, TS/JS, Python, Rust and Ruby; not Java/C#/PHP/Kotlin/Swift |
| Type-name ownership | Indexed languages; not Kotlin/Swift |
| Imported dependency versus manifest validation, and manifest dependency graph | npm, Go and Cargo |
| Unused-file reachability | Indexed languages other than Ruby, conservatively disabled when coverage is inadequate; Kotlin/Swift are not judged |

Sources: [language registry](../../src/lang/registry.rs), [symbol rules](../../README.md#symbols), [unused files](../../README.md#unused-files), [declared dependencies](../../README.md#declared-dependencies).

Additional limits:

- A Go backend and TypeScript frontend share policy and reporting; this does not discover a `fetch('/orders')` to Go HTTP-handler edge. Static imports/references and manifests are not a network-call graph. Sonargraph has a separate framework-specific Spring/Feign plugin for such relationships. [Easement mechanism](../../README.md#how-it-works), [Spring plugin](https://eclipse.hello2morrow.com/doc/standalone/content/spring_microservices_plugin.html).
- `--diff` reports changed import lines. Editing only a rule or deleting a manifest dependency can require a full check to expose the resulting violation. It is not equivalent to validating every consequence of a PR. [Diff semantics](../../README.md#diff-mode-judging-a-pull-request).
- Baseline entries group by rule, file, target and components, not occurrence count. An additional use with the same key in the same file is legalized by the existing entry. Shrinking the entry set does not prove the number of offending occurrences cannot grow. [Baseline semantics](../../README.md#baseline-adopting-on-an-existing-project).
- Zügel's `sourcesStale`/`arcRulesStale`/`configStale` describe model freshness; easement's uncovered-source fields describe coverage. Neither implies the other. Zügel currently documents Java, Python and C#, not the entire Sonargraph language matrix. [Zügel reference](https://blog.hello2morrow.com/2026/07/sonargraph-mcp-2/).

## Defensible conclusion

Inference from the reviewed evidence: the most promising capability story is a shared policy engine that can judge resolved code references and selected build/package declarations, expose useful agent queries, and qualify its own results. Definition-level forbidden reach, manifest/feature ownership and conservative policy pruning deserve concrete demonstrations. They are stronger evidence of differentiation than language count, YAML, baselines, diagrams or JSON alone.

This survey does not establish a unique full combination, runtime accuracy, speed or operational maturity. Those require executing the same representative fixtures against pinned tool versions.
