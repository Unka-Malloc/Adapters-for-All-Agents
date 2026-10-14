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
| `session/open` | `{spec: AgentSpec, cwd?, env?: {K: V}, resume?}` | `{sessionId, nativeSessionId, capabilities}` |
| `session/prompt` | `{sessionId, text, model?, effort?}` | `TurnOutcome` (`{stopReason, text?}`) |
| `session/cancel` | `{sessionId}` | `{}` |
| `session/close` | `{sessionId}` | `{}` (idempotent) |

`AgentSpec` is the crate's canonical spec (serde, `snake_case` internals
documented in `model.rs`): `id`, `clientName?`, `command{program,args,cwd?,
env}`, `maxOutputBytes?`, plus the flattened lane — `"lane": "acp" | "rpc" |
"cli"` with the lane's own fields.

## Traffic (sidecar → host)

- `session/event` notification — `{sessionId, event: AgentEvent}` streamed
  while a prompt is in flight (`MessageChunk`, `ToolCall`, `Raw`, …).
- `permission/request` request — `{sessionId, prompt: PermissionPrompt}`;
  the host responds `{cancelled: true}` or `{selected: "<optionId>"}`.
  Unanswered permissions fail closed (the agent sees `cancelled`).

## Lifecycle

stdin EOF is the shutdown signal: the sidecar closes every open session
(terminating agent process trees), fails pending calls with
`peer_closed`, and exits 0.
