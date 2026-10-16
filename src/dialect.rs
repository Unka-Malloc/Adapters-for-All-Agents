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
    AdaptationPlan, ArtifactScope, CommandDecl, HookDecl, HookEvent, HookIntent, MechanismSet,
    SubagentDecl, Support, WriteMode,
};

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

/// How an agent stores lifecycle hooks.
#[derive(Clone, Copy, Debug)]
pub enum HookSurface {
    /// No hook store — every hook reports unsupported.
    None,
    /// Claude dialect: a JSON document deep-merged at `at` with
    /// `{"Event":[{matcher?,hooks:[{type:command,command,timeout}]}]}`.
    /// `events` are the semantic events the store accepts (native names
    /// match 1:1). Used by Claude Code, Codex, Devin and Command Code.
    Claude {
        file: &'static str,
        at: &'static str,
        events: &'static [HookEvent],
    },
    /// Grok's variant: one whole file per hook owner in `dir`, containing
    /// the same Claude-shape fragment.
    ClaudeBundle {
        dir: &'static str,
        events: &'static [HookEvent],
    },
    /// Gemini/Antigravity named bundles merged into one document:
    /// `{"a4aa":{"Event":[{command,timeout,type}],"enabled":true}}`.
    /// `map` translates semantic events to native names.
    NamedBundle {
        file: &'static str,
        map: &'static [(HookEvent, &'static str)],
    },
    /// Cursor's flat store: `{version:1,hooks:{<event>:[{command}]}}`
    /// merged at the document root. `map` translates event names.
    Flat {
        file: &'static str,
        map: &'static [(HookEvent, &'static str)],
    },
    /// Code-extension agents: hooks are generated code units (Pi
    /// extensions, OpenCode plugins), not config.
    Extension {
        file: &'static str,
        kind: ExtensionKind,
        map: &'static [(HookEvent, &'static str)],
    },
}

/// Generated-code dialects for [`HookSurface::Extension`].
#[derive(Clone, Copy, Debug)]
pub enum ExtensionKind {
    /// `~/.pi/agent/extensions/*.ts`: `pi.on('<event>', …)`.
    Pi,
    /// `~/.config/opencode/plugins/*.js`: plugin event handlers.
    OpenCode,
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
            hooks: HookSurface::None,
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
        emit_hooks(self, set, &mut plan);
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

// ------------------------------- hooks ------------------------------------

fn emit_hooks(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    let item = |hook: &HookDecl| format!("hooks.{}", hook.event.kebab_name());
    match &dialect.hooks {
        HookSurface::None => {
            for hook in &set.hooks {
                plan.report(
                    item(hook),
                    Support::Unsupported,
                    Some("no hook surface for this dialect"),
                );
            }
        }
        HookSurface::Claude { file, at, events } => {
            let (emit, missing): (Vec<_>, Vec<_>) =
                set.hooks.iter().partition(|h| events.contains(&h.event));
            for hook in missing {
                plan.report(
                    item(hook),
                    Support::Unsupported,
                    Some("event not exposed by this agent's hook store"),
                );
            }
            for hook in &emit {
                plan.report(item(hook), Support::Native, None);
            }
            if !emit.is_empty() {
                plan.push(
                    ArtifactScope::UserHome,
                    file,
                    serde_json::to_string_pretty(&claude_hook_fragment(&emit)["hooks"])
                        .unwrap_or_default(),
                    WriteMode::JsonMerge { at: at.to_string() },
                );
            }
        }
        HookSurface::ClaudeBundle { dir, events } => {
            let (emit, missing): (Vec<_>, Vec<_>) =
                set.hooks.iter().partition(|h| events.contains(&h.event));
            for hook in missing {
                plan.report(
                    item(hook),
                    Support::Unsupported,
                    Some("event not exposed by this agent's hook store"),
                );
            }
            for hook in &emit {
                plan.report(item(hook), Support::Native, None);
            }
            if !emit.is_empty() {
                plan.push(
                    ArtifactScope::UserHome,
                    &format!("{dir}/a4aa.json"),
                    serde_json::to_string_pretty(&claude_hook_fragment(&emit)).unwrap_or_default(),
                    WriteMode::Create,
                );
            }
        }
        HookSurface::NamedBundle { file, map } => {
            let mut bundle = serde_json::Map::new();
            let mut any = false;
            for hook in &set.hooks {
                match map.iter().find(|(event, _)| *event == hook.event) {
                    Some((_, name)) => {
                        any = true;
                        let mut entry = serde_json::json!({
                            "command": hook.command,
                            "type": "command",
                        });
                        if let Some(timeout) = hook.timeout {
                            entry["timeout"] = serde_json::json!(timeout);
                        }
                        bundle
                            .entry(name.to_string())
                            .or_insert_with(|| serde_json::json!([]))
                            .as_array_mut()
                            .expect("bundle list")
                            .push(entry);
                        plan.report(item(hook), Support::Adapted, None);
                    }
                    None => plan.report(
                        item(hook),
                        Support::Unsupported,
                        Some("no bundle event equivalent"),
                    ),
                }
            }
            if any {
                bundle.insert("enabled".to_owned(), serde_json::json!(true));
                plan.push(
                    ArtifactScope::UserHome,
                    file,
                    serde_json::to_string_pretty(&serde_json::json!({"a4aa": bundle}))
                        .unwrap_or_default(),
                    WriteMode::JsonMerge { at: String::new() },
                );
            }
        }
        HookSurface::Flat { file, map } => {
            let mut hooks = serde_json::Map::new();
            let mut any = false;
            for hook in &set.hooks {
                match map.iter().find(|(event, _)| *event == hook.event) {
                    Some((_, name)) => {
                        any = true;
                        hooks
                            .entry(name.to_string())
                            .or_insert_with(|| serde_json::json!([]))
                            .as_array_mut()
                            .expect("event list")
                            .push(serde_json::json!({ "command": hook.command }));
                        plan.report(item(hook), Support::Adapted, None);
                    }
                    None => plan.report(
                        item(hook),
                        Support::Unsupported,
                        Some("no flat-store event equivalent"),
                    ),
                }
            }
            if any {
                plan.push(
                    ArtifactScope::UserHome,
                    file,
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "version": 1, "hooks": hooks }),
                    )
                    .unwrap_or_default(),
                    WriteMode::JsonMerge { at: String::new() },
                );
            }
        }
        HookSurface::Extension { file, kind, map } => {
            // One admission pass for generated-code observers: intent
            // support and event mapping are dialect-neutral policy with a
            // single authoritative implementation; only the rendering
            // format branches per extension kind.
            let (surface, adapted_note) = match kind {
                ExtensionKind::Pi => ("pi extension", "emitted as a generated pi extension"),
                ExtensionKind::OpenCode => (
                    "opencode plugin",
                    "emitted inside a generated opencode plugin",
                ),
            };
            let mut emitted = Vec::new();
            for hook in &set.hooks {
                if hook.intent != HookIntent::Notify {
                    plan.report(item(hook), Support::Unsupported,
                        Some("generated event observers support Notify only; no veto or output injection"));
                    continue;
                }
                match map.iter().find(|(event, _)| *event == hook.event) {
                    Some((_, event)) => {
                        emitted.push((*event, hook));
                        plan.report(item(hook), Support::Adapted, Some(adapted_note));
                    }
                    None => plan.report(
                        item(hook),
                        Support::Unsupported,
                        Some(&format!("no {surface} event equivalent")),
                    ),
                }
            }
            if !emitted.is_empty() {
                let body = match kind {
                    ExtensionKind::Pi => pi_extension_body(&emitted),
                    ExtensionKind::OpenCode => opencode_plugin_body(&emitted),
                };
                plan.push(ArtifactScope::UserHome, file, body, WriteMode::Create);
            }
        }
    }
}

/// Pi extension body: one `pi.on` observer per admitted hook. Observers
/// never veto — a failed command reports the event, command and reason to
/// the host log (`console.error`, the extension host's diagnostic channel)
/// and is never rethrown.
fn pi_extension_body(emitted: &[(&str, &HookDecl)]) -> String {
    let mut body = String::from("// @a4aa-managed\nexport default function (pi) {\n");
    for (event, hook) in emitted {
        let command = serde_json::to_string(&hook.command).unwrap_or_default();
        body.push_str(&format!(
            "  pi.on('{event}', async () => {{ const {{ execSync }} = await import('node:child_process'); try {{ execSync({command}, {{ stdio: 'pipe' }}) }} catch (error) {{ console.error('[a4aa] observer hook failed', {{ event: '{event}', command: {command}, reason: String(error) }}) }} }})\n"
        ));
    }
    body.push_str("}\n");
    body
}

/// OpenCode plugin body: one event dispatcher per admitted hook, same
/// fail-open diagnostic contract as [`pi_extension_body`].
fn opencode_plugin_body(emitted: &[(&str, &HookDecl)]) -> String {
    let mut body = String::from(
        "// @a4aa-managed\nexport const a4aaHooks = async () => ({\n  event: async ({ event }) => {\n",
    );
    for (event, hook) in emitted {
        let command = serde_json::to_string(&hook.command).unwrap_or_default();
        body.push_str(&format!(
            "    if (event.type === '{event}') {{ const {{ execSync }} = await import('node:child_process'); try {{ execSync({command}) }} catch (error) {{ console.error('[a4aa] observer hook failed', {{ event: '{event}', command: {command}, reason: String(error) }}) }} }}\n"
        ));
    }
    body.push_str("  },\n})\n");
    body
}

/// Claude-dialect hook fragment: `{"hooks":{"Event":[{matcher?,hooks:
/// [{type:command,command,timeout}]}]}}` grouped per event.
fn claude_hook_fragment(hooks: &[&HookDecl]) -> serde_json::Value {
    let mut events = serde_json::Map::new();
    for hook in hooks {
        let mut command = serde_json::json!({
            "type": "command",
            "command": hook.command,
        });
        if let Some(timeout) = hook.timeout {
            command["timeout"] = serde_json::json!(timeout);
        }
        let mut entry = serde_json::json!({ "hooks": [command] });
        if let Some(matcher) = &hook.matcher {
            entry["matcher"] = serde_json::json!(matcher);
        }
        events
            .entry(claude_event_name(hook.event))
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .expect("event list")
            .push(entry);
    }
    serde_json::json!({ "hooks": events })
}

fn claude_event_name(event: HookEvent) -> String {
    match event {
        HookEvent::SessionStart => "SessionStart",
        HookEvent::SessionEnd => "SessionEnd",
        HookEvent::UserPromptSubmit => "UserPromptSubmit",
        HookEvent::PreToolUse => "PreToolUse",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::Stop => "Stop",
        HookEvent::SubagentStart => "SubagentStart",
        HookEvent::SubagentStop => "SubagentStop",
        HookEvent::PermissionRequest => "PermissionRequest",
        HookEvent::Notification => "Notification",
    }
    .to_owned()
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

// ----------------------------- subagents -----------------------------------

fn emit_subagents(dialect: &Dialect, set: &MechanismSet, plan: &mut AdaptationPlan) {
    for sub in &set.subagents {
        let item = format!("subagents.{}", sub.name);
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
