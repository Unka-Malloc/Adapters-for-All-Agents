# Native-capability master table

Machine source: each `a4aa-<agent>` crate publishes `NATIVE`
(`crates/a4aa-<agent>/src/native.rs`). `a4aa_dialects::natives()`
aggregates them. This document is the human aggregate those consts must
match.

`last_verified` is an ISO date (`YYYY-MM-DD`) and a **release gate**, not
a per-commit lint. Refresh every dialect table against the installed
binary and vendor docs before a release. The train refuses a candidate
whose dates fall outside the freshness window:

```
python3 .github/release.py freshness --window-days=14
```

Cell vocabulary (matches `a4aa::NativeStatus` kebab-case):

| Status | Meaning |
| --- | --- |
| `native` | The agent offers this natively and A4AA adapts that native form |
| `not-offered` | The agent does not offer this |
| `unadapted` | The agent offers this natively; A4AA has not adapted it yet |
| `non-native` | Currently reached through a plugin or external adapter (flagged for removal) |

Capability ordering for interaction surfaces: Serve > SDK > ACP/RPC > CLI.

Copilot is not in this table: there is no `a4aa-copilot` crate. Its
installed `copilot --acp` surface is native but unowned here.

## Interaction surfaces

| Agent | last_verified | serve | sdk | acp | rpc | cli | pty |
| --- | --- | --- | --- | --- | --- | --- | --- |
| antigravity | 2026-10-21 | not-offered | not-offered | not-offered | not-offered | native | native |
| claude | 2026-10-21 | not-offered | not-offered | not-offered | not-offered | native | native |
| codex | 2026-10-24 | not-offered | not-offered | not-offered | native | native | native |
| commandcode | 2026-10-21 | not-offered | not-offered | not-offered | not-offered | native | native |
| cursor | 2026-10-21 | not-offered | not-offered | not-offered | not-offered | native | native |
| devin | 2026-10-21 | not-offered | not-offered | native | not-offered | native | native |
| dsh | 2026-10-24 | not-offered | native | native | native | not-offered | native |
| factory | 2026-10-21 | native | not-offered | native | native | native | native |
| grok | 2026-10-21 | not-offered | not-offered | native | not-offered | native | native |
| hermes | 2026-10-21 | not-offered | not-offered | native | not-offered | not-offered | native |
| kilocode | 2026-10-21 | unadapted | not-offered | native | not-offered | not-offered | native |
| kimicode | 2026-10-21 | not-offered | not-offered | native | not-offered | native | native |
| kiro | 2026-10-21 | unadapted | not-offered | native | unadapted | native | native |
| openclaw | 2026-10-21 | unadapted | not-offered | native | not-offered | native | native |
| opencode | 2026-10-21 | native | not-offered | native | not-offered | not-offered | native |
| pi | 2026-10-21 | not-offered | not-offered | not-offered | native | native | native |

Primary lane on each card (capability-ordered, plugin surfaces dropped):

| Agent | Card primary | Native form used | Notes |
| --- | --- | --- | --- |
| antigravity | cli | `agy --print --output-format stream-json` | `agy_acp_server.par` dropped |
| claude | cli | `claude --print` stream-json | vendored `claude-agent-acp` dropped |
| codex | rpc | `codex app-server --stdio` | vendored `codex-acp` dropped; host MCP servers installed with `-c mcp_servers.*` |
| commandcode | cli | `command-code --print --output-format json` | no ACP/SDK/serve |
| cursor | cli | `cursor-agent --print` stream-json | no acp subcommand |
| devin | acp | `devin acp` | CLI `-p` is an alternate |
| dsh | rpc (sdk) | `dsh --profile sdk` | capability order ranks SDK above ACP; `dsh acp` is the ordered alternate with durable resume/cancel/streaming |
| factory | acp | `droid exec --output-format acp` | first-party; daemon WebSocket and stream-jsonrpc are alternates |
| grok | acp | `grok agent` stdio | CLI `-p` streaming-json is an alternate |
| hermes | acp | `hermes acp` | no headless print lane |
| kilocode | acp | `kilo acp` (OpenCode-fork stock) | `kilo serve` HTTP+SSE is native and unadapted; should become primary |
| kimicode | acp | `kimi acp` | CLI `-p` stream-json is an alternate |
| kiro | acp | `kiro-cli acp` → bundled `kiro-cli-chat` | `kiro-cli-chat serve` WebSocket is native and unadapted |
| openclaw | cli | `openclaw agent exec --json` | Gateway ACP is the alternate; Gateway itself is unadapted serve |
| opencode | serve | `opencode serve --port 0` | `opencode acp` is the ordered alternate |
| pi | rpc | `pi --mode rpc` | custom pi-acp retired; `--print` is the cli alternate |

## MCP registration and config locations

| Agent | last_verified | MCP mechanism | MCP detail | settings | hooks | permissions | model catalog |
| --- | --- | --- | --- | --- | --- | --- | --- |
| antigravity | 2026-10-21 | config-key | `.gemini/config/mcp_config.json mcpServers` | `{home}/.gemini/antigravity-cli/settings.json` | `{home}/.gemini/config/hooks.json` | — | `agy models` |
| claude | 2026-10-21 | config-key | `.mcp.json mcpServers` (project, not adapted) | `{home}/.claude/settings.json` | `{home}/.claude/settings.json hooks` | `{home}/.claude/settings.json permissions.defaultMode` | — |
| codex | 2026-10-24 | config-key | `codex app-server -c mcp_servers.<name>.{command,args,env_vars}`; `default_tools_approval_mode` only for servers named in `mcpConfig.toolsApprovalServers`; config.toml `[mcp_servers]` | `{home}/.codex/config.toml` | `{home}/.codex/hooks.json` | sandbox flags | `model/list` |
| commandcode | 2026-10-21 | unsupported | `McpSurface::None` | `{home}/.commandcode/config.json` | `{home}/.commandcode/settings.json hooks` | `--permission-mode` | `command-code --list-models` |
| cursor | 2026-10-21 | config-key | `.cursor/mcp.json mcpServers` | `{home}/.cursor/cli-config.json` | `{home}/.cursor/hooks.json` | `{home}/.cursor/cli-config.json permissions` | model-id suffix |
| devin | 2026-10-21 | config-key | `.config/devin/mcp_config.json mcpServers` | `{home}/.config/devin/config.json` | `{home}/.config/devin/config.json hooks` | `--permission-mode` | `devin models` |
| dsh | 2026-10-24 | unsupported | `McpSurface::None` | `{home}/.dsh/.credentials.yaml` | — | — | ACP `session/new` configOptions; Command Code route provisioned by `a4aa-dsh mount` |
| factory | 2026-10-21 | config-key | `.factory/settings.json mcp` | `{home}/.factory/settings.json` | — | `--auto` autonomy | `droid.list_models` |
| grok | 2026-10-21 | protocol | ACP mcp http/sse | `{home}/.grok/config.toml` | `{home}/.grok/hooks` | `--permission-mode` | `grok models` |
| hermes | 2026-10-21 | unsupported | `McpSurface::None` | `{home}/.hermes/config.yaml` | — | — | — |
| kilocode | 2026-10-21 | config-key | `.config/kilo/kilo.json mcp` | `{home}/.config/kilo/kilo.json` | `{home}/.config/kilo/plugin` | OpenCode per-tool object inexpressible | `kilo models` |
| kimicode | 2026-10-21 | protocol | ACP mcp http/sse | `{home}/.kimi-code/config.toml` | — | distinct flags, not a selector | — |
| kiro | 2026-10-21 | config-key | `.kiro/settings/mcp.json mcpServers` | `{home}/.kiro` | IDE trigger hooks unmapped | — | `kiro-cli-chat chat --list-models` |
| openclaw | 2026-10-21 | unsupported | `McpSurface::None` | `{home}/.openclaw` | — | — | `openclaw models` |
| opencode | 2026-10-21 | config-key | `.config/opencode/opencode.json mcp` | `{home}/.config/opencode/opencode.jsonc` | `{home}/.config/opencode/plugins/a4aa.js` | — | serve / ACP |
| pi | 2026-10-21 | unsupported | `McpSurface::None` | `{home}/.pi/agent/settings.json` | `{home}/.pi/agent/extensions/*.ts` | — | `get_available_models` |

## AwP primitives

| Agent | last_verified | model | effort | open | resume | cancel | streaming | instructions | permission modes | skills | hooks | subagents | plugins |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| antigravity | 2026-10-21 | native | native | native | native | native | native | native | not-offered | native | native | native | not-offered |
| claude | 2026-10-21 | native | native | native | native | native | native | native | native | native | native | native | not-offered |
| codex | 2026-10-24 | native | native | native | native | native | native | native | native | native | native | native | not-offered |
| commandcode | 2026-10-21 | native | native | native | native | native | native | native | native | native | native | not-offered | not-offered |
| cursor | 2026-10-21 | native | native | native | native | native | native | native | not-offered | native | native | native | not-offered |
| devin | 2026-10-21 | native | not-offered | native | native | native | native | native | native | native | native | native | not-offered |
| dsh | 2026-10-24 | native | native | native | native | native | native | native | not-offered | native | not-offered | not-offered | not-offered |
| factory | 2026-10-21 | native | native | native | native | native | native | native | native | native | not-offered | native | not-offered |
| grok | 2026-10-21 | native | native | native | native | native | native | native | native | native | native | native | not-offered |
| hermes | 2026-10-21 | not-offered | native | native | not-offered | native | native | native | not-offered | native | not-offered | not-offered | native |
| kilocode | 2026-10-21 | native | native | native | native | native | native | native | not-offered | native | native | native | native |
| kimicode | 2026-10-21 | native | not-offered | native | native | native | native | native | not-offered | native | not-offered | not-offered | not-offered |
| kiro | 2026-10-21 | native | native | native | native | native | native | native | not-offered | native | not-offered | native | not-offered |
| openclaw | 2026-10-21 | native | native | native | not-offered | native | native | native | not-offered | native | not-offered | native | not-offered |
| opencode | 2026-10-21 | native | native | native | native | native | native | native | native | native | native | native | not-offered |
| pi | 2026-10-21 | native | native | native | native | native | native | native | not-offered | native | native | not-offered | native |

## Remaining native-lane audit

Recorded 2026-10-21 against each dialect crate's `NATIVE` table and the
installed binaries named in those tables. Copilot has no crate here.

| Agent | Card primary | Native surfaces | Missing native surface | Plugin / external adapter | Verdict |
| --- | --- | --- | --- | --- | --- |
| grok | acp (`grok agent` stdio) | acp, cli (`grok -p` streaming-json), pty | none | none | keep; CLI is the ordered alternate |
| openclaw | cli (`openclaw agent exec --json`) | cli, acp (Gateway), pty | Gateway serve (unadapted) | none; Gateway ACP is first-party | keep exec primary; adapt Gateway serve later |
| open-claw | same crate as openclaw (`a4aa-open-claw`, id `openclaw`) | same | same | same | same |
| copilot | unowned — there is no `a4aa-copilot` crate | installed `copilot --acp` is native on the host | crate and card missing | none owned here | unowned; do not claim adapted |
| devin | acp (`devin acp`) | acp, cli (`devin -p`), pty | none | none | keep; CLI is an alternate |
| hermes | acp (`hermes acp`) | acp, pty | no headless print CLI; resume not offered | none | keep; print/resume are not-offered, not unadapted |
| kimi | acp (`kimi acp`; crate id `kimicode`) | acp, cli (`kimi -p` stream-json), pty | none | none | keep; CLI is the ordered alternate |
| kiro | acp (`kiro-cli acp` → bundled `kiro-cli-chat`) | acp, cli (stream-json), pty | `kiro-cli-chat serve` WebSocket (unadapted); rpc cell also unadapted for that transport | none | keep acp until serve is adapted; evidence: `kiro-cli-chat serve` is a first-party WebSocket and is not on this card |
| command-code | cli (`command-code --print --output-format json`) | cli, pty | no ACP/SDK/serve | none | keep |
| factory | acp (`droid exec --output-format acp`) | serve (daemon WS), acp, rpc (stream-jsonrpc + daemon), cli, pty | none | none; `droid exec --output-format acp` is first-party, not a plugin | keep; daemon WS and stream-jsonrpc are declared alternates |
| kilo-code | acp (`kilo acp`, OpenCode-fork stock) | acp, pty | `kilo serve` HTTP+SSE (native but unadapted) | `.config/kilo/plugin` is the dialect's native plugin directory, not an external ACP adapter | promote serve to card primary, keep acp as the ordered alternate |

kilo-code serve-vs-acp: `kilo acp` is a native OpenCode-fork subcommand (not a plugin). `kilo serve` HTTP+SSE is also native; A4AA still cards ACP. Serve should become primary.

kiro serve: `kiro-cli acp` delegates to bundled `kiro-cli-chat` ACP (native). `kiro-cli-chat serve` WebSocket is a separate first-party surface, recorded as unadapted serve (and unadapted rpc transport) — not yet a card lane.
