---
name: a4aa-sidecar
description: Drive a4aa adapters from any language through the `a4aa serve` sidecar — a4aa.v1 NDJSON-RPC over stdio.
---

# a4aa Sidecar mode

Spawn `a4aa serve [--trust-host-env]` and speak `a4aa.v1` — one JSON-RPC
2.0 message per NDJSON line on the child's stdin/stdout. The wire contract
is `docs/sidecar-protocol.md` in the source repository.

## When to use

- The host is not Rust (Go, TypeScript, Python, …) and cannot embed the
  crates.
- Process isolation is wanted: the sidecar holds the agent process trees,
  and stdin EOF closes every open session.

## Session shape

1. `session/open` `{spec, cwd?, env?, resume?, options?}` →
   `{sessionId, nativeSessionId, capabilities, agentInfo, sessionState}`.
   The `spec` is the same `AgentSpec` an SDK host materializes, carried as
   a host-owned document.
2. `session/prompt` `{sessionId, text, model?, effort?}` → `TurnOutcome`;
   `session/event` notifications stream while the turn runs.
3. `session/cancel` `{sessionId}` — id-bearing request only; the
   notification form is ignored by design.
4. `session/close` `{sessionId}` — idempotent; acknowledges after process
   cleanup.

## Rules that keep the contract

- Request ids echo verbatim; every method runs on its own task — a long
  `prompt` never blocks `cancel`.
- `permission/request` arrives as a sidecar → host request; answer
  `{selected}` or `{cancelled: true}`. Unanswered permissions fail closed.
- The environment boundary is enforced at spawn: untrusted callers get the
  inert `INHERITED_ENV` allowlist plus resolved literals; `secret`
  references are refused and `InheritAll` crosses the wire only under the
  launcher's `--trust-host-env` opt-in.
- Max frame is 64 MiB plus terminator slack; oversized or malformed lines
  surface as protocol errors while the stream continues.
