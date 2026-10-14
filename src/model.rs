//! Provider-neutral adapter model.
//!
//! An [`AgentSpec`] fully describes how to reach one agent: which lane speaks
//! to it (`acp`, `rpc`, `cli`), the command to spawn, its working directory,
//! and the environment it may see. Specs are plain data so hosts in other
//! languages can supply them over the sidecar protocol.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::env::EnvPolicy;

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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CliLaneSpec {
    /// Argument template. `{prompt}`, `{model}` and `{effort}` placeholders
    /// are substituted per turn.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub prompt_channel: PromptChannel,
    #[serde(default)]
    pub stream: StreamMode,
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
    /// Called once at open when the agent is session-oriented; its result's
    /// `sessionId`/`session_id`/`id` field becomes the session binding.
    #[serde(default)]
    pub session_open: Option<RpcCallSpec>,
    /// Awaited per turn; the result ends the turn.
    pub prompt: RpcCallSpec,
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
    /// Dotted path to the text inside a prompt result (`"text"`,
    /// `"message.content"`). Absent → the whole result serializes as text.
    #[serde(default)]
    pub result_text_path: Option<String>,
    /// Methods the consumer may call verbatim through the lane handle.
    #[serde(default)]
    pub passthrough: Vec<String>,
}

impl RpcLaneSpec {
    /// Declared capabilities — the single authority both `Adapter` and the
    /// open session report.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: self.text_event_method.is_some(),
            cancellation: self.cancel.is_some(),
            session_resume: false,
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
    },
    Rpc(Box<RpcLaneSpec>),
    Cli(CliLaneSpec),
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
    /// Output exceeded `max_output_bytes`; delivery is complete up to this
    /// point and no further chunks arrive.
    Truncated,
    /// Lane-native event that did not map onto a canonical kind.
    Raw { raw: serde_json::Value },
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
}
