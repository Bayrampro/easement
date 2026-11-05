# easement

An architecture quality gate: a declared policy says which parts of a project may depend on which, and easement judges the project's real dependencies against it, saying what its verdict does not cover.

## Policy

**Policy**:
Everything the project declares about allowed structure: components, rules, layers, surfaces and examples, across every file that contributes to it.
_Avoid_: config, rules (a rule is one part of the policy)

**Policy input**:
A file whose content is part of the policy: the configuration, every configuration it extends, the drawn diagram.
_Avoid_: config files

**Component**:
A named set of files that the policy treats as one owner; a file belongs to at most one component.
_Avoid_: module, package, layer

**Grant**:
What permits one component to depend on another: an explicit allowance, a common component, a lower layer, being test code, or being the same component.
_Avoid_: allow source, permission

**Prohibition**:
An explicit statement that one component must not depend on or reach another; it overrides every grant.
_Avoid_: deny, ban

**Surface**:
The part of a component that other components may use. The public surface is open to every consumer the policy lets in; a named surface is open only to the consumers it lists.
_Avoid_: interface (reserved for language-level interfaces and contracts), API, entry point

## Decisions

**Dependency decision**:
The verdict on one dependency: allowed by a grant, or rejected by one rule, with the reason the policy gives.
_Avoid_: result, outcome

**Violation**:
A dependency or structure the policy rejects, located in the source or the policy.

**Proposed dependency**:
A dependency judged before the code that would create it exists.
_Avoid_: hypothetical edge, synthetic edge, preflight edge

**Policy example**:
A proposed dependency kept in the policy together with the decision it must receive, so that a policy change that flips it fails.
_Avoid_: contract test, policy test, contract

**Policy delta**:
The dependency decisions that differ between two policies over the same code.
_Avoid_: config diff

## Trust in a verdict

**Narrowed verdict**:
A verdict that, on request, judges only part of the project or only part of a change.
_Avoid_: partial verdict

**Evidence gap**:
Code or facts the check could not read or could not judge, so a clean verdict says nothing about them.
_Avoid_: partial, unread reason, missing coverage

**Accepted gap**:
An evidence gap the policy declares deliberate; it is still reported and never counts as read.
_Avoid_: ignored file, covered file

**Legalized violation**:
A violation recorded in the baseline as accepted debt until it is fixed.
_Avoid_: ignored violation, suppressed violation

**Suppression**:
An exception written at one source line, with its reason, for one rule.
_Avoid_: ignore, legalization
