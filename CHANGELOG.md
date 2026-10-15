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
- Expose the same runtime capabilities through the `a4aa` sidecar.
- Publish the matching adapter crates and `a4aa-dialects` registry at 0.2.0.

### Updating from 0.1.x

Update custom `SessionRequest` values with a `SessionConfig` (or use their
defaults). Construct CLI lane variants with `LaneSpec::Cli(Box::new(...))`.
Custom lane descriptors must account for the new configuration and lifecycle
fields; use defaults for optional behavior. Match the new `AgentEvent::Usage`
variant when handling events exhaustively. RPC cancellation is asynchronous.
Existing 0.1.x registry packages remain available for consumers that have not
updated their source yet.
