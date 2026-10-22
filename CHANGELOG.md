# Changelog

## 0.3.0

A breaking update from 0.2.x: the lane rework adds required fields to
constructible lane/surface types, and `command_code_patch` takes the
gateway base URL as a parameter. Update dependent crates together.

- Rework the acp/rpc/cli lane surfaces for lane independence, add the
  `max_frame_bytes` session bound, and tighten the sidecar session
  contract; permission/request now reports the host-facing session id,
  with the agent-native id retained under `raw`.
- Add per-agent `NATIVE` capability tables aggregated by
  `a4aa_dialects::natives()` into `docs/native-capability.md`; table
  freshness is enforced as a release gate by the generated release train
  (`.github/release-train.py`, weekly train / manual / tag-push paths).
- Mandate environment fidelity: specs declare how values resolve from
  the user's real environment; agents are never launched empty.
- Surface quota, usage-limit and capacity refusals as retryable, certain
  `agent_quota_exhausted` on all lanes so custody can fail over to the
  next candidate; the refusal classification is shared from `error.rs`,
  and unclassified failures keep the `error` stop reason.
- Restrict `InheritAll` over the sidecar wire to launches declaring
  `--trust-host-env`; untrusted callers are confined to the inert
  allowlist plus resolved literals, per the Environment Fidelity
  contract's custody boundary.
- Bound NDJSON frames at 1 MiB per lane by default with per-spec
  `max_frame_bytes` opt-in (pi declares 64 MiB); the sidecar↔host link
  carries its own documented 64 MiB bound.
- Make `command_code_patch` take the gateway `base_url` explicitly and
  require `https://`.
- Fix `StderrTail` truncation to cut on UTF-8 boundaries so a long
  multibyte stderr stream cannot panic and poison the tail lock.

## 0.2.1

Documentation and release-plumbing release; no behavior changes.

- Add `docs/design-direction.md` stating A4AA's native-max design
  direction: host capabilities are translated into a shared language, and
  declared capability gaps are reported, never silently dropped.
- Make the tag-triggered publish workflow treat an already-uploaded crate
  as a successful skip instead of a retry failure.

## 0.2.0

This release expands the public runtime descriptors and session configuration.
It is a breaking update from 0.1.x; update the core and adapter crates together.

- Add agent cards describing discovery, protocol entry points, supported
  selectors, usage reporting, and history locations as data.
- Add explicit session model, reasoning effort, mode, and permission selectors.
  Unsupported selectors fail before a turn begins.
- Extend ACP configuration negotiation, RPC session and completion handling,
  and CLI session binding, resume, cancellation, structured output, and usage.
- Preserve RPC notification order and report transport closure to pending turns.
- Keep completion notifications and streamed text reliable across bursts that
  arrive before a prompt acknowledgement.
- Honor explicit CLI cancellation during prompt writes and after output EOF.
- Preserve ACP cancellation during configuration and bound delivered ACP output
  without blocking permission or completion handling. Report resume-only support.
- Bound retained structured CLI output, drain oversized records, and continue
  reading session bindings and usage after the output budget is exhausted.
- Apply RPC output budgets at notification ingress, before slow host callbacks
  can accumulate payloads. Preserve response/permission progress and event order.
- Require valid acknowledgement IDs for correlated completion; preserve explicit
  cancellation before acknowledgements and during per-turn configuration.
- Reject unsupported resume requests at open, before spawning or reporting a binding.
- Keep Claude permission bypass opt-in. Generated extension observers support
  Notify hooks only; enforcement, injection and rewriting report Unsupported.
- Retain failed receipt evidence with its location in the typed error.
- Pump CLI input and output concurrently with bounded terminal read queues.
- Expand templates once without interpreting placeholder-like user data.
- Preserve session instructions in RPC prompt and configuration templates.
- Keep dispatched RPC turns owned by the session driver when a caller drops
  its waiting future; serialize request admission with explicit cancellation.
- Clean up descendants before reaping exited process-group leaders. On macOS,
  distinguish groups already in exit from real signal permission failures.
- Preserve required empty prompt arguments and JSON fields; omit only optional
  selectors that were not provided.
- Interpret removable CLI flags from literal templates, never expanded user text.
- Close active sessions through shared handles, including sidecar close and EOF,
  and reject new work once closing starts. Preserve cleanup failures without
  acknowledging a successful close.
- Propagate required RPC initialized-notification failures at the handshake stage.
- Resume Codex sessions without replaying historical turn payloads to the host.
  Fail pending RPC calls when a frame cannot be decoded instead of waiting for
  a response that was discarded.
- Derive card-generated CLI usage paths from the card's live usage declaration.
- Protect flat RPC control fields from parameter overrides, and surface
  contradictory model acknowledgements and invalid session receipts.
- Expose the same runtime capabilities through the `a4aa` sidecar.
- Publish the matching adapter crates and `a4aa-dialects` registry at 0.2.0.

### Updating from 0.1.x

Update custom `SessionRequest` values with a `SessionConfig` (or use their
defaults). Construct CLI lane variants with `LaneSpec::Cli(Box::new(...))`.
Custom lane descriptors must account for the new configuration and lifecycle
fields; use defaults for optional behavior. Direct `RpcLane` and `AcpLane`
construction now supplies `max_output_bytes` (`None` keeps unbounded output).
Card `CliSurface` no longer repeats `usage_path`; declare it once in
`UsageSurface.live.path`. Custom `CliLaneSpec` still carries its own usage path.
Match the new `AgentEvent::Usage`
variant when handling events exhaustively. RPC cancellation is asynchronous.
Existing 0.1.x registry packages remain available for consumers that have not
updated their source yet.

The 0.2.0 workspace and registry no longer include the following adapters:
`augment`, `codebuddy`, `craftagent`, `gemini`, `iflow`, `lingma`, `qoder`,
`qwen`, `roocode`, `trae`, and `windsurf`. Crate names use the `a4aa-`
prefix; the compound names are `a4aa-code-buddy`, `a4aa-craft-agent`, and
`a4aa-roo-code`. Hosts that still require these descriptors should retain the
applicable published 0.1.x adapter versions. Registry lookup in 0.2.0 returns
`None` for these identifiers; no replacement is selected automatically.
