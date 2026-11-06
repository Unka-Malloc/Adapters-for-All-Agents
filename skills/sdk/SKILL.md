---
name: a4aa-sdk
description: Embed the a4aa adapter crates in a Rust host — card/spec selection, Adapter sessions, dialect adaptation.
---

# a4aa SDK mode

Embed `a4aa` plus exactly the `a4aa-<agent>` crates the host drives. The
README's "Rust usage" section is the worked example.

## When to use

- The host is Rust and drives the lanes on its own tokio runtime.
- Zero IPC overhead and no extra process matter.
- The host wants `MechanismSet → AdaptationPlan` as a pure function.

## Canonical calls

```rust
let adapter = a4aa::Adapter::new(a4aa_<agent>::spec());
let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
let session = adapter.open_session(request, events).await?;
let outcome = session
    .prompt(a4aa::TurnRequest { text, ..Default::default() })
    .await?;
session.close().await?;
```

- `a4aa_<agent>::card()` — `&'static AgentCard` (surface, detect, features,
  usage, history roots); `spec()` materializes its `AgentSpec`.
- `a4aa_<agent>::dialect()` / `adapt(&MechanismSet)` — semantic declarations
  to artifact plans; a pure function, no process spawned.
- `a4aa_dialects::{dialect, card, native}(name)` — runtime name lookup for
  multi-agent hosts; pulls in every adapter crate, so single-agent hosts
  should depend on the individual `a4aa-<agent>` crate instead.
- `a4aa_<agent>::native()` — the verified native-capability table
  (interaction surfaces, AwP primitives, `last_verified`).

## Rules that keep the contract

- A spec's `command.env` starts sanitized; `InheritAll` exists only through
  the host's explicit opt-in (`spec_inheriting_host_env`, custody presets).
- Session cancellation is `session.cancel()`, never process signals — the
  supervision layer owns the tree (process groups on Unix, Job Objects on
  Windows).
- Drain the event receiver promptly: it is an unbounded queue the lane
  cannot apply backpressure to.
- Unsupported model/effort selections are refused, not silently dropped —
  route them only through selectors the card's lane advertises.
