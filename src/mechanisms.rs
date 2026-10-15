//! Mechanism declarations and adaptation plans — the dialect-free contract.
//!
//! A [`MechanismSet`] is the dialect-free declaration a host produces:
//! *what* the agent should enforce, inject or carry —
//! lifecycle hooks, standing rules, skills, commands, subagents, plugins,
//! permissions, MCP servers, native-profile selection. It contains no
//! agent-specific schema knowledge.
//!
//! [`crate::dialect::adapt`] turns a declaration set into an
//! [`AdaptationPlan`] for one [`crate::dialect::Dialect`] descriptor: the
//! concrete artifacts to materialize (config-file fragments, `SKILL.md`
//! packs, subagent definitions, extension stubs) and a per-item [`Support`]
//! report — `native` when the agent expresses the declaration directly,
//! `adapted` when it is translated with loss, and `unsupported` when the
//! agent has no mechanism for it. The plan is plain data; the host decides
//! where the `user-home`/`project` roots live and whether to write at all.
//!
//! Agent knowledge does not live here: each `a4aa-<agent>` crate in this
//! workspace owns one [`crate::dialect::Dialect`] constant describing its
//! agent's surfaces. This module is the shared vocabulary they adapt
//! against.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::AdapterError;

// ------------------------- semantic declarations -------------------------

/// Semantic lifecycle event a hook attaches to. Hosts write these names;
/// each dialect emitter maps them onto the agent's native event names or
/// reports the item unsupported.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
    SubagentStart,
    SubagentStop,
    PermissionRequest,
    Notification,
}

/// What a hook is for. Dialects that cannot implement the requested intent
/// report `Unsupported`; generated extension observers accept only `Notify`.
/// An observation-only callback must never claim to enforce or rewrite.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookIntent {
    /// Block or veto (e.g. deny a tool call, keep the agent working).
    #[default]
    Enforce,
    /// Feed hook output back into the agent's context.
    Inject,
    /// Rewrite what the model sees after the event.
    Rewrite,
    /// Observe only; never affects the run.
    Notify,
}

/// One lifecycle hook: a shell command the agent's hook store invokes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookDecl {
    pub event: HookEvent,
    /// Shell command executed by the agent's hook runner.
    pub command: String,
    #[serde(default)]
    pub intent: HookIntent,
    /// Tool-name/pattern filter where the dialect supports matchers.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Seconds the agent should allow the hook to run.
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// Standing rules materialized into the agent's rules/instructions file.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleDecl {
    /// Managed block name — the artifact carries it as the marker.
    pub name: String,
    /// Markdown content appended inside the managed block.
    pub content: String,
}

/// A skill pack: one `SKILL.md` the adapter places under the agent's
/// skills directory.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillDecl {
    /// Skill directory name (e.g. `code-review`).
    pub name: String,
    /// `SKILL.md` file content.
    pub content: String,
}

/// A custom slash command.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandDecl {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Prompt body the command expands to.
    pub content: String,
}

/// A declarative subagent the agent host can dispatch.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentDecl {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Reasoning effort where the agent exposes it.
    #[serde(default)]
    pub effort: Option<String>,
    /// Sandbox label where the agent exposes per-agent sandboxes.
    #[serde(default)]
    pub sandbox: Option<String>,
    /// Agent/developer instructions.
    pub instructions: String,
}

/// An installable plugin or extension reference. `source` is a
/// marketplace/package name or a local path the host resolves.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginDecl {
    pub id: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Semantic permission posture mapped onto the agent's allow/deny/mode
/// surface where one exists.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionDecl {
    /// Native mode label (`workspace-write`, `default`, `bypassPermissions`,
    /// autonomy level, ...). Adapters translate or report unsupported.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// The complete semantic mechanism declaration carried by a profile.
/// Everything is optional — an empty set is a valid no-op.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MechanismSet {
    #[serde(default)]
    pub hooks: Vec<HookDecl>,
    #[serde(default)]
    pub rules: Vec<RuleDecl>,
    #[serde(default)]
    pub skills: Vec<SkillDecl>,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    #[serde(default)]
    pub subagents: Vec<SubagentDecl>,
    #[serde(default)]
    pub plugins: Vec<PluginDecl>,
    #[serde(default)]
    pub permissions: Option<PermissionDecl>,
    /// Agent-side config-file MCP servers keyed by name — distinct from the
    /// profile's per-session `mcpServers`, which flow through the ACP lane.
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, serde_json::Value>,
    /// Select the agent's own profile mechanism instead of emitting
    /// artifacts (e.g. a codex `[profiles.*]` name or a DSH profile).
    #[serde(default)]
    pub native_profile: Option<String>,
}

impl HookEvent {
    /// Kebab-case name — the report item suffix and the wire form. Derived
    /// from the serde `kebab-case` rename so the two can never drift.
    pub fn kebab_name(self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(name)) => name,
            _ => unreachable!("unit variant serializes to its rename string"),
        }
    }
}

impl MechanismSet {
    /// Layered merge: named lists merge by `name`/`id` (later wins),
    /// hooks merge by `(event, command, matcher)`, permissions and
    /// `native_profile` replace. Mirrors profile field-level overlay rules.
    pub fn merge(&mut self, other: MechanismSet) {
        merge_named(&mut self.rules, other.rules, |d| &d.name);
        merge_named(&mut self.skills, other.skills, |d| &d.name);
        merge_named(&mut self.commands, other.commands, |d| &d.name);
        merge_named(&mut self.subagents, other.subagents, |d| &d.name);
        merge_named(&mut self.plugins, other.plugins, |d| &d.id);
        for hook in other.hooks {
            if !self.hooks.iter().any(|h| {
                h.event == hook.event && h.command == hook.command && h.matcher == hook.matcher
            }) {
                self.hooks.push(hook);
            }
        }
        if let Some(permissions) = other.permissions {
            self.permissions = Some(permissions);
        }
        self.mcp_servers.extend(other.mcp_servers);
        if let Some(native) = other.native_profile {
            self.native_profile = Some(native);
        }
    }

    /// Structural validation: names and bodies non-empty, no duplicate keys
    /// inside a list. Semantic support is a per-dialect question — this
    /// checks only what any host would call malformed.
    pub fn validate(&self) -> Result<(), AdapterError> {
        let malformed = |item: &str| {
            AdapterError::permanent("mechanism_malformed", "mechanisms/validate")
                .with_detail(item.to_owned())
        };
        // Names land inside artifact paths (`<dir>/<name>/SKILL.md`,
        // `<name>.toml`): a separator, `..` or control byte would let a
        // declaration escape the dialect's target directory.
        let path_safe = |name: &str| {
            !name.is_empty()
                && name != ".."
                && name != "."
                && !name.contains(['/', '\\'])
                && name.chars().all(|c| !c.is_control())
        };
        let mut names = BTreeMap::new();
        for rule in &self.rules {
            if rule.name.trim().is_empty() || rule.content.trim().is_empty() {
                return Err(malformed("rule requires a non-empty name and content"));
            }
            if !path_safe(&rule.name) {
                return Err(malformed("rule name is not path-safe"));
            }
            if names.insert(format!("rules:{}", rule.name), ()).is_some() {
                return Err(malformed("duplicate rule name"));
            }
        }
        for skill in &self.skills {
            if skill.name.trim().is_empty() || skill.content.trim().is_empty() {
                return Err(malformed("skill requires a non-empty name and content"));
            }
            if !path_safe(&skill.name) {
                return Err(malformed("skill name is not path-safe"));
            }
            if names.insert(format!("skills:{}", skill.name), ()).is_some() {
                return Err(malformed("duplicate skill name"));
            }
        }
        for command in &self.commands {
            if command.name.trim().is_empty() || command.content.trim().is_empty() {
                return Err(malformed("command requires a non-empty name and content"));
            }
            if !path_safe(&command.name) {
                return Err(malformed("command name is not path-safe"));
            }
            if names
                .insert(format!("commands:{}", command.name), ())
                .is_some()
            {
                return Err(malformed("duplicate command name"));
            }
        }
        for subagent in &self.subagents {
            if subagent.name.trim().is_empty() || subagent.instructions.trim().is_empty() {
                return Err(malformed(
                    "subagent requires a non-empty name and instructions",
                ));
            }
            if !path_safe(&subagent.name) {
                return Err(malformed("subagent name is not path-safe"));
            }
            if names
                .insert(format!("subagents:{}", subagent.name), ())
                .is_some()
            {
                return Err(malformed("duplicate subagent name"));
            }
        }
        for plugin in &self.plugins {
            if plugin.id.trim().is_empty() {
                return Err(malformed("plugin requires a non-empty id"));
            }
            if names.insert(format!("plugins:{}", plugin.id), ()).is_some() {
                return Err(malformed("duplicate plugin id"));
            }
        }
        for hook in &self.hooks {
            if hook.command.trim().is_empty() {
                return Err(malformed("hook requires a non-empty command"));
            }
        }
        for name in self.mcp_servers.keys() {
            if name.trim().is_empty() {
                return Err(malformed("mcp server names must be non-empty"));
            }
        }
        if let Some(native) = &self.native_profile {
            if native.trim().is_empty() {
                return Err(malformed("nativeProfile must be non-empty"));
            }
        }
        Ok(())
    }
}

fn merge_named<T>(base: &mut Vec<T>, overlay: Vec<T>, key: impl Fn(&T) -> &str) {
    for item in overlay {
        match base.iter_mut().find(|existing| key(existing) == key(&item)) {
            Some(existing) => *existing = item,
            None => base.push(item),
        }
    }
}

// ----------------------------- plan types ---------------------------------

/// Root an artifact path is relative to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactScope {
    /// Under the agent's user-level config home (`~/.codex`, `~/.claude`,
    /// `~/.config/opencode`, ...). Paths are home-relative.
    UserHome,
    /// Under the working project directory.
    Project,
}

/// How an artifact reaches the filesystem.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WriteMode {
    /// Create or replace the whole file.
    Create,
    /// Deep-merge `content` (a JSON object) into the file's document at
    /// `at` (dotted key path; empty = document root).
    JsonMerge { at: String },
    /// Insert `content` inside a managed marker block
    /// (`<!-- a4aa-managed:<name> -->` … `<!-- /a4aa-managed:<name> -->`),
    /// replacing an existing block with the same name.
    ManagedBlock { name: String },
}

/// One file the adaptation produces.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Artifact {
    pub scope: ArtifactScope,
    /// Path relative to the scope root.
    pub path: PathBuf,
    /// File content (JSON fragment for `json-merge`, file body otherwise).
    pub content: String,
    pub mode: WriteMode,
}

/// Per-item support level reported back to the declaration's author.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Support {
    /// The agent expresses the declaration directly.
    Native,
    /// Emitted through a translation; semantics may differ.
    Adapted,
    /// The agent has no mechanism for this declaration.
    Unsupported,
}

/// Support verdict for one declared item (`hooks.session-start`,
/// `subagents.reviewer`, `permissions`, ...).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupportEntry {
    pub item: String,
    pub support: Support,
    #[serde(default)]
    pub note: Option<String>,
}

/// What [`crate::dialect::adapt`] produces: artifacts to write plus the
/// per-item verdicts.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdaptationPlan {
    /// The producing dialect's `id` (`"codex"`, `"generic"`, ...).
    pub dialect: String,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub report: Vec<SupportEntry>,
}

impl AdaptationPlan {
    pub(crate) fn push(
        &mut self,
        scope: ArtifactScope,
        path: &str,
        content: String,
        mode: WriteMode,
    ) {
        self.artifacts.push(Artifact {
            scope,
            path: PathBuf::from(path),
            content,
            mode,
        });
    }

    pub(crate) fn report(&mut self, item: impl Into<String>, support: Support, note: Option<&str>) {
        self.report.push(SupportEntry {
            item: item.into(),
            support,
            note: note.map(str::to_owned),
        });
    }
}
