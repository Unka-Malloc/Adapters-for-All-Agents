# Changelog

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
- Bound retained structured CLI output, drain oversized records, and continue
  reading session bindings and usage after the output budget is exhausted.
- Apply output budgets to RPC results and delivered events without cancelling
  the turn. Preserve cancellation requested before the prompt acknowledgement.
- Pump CLI input and output concurrently with bounded terminal read queues.
- Expand templates once without interpreting placeholder-like user data.
- Protect flat RPC control fields from parameter overrides, and surface
  contradictory model acknowledgements and invalid session receipts.
- Expose the same runtime capabilities through the `a4aa` sidecar.
- Publish the matching adapter crates and `a4aa-dialects` registry at 0.2.0.

### Updating from 0.1.x

Update custom `SessionRequest` values with a `SessionConfig` (or use their
defaults). Construct CLI lane variants with `LaneSpec::Cli(Box::new(...))`.
Custom lane descriptors must account for the new configuration and lifecycle
fields; use defaults for optional behavior. Direct `RpcLane` construction now
supplies `max_output_bytes` (`None` keeps unbounded output). Match the new `AgentEvent::Usage`
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
