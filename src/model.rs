//! Provider-neutral adapter model.
//!
//! An [`AgentSpec`] fully describes how to reach one agent: which lane speaks
//! to it (`acp`, `rpc`, `cli`), the command to spawn, its working directory,
//! and the environment it may see. Specs are plain data so hosts in other
//! languages can supply them over the sidecar protocol.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::env::{EnvPolicy, EnvSource};
use crate::template::{contains_placeholder, contains_placeholder_text};

/// Access lane selected for an agent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaneKind {
    /// Agent Client Protocol over stdio (NDJSON / JSON-RPC 2.0).
    Acp,
    /// Host-defined JSON-RPC methods over stdio NDJSON framing.
    Rpc,
    /// Argv-driven CLI: prompt by argument template or stdin, output on
    /// stdout (pipe) or a pseudo-terminal (pty).
    Cli,
    /// The agent's own first-party HTTP server, driven through its REST
    /// surface with request/response polling.
    Serve,
}

/// How a CLI lane carries the prompt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptChannel {
    /// Substitute `{prompt}` in the argument template; fall back to stdin
    /// when no argument carries the placeholder.
    #[default]
    ArgOrStdin,
    /// Always write the prompt to the child's stdin, then close it.
    Stdin,
    /// Require a `{prompt}` placeholder in `args`; fail closed otherwise.
    ArgOnly,
}

/// CLI lane stream selection. `Pty` degrades to `Stdio` off-Unix.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamMode {
    #[default]
    Stdio,
    Pty,
}

/// What a `binding_file_env` receipt may carry besides the session id.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindingReceiptShape {
    /// A `conversationId`-family key alone is the whole contract.
    #[default]
    SessionId,
    /// The receipt may also carry the turn's token counters — extracted
    /// with the same field-name aliases the stream `usage_path` uses (for
    /// agents whose only accounting surfaces through the hook payload).
    SessionIdAndUsage,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CliLaneSpec {
    /// Argument template. `{prompt}`, `{model}`, `{effort}`, `{mode}`,
    /// `{permission}` and `{instructions}` placeholders are substituted per turn; an argument
    /// that is exactly a placeholder resolving empty — and the flag
    /// preceding it — is dropped.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub prompt_channel: PromptChannel,
    #[serde(default)]
    pub stream: StreamMode,
    /// When the session request carries a resume binding
    /// (`{sessionId}` substitutes it): appended to `args` normally, or
    /// the complete arg template when `resume_spawn_args` also reorders
    /// argv — flags valid only at session creation do not reappear.
    #[serde(default)]
    pub resume_args: Vec<String>,
    /// Replaces the command's spawn args (`command.args`) entirely when a
    /// resume binding exists — for agents whose resume shape reorders argv,
    /// like `codex exec resume <id>` versus `codex exec`. In that shape
    /// `resume_args` replaces `args` wholesale. Empty: spawn args stay
    /// constant across fresh and resumed turns.
    #[serde(default)]
    pub resume_spawn_args: Vec<String>,
    /// JSON payload written to stdin in place of the bare prompt when the
    /// prompt channel resolves to stdin. A string leaf that is exactly
    /// `{prompt}` receives the prompt as a JSON string; any other
    /// placeholder substitutes textually.
    #[serde(default)]
    pub stdin_template: Option<serde_json::Value>,
    /// Treat stdout as newline-delimited JSON: every complete line also
    /// can arrive as `AgentEvent::Raw` within the output budget, and the
    /// declared paths below are extracted even after delivery is truncated.
    #[serde(default)]
    pub json_lines: bool,
    /// Dotted path to the session identifier on a stdout JSON line (for
    /// example `session_id` on a stream-json init event). The first value
    /// observed becomes the session binding.
    #[serde(default)]
    pub session_id_path: Option<String>,
    /// Dotted path to a token-usage object on a stdout JSON line; each
    /// observation emits `AgentEvent::Usage` and the last one lands on
    /// `TurnOutcome::usage`.
    #[serde(default)]
    pub usage_path: Option<String>,
    /// Arguments run once at open when no resume binding exists; the first
    /// non-empty stdout line becomes the session binding (for example
    /// `create-chat`). Empty: the CLI has no session-creation command.
    #[serde(default)]
    pub open_args: Vec<String>,
    /// Environment variable the lane sets to a per-turn scratch file path;
    /// an agent-side hook writes the session receipt there and the lane
    /// reads `conversationId`-family keys from it after the turn (for
    /// agents whose identity never reaches stdout).
    /// Missing receipts are optional. Existing invalid or unreadable receipts
    /// produce a typed error and remain available for diagnosis.
    #[serde(default)]
    pub binding_file_env: Option<String>,
    /// What the receipt may carry beyond the session id.
    #[serde(default)]
    pub binding_receipt_shape: BindingReceiptShape,
    /// On a `json_lines` stream, the discriminator value of the record
    /// carrying the turn's authoritative text (for example `"result"`).
    #[serde(default)]
    pub result_selector: Option<String>,
    /// Discriminator field for `resultSelector`; defaults to `type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_selector_field: Option<String>,
    /// Dotted path to the final text inside the `result_selector` record
    /// (for example `result`).
    #[serde(default)]
    pub result_path: Option<String>,
    /// Execution modes this CLI adapter can pass through. An empty list
    /// means the lane cannot apply a requested session mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_modes: Vec<String>,
    /// Permission presets this CLI adapter can pass through. An empty list
    /// means the lane cannot apply a requested permission preset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_modes: Vec<String>,
    /// Agents whose model ids carry reasoning effort as a `-<level>`
    /// suffix (Cursor's `cursor-grok-4.6-high`, `gpt-5.4-xhigh` …): a
    /// requested `effort` rewrites the tier segment of the effective
    /// model value instead of travelling as its own argument, so the
    /// `{model}` placeholder is the effort's carrier. Lists every level
    /// that suffix vocabulary uses; empty means the lane has no such
    /// encoding and `effort` needs its own placeholder. A trailing
    /// `-fast` speed marker survives the rewrite.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_model_suffixes: Vec<String>,
    /// MCP servers the host asked this lane to install. The cli lane
    /// declares no native MCP registration surface, so a non-empty list is
    /// refused at session open. Carrying the list (instead of dropping it
    /// in the custody layer) is what keeps the refusal explicit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<serde_json::Value>,
}

/// One configured JSON-RPC call: method plus a params template. String
/// leaves may carry `{prompt}`, `{sessionId}`, `{model}`, `{effort}` and
/// `{cwd}` placeholders, substituted per call.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcCallSpec {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Generic JSON-RPC lane: the host names the method surface, the lane owns
/// framing, correlation, and lifecycle.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcLaneSpec {
    /// Handshake before any session call: `initialize` is called first and
    /// `initialized` (when configured) is sent as a notification once it
    /// resolves. Agents without a handshake leave both unset.
    #[serde(default)]
    pub initialize: Option<RpcCallSpec>,
    /// Notification sent after `initialize` resolves (for example the
    /// protocol's `initialized` notification).
    #[serde(default)]
    pub initialized: Option<String>,
    /// Called once at open when the agent is session-oriented; the first
    /// `sessionId`/`session_id`/`id` field found in its result becomes the
    /// session binding (nested objects are searched to depth two).
    #[serde(default)]
    pub session_open: Option<RpcCallSpec>,
    /// Called at open instead of `session_open` when the request carries a
    /// resume binding; `{sessionId}` substitutes the requested binding.
    #[serde(default)]
    pub session_resume: Option<RpcCallSpec>,
    /// Awaited per turn. Its result ends the turn unless `completion`
    /// names a notification that does.
    pub prompt: RpcCallSpec,
    /// Async turn completion for agents whose `prompt` call only
    /// acknowledges the turn.
    #[serde(default)]
    pub completion: Option<RpcCompletionSpec>,
    /// Notification sent on explicit user cancellation.
    #[serde(default)]
    pub cancel: Option<RpcCallSpec>,
    /// Called on close, if the agent has a teardown method.
    #[serde(default)]
    pub session_close: Option<RpcCallSpec>,
    /// Inbound notification method carrying stream text (`params.text`,
    /// `params.content`, or a bare string). Other notifications arrive as
    /// `AgentEvent::Raw`.
    #[serde(default)]
    pub text_event_method: Option<String>,
    /// Dotted path inside a `text_event_method` notification to the text
    /// payload — a string leaf, or an array of `{type,text}` blocks whose
    /// text is concatenated (for example `event.data.message.content`).
    #[serde(default)]
    pub text_path: Option<String>,
    /// Dotted path to the text inside a prompt result (`"text"`,
    /// `"message.content"`). Absent → the whole result serializes as text.
    #[serde(default)]
    pub result_text_path: Option<String>,
    /// Calls issued before every `prompt` call once the session exists —
    /// per-turn configuration the agent exposes as separate requests (for
    /// example model/thinking-level selection). A call whose params
    /// template resolves to an empty object is skipped for that turn.
    #[serde(default)]
    pub pre_turn: Vec<RpcCallSpec>,
    /// How a resume binding maps onto the wire session id.
    #[serde(default)]
    pub session_scope: SessionScope,
    /// Explicit cancellation terminates the agent process instead of
    /// issuing a `cancel` call — the honest contract for agents whose only
    /// cancel semantic is process teardown. The session is not reusable
    /// after such a cancel.
    #[serde(default)]
    pub kill_on_cancel: bool,
    /// Framing conventions for agents whose stdio protocol is not stock
    /// JSON-RPC 2.0 (for example a flat `{type,id,…}` command envelope).
    /// `None` is plain JSON-RPC over NDJSON.
    #[serde(default)]
    pub envelope: Option<RpcEnvelopeSpec>,
    /// Methods the consumer may call verbatim through the lane handle.
    #[serde(default)]
    pub passthrough: Vec<String>,
    /// Byte transport the lane speaks the envelopes over. `Stdio` (the
    /// default) is the agent process's own stdin/stdout; `WebSocket`
    /// spawns the agent's first-party daemon, reads its announced
    /// `ws://` endpoint on stdout, and carries the same frames over the
    /// socket. `{port}` in the command's argv substitutes a free
    /// loopback port the lane picks.
    #[serde(default)]
    pub transport: RpcTransport,
    /// Largest inbound frame the lane accepts. `None` uses the shared
    /// default; a declared value is clamped so it can only widen, never
    /// weaken, the bound. Agents whose terminal events embed payloads as
    /// large as a full transcript (pi's `agent_end`) must declare room
    /// for their real frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_frame_bytes: Option<usize>,
    /// Dotted path inside a notification's params to a token-usage object
    /// (for example `message.usage` on pi's `turn_end`). Captured at
    /// ingress so usage survives output-budget truncation; the last
    /// observation lands on `TurnOutcome::usage`. Absent → the
    /// conventional `params.usage` / `params.message.usage` carriers are
    /// scanned instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_path: Option<String>,
    /// Dotted path on any notification's params marking a failed turn —
    /// for agents that report an errored turn inside a normal completion
    /// flow (for example pi's `message.stopReason` on `message_end`/
    /// `turn_end`). Absent disables stream-level failure detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_path: Option<String>,
    /// Marker value `failure_path` reports for a failed turn (for
    /// example `"error"`). Absent: any extracted marker means failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_value: Option<String>,
    /// Dotted path to the failure's detail text (for example
    /// `message.errorMessage`); landed on the error's detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_message_path: Option<String>,
    /// MCP servers installed for every session, in the ACP declaration shape
    /// (`{name, command, args, env}`). A lane with no `mcp_config` refuses a
    /// non-empty list at open instead of silently running without tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<serde_json::Value>,
    /// How this lane installs `mcp_servers`. `None` is an explicit
    /// no-registration declaration; it is not permission to drop the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_config: Option<RpcMcpConfig>,
}

/// How an rpc lane installs MCP servers before the first session call.
///
/// The config-override shape is for agents that accept dotted `-c
/// key=value` overrides on their own argv. Environment values never ride
/// that command line: each server's names are forwarded through the child's
/// environment, so a run token stays out of process listings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcMcpConfig {
    /// Argv flag introducing one dotted `key=value` override (`-c`).
    pub flag: String,
    /// Dotted config root under which servers are named
    /// (`mcp_servers`).
    pub root: String,
    /// Config key that forwards environment variable names into the server
    /// process (`env_vars`). Empty means env-carrying servers are refused.
    #[serde(default)]
    pub env_vars_key: String,
    /// How to express a tool-approval default for the servers named in
    /// `tools_approval_servers`. `None` means this lane cannot express one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_approval: Option<RpcMcpToolApproval>,
    /// Exact server names whose tools are pre-approved. Approval is never
    /// card-wide: a server absent from this list keeps the agent's prompting
    /// default, and an empty list pre-approves nothing. The host names each
    /// trusted server explicitly instead of every server inheriting one
    /// blanket setting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_approval_servers: Vec<String>,
}

/// One config key/value pair that pre-approves a server's tools.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcMcpToolApproval {
    /// Config key under the server table (`default_tools_approval_mode`).
    pub key: String,
    /// Value installed for that key (`approve`).
    pub value: String,
}

/// Where an rpc lane's bytes flow.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RpcTransport {
    /// The agent process speaks the protocol on its own stdio.
    #[default]
    Stdio,
    /// The command is a first-party server that announces
    /// `<announce><ws-url>` on stdout; the lane connects and drives the
    /// protocol over the WebSocket.
    WebSocket {
        /// Stdout prefix whose remainder is the `ws://` endpoint (for
        /// example `WebSocket endpoint: `).
        announce: String,
    },
}

/// Whether the wire session id is agent-durable or scoped to one process.
///
/// `Process`: every spawn mints a fresh wire id — `{requested}-<nonce>`
/// when the request carries a resume binding, `a4aa-<nonce>` otherwise —
/// because the agent's durable store rejects reusing an id across process
/// lifetimes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionScope {
    #[default]
    Agent,
    Process,
}

/// Wire conventions for an rpc lane that is not stock JSON-RPC 2.0.
///
/// The flat shape (all fields set) describes envelopes like
/// `{type,id,...inline params}` with responses
/// `{type:"response",command,success,id,data|error}`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcEnvelopeSpec {
    /// Field carrying the command/method name (`"type"`). Also
    /// discriminates inbound frames: response, request, or notification.
    pub method_field: String,
    /// Outbound call ids render as `"a4aa-<n>"` strings.
    #[serde(default)]
    pub string_ids: bool,
    /// Merge the call params object into the outbound frame's top level.
    #[serde(default)]
    pub params_inline: bool,
    /// `method_field` value marking an inbound response (`"response"`).
    #[serde(default)]
    pub response_method: Option<String>,
    /// Field carrying an inbound request/notification payload (`"data"`).
    /// Absent: `params`.
    #[serde(default)]
    pub data_field: Option<String>,
    /// Field carrying the call result on a response (`"data"`); absent:
    /// `result`.
    #[serde(default)]
    pub result_field: Option<String>,
    /// Field carrying the failure payload on a response; absent: `error`.
    #[serde(default)]
    pub error_field: Option<String>,
    /// Boolean field whose `false` value marks a response failed
    /// (`"success"`).
    #[serde(default)]
    pub success_field: Option<String>,
    /// Field on an outbound response echoing the original command name
    /// (`"command"`).
    #[serde(default)]
    pub command_field: Option<String>,
    /// Field that classifies inbound frames by value (`"type"` carrying
    /// `"request"`/`"response"`/`"notification"`); `response_method` is
    /// compared against it. Absent: `method_field` doubles as the
    /// classifier (flat envelopes where the command name occupies the
    /// same slot).
    #[serde(default)]
    pub discriminant_field: Option<String>,
    /// Discriminant value stamped on outbound requests — `"request"` on
    /// factory-style envelopes. Only meaningful with `discriminant_field`.
    #[serde(default)]
    pub request_marker: Option<String>,
    /// Discriminant value stamped on outbound notifications —
    /// `"notification"` on factory-style envelopes. Only meaningful with
    /// `discriminant_field`.
    #[serde(default)]
    pub notification_marker: Option<String>,
    /// Constant members merged into every outbound frame — protocol
    /// identity tags like `{"jsonrpc":"2.0",
    /// "factoryApiVersion":"1.0.0"}`. Frame-kind tagging belongs to
    /// `request_marker`/`notification_marker`, never to this set.
    #[serde(default)]
    pub const_fields: std::collections::BTreeMap<String, String>,
}

/// Async turn completion for agents whose `prompt` call only acknowledges
/// the turn: the named notification, not the call response, ends it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcCompletionSpec {
    /// Notification method that ends the turn (for example
    /// `turn/completed`).
    pub method: String,
    /// Dotted path in the notification params holding the turn id. When
    /// set, only a notification matching the id the prompt response
    /// reported ends the turn.
    #[serde(default)]
    pub turn_id_path: Option<String>,
    /// Dotted path to the terminal status string inside the notification
    /// params (for example `turn.status`); becomes `TurnOutcome::stop_reason`.
    #[serde(default)]
    pub status_path: Option<String>,
    /// Only a notification whose `status_path` equals this value ends the
    /// turn (for example `"idle"` on a `session.status` stream that also
    /// reports busy states). Absent: any matching notification completes.
    #[serde(default)]
    pub status_value: Option<String>,
}

impl CliLaneSpec {
    /// Declared capabilities: explicit cancel terminates the in-flight
    /// turn's process; resume exists when `resume_args` does or the argv
    /// template itself carries a `{sessionId}` placeholder.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            cancellation: true,
            session_resume: !self.resume_args.is_empty()
                || !self.resume_spawn_args.is_empty()
                || self.args.iter().any(|arg| arg.contains("{sessionId}")),
            permission_prompts: false,
            detail: None,
        }
    }
}

impl RpcLaneSpec {
    /// Declared capabilities — the single authority both `Adapter` and the
    /// open session report.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: self.text_event_method.is_some(),
            cancellation: self.cancel.is_some() || self.kill_on_cancel,
            session_resume: self.session_resume.is_some()
                || self.session_scope == SessionScope::Process,
            permission_prompts: false,
            detail: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "lane", rename_all = "kebab-case")]
pub enum LaneSpec {
    Acp {
        /// MCP servers forwarded in `session/new`. Serialized verbatim.
        #[serde(default)]
        mcp_servers: Vec<serde_json::Value>,
        /// Try `session/load` when a resume binding exists.
        #[serde(default)]
        prefer_load: bool,
        /// Credential the lane resolves and sends as `_meta.api_key` on
        /// the `authenticate` retry when session establishment is refused
        /// with auth-required. Absent: the stock method-id-only
        /// authenticate attempt.
        #[serde(default)]
        auth_key: Option<EnvSource>,
    },
    Rpc(Box<RpcLaneSpec>),
    Cli(Box<CliLaneSpec>),
    Serve {
        /// MCP servers merged into the server's injected configuration
        /// (`mcp.servers`) before spawn; the server connects them lazily
        /// per session. ACP-style `{name, command, args, env}` entries are
        /// translated to the server shape.
        #[serde(default)]
        mcp_servers: Vec<serde_json::Value>,
    },
}

/// How to spawn the agent process.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: EnvPolicy,
}

/// One agent the adapter can drive.
///
/// No `deny_unknown_fields`: `#[serde(flatten)]` needs to absorb the lane's
/// own keys (`lane`, `mcpServers`, ...) into `LaneSpec`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSpec {
    /// Opaque catalog identifier — never a path, command, or endpoint.
    pub id: String,
    /// Display name reported to agents that take a client identity.
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(flatten)]
    pub lane: LaneSpec,
    pub command: CommandSpec,
    /// Per-turn output cap; `None` is unbounded.
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
}

impl AgentSpec {
    /// Report which profile selectors this concrete lane applies.
    ///
    /// `Session` covers `SessionRequest::config`; `Turn` covers the model and
    /// effort passed on each prompt. ACP returns `Negotiated` until a
    /// session has reported its configuration choices.
    pub fn selector_support(&self, phase: SelectorPhase) -> SelectorSupportSet {
        self.lane.selector_support(phase)
    }
}

impl LaneSpec {
    /// Report selector support from the active lane template, without an
    /// AgentCard lookup or a second product catalog.
    pub fn selector_support(&self, phase: SelectorPhase) -> SelectorSupportSet {
        match self {
            Self::Acp { .. } => match phase {
                SelectorPhase::Session => SelectorSupportSet::negotiated(),
                SelectorPhase::Turn => SelectorSupportSet {
                    model: SelectorSupport::Negotiated,
                    effort: SelectorSupport::Negotiated,
                    mode: SelectorSupport::Unsupported,
                    permission: SelectorSupport::Unsupported,
                },
            },
            Self::Cli(spec) => {
                let prompt_via_stdin = match spec.prompt_channel {
                    PromptChannel::Stdin => true,
                    PromptChannel::ArgOnly => false,
                    PromptChannel::ArgOrStdin => !spec
                        .args
                        .iter()
                        .any(|argument| contains_placeholder_text(argument, "prompt")),
                };
                let consumes = |names: &[&str]| {
                    names.iter().any(|name| {
                        spec.args
                            .iter()
                            .any(|argument| contains_placeholder_text(argument, name))
                            || (prompt_via_stdin
                                && spec
                                    .stdin_template
                                    .as_ref()
                                    .is_some_and(|template| contains_placeholder(template, name)))
                    })
                };
                let model = consumes(&["model", "modelProvider", "modelId"]);
                // Effort folds into the model value wherever the lane
                // encodes tiers inside the model id — it needs no
                // placeholder of its own then.
                let effort =
                    consumes(&["effort"]) || (model && !spec.effort_model_suffixes.is_empty());
                match phase {
                    SelectorPhase::Session => SelectorSupportSet {
                        model: selector_support(model),
                        effort: selector_support(effort),
                        mode: selector_support(consumes(&["mode"])),
                        permission: selector_support(consumes(&["permission"])),
                    },
                    SelectorPhase::Turn => SelectorSupportSet {
                        model: selector_support(model),
                        effort: selector_support(effort),
                        mode: SelectorSupport::Unsupported,
                        permission: SelectorSupport::Unsupported,
                    },
                }
            }
            Self::Rpc(spec) => rpc_selector_support(spec, phase, None),
            Self::Serve { .. } => match phase {
                // `model` is applied through the server's model endpoint;
                // `permission` is negotiated against the configuration the
                // spec injects at spawn — the lane verifies it denies
                // mutation rather than trusting a requested preset.
                SelectorPhase::Session => SelectorSupportSet {
                    model: SelectorSupport::Applied,
                    effort: SelectorSupport::Unsupported,
                    mode: SelectorSupport::Unsupported,
                    permission: SelectorSupport::Negotiated,
                },
                SelectorPhase::Turn => SelectorSupportSet {
                    model: SelectorSupport::Applied,
                    effort: SelectorSupport::Unsupported,
                    mode: SelectorSupport::Unsupported,
                    permission: SelectorSupport::Unsupported,
                },
            },
        }
    }
}

impl RpcLaneSpec {
    pub(crate) fn session_selector_support(&self, resuming: bool) -> SelectorSupportSet {
        rpc_selector_support(self, SelectorPhase::Session, Some(resuming))
    }
}

fn rpc_selector_support(
    spec: &RpcLaneSpec,
    phase: SelectorPhase,
    resuming: Option<bool>,
) -> SelectorSupportSet {
    let uses_call = |call: &RpcCallSpec, names: &[&str]| {
        names
            .iter()
            .any(|name| contains_placeholder(&call.params, name))
    };
    let uses_session = |names: &[&str]| {
        let setup = if spec.session_scope == SessionScope::Process {
            false
        } else {
            match resuming {
                Some(true) => spec
                    .session_resume
                    .as_ref()
                    .or(spec.session_open.as_ref())
                    .is_some_and(|call| uses_call(call, names)),
                Some(false) => spec
                    .session_open
                    .as_ref()
                    .is_some_and(|call| uses_call(call, names)),
                None => {
                    spec.session_open
                        .as_ref()
                        .is_some_and(|call| uses_call(call, names))
                        || spec
                            .session_resume
                            .as_ref()
                            .is_some_and(|call| uses_call(call, names))
                }
            }
        };
        spec.initialize
            .as_ref()
            .is_some_and(|call| uses_call(call, names))
            || setup
    };
    let uses_turn = |names: &[&str]| {
        uses_call(&spec.prompt, names) || spec.pre_turn.iter().any(|call| uses_call(call, names))
    };
    match phase {
        SelectorPhase::Session => SelectorSupportSet {
            // Session defaults also feed the per-turn templates. At open,
            // only the setup call actually used by this request counts.
            model: selector_support(
                uses_session(&["model", "modelProvider", "modelId"])
                    || uses_turn(&["model", "modelProvider", "modelId"]),
            ),
            effort: selector_support(uses_session(&["effort"]) || uses_turn(&["effort"])),
            mode: selector_support(uses_session(&["mode"])),
            permission: selector_support(uses_session(&["permission"])),
        },
        SelectorPhase::Turn => SelectorSupportSet {
            model: selector_support(uses_turn(&["model", "modelProvider", "modelId"])),
            effort: selector_support(uses_turn(&["effort"])),
            mode: SelectorSupport::Unsupported,
            permission: SelectorSupport::Unsupported,
        },
    }
}

fn selector_support(applied: bool) -> SelectorSupport {
    if applied {
        SelectorSupport::Applied
    } else {
        SelectorSupport::Unsupported
    }
}

/// Which configuration stage consumes a selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectorPhase {
    /// `SessionRequest::config` during session establishment.
    Session,
    /// The effective per-turn model and effort.
    Turn,
}

/// Whether a selector is applied now, will be resolved through ACP
/// negotiation, or has no consumer in the active A4AA lane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SelectorSupport {
    Applied,
    Negotiated,
    #[default]
    Unsupported,
}

/// Per-selector support for one phase.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SelectorSupportSet {
    pub model: SelectorSupport,
    pub effort: SelectorSupport,
    pub mode: SelectorSupport,
    pub permission: SelectorSupport,
}

impl SelectorSupportSet {
    fn negotiated() -> Self {
        Self {
            model: SelectorSupport::Negotiated,
            effort: SelectorSupport::Negotiated,
            mode: SelectorSupport::Negotiated,
            permission: SelectorSupport::Negotiated,
        }
    }
}

/// Capabilities negotiated or declared for an open session.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub streaming: bool,
    pub cancellation: bool,
    pub session_resume: bool,
    pub permission_prompts: bool,
    /// Raw negotiated capability payload when the lane exposes one (ACP).
    #[serde(default)]
    pub detail: Option<serde_json::Value>,
}

/// One streamed event from a running turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AgentEvent {
    /// Incremental agent output, exactly as produced.
    MessageChunk { text: String },
    /// The agent announced tool activity; `raw` is the lane-native payload.
    ToolCall {
        summary: String,
        raw: Option<serde_json::Value>,
    },
    /// The agent updated its plan.
    Plan { raw: serde_json::Value },
    /// Retained output or structured record delivery was truncated. Plain
    /// text chunks stop at the output budget; control metadata may continue.
    Truncated,
    /// Lane-native event that did not map onto a canonical kind.
    Raw { raw: serde_json::Value },
    /// The agent reported token usage; `raw` preserves the source payload.
    Usage {
        usage: TokenUsage,
        #[serde(default)]
        raw: Option<serde_json::Value>,
    },
}

/// Token counters an agent reported for a turn or event. Every field is
/// optional: an absent counter is absent data, never a zero.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenUsage {
    pub input: Option<u64>,
    #[serde(default)]
    pub cached_input: Option<u64>,
    #[serde(default)]
    pub cached_write: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
    #[serde(default)]
    pub thought: Option<u64>,
    #[serde(default)]
    pub total: Option<u64>,
}

/// Extract token counters from a usage object. Field aliases cover the
/// vendor spellings (`input_tokens`, `promptTokens`, `cache_read…`,
/// pi's bare `input`/`cacheRead`); an absent counter stays absent —
/// the adapter never invents a zero.
pub(crate) fn token_usage(value: &serde_json::Value) -> Option<TokenUsage> {
    let object = value.as_object()?;
    let pick = |names: &[&str]| names.iter().find_map(|name| object.get(*name)?.as_u64());
    let usage = TokenUsage {
        input: pick(&[
            "input",
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokens",
        ]),
        cached_input: pick(&[
            "cacheRead",
            "cached_input_tokens",
            "cachedInputTokens",
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "cache_read_tokens",
        ]),
        cached_write: pick(&[
            "cacheWrite",
            "cached_write_tokens",
            "cachedWriteTokens",
            "cache_write_tokens",
        ]),
        output: pick(&[
            "output",
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "completionTokens",
        ]),
        thought: pick(&["thought_tokens", "thoughtTokens", "reasoning_tokens"]),
        total: pick(&["total_tokens", "totalTokens"]),
    };
    (usage.input.is_some()
        || usage.cached_input.is_some()
        || usage.cached_write.is_some()
        || usage.output.is_some()
        || usage.thought.is_some()
        || usage.total.is_some())
    .then_some(usage)
}

/// Terminal state of one turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnOutcome {
    pub stop_reason: String,
    /// Accumulated message text (all `MessageChunk`s concatenated).
    pub text: String,
    pub truncated: bool,
    /// Exit status where a process produced one.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Last token usage the agent reported during the turn.
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

/// An agent's self-reported implementation identity. Self-reported — an
/// adapter never treats it as attestation of the launched process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
}

/// Session-scoped configuration a host asks the agent to apply after
/// session establishment. Every field is optional; an unset field leaves
/// the agent's own default untouched. Lanes that cannot express a requested
/// value fail the open rather than silently ignoring it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionConfig {
    /// Requested model selection.
    pub model: Option<String>,
    /// Requested session mode.
    pub mode: Option<String>,
    /// Requested reasoning effort level.
    pub effort: Option<String>,
    /// Requested permission preset.
    pub permission: Option<String>,
    /// Session-scoped guidance delivered through the agent's native channel
    /// where a lane template carries the `{instructions}` placeholder.
    pub instructions: Option<String>,
    /// Provider identity for agents whose handshake carries one (the
    /// `{provider}` placeholder).
    pub provider: Option<String>,
}

/// Request to open a session.
#[derive(Clone, Debug, Default)]
pub struct SessionRequest {
    /// Resume binding from an earlier session, when the lane supports it.
    pub resume: Option<String>,
    /// Override the spec's working directory for this session.
    pub cwd: Option<PathBuf>,
    /// Extra environment applied after `EnvPolicy` resolution.
    pub env: BTreeMap<String, String>,
    /// Session-scoped configuration applied once the session exists.
    pub config: SessionConfig,
}

/// One prompt submission.
#[derive(Clone, Debug, Default)]
pub struct TurnRequest {
    pub text: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}
