//! Lane-neutral spec-template machinery: `{placeholder}` substitution for
//! declared params/argv/stdin templates, and dotted-path extraction for
//! lane-declared value selectors.
//!
//! This module is deliberately not a lane: the substitution semantics the
//! spec fields document (`{prompt}`, `{sessionId}`, `{model}`, …) are
//! shared vocabulary, so every lane consumes them from here rather than
//! borrowing another lane's internals.

use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub(crate) struct Vars {
    values: BTreeMap<String, String>,
    absent: Vec<String>,
}
impl Vars {
    pub(crate) fn absent_names(&self) -> impl Iterator<Item = &String> {
        self.absent.iter()
    }
    pub(crate) fn is_absent_option(&self, template: &str) -> bool {
        template
            .strip_prefix('{')
            .and_then(|t| t.strip_suffix('}'))
            .is_some_and(|name| self.absent.iter().any(|absent| absent == name))
    }
    /// A resolved `{secret:NAME}` value — stored under its full placeholder
    /// name so `secret:…` keys can never collide with plain vars.
    pub(crate) fn set_secret(&mut self, name: &str, value: String) {
        self.values.insert(format!("secret:{name}"), value);
    }
    /// An unresolved `{secret:NAME}` drops its leaf like any absent
    /// optional selector — the wire never sees the literal placeholder,
    /// and either/or credential pairs degrade to the credential present.
    pub(crate) fn absent_secret(&mut self, name: &str) {
        self.absent.push(format!("secret:{name}"));
    }
}

/// `"{secret:NAME}"` placeholder names present in a params template —
/// resolved by the lane before substitution.
pub(crate) fn secret_names(template: &Value) -> Vec<String> {
    fn walk(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::String(text) => {
                let mut rest = text.as_str();
                while let Some(open) = rest.find("{secret:") {
                    let tail = &rest[open + 8..];
                    if let Some(end) = tail.find('}') {
                        found.push(tail[..end].to_owned());
                        rest = &tail[end + 1..];
                    } else {
                        break;
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            Value::Object(map) => map.values().for_each(|value| walk(value, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(template, &mut found);
    found
}

/// Whether a template consumes one of the adapter's declared selector
/// placeholders. This keeps lane support discovery beside the substitution
/// rules the lane actually uses.
pub(crate) fn contains_placeholder(template: &Value, name: &str) -> bool {
    match template {
        Value::String(text) => contains_placeholder_text(text, name),
        Value::Array(items) => items.iter().any(|item| contains_placeholder(item, name)),
        Value::Object(fields) => fields
            .values()
            .any(|value| contains_placeholder(value, name)),
        _ => false,
    }
}

pub(crate) fn contains_placeholder_text(text: &str, name: &str) -> bool {
    text.contains(&format!("{{{name}}}"))
}

/// Substitution inputs for call params, argv and stdin templates. Only
/// placeholders present in a template are read; unset options substitute as
/// the empty string, never leaking a literal `{model}` onto the wire.
#[derive(Default)]
pub(crate) struct VarSpec<'a> {
    pub session_id: Option<&'a str>,
    pub cwd: &'a str,
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub instructions: Option<&'a str>,
    pub permission: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub provider: Option<&'a str>,
}

pub(crate) fn vars(input: VarSpec<'_>) -> Vars {
    let absent = [
        ("model", input.model),
        ("modelId", input.model),
        (
            "modelProvider",
            input
                .model
                .and_then(|model| model.split_once('/').map(|(provider, _)| provider)),
        ),
        ("effort", input.effort),
        ("mode", input.mode),
        ("instructions", input.instructions),
        ("permission", input.permission),
        ("provider", input.provider),
        ("sessionId", input.session_id),
        ("turnId", input.turn_id),
    ]
    .into_iter()
    .filter(|(_, value)| value.is_none())
    .map(|(name, _)| name.to_owned())
    .collect();
    let mut vars = BTreeMap::new();
    vars.insert("cwd".to_owned(), input.cwd.to_owned());
    vars.insert("prompt".to_owned(), input.prompt.to_owned());
    let model = input.model.unwrap_or_default();
    vars.insert("model".to_owned(), model.to_owned());
    // Qualified `"provider/model"` selections split into the two
    // placeholders some agents address separately; an unqualified model
    // leaves `{modelProvider}` empty.
    let (model_provider, model_id) = model.split_once('/').unwrap_or(("", model));
    vars.insert("modelProvider".to_owned(), model_provider.to_owned());
    vars.insert("modelId".to_owned(), model_id.to_owned());
    vars.insert(
        "provider".to_owned(),
        input.provider.unwrap_or_default().to_owned(),
    );
    vars.insert(
        "effort".to_owned(),
        input.effort.unwrap_or_default().to_owned(),
    );
    vars.insert("mode".to_owned(), input.mode.unwrap_or_default().to_owned());
    vars.insert(
        "instructions".to_owned(),
        input.instructions.unwrap_or_default().to_owned(),
    );
    vars.insert(
        "permission".to_owned(),
        input.permission.unwrap_or_default().to_owned(),
    );
    if let Some(session) = input.session_id {
        vars.insert("sessionId".to_owned(), session.to_owned());
    }
    if let Some(turn) = input.turn_id {
        vars.insert("turnId".to_owned(), turn.to_owned());
    }
    Vars {
        values: vars,
        absent,
    }
}

/// Expand only tokens in the original template. Inserted values are opaque
/// data, even if they themselves contain placeholder-shaped text.
pub(crate) fn substitute_text(template: &str, vars: &Vars) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        let tail = &rest[open + 1..];
        if let Some(end) = tail.find(['{', '}']) {
            if tail.as_bytes()[end] == b'}' {
                if let Some(value) = vars.values.get(&tail[..end]) {
                    output.push_str(value);
                } else {
                    output.push('{');
                    output.push_str(&tail[..=end]);
                }
                rest = &tail[end + 1..];
                continue;
            }
        }
        output.push('{');
        rest = tail;
    }
    output.push_str(rest);
    output
}

/// Recursively substitute `{var}` inside string leaves of a params template.
///
/// A leaf that is exactly `{var}` receives the value as a JSON string — a
/// prompt containing quotes or newlines cannot corrupt the template. In an
/// object, an absent optional selector leaf is dropped. Required prompt text
/// and explicitly provided empty values remain present.
pub(crate) fn substitute(template: &Value, vars: &Vars) -> Value {
    match template {
        Value::String(text) => Value::String(substitute_text(text, vars)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| substitute(item, vars)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .filter_map(|(key, value)| {
                    let resolved = substitute(value, vars);
                    let absent = matches!(value, Value::String(t) if vars.is_absent_option(t));
                    (!absent).then(|| (key.clone(), resolved))
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Walk a dotted path into a JSON value.
pub(crate) fn extract_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cursor = value;
    for segment in path.split('.') {
        cursor = cursor.get(segment)?;
    }
    Some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_empty_text_and_provided_empty_instructions_are_preserved() {
        let variables = vars(VarSpec {
            prompt: "",
            instructions: Some(""),
            ..Default::default()
        });
        assert_eq!(
            substitute(
                &json!({"text":"{prompt}","instructions":"{instructions}","model":"{model}"}),
                &variables
            ),
            json!({"text":"","instructions":""})
        );
    }

    #[test]
    fn substitution_never_reinterprets_inserted_values() {
        let input = "explain {provider} \"quoted\"\nnext";
        let vars = vars(VarSpec {
            prompt: input,
            instructions: Some("keep {sessionId}"),
            session_id: Some("binding-{provider}"),
            ..Default::default()
        });
        assert_eq!(
            substitute_text("{prompt}|{instructions}|{sessionId}", &vars),
            format!("{input}|keep {{sessionId}}|binding-{{provider}}")
        );
        assert_eq!(
            substitute(
                &json!({"text":"prefix {prompt} suffix", "model":"{model}",
                                     "instructions":"{instructions}"}),
                &vars
            ),
            json!({"text":format!("prefix {input} suffix"),"instructions":"keep {sessionId}"})
        );
        assert_eq!(substitute_text("{{prompt}}", &vars), format!("{{{input}}}"));
        assert_eq!(substitute_text("{unknown}", &vars), "{unknown}");
    }
}
