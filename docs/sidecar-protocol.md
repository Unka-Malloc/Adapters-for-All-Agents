# a4aa.v1 — sidecar wire protocol

`a4aa serve` runs a JSON-RPC 2.0 service over NDJSON on its own stdin/stdout.
Hosts in any language (Go, TypeScript, …) spawn the binary and reuse this
crate's adapters verbatim — one subprocess per host process.

## Framing

- One JSON-RPC message per line (`\n`-terminated), UTF-8, max 1 MiB + line
  terminator slack. Oversized or malformed lines are reported through the
  delegate's protocol-error path and the stream continues.
- Request ids are arbitrary JSON values and are echoed verbatim (ACP-style
  UUID strings are legal ids).
- Every method is dispatched on its own task: a long-running
  `session/prompt` never blocks `session/cancel` or events behind it.

## Methods (host → sidecar)

| Method | Params | Result |
| --- | --- | --- |
| `ping` | `{}` | `{protocol: "a4aa.v1", version: <crate version>}` |
| `session/open` | `{spec: AgentSpec, cwd?, env?: {K: V}, resume?, options?: {model?, mode?, effort?, permission?}}` | `{sessionId, nativeSessionId, capabilities, agentInfo, sessionState}` |
| `session/prompt` | `{sessionId, text, model?, effort?}` | `TurnOutcome` (`{stopReason, text?, usage?}`) |
| `session/cancel` | `{sessionId}` | `{}` |
| `session/close` | `{sessionId}` | `{}` (idempotent) |

`session/cancel` is only defined as an id-bearing request: its outcome
(cancelled, or an unknown-session error) is acknowledged in the response. A
notification-form `session/cancel` is ignored with a stderr warning — never
silently applied.

`AgentSpec` is the crate's canonical spec (serde, `camelCase` fields
documented in `model.rs`): `id`, `clientName?`, `command{program,args,cwd?,
env}`, `maxOutputBytes?`, plus the flattened lane — `"lane": "acp" | "rpc" |
"cli" | "serve"` with the lane's own fields.

Session options select the agent's model, reasoning effort, execution mode,
and permission preset. A lane rejects unsupported selectors before starting
work. ACP choices are negotiated with the agent; `agentInfo` and `sessionState`
contain the returned agent metadata and configuration, or `null` for other
lanes. Typed adapter errors appear in the JSON-RPC error's `data` object as
`code`, `stage`, `retryable`, and `uncertainEffect`.

## Traffic (sidecar → host)

- `session/event` notification — `{sessionId, event: AgentEvent}` streamed
  while a prompt is in flight (`MessageChunk`, `ToolCall`, `Usage`, `Raw`, …).
  Event variants use their serde wire representation from `model.rs`.
- `permission/request` request — `{sessionId, toolCallId, title, kind,
  contentTexts, options, raw}`;
  the host responds `{cancelled: true}` or `{selected: "<optionId>"}`.
  Unanswered permissions fail closed (the agent sees `cancelled`).

## Lifecycle

`session/close` also closes an active turn through the shared session handle.
It acknowledges only after process cleanup; new prompts are rejected once
closing starts. A cleanup failure remains visible through the retained session
entry; a successful close removes the entry.

stdin EOF closes every open session, including sessions still referenced by
active requests. Pending turns settle with cancellation or a closed-session
error; calls left on a closed transport fail with `peer_closed`. The sidecar
exits 0 after successful cleanup, or nonzero if session cleanup fails.
