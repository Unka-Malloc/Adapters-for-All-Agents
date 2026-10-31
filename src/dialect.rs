//! Agent dialects: the descriptor per-agent crates declare, and the engine
//! that adapts semantic [`MechanismSet`]s onto them.
//!
//! A [`Dialect`] is plain data — which hook store the agent keeps, where its
//! skills/commands/subagents live, how its permission and MCP config files
//! are shaped. `a4aa` core owns no agent knowledge: each `a4aa-<agent>`
//! workspace crate exports one `DIALECT` constant built from these surface
//! types, so installing or removing an agent adapter never touches another
//! crate. [`adapt`] interprets a descriptor against a declaration set and
//! produces the artifact plan plus the per-item [`Support`] report.
//!
//! `Dialect::generic()` is the portable baseline (`AGENTS.md` rules block,
//! `.agents/skills`) for hosts driving an agent with no dedicated crate.

use crate::mechanisms::{
    AdaptationPlan, ArtifactScope, CommandDecl, MechanismSet, SubagentDecl, Support, WriteMode,
};

pub use ahe::install::{ExtensionKind, HookSurface};

// ----------------------------- descriptor ---------------------------------

/// One agent's native mechanism surface, expressed as data. All paths are
/// relative to the [`ArtifactScope`] the host supplies — `user-home` paths
/// start at the agent's config home's parent (`~`), `project` paths at the
/// working directory.
#[derive(Clone, Debug)]
pub struct Dialect {
    /// Agent id used in the plan's `dialect` field.
    pub id: &'static str,
    /// How this agent stores lifecycle hooks.
    pub hooks: HookSurface,
    /// Where standing rules materialize, if anywhere.
    pub rules: Option<Target>,
    /// Home-relative skills directory (`<dir>/<name>/SKILL.md`).
    pub skills_dir: Option<&'static str>,
    /// How custom commands are declared.
    pub commands: CommandSurface,
    /// How declarative subagents are stored.
    pub subagents: SubagentSurface,
    /// How plugins/extensions install.
    pub plugins: PluginSurface,
    /// How permission posture is configured.
    pub permissions: PermissionSurface,
    /// How config-file MCP servers merge.
    pub mcp: McpSurface,
    /// Whether the agent has its own profile selection mechanism
    /// (`codex --profile`, a DSH profile, Devin Desktop profiles).
    pub native_profiles: bool,
}

/// A file location for managed-block rules.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub scope: ArtifactScope,
    pub path: &'static str,
}

/// How custom slash commands are declared.
#[derive(Clone, Copy, Debug)]
pub enum CommandSurface {
    None,
    /// `<dir>/<name>.md` markdown commands (Claude).
    MarkdownDir(&'static str),
    /// `<dir>/<name>.md` custom prompts (Codex prompts dir).
    PromptDir(&'static str),
    /// `<dir>/<name>.toml` Gemini command files.
    TomlDir(&'static str),
    /// Agents whose slash command *is* a skill pack: the declaration emits
    /// `<dir>/<name>/SKILL.md` with a user-only trigger (Devin, Windsurf
    /// family). Reported `Adapted` — a command rides the skill mechanism.
    SkillDir(&'static str),
}

/// How declarative subagents are stored.
#[derive(Clone, Copy, Debug)]
pub enum SubagentSurface {
    None,
    /// `<dir>/<name>.toml` Codex agent definitions.
    CodexToml(&'static str),
    /// `<dir>/<name>.md` frontmatter agents (Claude, Cursor, OpenCode,
    /// Factory droids, OpenClaw).
    MarkdownDir(&'static str),
    /// `<dir>/<name>.json` Kiro agent definitions.
    KiroJson(&'static str),
}

/// How plugins/extensions install.
#[derive(Clone, Copy, Debug)]
pub enum PluginSurface {
    None,
    /// A plugin installs as a verbatim file under `dir` when its `source`
    /// ends with `extension` (Hermes `plugin.yaml`, Pi `*.ts` extensions).
    ManifestFile {
        dir: &'static str,
        extension: &'static str,
    },
}

/// How permission posture is configured.
#[derive(Clone, Copy, Debug)]
pub enum PermissionSurface {
    None,
    /// Deep-merge `{allow, deny[, mode_key: mode]}` into `file` at `at`.
    /// `mode_key: None` means the store has no mode concept — a declared
    /// `mode` reports unsupported.
    JsonMerge {
        file: &'static str,
        at: &'static str,
        mode_key: Option<&'static str>,
    },
}

/// How config-file MCP servers merge.
#[derive(Clone, Copy, Debug)]
pub enum McpSurface {
    None,
    /// Deep-merge `{<key>: {name: server}}` into `file` at the document
    /// root. `adapted` marks stores whose key or shape is a translation.
    JsonMerge {
        file: &'static str,
        scope: ArtifactScope,
        key: &'static str,
        adapted: bool,
    },
}

// ------------------------------ engine ------------------------------------

impl Dialect {
    /// The portable baseline: `AGENTS.md` managed blocks and `.agents/skills`.
    pub const fn generic() -> Dialect {
        Dialect {
            id: "generic",
            hooks: ahe::install::harnesses::GENERIC.surface,
            rules: Some(Target {
                scope: ArtifactScope::Project,
                path: "AGENTS.md",
            }),
            skills_dir: Some(".agents/skills"),
            commands: CommandSurface::None,
            subagents: SubagentSurface::None,
            plugins: PluginSurface::None,
            permissions: PermissionSurface::None,
            mcp: McpSurface::None,
            native_profiles: false,
        }
    }

    /// Translate semantic declarations into this agent's artifact plan.
    /// Report order follows the declaration set's field order.
    pub fn adapt(&self, set: &MechanismSet) -> AdaptationPlan {
        let mut plan = AdaptationPlan {
            dialect: self.id.to_owned(),
            ..Default::default()
        };
        let installation = ahe::install::adapt(&self.hooks, &set.hooks);
        for artifact in installation.artifacts {
            let mode = match artifact.mode {
                ahe::install::WriteMode::Create => WriteMode::Create,
                ahe::install::WriteMode::JsonMerge { at } => WriteMode::JsonMerge { at },
            };
            plan.push(
                ArtifactScope::UserHome,
                &artifact.path.to_string_lossy(),
                artifact.content,
                mode,
            );
        }
        for entry in installation.report {
            let support = match entry.support {
                ahe::install::Support::Native => Support::Native,
                ahe::install::Support::Adapted => Support::Adapted,
                ahe::install::Support::Unsupported => Support::Unsupported,
            };
            plan.report(entry.item, support, entry.note.as_deref());
        }
        emit_rules(self, set, &mut plan);
        emit_skills(self, set, &mut plan);
        emit_commands(self, set, &mut plan);
        emit_subagents(self, set, &mut plan);
        emit_plugins(self, set, &mut plan);
        emit_permissions(self, set, &mut plan);
        emit_mcp(self, set, &mut plan);
        if set.native_profile.is_some() {
            plan.report(
                "native-profile",
                if self.native_profiles {
                    Support::Native
                } else {
                    Support::Unsupported
                },
                (!self.native_profiles).then_some("agent exposes no native profile selection"),
            );
        }
        plan
    }
}

/// Free-function form: `a4aa::adapt(&a4aa_codex::DIALECT, &set)`.
pub fn adapt(dialect: &Dialect, set: &MechanismSet) -> AdaptationPlan {
    dialect.adapt(set)
}

// ------------------------------ rules --------------------------------------

fn emit_rules(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    let Some(target) = dialect.rules else {
        for rule in &set.rules {
            plan.report(
                format!("rules.{}", rule.name),
                Support::Unsupported,
                Some("no rules/instructions file surface for this dialect"),
            );
        }
        return;
    };
    for rule in &set.rules {
        plan.push(
            target.scope,
            target.path,
            format!("{}\n", rule.content),
            WriteMode::ManagedBlock {
                name: rule.name.clone(),
            },
        );
        plan.report(format!("rules.{}", rule.name), Support::Native, None);
    }
}

// ------------------------------ skills -------------------------------------

fn emit_skills(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    let Some(dir) = dialect.skills_dir else {
        for skill in &set.skills {
            plan.report(
                format!("skills.{}", skill.name),
                Support::Unsupported,
                Some("no user-level skills directory for this dialect"),
            );
        }
        return;
    };
    for skill in &set.skills {
        plan.push(
            ArtifactScope::UserHome,
            &format!("{dir}/{}/SKILL.md", skill.name),
            skill.content.clone(),
            WriteMode::Create,
        );
        plan.report(format!("skills.{}", skill.name), Support::Native, None);
    }
}

// ----------------------------- commands ------------------------------------

fn emit_commands(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    for command in &set.commands {
        let item = format!("commands.{}", command.name);
        match dialect.commands {
            CommandSurface::None => plan.report(
                item,
                Support::Unsupported,
                Some("no custom-command surface for this dialect"),
            ),
            CommandSurface::MarkdownDir(dir) => {
                plan.push(
                    ArtifactScope::UserHome,
                    &format!("{dir}/{}.md", command.name),
                    command_body_md(command),
                    WriteMode::Create,
                );
                plan.report(item, Support::Native, None);
            }
            CommandSurface::PromptDir(dir) => {
                plan.push(
                    ArtifactScope::UserHome,
                    &format!("{dir}/{}.md", command.name),
                    command_body_md(command),
                    WriteMode::Create,
                );
                plan.report(item, Support::Adapted, Some("emitted as a custom prompt"));
            }
            CommandSurface::TomlDir(dir) => {
                let description =
                    toml_string(command.description.as_deref().unwrap_or(&command.name));
                let prompt = toml_string(&command.content);
                plan.push(
                    ArtifactScope::UserHome,
                    &format!("{dir}/{}.toml", command.name),
                    format!("description = {description}\nprompt = {prompt}\n"),
                    WriteMode::Create,
                );
                plan.report(item, Support::Adapted, None);
            }
            CommandSurface::SkillDir(dir) => {
                plan.push(
                    ArtifactScope::UserHome,
                    &format!("{dir}/{}/SKILL.md", command.name),
                    skill_command_body(command),
                    WriteMode::Create,
                );
                plan.report(
                    item,
                    Support::Adapted,
                    Some("emitted as a skill slash command"),
                );
            }
        }
    }
}

fn command_body_md(command: &CommandDecl) -> String {
    match &command.description {
        Some(description) => {
            format!(
                "---\ndescription: {description}\n---\n\n{}\n",
                command.content
            )
        }
        None => format!("{}\n", command.content),
    }
}

/// A command emitted as a skill pack: `name`/`description` frontmatter plus
/// `triggers: [user]` so the pack stays user-invoked and is never
/// auto-triggered by the agent — a slash command is not an autonomous skill.
fn skill_command_body(command: &CommandDecl) -> String {
    let mut front = format!("---\nname: {}\n", command.name);
    if let Some(description) = &command.description {
        front.push_str(&format!("description: {description}\n"));
    }
    front.push_str("triggers:\n  - user\n---\n\n");
    format!("{front}{}\n", command.content)
}

// ----------------------------- subagents -----------------------------------

fn emit_subagents(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    for sub in &set.subagents {
        let item = format!("subagents.{}", sub.name);
        // An explicitly requested sandbox must be expressible AND enforced
        // by the native format. Otherwise fail closed: report the request
        // unsupported and emit nothing — an unconstrained subagent that
        // merely looks declared must never be installed.
        if sub.sandbox.is_some() && !surface_enforces_sandbox(dialect.subagents) {
            plan.report(
                item,
                Support::Unsupported,
                Some("the agent's subagent format cannot enforce the requested sandbox; nothing was emitted"),
            );
            continue;
        }
        match dialect.subagents {
            SubagentSurface::None => plan.report(
                item,
                Support::Unsupported,
                Some("no declarative subagent surface for this dialect"),
            ),
            SubagentSurface::CodexToml(dir) => {
                let mut body = format!("name = {}\n", toml_string(&sub.name));
                if let Some(description) = &sub.description {
                    body.push_str(&format!("description = {}\n", toml_string(description)));
                }
                if let Some(model) = &sub.model {
                    body.push_str(&format!("model = {}\n", toml_string(model)));
                }
                if let Some(effort) = &sub.effort {
                    body.push_str(&format!(
                        "model_reasoning_effort = {}\n",
                        toml_string(effort)
                    ));
                }
                if let Some(sandbox) = &sub.sandbox {
                    body.push_str(&format!("sandbox_mode = {}\n", toml_string(sandbox)));
                }
                body.push_str(&format!(
                    "\ndeveloper_instructions = \"\"\"\n{}\n\"\"\"\n",
                    sub.instructions
                ));
                plan.push(
                    ArtifactScope::UserHome,
                    &format!("{dir}/{}.toml", sub.name),
                    body,
                    WriteMode::Create,
                );
                plan.report(item, Support::Native, None);
            }
            SubagentSurface::MarkdownDir(dir) => {
                plan.push(
                    ArtifactScope::UserHome,
                    &format!("{dir}/{}.md", sub.name),
                    subagent_body_md(sub),
                    WriteMode::Create,
                );
                plan.report(item, Support::Native, None);
            }
            SubagentSurface::KiroJson(dir) => {
                let body = serde_json::to_string_pretty(&serde_json::json!({
                    "name": sub.name,
                    "model": sub.model.clone().unwrap_or_default(),
                    "tools": [],
                    "mcpServers": {},
                    "prompt": sub.instructions,
                }))
                .unwrap_or_default();
                plan.push(
                    ArtifactScope::UserHome,
                    &format!("{dir}/{}.json", sub.name),
                    format!("{body}\n"),
                    WriteMode::Create,
                );
                plan.report(item, Support::Native, None);
            }
        }
    }
}

/// Whether the surface's native subagent format carries a sandbox knob the
/// agent runtime actually enforces (Codex `sandbox_mode`).
fn surface_enforces_sandbox(surface: SubagentSurface) -> bool {
    matches!(surface, SubagentSurface::CodexToml(_))
}

fn subagent_body_md(sub: &SubagentDecl) -> String {
    let mut front = format!("---\nname: {}\n", sub.name);
    if let Some(description) = &sub.description {
        front.push_str(&format!("description: {description}\n"));
    }
    if let Some(model) = &sub.model {
        front.push_str(&format!("model: {model}\n"));
    }
    front.push_str("---\n\n");
    format!("{front}{}\n", sub.instructions)
}

// ------------------------------ plugins ------------------------------------

fn emit_plugins(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    for plugin in &set.plugins {
        let item = format!("plugins.{}", plugin.id);
        match dialect.plugins {
            PluginSurface::None => plan.report(
                item,
                Support::Unsupported,
                Some("plugin installation is host-managed for this dialect"),
            ),
            PluginSurface::ManifestFile { dir, extension } => match &plugin.source {
                Some(source) if source.ends_with(extension) => {
                    let note =
                        format!("host installs {source} under {dir} (verbatim file, no rewrite)");
                    plan.report(item, Support::Adapted, Some(&note));
                }
                _ => plan.report(
                    item,
                    Support::Unsupported,
                    Some("source does not match the dialect's plugin file form"),
                ),
            },
        }
    }
}

// ---------------------------- permissions ----------------------------------

fn emit_permissions(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    let Some(permissions) = &set.permissions else {
        return;
    };
    match dialect.permissions {
        PermissionSurface::None => plan.report(
            "permissions",
            Support::Unsupported,
            Some("no declarative permission surface for this dialect"),
        ),
        PermissionSurface::JsonMerge { file, at, mode_key } => {
            let mut fragment = serde_json::json!({
                "allow": permissions.allow,
                "deny": permissions.deny,
            });
            if !permissions.ask.is_empty() {
                fragment["ask"] = serde_json::json!(permissions.ask);
            }
            match (&permissions.mode, mode_key) {
                (Some(mode), Some(key)) => {
                    fragment[key] = serde_json::json!(mode);
                }
                (Some(_), None) => plan.report(
                    "permissions.mode",
                    Support::Unsupported,
                    Some("the store carries allow/deny lists, not a mode"),
                ),
                _ => {}
            }
            plan.push(
                ArtifactScope::UserHome,
                file,
                serde_json::to_string_pretty(&fragment).unwrap_or_default(),
                WriteMode::JsonMerge { at: at.to_owned() },
            );
            plan.report("permissions", Support::Native, None);
        }
    }
}

// --------------------------------- MCP -------------------------------------

fn emit_mcp(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    if set.mcp_servers.is_empty() {
        return;
    }
    match dialect.mcp {
        McpSurface::None => plan.report(
            "mcp-servers",
            Support::Unsupported,
            Some("agent MCP config is not a mergeable JSON document for this dialect"),
        ),
        McpSurface::JsonMerge {
            file,
            scope,
            key,
            adapted,
        } => {
            let fragment = serde_json::json!({ key: set.mcp_servers });
            plan.push(
                scope,
                file,
                serde_json::to_string_pretty(&fragment).unwrap_or_default(),
                WriteMode::JsonMerge { at: String::new() },
            );
            plan.report(
                "mcp-servers",
                if adapted {
                    Support::Adapted
                } else {
                    Support::Native
                },
                adapted.then_some("merged under a dialect-specific key"),
            );
        }
    }
}

// ------------------------------ helpers ------------------------------------

fn toml_string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

// Re-exported for the dialect descriptor crates' convenience when they
// declare hook event lists.
pub use crate::mechanisms::HookEvent as Event;
