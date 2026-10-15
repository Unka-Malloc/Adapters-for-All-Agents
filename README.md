# a4aa — Adapter For All Agents

One crate owns every agent-client adaptation path a host needs. Any Rust
application can embed this library in-process; hosts in other languages
(Go, TypeScript, …) spawn the `a4aa serve` sidecar and get the identical
adapter behavior over stdio JSON-RPC.

## What it provides

| Layer | Module | Responsibility |
| --- | --- | --- |
| Supervision | `supervision` | Process-group spawn (`command-group`: Unix pgid, Windows Job Object), sanitized environment, bounded pipes, `interrupt` (SIGINT → grace → kill) and tree teardown |
| Transport | `transport` | Piped stdio and Unix PTY (raw slave, streaming master, resize) |
| Framing | `framing` | Bounded NDJSON frames (1 MiB default cap) |
| RPC | `rpc` | Bidirectional JSON-RPC 2.0 peer: correlated outbound calls, peer-initiated requests/notifications dispatched per-task, half-close and fail-closed pending calls |
| Lanes | `proto` | `acp` (official `agent-client-protocol` 3.x), `rpc` (host-declared methods/params), `cli` (argv template or stdin prompt, stdio or PTY streaming) |
| Facade | `adapter` | `Adapter` / `AgentSession` lane-erased API: `open_session`, `prompt`, `cancel`, `close`, `before_turn` hook, permission handler |
| Sidecar | `src/bin/a4aa.rs` | `a4aa serve` — `a4aa.v1` NDJSON-RPC on its own stdio; `a4aa fixture-agent` — deterministic test agent |
| Mechanisms | `mechanisms` + `dialect` | Semantic declarations (`MechanismSet`) and the descriptor-driven adaptation engine producing an `AdaptationPlan` of artifacts + support report |

## Mechanism adaptation and per-agent crates

Beyond driving agents, a4aa adapts *configuration*: a `MechanismSet`
(hooks, rules, skills, commands, subagents, plugins, permissions, MCP
servers, native-profile selection) is translated into an agent-native
`AdaptationPlan` — the concrete config fragments to write plus a per-item
`native | adapted | unsupported` report.

Agent knowledge does not live in this crate. Each agent owns one small
descriptor crate under `crates/`, which depends only on `a4aa`:

```toml
# A host that drives only Pi installs exactly one adapter.
a4aa-pi = "0.1"
```

```rust
let plan = a4aa_pi::adapt(&mechanisms); // or a4aa::adapt(a4aa_pi::dialect(), &set)
```

| Crate | Agent knowledge it owns |
| --- | --- |
| `a4aa` (this crate) | Shared vocabulary only: `MechanismSet`, `Dialect`/`HookSurface`/surface types, the `adapt` engine, `Dialect::generic()` |
| `crates/a4aa-<agent>` × 16 | That agent's `Dialect` const: hook-store shape, skills/commands/subagents paths, permission/MCP config surfaces |
| `crates/a4aa-dialects` | Optional name→`Dialect` registry for multi-agent hosts. Pulls in all adapters; single-agent hosts should not use it |

Every adapter crate is independently installable and independently
maintained — a change to `a4aa-codex`'s descriptor never touches
`a4aa-pi`'s dependency graph. A multi-agent host picks exactly the adapter
crates it needs — or `a4aa-dialects` for runtime name lookup; no mandatory
registry stands between a host and a single agent adapter.

## Invariants

- **Zero-assumption output** — natural language is a valid turn result; no
  lane forces a schema onto agent output.
- **Fail closed** — unknown inbound requests are rejected; unresolved
  `EnvValue::Secret` and a missing permission handler never grant.
- **Explicit cancellation only** — `cancel()` is the sole cancel path;
  observer drops or transport errors are never rewritten as user cancels.
- **Bounded resources** — output caps drain-and-discard; a chatty agent can
  never exhaust host memory or stall its own pipes. Event channels are
  intentionally unbounded queues between lane and consumer: the consumer
  must drain them promptly, since the lane cannot apply backpressure to a
  host that stops reading.

## Rust usage

```rust
use a4aa::{Adapter, AgentEvent, AgentSpec, CliLaneSpec, CommandSpec,
           LaneSpec, SessionRequest, TurnRequest};
use tokio::sync::mpsc;

let spec = AgentSpec {
    id: "my-agent".into(),
    client_name: None,
    lane: LaneSpec::Cli(Box::new(CliLaneSpec {
        args: vec!["{prompt}".into()],
        ..Default::default()
    })),
    command: CommandSpec {
        program: "my-agent-cli".into(),
        ..Default::default()
    },
    max_output_bytes: None,
};
let adapter = Adapter::new(spec);
let (events, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
let session = adapter
    .open_session(SessionRequest::default(), events)
    .await?;
let outcome = session
    .prompt(TurnRequest {
        text: "hello".into(),
        ..Default::default()
    })
    .await?;
session.close().await?;
```

## Sidecar usage (Go / TypeScript)

Spawn `a4aa serve`; speak NDJSON JSON-RPC `a4aa.v1` on its stdio:

```jsonc
// → request
{"jsonrpc":"2.0","id":1,"method":"session/open","params":{"protocol":"a4aa.v1","spec":{...},"session":{...}}}
// → request (returns when the turn ends)
{"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{"sessionId":"...","text":"..."}}
// → notification
{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"..."}}
// ← events stream back as notifications
{"jsonrpc":"2.0","method":"session/event","params":{"sessionId":"...","event":{...}}}
```

Permission requests are forwarded as server→client requests
(`permission/request`); the host answers with the selected option or lets it
fail closed to `cancelled`. The full method table is
[docs/sidecar-protocol.md](docs/sidecar-protocol.md).

## Verify

```sh
scripts/verify.sh   # fmt + clippy + cargo test --all-targets
```

## Status

The crate is named `a4aa` (Apache-2.0); the sidecar protocol is versioned
as `a4aa.v1`.
