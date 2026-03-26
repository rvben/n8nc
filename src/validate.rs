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

    diagnostics
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

    use super::{Severity, validate_workflow_value};

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
}
