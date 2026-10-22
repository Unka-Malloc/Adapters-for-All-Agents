//! Operator dump of card specs for `va set agent --file`.
//! Not a gate: ignored unless explicitly requested.

#[test]
#[ignore = "operator dump of card specs for va set --file"]
fn dump_registry_fragments() {
    let dir = std::path::Path::new("tmp/va-native");
    std::fs::create_dir_all(dir).expect("create tmp/va-native");
    let entries = [
        ("pi-default", "pi", "Pi"),
        ("antigravity-default", "antigravity", "Google Antigravity"),
        ("claude-default", "claude", "Claude Code"),
        ("codex-default", "codex", "Codex"),
    ];
    for (key, name, client_name) in entries {
        let card = a4aa_dialects::card(name).unwrap_or_else(|| panic!("{name} card"));
        let mut specs = card.specs();
        let mut spec = specs.remove(0);
        spec.id = key.to_owned();
        spec.client_name = Some(client_name.to_owned());
        for alternate in &mut specs {
            alternate.id = key.to_owned();
        }
        let mut value = serde_json::to_value(&spec).expect("serialize spec");
        if !specs.is_empty() {
            value.as_object_mut().expect("spec object").insert(
                "alternates".into(),
                serde_json::to_value(&specs).expect("serialize alternates"),
            );
        }
        let path = dir.join(format!("{key}.json"));
        let body = serde_json::to_string_pretty(&value).expect("pretty json");
        std::fs::write(&path, body)
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    }
}
