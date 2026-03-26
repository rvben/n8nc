use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde_json::{Map, Number, Value, json};

use crate::{
    canonical::{canonicalize_workflow, pretty_json},
    error::AppError,
    repo::{load_workflow_file, slugify, workflow_id},
};

const WEBHOOK_NODE_TYPE: &str = "n8n-nodes-base.webhook";

#[derive(Debug, Clone)]
pub struct EditResult {
    pub path: PathBuf,
    pub workflow: Value,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathToken {
    Key(String),
    Index(usize),
}

pub fn create_workflow(
    path: &Path,
    name: &str,
    workflow_id: Option<&str>,
    active: bool,
) -> Result<EditResult, AppError> {
    if path.exists() {
        return Err(AppError::validation(
            "workflow",
            format!("{} already exists.", path.display()),
        )
        .with_suggestion("Choose a different path or remove the existing file first."));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::validation(
                "workflow",
                format!("Failed to create {}: {err}", parent.display()),
            )
        })?;
    }

    let workflow = canonicalize_workflow(&json!({
        "id": workflow_id
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| generated_local_id("draft")),
        "name": name,
        "active": active,
        "settings": {},
        "nodes": [],
        "connections": {}
    }))?;
    write_workflow(path, "workflow", &workflow)?;
    Ok(EditResult {
        path: path.to_path_buf(),
        workflow,
        changed: true,
    })
}

pub fn add_node(
    path: &Path,
    name: &str,
    node_type: &str,
    type_version: Option<f64>,
    x: i64,
    y: i64,
    disabled: bool,
) -> Result<EditResult, AppError> {
    mutate_workflow(path, "node", move |workflow| {
        let nodes = workflow_nodes_mut(workflow, "node")?;
        if nodes
            .iter()
            .any(|node| node_name(node).is_some_and(|existing| existing == name))
        {
            return Err(
                AppError::validation("node", format!("Node `{name}` already exists."))
                    .with_suggestion("Choose a different node name."),
            );
        }

        let mut node = Map::new();
        node.insert(
            "id".to_string(),
            Value::String(generated_local_id(&slugify(name))),
        );
        node.insert("name".to_string(), Value::String(name.to_string()));
        node.insert("type".to_string(), Value::String(node_type.to_string()));
        node.insert(
            "typeVersion".to_string(),
            json_number(type_version.unwrap_or_else(|| default_type_version(node_type)))?,
        );
        node.insert("position".to_string(), json!([x, y]));
        node.insert("parameters".to_string(), Value::Object(Map::new()));
        if disabled {
            node.insert("disabled".to_string(), Value::Bool(true));
        }
        apply_node_defaults(&mut node);
        nodes.push(Value::Object(node));
        Ok(())
    })
}

pub fn set_node_value(
    path: &Path,
    node_name: &str,
    raw_path: &str,
    value: Value,
) -> Result<EditResult, AppError> {
    set_node_value_inner(path, "node", node_name, raw_path, value)
}

pub fn set_node_expression(
    path: &Path,
    node_name: &str,
    raw_path: &str,
    expression: &str,
) -> Result<EditResult, AppError> {
    set_node_value_inner(
        path,
        "expr",
        node_name,
        raw_path,
        Value::String(normalize_expression(expression)),
    )
}

pub fn set_credential_reference(
    path: &Path,
    node_name: &str,
    credential_type: &str,
    credential_id: &str,
    credential_name: Option<&str>,
) -> Result<EditResult, AppError> {
    mutate_workflow(path, "credential", move |workflow| {
        let node = find_node_mut(workflow, node_name, "credential")?;
        let node_object = node.as_object_mut().ok_or_else(|| {
            AppError::validation("credential", "Workflow node entry must be a JSON object.")
        })?;
        let credentials = node_object
            .entry("credentials".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let credentials_object = credentials.as_object_mut().ok_or_else(|| {
            AppError::validation(
                "credential",
                format!("Node `{node_name}` has a non-object `credentials` field."),
            )
        })?;

        let preserved_name = credentials_object
            .get(credential_type)
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str);
        let mut credential = Map::new();
        credential.insert("id".to_string(), Value::String(credential_id.to_string()));
        if let Some(name) = credential_name.or(preserved_name) {
            credential.insert("name".to_string(), Value::String(name.to_string()));
        }
        credentials_object.insert(credential_type.to_string(), Value::Object(credential));
        Ok(())
    })
}

pub fn add_connection(
    path: &Path,
    from: &str,
    to: &str,
    kind: &str,
    target_kind: Option<&str>,
    output_index: usize,
    input_index: usize,
) -> Result<EditResult, AppError> {
    let target_kind = target_kind.unwrap_or(kind).to_string();
    mutate_workflow(path, "conn", move |workflow| {
        ensure_node_exists(workflow, from, "conn")?;
        ensure_node_exists(workflow, to, "conn")?;

        let connections = workflow_connections_mut(workflow, "conn")?;
        let source_entry = connections
            .entry(from.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let source_object = source_entry.as_object_mut().ok_or_else(|| {
            AppError::validation(
                "conn",
                format!("Connection bucket for node `{from}` must be an object."),
            )
        })?;
        let output_entry = source_object
            .entry(kind.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let output_branches = output_entry.as_array_mut().ok_or_else(|| {
            AppError::validation(
                "conn",
                format!("Connection output `{kind}` for node `{from}` must be an array."),
            )
        })?;
        while output_branches.len() <= output_index {
            output_branches.push(Value::Array(Vec::new()));
        }
        let branch = output_branches[output_index].as_array_mut().ok_or_else(|| {
            AppError::validation(
                "conn",
                format!(
                    "Connection branch `{kind}[{output_index}]` for node `{from}` must be an array."
                ),
            )
        })?;

        let connection = json!({
            "node": to,
            "type": target_kind,
            "index": input_index
        });
        if !branch.iter().any(|existing| existing == &connection) {
            branch.push(connection);
        }
        Ok(())
    })
}

pub fn rename_node(
    path: &Path,
    current_name: &str,
    new_name: &str,
) -> Result<EditResult, AppError> {
    let next_name = new_name.trim();
    if next_name.is_empty() {
        return Err(AppError::usage("node", "New node name must not be empty."));
    }

    mutate_workflow(path, "node", move |workflow| {
        {
            let nodes = workflow_nodes_mut(workflow, "node")?;
            if nodes
                .iter()
                .any(|node| node_name(node).is_some_and(|name| name == next_name))
            {
                return Err(AppError::validation(
                    "node",
                    format!("Node `{next_name}` already exists."),
                ));
            }

            let node = nodes
                .iter_mut()
                .find(|node| node_name(node).is_some_and(|name| name == current_name))
                .ok_or_else(|| {
                    AppError::not_found("node", format!("Node `{current_name}` was not found."))
                })?;
            let node_object = node.as_object_mut().ok_or_else(|| {
                AppError::validation("node", "Workflow node entry must be a JSON object.")
            })?;
            node_object.insert("name".to_string(), Value::String(next_name.to_string()));
        }

        let connections = workflow_connections_mut(workflow, "node")?;
        if let Some(outbound) = connections.remove(current_name) {
            connections.insert(next_name.to_string(), outbound);
        }
        rename_connection_targets(connections, current_name, next_name);
        Ok(())
    })
}

pub fn remove_node(path: &Path, target_name: &str) -> Result<EditResult, AppError> {
    mutate_workflow(path, "node", move |workflow| {
        {
            let nodes = workflow_nodes_mut(workflow, "node")?;
            let original_len = nodes.len();
            nodes.retain(|node| node_name(node) != Some(target_name));
            if nodes.len() == original_len {
                return Err(AppError::not_found(
                    "node",
                    format!("Node `{target_name}` was not found."),
                ));
            }
        }

        let connections = workflow_connections_mut(workflow, "node")?;
        connections.remove(target_name);
        remove_connection_targets(connections, target_name, None, None, None, None);
        Ok(())
    })
}

pub fn remove_connection(
    path: &Path,
    from: &str,
    to: &str,
    kind: &str,
    target_kind: Option<&str>,
    output_index: Option<usize>,
    input_index: Option<usize>,
) -> Result<EditResult, AppError> {
    let target_kind = target_kind.map(ToOwned::to_owned);
    mutate_workflow(path, "conn", move |workflow| {
        let connections = workflow_connections_mut(workflow, "conn")?;
        let Some(source_entry) = connections.get_mut(from) else {
            return Err(AppError::not_found(
                "conn",
                format!("Node `{from}` has no recorded outbound connections."),
            ));
        };
        let source_object = source_entry.as_object_mut().ok_or_else(|| {
            AppError::validation(
                "conn",
                format!("Connection bucket for node `{from}` must be an object."),
            )
        })?;
        let Some(output_entry) = source_object.get_mut(kind) else {
            return Err(AppError::not_found(
                "conn",
                format!("Node `{from}` has no `{kind}` connections."),
            ));
        };
        let branches = output_entry.as_array_mut().ok_or_else(|| {
            AppError::validation(
                "conn",
                format!("Connection output `{kind}` for node `{from}` must be an array."),
            )
        })?;

        let mut removed = false;
        for (branch_index, branch) in branches.iter_mut().enumerate() {
            if output_index.is_some_and(|index| index != branch_index) {
                continue;
            }
            let branch_entries = branch.as_array_mut().ok_or_else(|| {
                AppError::validation(
                    "conn",
                    format!(
                        "Connection branch `{kind}[{branch_index}]` for node `{from}` must be an array."
                    ),
                )
            })?;
            let before = branch_entries.len();
            branch_entries.retain(|entry| {
                !connection_entry_matches(entry, to, target_kind.as_deref(), input_index)
            });
            if branch_entries.len() != before {
                removed = true;
            }
        }

        if !removed {
            return Err(AppError::not_found(
                "conn",
                format!("No connection matched `{from}` -> `{to}`."),
            ));
        }
        Ok(())
    })
}

pub fn default_workflow_file_name(name: &str, workflow_id: &str) -> String {
    format!("{}--{}.workflow.json", slugify(name), workflow_id)
}

fn set_node_value_inner(
    path: &Path,
    command: &'static str,
    node_name: &str,
    raw_path: &str,
    value: Value,
) -> Result<EditResult, AppError> {
    let normalized_path = normalize_node_path(command, raw_path)?;
    let tokens = parse_path(command, &normalized_path)?;

    mutate_workflow(path, command, move |workflow| {
        let node = find_node_mut(workflow, node_name, command)?;
        let previous_path = webhook_path(node);
        set_path_value(command, node, &tokens, value.clone())?;
        if normalized_path == "parameters.path" {
            sync_webhook_node_after_path_change(node, previous_path.as_deref())?;
        }
        Ok(())
    })
}

fn mutate_workflow<F>(
    path: &Path,
    command: &'static str,
    mutator: F,
) -> Result<EditResult, AppError>
where
    F: FnOnce(&mut Value) -> Result<(), AppError>,
{
    let loaded = load_workflow_file(path, command)?;
    let mut workflow = canonicalize_workflow(&loaded)?;
    ensure_editable_workflow_shape(&mut workflow, command)?;
    let before = workflow.clone();
    mutator(&mut workflow)?;
    let workflow = canonicalize_workflow(&workflow)?;
    write_workflow(path, command, &workflow)?;

    Ok(EditResult {
        path: path.to_path_buf(),
        changed: workflow != before,
        workflow,
    })
}

fn write_workflow(path: &Path, command: &'static str, workflow: &Value) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::validation(
                command,
                format!("Failed to create {}: {err}", parent.display()),
            )
        })?;
    }
    let rendered = pretty_json(workflow)?;
    fs::write(path, rendered).map_err(|err| {
        AppError::validation(
            command,
            format!("Failed to write {}: {err}", path.display()),
        )
    })
}

fn ensure_editable_workflow_shape(
    workflow: &mut Value,
    command: &'static str,
) -> Result<(), AppError> {
    let object = workflow
        .as_object_mut()
        .ok_or_else(|| AppError::validation(command, "Workflow payload must be a JSON object."))?;
    match object.get("nodes") {
        Some(Value::Array(_)) => {}
        Some(_) => {
            return Err(AppError::validation(
                command,
                "Workflow `nodes` field must be an array.",
            ));
        }
        None => {
            object.insert("nodes".to_string(), Value::Array(Vec::new()));
        }
    }
    match object.get("connections") {
        Some(Value::Object(_)) => {}
        Some(_) => {
            return Err(AppError::validation(
                command,
                "Workflow `connections` field must be an object.",
            ));
        }
        None => {
            object.insert("connections".to_string(), Value::Object(Map::new()));
        }
    }
    Ok(())
}

fn workflow_nodes_mut<'a>(
    workflow: &'a mut Value,
    command: &'static str,
) -> Result<&'a mut Vec<Value>, AppError> {
    workflow
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| AppError::validation(command, "Workflow is missing a `nodes` array."))
}

fn workflow_connections_mut<'a>(
    workflow: &'a mut Value,
    command: &'static str,
) -> Result<&'a mut Map<String, Value>, AppError> {
    workflow
        .get_mut("connections")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppError::validation(command, "Workflow is missing a `connections` object."))
}

fn find_node_mut<'a>(
    workflow: &'a mut Value,
    target_name: &str,
    command: &'static str,
) -> Result<&'a mut Value, AppError> {
    workflow_nodes_mut(workflow, command)?
        .iter_mut()
        .find(|node| node_name(node).is_some_and(|name| name == target_name))
        .ok_or_else(|| AppError::not_found(command, format!("Node `{target_name}` was not found.")))
}

fn ensure_node_exists(
    workflow: &Value,
    target_name: &str,
    command: &'static str,
) -> Result<(), AppError> {
    let nodes = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::validation(command, "Workflow is missing a `nodes` array."))?;
    if nodes
        .iter()
        .any(|node| node_name(node).is_some_and(|name| name == target_name))
    {
        Ok(())
    } else {
        Err(AppError::not_found(
            command,
            format!("Node `{target_name}` was not found."),
        ))
    }
}

fn rename_connection_targets(
    connections: &mut Map<String, Value>,
    current_name: &str,
    next_name: &str,
) {
    for value in connections.values_mut() {
        let Some(outputs) = value.as_object_mut() else {
            continue;
        };
        for branch_group in outputs.values_mut() {
            let Some(branches) = branch_group.as_array_mut() else {
                continue;
            };
            for branch in branches {
                let Some(entries) = branch.as_array_mut() else {
                    continue;
                };
                for entry in entries {
                    if entry.get("node").and_then(Value::as_str) == Some(current_name) {
                        if let Some(object) = entry.as_object_mut() {
                            object.insert("node".to_string(), Value::String(next_name.to_string()));
                        }
                    }
                }
            }
        }
    }
}

fn remove_connection_targets(
    connections: &mut Map<String, Value>,
    target_name: &str,
    target_kind: Option<&str>,
    output_kind: Option<&str>,
    output_index: Option<usize>,
    input_index: Option<usize>,
) {
    for outputs in connections.values_mut() {
        let Some(output_map) = outputs.as_object_mut() else {
            continue;
        };
        for (kind, branch_group) in output_map.iter_mut() {
            if output_kind.is_some_and(|expected| expected != kind) {
                continue;
            }
            let Some(branches) = branch_group.as_array_mut() else {
                continue;
            };
            for (branch_index, branch) in branches.iter_mut().enumerate() {
                if output_index.is_some_and(|expected| expected != branch_index) {
                    continue;
                }
                let Some(entries) = branch.as_array_mut() else {
                    continue;
                };
                entries.retain(|entry| {
                    !connection_entry_matches(entry, target_name, target_kind, input_index)
                });
            }
        }
    }
}

fn connection_entry_matches(
    entry: &Value,
    target_name: &str,
    target_kind: Option<&str>,
    input_index: Option<usize>,
) -> bool {
    if entry.get("node").and_then(Value::as_str) != Some(target_name) {
        return false;
    }
    if target_kind
        .is_some_and(|expected| entry.get("type").and_then(Value::as_str) != Some(expected))
    {
        return false;
    }
    if let Some(expected_index) = input_index {
        let Some(actual_index) = entry
            .get("index")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
        else {
            return false;
        };
        if actual_index != expected_index {
            return false;
        }
    }
    true
}

fn node_name(node: &Value) -> Option<&str> {
    node.get("name").and_then(Value::as_str)
}

fn generated_local_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        Utc::now().timestamp_millis(),
        std::process::id()
    )
}

fn default_type_version(node_type: &str) -> f64 {
    if node_type == WEBHOOK_NODE_TYPE {
        2.0
    } else {
        1.0
    }
}

fn apply_node_defaults(node: &mut Map<String, Value>) {
    if node.get("type").and_then(Value::as_str) == Some(WEBHOOK_NODE_TYPE) {
        node.insert(
            "webhookId".to_string(),
            Value::String(default_webhook_id(node)),
        );
    }
}

fn default_webhook_id(node: &Map<String, Value>) -> String {
    node.get("parameters")
        .and_then(Value::as_object)
        .and_then(|parameters| parameters.get("path"))
        .and_then(Value::as_str)
        .map(normalize_webhook_path)
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| {
            node.get("name")
                .and_then(Value::as_str)
                .map(slugify)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| generated_local_id("webhook"))
        })
}

fn normalize_webhook_path(path: &str) -> String {
    path.trim_matches('/').to_string()
}

fn webhook_path(node: &Value) -> Option<String> {
    node.get("parameters")
        .and_then(Value::as_object)
        .and_then(|parameters| parameters.get("path"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn sync_webhook_node_after_path_change(
    node: &mut Value,
    previous_path: Option<&str>,
) -> Result<(), AppError> {
    if node.get("type").and_then(Value::as_str) != Some(WEBHOOK_NODE_TYPE) {
        return Ok(());
    }
    let node_slug = slugify(node_name(node).unwrap_or("Webhook"));

    let node_object = node.as_object_mut().ok_or_else(|| {
        AppError::validation("node", "Workflow node entry must be a JSON object.")
    })?;
    let parameters = node_object
        .get_mut("parameters")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            AppError::validation(
                "node",
                "Webhook node is missing an object `parameters` field.",
            )
        })?;
    let Some(path_value) = parameters.get_mut("path") else {
        return Ok(());
    };
    let Some(path) = path_value.as_str() else {
        return Ok(());
    };
    let normalized = normalize_webhook_path(path);
    *path_value = Value::String(normalized.clone());

    let existing_webhook_id = node_object
        .get("webhookId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let previous_normalized = previous_path.map(normalize_webhook_path);
    if existing_webhook_id.as_deref().is_none()
        || existing_webhook_id.as_deref() == Some("")
        || previous_normalized
            .as_deref()
            .is_some_and(|previous| existing_webhook_id.as_deref() == Some(previous))
        || existing_webhook_id
            .as_deref()
            .is_some_and(|value| value == node_slug)
    {
        node_object.insert("webhookId".to_string(), Value::String(normalized));
    }

    let current_version = node_object.get("typeVersion").and_then(Value::as_f64);
    if current_version.is_none_or(|version| version < 2.0) {
        node_object.insert("typeVersion".to_string(), json_number(2.0)?);
    }
    Ok(())
}

fn json_number(value: f64) -> Result<Value, AppError> {
    if !value.is_finite() {
        return Err(AppError::usage(
            "node",
            "`--type-version` must be a finite number.",
        ));
    }
    if value.fract() == 0.0 {
        Ok(Value::Number(Number::from(value as i64)))
    } else {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| AppError::usage("node", "`--type-version` is not valid JSON."))
    }
}

fn normalize_expression(expression: &str) -> String {
    let trimmed = expression.trim();
    if trimmed.starts_with("={{") && trimmed.ends_with("}}") {
        trimmed.to_string()
    } else if trimmed.starts_with("{{") && trimmed.ends_with("}}") {
        format!("={trimmed}")
    } else {
        let mut out = String::from("={{");
        out.push_str(trimmed);
        out.push_str("}}");
        out
    }
}

fn normalize_node_path(command: &'static str, raw_path: &str) -> Result<String, AppError> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(AppError::usage(command, "Node path must not be empty."));
    }

    let root_segment = trimmed.split(['.', '[']).next().unwrap_or_default().trim();
    if root_segment.is_empty() {
        return Err(AppError::usage(command, "Node path must not be empty."));
    }

    if matches!(root_segment, "id" | "name" | "type" | "credentials") {
        let suggestion = if root_segment == "credentials" {
            "Use `n8nc credential set` for credential references."
        } else {
            "This field is not editable through `node set` yet."
        };
        return Err(AppError::usage(
            command,
            format!("Node path `{trimmed}` is not supported. {suggestion}"),
        ));
    }

    if matches!(
        root_segment,
        "parameters"
            | "position"
            | "disabled"
            | "notes"
            | "typeVersion"
            | "alwaysOutputData"
            | "retryOnFail"
            | "maxTries"
            | "waitBetweenTries"
            | "waitBetweenTriesMs"
            | "executeOnce"
            | "continueOnFail"
            | "onError"
            | "webhookId"
    ) {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("parameters.{trimmed}"))
    }
}

fn parse_path(command: &'static str, path: &str) -> Result<Vec<PathToken>, AppError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = path.chars().collect();
    let mut current = String::new();
    let mut index = 0usize;

    while index < chars.len() {
        match chars[index] {
            '.' => {
                if current.is_empty() {
                    if matches!(tokens.last(), Some(PathToken::Index(_))) {
                        index += 1;
                        continue;
                    }
                    return Err(AppError::usage(command, format!("Invalid path `{path}`.")));
                }
                tokens.push(PathToken::Key(std::mem::take(&mut current)));
                index += 1;
            }
            '[' => {
                if !current.is_empty() {
                    tokens.push(PathToken::Key(std::mem::take(&mut current)));
                }
                index += 1;
                let start = index;
                while index < chars.len() && chars[index] != ']' {
                    index += 1;
                }
                if index == chars.len() {
                    return Err(AppError::usage(command, format!("Invalid path `{path}`.")));
                }
                let parsed = chars[start..index]
                    .iter()
                    .collect::<String>()
                    .parse::<usize>()
                    .map_err(|_| {
                        AppError::usage(command, format!("Invalid array index in `{path}`."))
                    })?;
                tokens.push(PathToken::Index(parsed));
                index += 1;
            }
            ch => {
                current.push(ch);
                index += 1;
            }
        }
    }

    if !current.is_empty() {
        tokens.push(PathToken::Key(current));
    }
    if tokens.is_empty() {
        Err(AppError::usage(command, format!("Invalid path `{path}`.")))
    } else {
        Ok(tokens)
    }
}

fn set_path_value(
    command: &'static str,
    root: &mut Value,
    tokens: &[PathToken],
    value: Value,
) -> Result<bool, AppError> {
    let Some((last, prefix)) = tokens.split_last() else {
        return Err(AppError::usage(command, "Node path must not be empty."));
    };

    let mut current = root;
    for (index, token) in prefix.iter().enumerate() {
        let next = &tokens[index + 1];
        current = match token {
            PathToken::Key(key) => {
                let object = current.as_object_mut().ok_or_else(|| {
                    AppError::validation(
                        command,
                        format!("Cannot descend into `{key}` because the current value is not an object."),
                    )
                })?;
                let entry = object
                    .entry(key.clone())
                    .or_insert_with(|| container_for(next));
                if entry.is_null() {
                    *entry = container_for(next);
                }
                ensure_container_type(command, entry, next, key)?;
                entry
            }
            PathToken::Index(array_index) => {
                let array = current.as_array_mut().ok_or_else(|| {
                    AppError::validation(
                        command,
                        format!(
                            "Cannot descend into index [{}] because the current value is not an array.",
                            array_index
                        ),
                    )
                })?;
                while array.len() <= *array_index {
                    array.push(Value::Null);
                }
                if array[*array_index].is_null() {
                    array[*array_index] = container_for(next);
                }
                ensure_container_type(
                    command,
                    &array[*array_index],
                    next,
                    &format!("[{array_index}]"),
                )?;
                &mut array[*array_index]
            }
        };
    }

    match last {
        PathToken::Key(key) => {
            let object = current.as_object_mut().ok_or_else(|| {
                AppError::validation(
                    command,
                    format!("Cannot set `{key}` because the target value is not an object."),
                )
            })?;
            let changed = object.get(key) != Some(&value);
            object.insert(key.clone(), value);
            Ok(changed)
        }
        PathToken::Index(array_index) => {
            let array = current.as_array_mut().ok_or_else(|| {
                AppError::validation(
                    command,
                    format!(
                        "Cannot set index [{}] because the target value is not an array.",
                        array_index
                    ),
                )
            })?;
            while array.len() <= *array_index {
                array.push(Value::Null);
            }
            let changed = array.get(*array_index) != Some(&value);
            array[*array_index] = value;
            Ok(changed)
        }
    }
}

fn container_for(next: &PathToken) -> Value {
    match next {
        PathToken::Key(_) => Value::Object(Map::new()),
        PathToken::Index(_) => Value::Array(Vec::new()),
    }
}

fn ensure_container_type(
    command: &'static str,
    value: &Value,
    next: &PathToken,
    label: &str,
) -> Result<(), AppError> {
    match next {
        PathToken::Key(_) if value.is_object() => Ok(()),
        PathToken::Index(_) if value.is_array() => Ok(()),
        PathToken::Key(_) => Err(AppError::validation(
            command,
            format!("Cannot descend through `{label}` because it is not an object."),
        )),
        PathToken::Index(_) => Err(AppError::validation(
            command,
            format!("Cannot descend through `{label}` because it is not an array."),
        )),
    }
}

pub fn workflow_id_string(workflow: &Value) -> Option<String> {
    workflow_id(workflow)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::{
        add_connection, add_node, create_workflow, default_workflow_file_name,
        normalize_expression, parse_path, remove_connection, remove_node, rename_node,
        set_credential_reference, set_node_expression, set_node_value,
    };
    use crate::repo::load_workflow_file;

    #[test]
    fn create_workflow_builds_draft_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp
            .path()
            .join(default_workflow_file_name("Order Alert", "wf-1"));

        let result =
            create_workflow(&path, "Order Alert", Some("wf-1"), false).expect("create workflow");

        assert!(result.changed);
        assert_eq!(
            result.workflow.get("name").and_then(|value| value.as_str()),
            Some("Order Alert")
        );
        assert!(path.exists());
    }

    #[test]
    fn add_node_and_set_parameter_path() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("example.workflow.json");
        create_workflow(&path, "Example", Some("wf-1"), false).expect("create");

        add_node(
            &path,
            "HTTP Request",
            "n8n-nodes-base.httpRequest",
            Some(4.2),
            10,
            20,
            false,
        )
        .expect("add node");
        set_node_value(&path, "HTTP Request", "options.timeout", json!(30)).expect("set node");

        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        let node = workflow
            .get("nodes")
            .and_then(Value::as_array)
            .and_then(|nodes| nodes.first())
            .expect("node");
        assert_eq!(
            node.get("parameters")
                .and_then(|value| value.get("options"))
                .and_then(|value| value.get("timeout"))
                .and_then(Value::as_i64),
            Some(30)
        );
    }

    #[test]
    fn expression_set_wraps_raw_expression() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("example.workflow.json");
        create_workflow(&path, "Example", Some("wf-1"), false).expect("create");
        add_node(&path, "Code", "n8n-nodes-base.code", Some(2.0), 0, 0, false).expect("add node");

        set_node_expression(&path, "Code", "jsCode", "$json.message").expect("set expr");

        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        let value = workflow
            .get("nodes")
            .and_then(Value::as_array)
            .and_then(|nodes| nodes.first())
            .and_then(|node| node.get("parameters"))
            .and_then(|value| value.get("jsCode"))
            .and_then(Value::as_str);
        assert_eq!(value, Some("={{$json.message}}"));
        assert_eq!(normalize_expression("{{ $json.id }}"), "={{ $json.id }}");
    }

    #[test]
    fn connection_add_is_deduplicated() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("example.workflow.json");
        create_workflow(&path, "Example", Some("wf-1"), false).expect("create");
        add_node(
            &path,
            "Start",
            "n8n-nodes-base.manualTrigger",
            Some(1.0),
            0,
            0,
            false,
        )
        .expect("add start");
        add_node(
            &path,
            "HTTP",
            "n8n-nodes-base.httpRequest",
            Some(4.2),
            200,
            0,
            false,
        )
        .expect("add http");

        add_connection(&path, "Start", "HTTP", "main", None, 0, 0).expect("connect once");
        let result =
            add_connection(&path, "Start", "HTTP", "main", None, 0, 0).expect("connect twice");

        assert!(!result.changed);
        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        let branch = workflow
            .get("connections")
            .and_then(|value| value.get("Start"))
            .and_then(|value| value.get("main"))
            .and_then(Value::as_array)
            .and_then(|branches| branches.first())
            .and_then(Value::as_array)
            .expect("connection branch");
        assert_eq!(branch.len(), 1);
    }

    #[test]
    fn credential_set_preserves_existing_name() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("example.workflow.json");
        create_workflow(&path, "Example", Some("wf-1"), false).expect("create");
        add_node(
            &path,
            "Slack",
            "n8n-nodes-base.slack",
            Some(2.0),
            0,
            0,
            false,
        )
        .expect("add node");

        set_credential_reference(&path, "Slack", "slackApi", "cred-1", Some("Primary Slack"))
            .expect("set credential");
        set_credential_reference(&path, "Slack", "slackApi", "cred-2", None)
            .expect("update credential");

        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        let credential = workflow
            .get("nodes")
            .and_then(Value::as_array)
            .and_then(|nodes| nodes.first())
            .and_then(|node| node.get("credentials"))
            .and_then(|value| value.get("slackApi"))
            .expect("credential ref");
        assert_eq!(credential.get("id").and_then(Value::as_str), Some("cred-2"));
        assert_eq!(
            credential.get("name").and_then(Value::as_str),
            Some("Primary Slack")
        );
    }

    #[test]
    fn parse_path_handles_nested_arrays() {
        let tokens = parse_path("node", "parameters.rules[0].value").expect("path");
        assert_eq!(tokens.len(), 4);
    }

    #[test]
    fn webhook_node_defaults_to_type_version_two_and_auto_webhook_id() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("webhook.workflow.json");
        create_workflow(&path, "Webhook Flow", Some("wf-1"), false).expect("create");

        add_node(
            &path,
            "Webhook",
            "n8n-nodes-base.webhook",
            None,
            0,
            0,
            false,
        )
        .expect("add webhook");

        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        let node = workflow["nodes"]
            .as_array()
            .and_then(|nodes| nodes.first())
            .expect("node");
        assert_eq!(node["typeVersion"], 2);
        assert_eq!(node["webhookId"], "webhook");
    }

    #[test]
    fn webhook_path_updates_webhook_id_and_normalizes_path() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("webhook.workflow.json");
        create_workflow(&path, "Webhook Flow", Some("wf-1"), false).expect("create");
        add_node(
            &path,
            "Webhook",
            "n8n-nodes-base.webhook",
            None,
            0,
            0,
            false,
        )
        .expect("add webhook");

        set_node_value(&path, "Webhook", "path", json!("/orders/new/")).expect("set path");

        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        let node = workflow["nodes"]
            .as_array()
            .and_then(|nodes| nodes.first())
            .expect("node");
        assert_eq!(node["parameters"]["path"], "orders/new");
        assert_eq!(node["webhookId"], "orders/new");
        assert_eq!(node["typeVersion"], 2);
    }

    #[test]
    fn rename_node_rewrites_connections() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("example.workflow.json");
        create_workflow(&path, "Example", Some("wf-1"), false).expect("create");
        add_node(
            &path,
            "Start",
            "n8n-nodes-base.manualTrigger",
            Some(1.0),
            0,
            0,
            false,
        )
        .expect("add start");
        add_node(
            &path,
            "HTTP",
            "n8n-nodes-base.httpRequest",
            Some(4.2),
            200,
            0,
            false,
        )
        .expect("add http");
        add_connection(&path, "Start", "HTTP", "main", None, 0, 0).expect("connect");

        rename_node(&path, "HTTP", "Request").expect("rename node");

        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        assert_eq!(workflow["nodes"][1]["name"], "Request");
        assert_eq!(
            workflow["connections"]["Start"]["main"][0][0]["node"],
            "Request"
        );
    }

    #[test]
    fn remove_node_prunes_connections() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("example.workflow.json");
        create_workflow(&path, "Example", Some("wf-1"), false).expect("create");
        add_node(
            &path,
            "Start",
            "n8n-nodes-base.manualTrigger",
            Some(1.0),
            0,
            0,
            false,
        )
        .expect("add start");
        add_node(
            &path,
            "HTTP",
            "n8n-nodes-base.httpRequest",
            Some(4.2),
            200,
            0,
            false,
        )
        .expect("add http");
        add_connection(&path, "Start", "HTTP", "main", None, 0, 0).expect("connect");

        remove_node(&path, "HTTP").expect("remove node");

        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        assert_eq!(workflow["nodes"].as_array().map(Vec::len), Some(1));
        assert_eq!(workflow["connections"]["Start"]["main"][0], json!([]));
    }

    #[test]
    fn remove_connection_removes_single_edge() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("example.workflow.json");
        create_workflow(&path, "Example", Some("wf-1"), false).expect("create");
        add_node(
            &path,
            "Start",
            "n8n-nodes-base.manualTrigger",
            Some(1.0),
            0,
            0,
            false,
        )
        .expect("add start");
        add_node(
            &path,
            "HTTP",
            "n8n-nodes-base.httpRequest",
            Some(4.2),
            200,
            0,
            false,
        )
        .expect("add http");
        add_node(
            &path,
            "Slack",
            "n8n-nodes-base.slack",
            Some(2.0),
            400,
            0,
            false,
        )
        .expect("add slack");
        add_connection(&path, "Start", "HTTP", "main", None, 0, 0).expect("connect http");
        add_connection(&path, "Start", "Slack", "main", None, 0, 0).expect("connect slack");

        remove_connection(&path, "Start", "HTTP", "main", None, Some(0), Some(0))
            .expect("remove connection");

        let workflow = load_workflow_file(&path, "test").expect("load workflow");
        let branch = workflow["connections"]["Start"]["main"][0]
            .as_array()
            .expect("connection branch");
        assert_eq!(branch.len(), 1);
        assert_eq!(branch[0]["node"], "Slack");
    }
}
