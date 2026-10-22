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

## Environment fidelity (hard rule)

On a *trusted local launch* agent infrastructure never launches an
agent with an empty or minimal "no variables" environment — the user's
machine is the source of truth. Presets, cards and specs declare how
every needed value is *resolved* from the real environment: host
environment variables (PATH and friends), values inside configuration
files (TOML, YAML or JSON — read by key lookup, never copied by hand),
and every model or provider the agent can use. The custody layer
resolves those declarations into concrete values before spawn; the
child receives the user's real environment plus explicit overrides.

An agent started "clean" loses the user's configuration, credentials,
providers and tools — that is a defect, not hardening. Security lives
in *where* values resolve (locally, at the custody boundary) and
*what* may cross a remote boundary (literal values, not references) —
never in running with no environment at all.

Fidelity is an explicit grant, not a default. A card's `spec()`
materializes a sanitized environment — adopting the card alone never
exposes host credentials. `InheritAll` exists only through the host's
own opt-in (`spec_inheriting_host_env()`, custody presets, and
`a4aa serve --trust-host-env` on the trusted local chain). An
untrusted wire caller — a sidecar client — never receives ambient
credentials: `InheritAll` crosses the sidecar boundary only under the
launcher's explicit trusted-caller opt-in, and every other remote
policy is confined to the inert allowlist plus resolved literals.

## Verify

`scripts/verify.sh` — fmt + clippy + tests across the workspace.
