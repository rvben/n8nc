use std::{collections::BTreeSet, path::Path};

use serde::Serialize;
use serde_json::Value;

use crate::{
    error::AppError,
    repo::{load_workflow_file, sidecar_path_for, workflow_id},
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

pub fn validate_workflow_path(path: &Path) -> Result<Vec<Diagnostic>, AppError> {
    let workflow = load_workflow_file(path, "validate")?;
    let file = path.to_string_lossy().to_string();
    Ok(validate_workflow_value(&workflow, &file, path))
}

pub fn sensitive_data_diagnostics(path: &Path) -> Result<Vec<Diagnostic>, AppError> {
    let workflow = load_workflow_file(path, "validate")?;
    let file = path.to_string_lossy().to_string();
    Ok(collect_sensitive_value_diagnostics(&workflow, &file))
}

fn validate_workflow_value(workflow: &Value, file: &str, path: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let Some(object) = workflow.as_object() else {
        diagnostics.push(Diagnostic {
            code: "workflow.not_object".to_string(),
            severity: Severity::Error,
            message: "Workflow payload must be a JSON object.".to_string(),
            file: file.to_string(),
            path: Some("$".to_string()),
            suggestion: None,
        });
        return diagnostics;
    };

    if object.get("id").is_none() {
        diagnostics.push(Diagnostic {
            code: "workflow.id_missing".to_string(),
            severity: Severity::Error,
            message: "Workflow payload is missing `id`.".to_string(),
            file: file.to_string(),
            path: Some("$.id".to_string()),
            suggestion: Some("Pull the workflow again or restore the ID field.".to_string()),
        });
    }

    let nodes = object.get("nodes");
    if !matches!(nodes, Some(Value::Array(_))) {
        diagnostics.push(Diagnostic {
            code: "workflow.nodes_missing".to_string(),
            severity: Severity::Error,
            message: "Workflow payload is missing a `nodes` array.".to_string(),
            file: file.to_string(),
            path: Some("$.nodes".to_string()),
            suggestion: None,
        });
    }

    let connections = object.get("connections");
    if !matches!(connections, Some(Value::Object(_))) {
        diagnostics.push(Diagnostic {
            code: "workflow.connections_missing".to_string(),
            severity: Severity::Error,
            message: "Workflow payload is missing a `connections` object.".to_string(),
            file: file.to_string(),
            path: Some("$.connections".to_string()),
            suggestion: None,
        });
    }

    let mut names = BTreeSet::new();
    if let Some(Value::Array(nodes)) = nodes {
        for (index, node) in nodes.iter().enumerate() {
            match node.get("name").and_then(Value::as_str) {
                Some(name) if !name.is_empty() => {
                    if !names.insert(name.to_string()) {
                        diagnostics.push(Diagnostic {
                            code: "workflow.duplicate_node_name".to_string(),
                            severity: Severity::Error,
                            message: format!("Duplicate node name `{name}`."),
                            file: file.to_string(),
                            path: Some(format!("$.nodes[{index}].name")),
                            suggestion: Some(
                                "Rename the node so every node name is unique.".to_string(),
                            ),
                        });
                    }
                }
                _ => diagnostics.push(Diagnostic {
                    code: "workflow.node_name_missing".to_string(),
                    severity: Severity::Error,
                    message: "A node is missing a string `name` field.".to_string(),
                    file: file.to_string(),
                    path: Some(format!("$.nodes[{index}].name")),
                    suggestion: None,
                }),
            }
        }
    }

    if let Some(connections) = connections {
        let mut targets = Vec::new();
        collect_connection_targets(connections, "$.connections", &mut targets);
        for (path, target) in targets {
            if !names.contains(&target) {
                diagnostics.push(Diagnostic {
                    code: "workflow.connection_target_missing".to_string(),
                    severity: Severity::Error,
                    message: format!("Connection points to missing node `{target}`."),
                    file: file.to_string(),
                    path: Some(path),
                    suggestion: Some(
                        "Remove the connection or restore the missing node.".to_string(),
                    ),
                });
            }
        }
    }

    if path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|name| name.ends_with(".workflow.json"))
        .unwrap_or(false)
    {
        let meta_path = sidecar_path_for(path);
        if meta_path.exists() {
            match std::fs::read_to_string(&meta_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            {
                Some(meta) => {
                    let expected_id = workflow_id(workflow);
                    let meta_id = meta
                        .get("workflow_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    if expected_id.is_some() && meta_id.is_some() && expected_id != meta_id {
                        diagnostics.push(Diagnostic {
                            code: "workflow.meta_mismatch".to_string(),
                            severity: Severity::Error,
                            message: "Metadata sidecar does not match the workflow ID.".to_string(),
                            file: meta_path.to_string_lossy().to_string(),
                            path: Some("$.workflow_id".to_string()),
                            suggestion: Some(
                                "Pull the workflow again to regenerate the sidecar.".to_string(),
                            ),
                        });
                    }
                }
                None => diagnostics.push(Diagnostic {
                    code: "workflow.meta_parse_failed".to_string(),
                    severity: Severity::Error,
                    message: "Metadata sidecar exists but could not be parsed.".to_string(),
                    file: meta_path.to_string_lossy().to_string(),
                    path: None,
                    suggestion: Some(
                        "Re-pull the workflow to recreate the metadata sidecar.".to_string(),
                    ),
                }),
            }
        }
    }

    diagnostics.extend(collect_sensitive_value_diagnostics(workflow, file));
    diagnostics
}

fn collect_sensitive_value_diagnostics(workflow: &Value, file: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    collect_sensitive_value_diagnostics_inner(workflow, "$", None, file, &mut diagnostics);
    diagnostics
}

fn collect_sensitive_value_diagnostics_inner(
    value: &Value,
    path: &str,
    key: Option<&str>,
    file: &str,
    out: &mut Vec<Diagnostic>,
) {
    match value {
        Value::Object(map) => {
            for (child_key, child_value) in map {
                let child_path = format!("{path}.{}", child_key);
                collect_sensitive_value_diagnostics_inner(
                    child_value,
                    &child_path,
                    Some(child_key),
                    file,
                    out,
                );
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_sensitive_value_diagnostics_inner(
                    item,
                    &format!("{path}[{index}]"),
                    key,
                    file,
                    out,
                );
            }
        }
        Value::String(text) => {
            if let Some(diagnostic) = sensitive_value_diagnostic(file, path, key, text) {
                out.push(diagnostic);
            }
        }
        _ => {}
    }
}

fn sensitive_value_diagnostic(
    file: &str,
    path: &str,
    key: Option<&str>,
    value: &str,
) -> Option<Diagnostic> {
    let trimmed = value.trim();
    if trimmed.is_empty() || looks_dynamic_reference(trimmed) || looks_placeholder(trimmed) {
        return None;
    }

    let suggestion = Some(
        "Move the value into n8n credentials or an env-backed expression before committing."
            .to_string(),
    );

    if trimmed.contains("-----BEGIN ") && trimmed.contains("PRIVATE KEY-----") {
        return Some(Diagnostic {
            code: "workflow.private_key_literal".to_string(),
            severity: Severity::Warning,
            message: "Found inline private key material in the workflow payload.".to_string(),
            file: file.to_string(),
            path: Some(path.to_string()),
            suggestion,
        });
    }

    if string_embeds_basic_auth_url(trimmed) {
        return Some(Diagnostic {
            code: "workflow.url_embeds_credentials".to_string(),
            severity: Severity::Warning,
            message: "Found a URL with embedded credentials in the workflow payload.".to_string(),
            file: file.to_string(),
            path: Some(path.to_string()),
            suggestion,
        });
    }

    if likely_sensitive_value(trimmed) {
        return Some(Diagnostic {
            code: "workflow.secret_literal".to_string(),
            severity: Severity::Warning,
            message: "Found a literal value that looks like a secret token or key.".to_string(),
            file: file.to_string(),
            path: Some(path.to_string()),
            suggestion,
        });
    }

    if key.is_some_and(key_name_looks_sensitive) {
        return Some(Diagnostic {
            code: "workflow.sensitive_field_literal".to_string(),
            severity: Severity::Warning,
            message: "Found a literal value in a field name that usually stores secrets."
                .to_string(),
            file: file.to_string(),
            path: Some(path.to_string()),
            suggestion,
        });
    }

    None
}

fn key_name_looks_sensitive(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();

    [
        "apikey",
        "accesstoken",
        "refreshtoken",
        "token",
        "password",
        "passwd",
        "passphrase",
        "secret",
        "clientsecret",
        "privatekey",
        "authorization",
    ]
    .iter()
    .any(|suffix| normalized == *suffix || normalized.ends_with(suffix))
}

fn looks_dynamic_reference(value: &str) -> bool {
    let trimmed = value.trim();
    (trimmed.starts_with("={{") && trimmed.ends_with("}}"))
        || (trimmed.starts_with("{{") && trimmed.ends_with("}}"))
        || trimmed.contains("$env.")
        || trimmed.contains("$json.")
        || trimmed.contains("$node[")
        || trimmed.contains("$parameter[")
}

fn looks_placeholder(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.contains("your-api-key")
        || normalized.contains("your api key")
        || normalized.contains("replace-me")
        || normalized.contains("changeme")
        || normalized == "<secret>"
        || normalized == "<token>"
}

fn likely_sensitive_value(value: &str) -> bool {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();

    (trimmed.starts_with("sk-") && trimmed.len() >= 20)
        || (trimmed.starts_with("ghp_") && trimmed.len() >= 20)
        || (trimmed.starts_with("github_pat_") && trimmed.len() >= 20)
        || (trimmed.starts_with("xoxb-") && trimmed.len() >= 12)
        || (trimmed.starts_with("xoxp-") && trimmed.len() >= 12)
        || (lower.starts_with("bearer ") && trimmed.len() >= 16)
}

fn string_embeds_basic_auth_url(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if scheme.is_empty() || !scheme.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }
    let Some(authority) = rest.split('/').next() else {
        return false;
    };
    let Some((userinfo, _host)) = authority.rsplit_once('@') else {
        return false;
    };
    let Some((user, password)) = userinfo.split_once(':') else {
        return false;
    };
    !user.is_empty() && !password.is_empty()
}

fn collect_connection_targets(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            if let Some(target) = map.get("node").and_then(Value::as_str) {
                out.push((format!("{path}.node"), target.to_string()));
            }
            for (key, value) in map {
                collect_connection_targets(value, &format!("{path}.{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                collect_connection_targets(value, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Severity, sensitive_data_diagnostics, validate_workflow_value};

    #[test]
    fn detects_duplicate_names_and_missing_targets() {
        let workflow = json!({
            "id": "wf1",
            "nodes": [
                {"name": "Start"},
                {"name": "Start"}
            ],
            "connections": {
                "Start": {
                    "main": [[{"node": "Missing"}]]
                }
            }
        });
        let diagnostics =
            validate_workflow_value(&workflow, "wf.json", std::path::Path::new("wf.json"));
        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.code == "workflow.duplicate_node_name")
        );
        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.code == "workflow.connection_target_missing")
        );
        assert!(
            diagnostics
                .iter()
                .all(|diag| diag.severity == Severity::Error)
        );
    }

    #[test]
    fn detects_sensitive_literals_as_warnings() {
        let workflow = json!({
            "id": "wf1",
            "nodes": [
                {
                    "name": "HTTP Request",
                    "parameters": {
                        "password": "super-secret-value",
                        "url": "https://user:pass@example.com/path",
                        "token": "={{$env.API_TOKEN}}",
                        "privateKey": "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----"
                    }
                }
            ],
            "connections": {}
        });
        let diagnostics =
            validate_workflow_value(&workflow, "wf.json", std::path::Path::new("wf.json"));

        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.code == "workflow.sensitive_field_literal"
                    && diag.severity == Severity::Warning)
        );
        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.code == "workflow.url_embeds_credentials"
                    && diag.severity == Severity::Warning)
        );
        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.code == "workflow.private_key_literal"
                    && diag.severity == Severity::Warning)
        );
        assert!(
            diagnostics
                .iter()
                .all(|diag| diag.path.as_deref() != Some("$.nodes[0].parameters.token"))
        );
    }

    #[test]
    fn sensitive_data_scanner_reads_workflow_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("workflow.workflow.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "id": "wf1",
                "nodes": [
                    {
                        "name": "HTTP",
                        "parameters": {
                            "apiKey": "sk-test-12345678901234567890"
                        }
                    }
                ],
                "connections": {}
            }))
            .expect("serialize workflow"),
        )
        .expect("write workflow");

        let diagnostics = sensitive_data_diagnostics(&path).expect("scan workflow");

        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.code == "workflow.secret_literal"
                    || diag.code == "workflow.sensitive_field_literal")
        );
        assert!(
            diagnostics
                .iter()
                .all(|diag| diag.severity == Severity::Warning)
        );
    }
}
