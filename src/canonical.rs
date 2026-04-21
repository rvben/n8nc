use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::AppError;

/// Only these top-level keys are included in the canonical form. Server-managed
/// fields (`staticData`, `meta`, `pinData`, `sharedWithProjects`, …) are
/// excluded so that changes n8n makes between API calls never cause spurious
/// hash mismatches.
const CANONICAL_TOP_LEVEL_KEYS: &[&str] = &[
    "id",
    "name",
    "active",
    "tags",
    "settings",
    "nodes",
    "connections",
];

/// Keys stripped from every nested object (tag entries, node entries, etc.).
const VOLATILE_NESTED_KEYS: &[&str] = &["createdAt", "updatedAt"];

pub const CANONICAL_VERSION: u32 = 2;
pub const HASH_ALGORITHM: &str = "sha256";

pub fn canonicalize_workflow(input: &Value) -> Result<Value, AppError> {
    let object = input.as_object().ok_or_else(|| {
        AppError::validation("validate", "Workflow payload must be a JSON object.")
    })?;

    let mut out = Map::new();
    for key in CANONICAL_TOP_LEVEL_KEYS {
        if let Some(value) = object.get(*key) {
            out.insert((*key).to_string(), canonicalize_value(value));
        }
    }

    Ok(Value::Object(out))
}

pub fn canonicalize_generic_json(input: &Value) -> Value {
    canonicalize_value(input)
}

fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                if VOLATILE_NESTED_KEYS.contains(&key.as_str()) {
                    continue;
                }
                if let Some(value) = map.get(&key) {
                    out.insert(key, canonicalize_value(value));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_value).collect()),
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

    #[test]
    fn server_managed_top_level_fields_excluded_from_canonical() {
        let base = json!({
            "id": "wf1",
            "name": "Example",
            "nodes": [],
            "connections": {},
            "settings": {}
        });

        let with_server_fields = json!({
            "id": "wf1",
            "name": "Example",
            "nodes": [],
            "connections": {},
            "settings": {},
            "staticData": {"lastExecution": "2026-01-01"},
            "meta": {"instanceId": "abc123", "templateId": "42"},
            "pinData": {"Webhook": [{"json": {"test": true}}]},
            "sharedWithProjects": [{"id": "proj-1"}],
            "homeProject": {"id": "proj-1", "name": "Home"},
            "parentFolder": null
        });

        let hash_base =
            hash_value(&canonicalize_workflow(&base).expect("base")).expect("hash base");
        let hash_extra =
            hash_value(&canonicalize_workflow(&with_server_fields).expect("with server fields"))
                .expect("hash extra");

        assert_eq!(hash_base, hash_extra);
    }

    #[test]
    fn changing_static_data_does_not_change_hash() {
        let fetch_t1 = json!({
            "id": "wf1",
            "name": "Poller",
            "active": true,
            "nodes": [{"type": "n8n-nodes-base.emailReadImap", "name": "IMAP"}],
            "connections": {},
            "settings": {},
            "staticData": {"node:IMAP": {"lastChecked": 1700000000}}
        });

        let fetch_t2 = json!({
            "id": "wf1",
            "name": "Poller",
            "active": true,
            "nodes": [{"type": "n8n-nodes-base.emailReadImap", "name": "IMAP"}],
            "connections": {},
            "settings": {},
            "staticData": {"node:IMAP": {"lastChecked": 1700099999}}
        });

        let h1 = hash_value(&canonicalize_workflow(&fetch_t1).expect("t1")).expect("h1");
        let h2 = hash_value(&canonicalize_workflow(&fetch_t2).expect("t2")).expect("h2");

        assert_eq!(h1, h2, "staticData changes must not affect canonical hash");
    }

    #[test]
    fn tag_timestamps_excluded_from_canonical() {
        let with_tag_v1 = json!({
            "id": "wf1",
            "name": "Example",
            "nodes": [],
            "connections": {},
            "tags": [{"id": "t1", "name": "prod", "createdAt": "2025-01-01", "updatedAt": "2025-06-01"}]
        });

        let with_tag_v2 = json!({
            "id": "wf1",
            "name": "Example",
            "nodes": [],
            "connections": {},
            "tags": [{"id": "t1", "name": "prod", "createdAt": "2025-01-01", "updatedAt": "2026-03-15"}]
        });

        let h1 = hash_value(&canonicalize_workflow(&with_tag_v1).expect("v1")).expect("h1");
        let h2 = hash_value(&canonicalize_workflow(&with_tag_v2).expect("v2")).expect("h2");

        assert_eq!(
            h1, h2,
            "tag updatedAt changes must not affect canonical hash"
        );
    }

    #[test]
    fn node_created_at_excluded_from_canonical() {
        let node_v1 = json!({
            "id": "wf1",
            "name": "Example",
            "nodes": [{"id": "n1", "name": "HTTP", "type": "n8n-nodes-base.httpRequest", "createdAt": "2025-01-01"}],
            "connections": {}
        });

        let node_v2 = json!({
            "id": "wf1",
            "name": "Example",
            "nodes": [{"id": "n1", "name": "HTTP", "type": "n8n-nodes-base.httpRequest"}],
            "connections": {}
        });

        let h1 = hash_value(&canonicalize_workflow(&node_v1).expect("v1")).expect("h1");
        let h2 = hash_value(&canonicalize_workflow(&node_v2).expect("v2")).expect("h2");

        assert_eq!(
            h1, h2,
            "node-level createdAt must not affect canonical hash"
        );
    }

    #[test]
    fn canonical_version_is_2() {
        assert_eq!(super::CANONICAL_VERSION, 2);
    }

    #[test]
    fn canonical_only_keeps_known_top_level_fields() {
        let workflow = json!({
            "id": "wf1",
            "name": "Example",
            "active": true,
            "nodes": [],
            "connections": {},
            "tags": [],
            "settings": {},
            "staticData": null,
            "meta": {"instanceId": "x"},
            "pinData": {},
            "unknownFutureField": "surprise"
        });

        let canonical = canonicalize_workflow(&workflow).expect("canonical");
        let keys: Vec<&String> = canonical.as_object().unwrap().keys().collect();

        assert!(keys.contains(&&"id".to_string()));
        assert!(keys.contains(&&"name".to_string()));
        assert!(keys.contains(&&"active".to_string()));
        assert!(keys.contains(&&"nodes".to_string()));
        assert!(keys.contains(&&"connections".to_string()));
        assert!(keys.contains(&&"tags".to_string()));
        assert!(keys.contains(&&"settings".to_string()));

        assert!(!keys.contains(&&"staticData".to_string()));
        assert!(!keys.contains(&&"meta".to_string()));
        assert!(!keys.contains(&&"pinData".to_string()));
        assert!(!keys.contains(&&"unknownFutureField".to_string()));
    }
}
