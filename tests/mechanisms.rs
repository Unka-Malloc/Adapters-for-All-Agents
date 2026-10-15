//! Deterministic coverage for the mechanism layer: semantic declarations
//! in, agent-native artifact plans out — no agents, no disk. The engine
//! lives in `a4aa::dialect`; dialect-specific tests here exercise it
//! through the workspace's real `a4aa-<agent>` descriptor crates (dev-deps),
//! which is the same path a single-agent host takes.

use a4aa::{
    AdaptationPlan, ArtifactScope, CommandDecl, Dialect, HookDecl, HookEvent, HookIntent,
    MechanismSet, PermissionDecl, PluginDecl, RuleDecl, SkillDecl, SubagentDecl, Support,
    WriteMode,
};

fn hook(event: HookEvent, command: &str) -> HookDecl {
    HookDecl {
        event,
        command: command.to_owned(),
        intent: HookIntent::default(),
        matcher: None,
        timeout: None,
    }
}

fn support(plan: &AdaptationPlan, item: &str) -> Option<Support> {
    plan.report
        .iter()
        .find(|entry| entry.item == item)
        .map(|entry| entry.support)
}

#[test]
fn kilo_nonempty_mechanisms_use_kilo_paths_and_merge_contracts() {
    let set: MechanismSet = serde_json::from_value(serde_json::json!({
        "hooks": [
            {"event":"session-start","command":"start.sh","intent":"notify"},
            {"event":"pre-tool-use","command":"guard.sh","intent":"notify"}
        ],
        "skills":[{"name":"audit","content":"Review."}],
        "commands":[{"name":"ship","content":"Ship."}],
        "subagents":[{"name":"reviewer","instructions":"Review."}],
        "plugins":[{"id":"extra","source":"extra.ts"}],
        "permissions":{"allow":["Shell(ls)"],"deny":["Shell(rm)"]},
        "mcpServers":{"local":{"command":"fixture-mcp"}}
    }))
    .unwrap();
    let plan = a4aa_dialects::dialect("kilocode").unwrap().adapt(&set);
    let hooks = plan
        .artifacts
        .iter()
        .find(|a| a.path == std::path::Path::new(".config/kilo/plugin/a4aa.js"))
        .unwrap();
    assert_eq!(hooks.scope, ArtifactScope::UserHome);
    for entry in [
        "session.created",
        "tool.execute.before",
        "start.sh",
        "guard.sh",
    ] {
        assert!(hooks.content.contains(entry), "missing {entry}");
    }
    for path in [
        ".kilo/skills/audit/SKILL.md",
        ".config/kilo/command/ship.md",
        ".config/kilo/agents/reviewer.md",
    ] {
        assert!(plan
            .artifacts
            .iter()
            .any(|a| a.path == std::path::Path::new(path) && a.scope == ArtifactScope::UserHome));
    }
    let config = |key: &str| {
        let artifact = plan
            .artifacts
            .iter()
            .find(|a| {
                a.path == std::path::Path::new(".config/kilo/kilo.json")
                    && matches!(&a.mode, WriteMode::JsonMerge { at } if at == key)
            })
            .unwrap();
        assert_eq!(artifact.scope, ArtifactScope::UserHome);
        serde_json::from_str::<serde_json::Value>(&artifact.content).unwrap()
    };
    assert_eq!(
        config("permission")["allow"],
        serde_json::json!(["Shell(ls)"])
    );
    assert_eq!(
        config("permission")["deny"],
        serde_json::json!(["Shell(rm)"])
    );
    assert_eq!(config("")["mcp"]["local"]["command"], "fixture-mcp");
    for item in ["hooks.session-start", "hooks.pre-tool-use", "plugins.extra"] {
        assert_eq!(support(&plan, item), Some(Support::Adapted));
    }
    for item in [
        "skills.audit",
        "commands.ship",
        "subagents.reviewer",
        "permissions",
        "mcp-servers",
    ] {
        assert_eq!(support(&plan, item), Some(Support::Native));
    }
}

// --------------------------- declaration serde ----------------------------

#[test]
fn mechanism_set_round_trips_json() {
    let json = serde_json::json!({
        "hooks": [{
            "event": "session-start",
            "command": "echo hi",
            "intent": "inject",
            "matcher": "Write|Edit",
            "timeout": 10
        }],
        "rules": [{"name": "core", "content": "Be brief."}],
        "skills": [{"name": "review", "content": "---\nname: review\n---\nbody"}],
        "commands": [{"name": "ship", "content": "Ship it.", "description": "d"}],
        "subagents": [{"name": "verifier", "instructions": "Verify.", "model": "m"}],
        "plugins": [{"id": "linter", "source": "marketplace/linter"}],
        "permissions": {"mode": "workspace-write", "allow": ["Shell(ls)"], "deny": []},
        "mcpServers": {"fs": {"command": "fs-mcp"}},
        "nativeProfile": "reviewer"
    });
    let set: MechanismSet = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(set.hooks.len(), 1);
    assert_eq!(set.hooks[0].intent, HookIntent::Inject);
    assert!(set.plugins[0].enabled);
    let value = serde_json::to_value(&set).unwrap();
    assert_eq!(value["hooks"][0]["event"], "session-start");
    assert_eq!(value["nativeProfile"], "reviewer");
}

#[test]
fn merge_replaces_named_items_and_dedupes_hooks() {
    let mut base = MechanismSet {
        rules: vec![RuleDecl {
            name: "core".into(),
            content: "old".into(),
        }],
        hooks: vec![hook(HookEvent::Stop, "a")],
        permissions: Some(PermissionDecl {
            mode: Some("default".into()),
            ..Default::default()
        }),
        native_profile: Some("a".into()),
        ..Default::default()
    };
    let overlay = MechanismSet {
        rules: vec![RuleDecl {
            name: "core".into(),
            content: "new".into(),
        }],
        hooks: vec![hook(HookEvent::Stop, "a"), hook(HookEvent::Stop, "b")],
        permissions: Some(PermissionDecl {
            mode: Some("workspace-write".into()),
            ..Default::default()
        }),
        native_profile: Some("b".into()),
        ..Default::default()
    };
    base.merge(overlay);
    assert_eq!(base.rules[0].content, "new");
    assert_eq!(base.hooks.len(), 2);
    assert_eq!(
        base.permissions.unwrap().mode.as_deref(),
        Some("workspace-write")
    );
    assert_eq!(base.native_profile.as_deref(), Some("b"));
}

#[test]
fn validate_rejects_empty_and_duplicate_names() {
    for set in [
        MechanismSet {
            rules: vec![RuleDecl {
                name: " ".into(),
                content: "x".into(),
            }],
            ..Default::default()
        },
        MechanismSet {
            subagents: vec![
                SubagentDecl {
                    name: "a".into(),
                    instructions: "i".into(),
                    ..Default::default()
                },
                SubagentDecl {
                    name: "a".into(),
                    instructions: "i".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        MechanismSet {
            hooks: vec![hook(HookEvent::Stop, "  ")],
            ..Default::default()
        },
    ] {
        assert!(set.validate().is_err(), "{set:?}");
    }
    assert!(MechanismSet::default().validate().is_ok());
}

// ------------------------------ codex --------------------------------------

#[test]
fn codex_hooks_subagents_skills_native() {
    let set = MechanismSet {
        hooks: vec![
            hook(HookEvent::SessionStart, "session-start.sh"),
            hook(HookEvent::SessionEnd, "unsupported.sh"),
        ],
        subagents: vec![SubagentDecl {
            name: "reviewer".into(),
            description: Some("Reviews".into()),
            model: Some("gpt-6".into()),
            effort: Some("xhigh".into()),
            sandbox: Some("workspace-write".into()),
            instructions: "Review the diff.".into(),
        }],
        skills: vec![SkillDecl {
            name: "audit".into(),
            content: "SKILL".into(),
        }],
        native_profile: Some("reviewer".into()),
        ..Default::default()
    };
    let plan = a4aa_codex::adapt(&set);

    let hooks_artifact = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("hooks.json"))
        .expect("codex hooks artifact");
    assert_eq!(hooks_artifact.scope, ArtifactScope::UserHome);
    assert!(matches!(hooks_artifact.mode, WriteMode::JsonMerge { .. }));
    let fragment: serde_json::Value = serde_json::from_str(&hooks_artifact.content).unwrap();
    assert!(fragment["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("session-start"));
    assert!(fragment.get("SessionEnd").is_none());

    let toml = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("agents/reviewer.toml"))
        .expect("codex subagent toml");
    assert!(toml.content.contains("model_reasoning_effort = \"xhigh\""));
    assert!(toml.content.contains("developer_instructions"));

    let skill = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("skills/audit/SKILL.md"))
        .expect("codex skill");
    assert_eq!(skill.content, "SKILL");

    assert_eq!(support(&plan, "hooks.session-start"), Some(Support::Native));
    assert_eq!(
        support(&plan, "hooks.session-end"),
        Some(Support::Unsupported)
    );
    assert_eq!(support(&plan, "subagents.reviewer"), Some(Support::Native));
    assert_eq!(support(&plan, "skills.audit"), Some(Support::Native));
    assert_eq!(support(&plan, "native-profile"), Some(Support::Native));
}

// ------------------------------ claude -------------------------------------

#[test]
fn claude_rules_permissions_commands_mcp_native() {
    let set = MechanismSet {
        rules: vec![RuleDecl {
            name: "core".into(),
            content: "Rules text.".into(),
        }],
        commands: vec![CommandDecl {
            name: "ship".into(),
            description: Some("Ship it".into()),
            content: "Do the shipping.".into(),
        }],
        subagents: vec![SubagentDecl {
            name: "worker".into(),
            instructions: "Work.".into(),
            ..Default::default()
        }],
        permissions: Some(PermissionDecl {
            mode: Some("default".into()),
            allow: vec!["Shell(ls)".into()],
            deny: vec!["Shell(rm)".into()],
        }),
        mcp_servers: [("fs".to_owned(), serde_json::json!({"command": "fs-mcp"}))]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let plan = a4aa_claude::adapt(&set);

    let rules = plan
        .artifacts
        .iter()
        .find(|a| matches!(&a.mode, WriteMode::ManagedBlock { .. }))
        .expect("claude managed block");
    assert_eq!(rules.scope, ArtifactScope::Project);
    assert!(rules.path.ends_with("CLAUDE.md"));

    let perms = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with(".claude/settings.json"))
        .expect("claude permissions merge");
    let fragment: serde_json::Value = serde_json::from_str(&perms.content).unwrap();
    assert_eq!(fragment["allow"][0], "Shell(ls)");

    let command = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("commands/ship.md"))
        .expect("claude command");
    assert!(command.content.contains("description: Ship it"));

    let sub = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("agents/worker.md"))
        .expect("claude subagent");
    assert!(sub.content.contains("name: worker"));

    let mcp = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with(".mcp.json"))
        .expect("claude mcp merge");
    assert_eq!(mcp.scope, ArtifactScope::Project);

    assert_eq!(support(&plan, "rules.core"), Some(Support::Native));
    assert_eq!(support(&plan, "permissions"), Some(Support::Native));
    assert_eq!(support(&plan, "commands.ship"), Some(Support::Native));
    assert_eq!(support(&plan, "subagents.worker"), Some(Support::Native));
    assert_eq!(support(&plan, "mcp-servers"), Some(Support::Native));
}

// ------------------------------ cursor --------------------------------------

#[test]
fn cursor_flat_hooks_and_permissions() {
    let set = MechanismSet {
        hooks: vec![
            hook(HookEvent::UserPromptSubmit, "route.sh"),
            hook(HookEvent::PreToolUse, "guard.sh"),
        ],
        permissions: Some(PermissionDecl {
            mode: Some("yolo".into()),
            allow: vec!["Shell(ls)".into()],
            deny: vec![],
        }),
        ..Default::default()
    };
    let plan = a4aa_cursor::adapt(&set);

    let artifact = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with(".cursor/hooks.json"))
        .expect("cursor hooks artifact");
    let doc: serde_json::Value = serde_json::from_str(&artifact.content).unwrap();
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["hooks"]["beforeSubmitPrompt"][0]["command"], "route.sh");

    let config = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("cli-config.json"))
        .expect("cursor permissions");
    assert!(config.content.contains("Shell(ls)"));

    assert_eq!(
        support(&plan, "hooks.user-prompt-submit"),
        Some(Support::Adapted)
    );
    assert_eq!(
        support(&plan, "hooks.pre-tool-use"),
        Some(Support::Unsupported)
    );
    assert_eq!(support(&plan, "permissions"), Some(Support::Native));
    assert_eq!(
        support(&plan, "permissions.mode"),
        Some(Support::Unsupported)
    );
}

// ---------------------------- antigravity ------------------------------------

#[test]
fn antigravity_named_bundle_and_unsupported_events() {
    let set = MechanismSet {
        hooks: vec![
            hook(HookEvent::SessionStart, "start.sh"),
            hook(HookEvent::PreToolUse, "guard.sh"),
            hook(HookEvent::PostToolUse, "after.sh"),
            hook(HookEvent::Stop, "receipt.sh"),
            hook(HookEvent::Notification, "note.sh"),
        ],
        rules: vec![RuleDecl {
            name: "project".into(),
            content: "Use the project rules.".into(),
        }],
        skills: vec![SkillDecl {
            name: "review".into(),
            content: "Review carefully.".into(),
        }],
        commands: vec![CommandDecl {
            name: "ship".into(),
            description: None,
            content: "Prepare a release.".into(),
        }],
        subagents: vec![SubagentDecl {
            name: "reviewer".into(),
            instructions: "Review changes.".into(),
            ..Default::default()
        }],
        permissions: Some(PermissionDecl {
            mode: Some("request-review".into()),
            allow: vec!["command(git)".into()],
            deny: vec![],
        }),
        mcp_servers: [("fs".to_owned(), serde_json::json!({"command": "fs-mcp"}))]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let plan = a4aa_antigravity::adapt(&set);

    let artifact = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with(".gemini/config/hooks.json"))
        .expect("antigravity bundle artifact");
    let doc: serde_json::Value = serde_json::from_str(&artifact.content).unwrap();
    assert_eq!(doc["a4aa"]["SessionStart"][0]["command"], "start.sh");
    assert_eq!(doc["a4aa"]["PreToolUse"][0]["command"], "guard.sh");
    assert_eq!(doc["a4aa"]["PostToolUse"][0]["command"], "after.sh");
    assert_eq!(doc["a4aa"]["Stop"][0]["command"], "receipt.sh");
    assert_eq!(doc["a4aa"]["enabled"], true);

    let settings = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with(".gemini/config/mcp_config.json"))
        .expect("antigravity mcp merge");
    assert!(settings.content.contains("fs-mcp"));

    assert!(plan.artifacts.iter().any(|a| a.path.ends_with("GEMINI.md")));
    assert!(plan.artifacts.iter().any(|a| a
        .path
        .ends_with(".gemini/antigravity-cli/skills/review/SKILL.md")));
    assert!(plan
        .artifacts
        .iter()
        .any(|a| a.path.ends_with(".gemini/config/workflows/ship.md")));
    assert!(plan
        .artifacts
        .iter()
        .any(|a| a.path.ends_with(".gemini/config/agents/reviewer.md")));

    assert_eq!(
        support(&plan, "hooks.session-start"),
        Some(Support::Adapted)
    );
    assert_eq!(support(&plan, "hooks.pre-tool-use"), Some(Support::Adapted));
    assert_eq!(
        support(&plan, "hooks.post-tool-use"),
        Some(Support::Adapted)
    );
    assert_eq!(support(&plan, "hooks.stop"), Some(Support::Adapted));
    assert_eq!(
        support(&plan, "hooks.notification"),
        Some(Support::Unsupported)
    );
    assert_eq!(support(&plan, "rules.project"), Some(Support::Native));
    assert_eq!(support(&plan, "skills.review"), Some(Support::Native));
    assert_eq!(support(&plan, "commands.ship"), Some(Support::Native));
    assert_eq!(support(&plan, "subagents.reviewer"), Some(Support::Native));
    assert_eq!(support(&plan, "permissions"), Some(Support::Unsupported));
}

// -------------------------------- pi ---------------------------------------

#[test]
fn pi_hooks_become_extension_stub() {
    let set = MechanismSet {
        hooks: vec![
            HookDecl {
                intent: HookIntent::Notify,
                ..hook(HookEvent::SessionStart, "start.sh")
            },
            hook(HookEvent::PreToolUse, "guard.sh"),
        ],
        plugins: vec![PluginDecl {
            id: "extra".into(),
            source: Some("extra.ts".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let plan = a4aa_pi::adapt(&set);

    let ext = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("extensions/a4aa.ts"))
        .expect("pi extension stub");
    assert!(ext.content.contains("pi.on('session_start'"));
    assert!(ext.content.contains("start.sh"));
    assert!(!ext.content.contains("guard.sh"));

    assert_eq!(
        support(&plan, "hooks.session-start"),
        Some(Support::Adapted)
    );
    assert_eq!(
        support(&plan, "hooks.pre-tool-use"),
        Some(Support::Unsupported)
    );
    assert_eq!(support(&plan, "plugins.extra"), Some(Support::Adapted));
}

// ------------------------------ generic -------------------------------------

#[test]
fn generic_dialect_reports_unsupported_gracefully() {
    let set = MechanismSet {
        hooks: vec![hook(HookEvent::Stop, "x.sh")],
        rules: vec![RuleDecl {
            name: "core".into(),
            content: "Text.".into(),
        }],
        skills: vec![SkillDecl {
            name: "s".into(),
            content: "c".into(),
        }],
        subagents: vec![SubagentDecl {
            name: "w".into(),
            instructions: "i".into(),
            ..Default::default()
        }],
        native_profile: Some("n".into()),
        ..Default::default()
    };
    let plan = Dialect::generic().adapt(&set);

    let rules = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("AGENTS.md"))
        .expect("generic rules");
    assert_eq!(rules.scope, ArtifactScope::Project);

    let skill = plan
        .artifacts
        .iter()
        .find(|a| a.path.ends_with(".agents/skills/s/SKILL.md"))
        .expect("generic skill");
    assert_eq!(skill.scope, ArtifactScope::UserHome);

    assert_eq!(support(&plan, "hooks.stop"), Some(Support::Unsupported));
    assert_eq!(support(&plan, "rules.core"), Some(Support::Native));
    assert_eq!(support(&plan, "skills.s"), Some(Support::Native));
    assert_eq!(support(&plan, "subagents.w"), Some(Support::Unsupported));
    assert_eq!(support(&plan, "native-profile"), Some(Support::Unsupported));
}

#[test]
fn adaptation_plan_serializes_for_foreign_hosts() {
    let set = MechanismSet {
        hooks: vec![hook(HookEvent::SessionStart, "x.sh")],
        ..Default::default()
    };
    let plan = a4aa::adapt(a4aa_codex::dialect(), &set);
    let value = serde_json::to_value(&plan).unwrap();
    assert_eq!(value["dialect"], "codex");
    assert!(value["artifacts"].is_array());
    assert_eq!(value["report"][0]["support"], "native");
}

#[test]
fn generated_observers_never_claim_enforcement_or_injection() {
    for dialect in [
        a4aa_dialects::dialect("kilocode").unwrap(),
        a4aa_opencode::dialect(),
        a4aa_pi::dialect(),
    ] {
        for intent in [HookIntent::Enforce, HookIntent::Inject, HookIntent::Rewrite] {
            let plan = dialect.adapt(&MechanismSet {
                hooks: vec![
                    HookDecl {
                        intent,
                        ..hook(HookEvent::SessionStart, "guard.sh")
                    },
                    HookDecl {
                        intent,
                        ..hook(HookEvent::PreToolUse, "guard.sh")
                    },
                ],
                ..Default::default()
            });
            assert!(plan.artifacts.is_empty(), "{}", dialect.id);
            assert!(plan
                .report
                .iter()
                .all(|entry| entry.support == Support::Unsupported));
        }
    }
}
