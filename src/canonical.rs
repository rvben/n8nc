use std::collections::BTreeSet;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::AppError;

const VOLATILE_TOP_LEVEL_FIELDS: &[&str] = &["createdAt", "updatedAt", "versionId"];
const TOP_LEVEL_ORDER: &[&str] = &[
    "id",
    "name",
    "active",
    "tags",
    "settings",
    "nodes",
    "connections",
];

pub const CANONICAL_VERSION: u32 = 1;
pub const HASH_ALGORITHM: &str = "sha256";

pub fn canonicalize_workflow(input: &Value) -> Result<Value, AppError> {
    let object = input.as_object().ok_or_else(|| {
        AppError::validation("validate", "Workflow payload must be a JSON object.")
    })?;

    let mut stripped = Map::new();
    let volatile: BTreeSet<&str> = VOLATILE_TOP_LEVEL_FIELDS.iter().copied().collect();
    for (key, value) in object {
        if !volatile.contains(key.as_str()) {
            stripped.insert(key.clone(), value.clone());
        }
    }

    Ok(canonicalize_value(&Value::Object(stripped), true))
}

pub fn canonicalize_generic_json(input: &Value) -> Value {
    canonicalize_value(input, false)
}

fn canonicalize_value(value: &Value, top_level: bool) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            if top_level {
                for key in TOP_LEVEL_ORDER {
                    if let Some(value) = map.get(*key) {
                        out.insert((*key).to_string(), canonicalize_value(value, false));
                    }
                }
            }

            let mut extra_keys: Vec<_> = map.keys().cloned().collect();
            extra_keys.sort();
            for key in extra_keys {
                if top_level && TOP_LEVEL_ORDER.contains(&key.as_str()) {
                    continue;
                }
                if let Some(value) = map.get(&key) {
                    out.insert(key, canonicalize_value(value, false));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| canonicalize_value(item, false))
                .collect(),
        ),
        _ => value.clone(),
    }
}

pub fn pretty_json(value: &Value) -> Result<String, AppError> {
    let mut rendered = serde_json::to_string_pretty(value)
        .map_err(|err| AppError::validation("fmt", format!("Failed to serialize JSON: {err}")))?;
    rendered.push('\n');
    Ok(rendered)
}

pub fn hash_value(value: &Value) -> Result<String, AppError> {
    let encoded = serde_json::to_vec(value).map_err(|err| {
        AppError::validation(
            "push",
            format!("Failed to serialize JSON for hashing: {err}"),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&encoded);
    Ok(format!("{HASH_ALGORITHM}:{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{canonicalize_workflow, hash_value};

    #[test]
    fn canonicalization_removes_volatile_fields() {
        let workflow = json!({
            "updatedAt": "today",
            "nodes": [],
            "id": "wf1",
            "createdAt": "yesterday",
            "name": "Example"
        });

        let canonical = canonicalize_workflow(&workflow).expect("canonical workflow");
        assert!(canonical.get("updatedAt").is_none());
        assert!(canonical.get("createdAt").is_none());
        assert_eq!(
            canonical.get("id").and_then(|value| value.as_str()),
            Some("wf1")
        );
    }

    #[test]
    fn hashing_is_stable_for_equal_values() {
        let a = json!({"id":"wf1","name":"Example","nodes":[],"connections":{}});
        let b = json!({"connections":{},"nodes":[],"name":"Example","id":"wf1"});

        let hash_a = hash_value(&canonicalize_workflow(&a).expect("workflow a")).expect("hash a");
        let hash_b = hash_value(&canonicalize_workflow(&b).expect("workflow b")).expect("hash b");

        assert_eq!(hash_a, hash_b);
    }
}
