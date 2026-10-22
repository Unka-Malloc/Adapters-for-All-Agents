# Design direction: native-max adapters

A4AA's direction is **bottom-up**: agent-native capability → common
language. The native agent is maximal.

The framework translates whatever an agent natively offers — process
supervision, transports, protocol lanes, mechanism surfaces (hooks,
rules, skills, commands, subagents, plugins, permissions, MCP) — into
one shared vocabulary. It never invents semantics the agent lacks: the
union of expressible semantics is the union of what agents natively
offer.

## Completeness rule

Every interaction form an agent natively offers gets its own adapter.
Partial support is an **explicit declaration, never silent removal** —
encoded as lane completeness (`capabilities()`).

## Corollaries

- A missing lane or mechanism on an agent is a **capability fact**, not
  an adapter defect. It is reported as such (`unsupported`), never
  papered over.
- When a host cannot express a concept, the right answer is a declared
  gap in the adapter — never semantic invention inside the lane.
- Work on one lane must not require changes to a sibling lane; failures
  never propagate between lanes.

The complementary direction — a designed runtime language projecting a
subset of itself onto whatever each host natively expresses — is a valid
design for a different kind of system, but it is out of this
repository's scope.

Native support for each dialect is recorded in that crate's `NATIVE`
table and aggregated in [native-capability.md](native-capability.md).
A cell is `native`, `not-offered`, `unadapted`, or `non-native`
(plugin/external adapter, flagged for removal). `last_verified` must be
refreshed before a release.
