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

pub(crate) type Vars = BTreeMap<String, String>;

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
    vars
}

/// Recursively substitute `{var}` inside string leaves of a params template.
///
/// A leaf that is exactly `{var}` receives the value as a JSON string — a
/// prompt containing quotes or newlines cannot corrupt the template. In an
/// object, a leaf containing a placeholder that substitutes to nothing is
/// dropped: an absent option must not send `""` onto the wire.
pub(crate) fn substitute(template: &Value, vars: &Vars) -> Value {
    match template {
        Value::String(text) => {
            for (key, value) in vars {
                if *text == format!("{{{key}}}") {
                    return Value::String(value.clone());
                }
            }
            let mut result = text.clone();
            for (key, value) in vars {
                result = result.replace(&format!("{{{key}}}"), value);
            }
            Value::String(result)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| substitute(item, vars)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .filter_map(|(key, value)| {
                    let resolved = substitute(value, vars);
                    let absent = matches!(value, Value::String(t) if t.contains('{'))
                        && resolved.as_str().is_some_and(str::is_empty);
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
