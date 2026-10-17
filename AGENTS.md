# Agent rules for Adapter-For-All-Agents

## Lane independence (hard rule)

The protocol lanes — `acp`, `rpc`, `cli` — are separate adaptation
mechanisms, not variants of one. Architecture keeps them non-blocking in
both directions:

- **Runtime**: a lane's failure never propagates to another lane. Errors
  surface per session through `AdapterError`; no lane holds global state
  a sibling lane depends on.
- **Code**: a lane module (`src/proto/<lane>.rs`) must not import another
  lane's internals. Shared machinery lives in lane-neutral modules —
  `template` (spec placeholders and value selectors), `model`, `env`,
  `error`, `bounds`, `transport`, `framing`, `rpc` (the NDJSON peer, not
  the lane), `supervision`, `observe`. When two lanes need the same
  helper, move it out of the lane into a shared module instead of
  importing across lanes.
- **Development**: work on one lane must not require changes to a sibling
  lane. Extending or breaking one lane never obligates touching another;
  capabilities are declared per lane via `capabilities()`, never
  negotiated against siblings.

The same rule binds the adapter crates under `crates/`: `a4aa-<agent>`
depends on `a4aa` and on nothing else.

## Lane completeness (hard rule)

Every interaction form an agent natively offers gets its own adapter — serve,
acp, rpc, cli and pty are all adapted wherever the agent supports them.
Supporting one form never excuses dropping another: the adapter exists to
cover all of them. A form that is only partially implemented is declared and
reported as incomplete — never claimed supported, and never dropped by
deciding "another lane is enough".

## Verify

`scripts/verify.sh` — fmt + clippy + tests across the workspace.
