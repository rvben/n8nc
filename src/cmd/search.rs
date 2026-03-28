use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::{cli::SearchArgs, config::workflow_dir, error::AppError, repo::load_workflow_file};

use super::common::{Context, emit_json, load_loaded_repo};

const COMMAND: &str = "search";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct SearchMatch {
    node_name: String,
    node_type: String,
    field: String,
    value: String,
}

#[derive(Debug, Serialize)]
struct SearchResult {
    workflow_path: PathBuf,
    workflow_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    matches: Vec<SearchMatch>,
}

#[derive(Debug, Serialize)]
struct SearchOutput {
    total_matches: usize,
    workflows_matched: usize,
    results: Vec<SearchResult>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether `haystack` contains `needle`, respecting case sensitivity.
fn contains(haystack: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        haystack.contains(needle)
    } else {
        haystack.to_lowercase().contains(&needle.to_lowercase())
    }
}

/// Recursively walk a JSON value and yield `(field_path, string_value)` pairs.
fn walk_strings(value: &Value, prefix: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(s) => {
            out.push((prefix.to_string(), s.clone()));
        }
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                walk_strings(child, &path, out);
            }
        }
        Value::Array(arr) => {
            for (i, child) in arr.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                walk_strings(child, &path, out);
            }
        }
        Value::Number(n) => {
            out.push((prefix.to_string(), n.to_string()));
        }
        Value::Bool(b) => {
            out.push((prefix.to_string(), b.to_string()));
        }
        Value::Null => {}
    }
}

/// Check if a node matches the text query by serializing the node to JSON.
fn node_matches_text(node: &Value, query: &str, case_sensitive: bool) -> Vec<SearchMatch> {
    let node_name = node
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let node_type = node
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut matches = Vec::new();
    let mut pairs = Vec::new();
    walk_strings(node, "", &mut pairs);

    for (field, value) in pairs {
        if contains(&value, query, case_sensitive) {
            matches.push(SearchMatch {
                node_name: node_name.clone(),
                node_type: node_type.clone(),
                field,
                value,
            });
        }
    }

    matches
}

/// Check if a node matches the `--node-type` filter.
fn node_matches_type(node: &Value, type_filter: &str, case_sensitive: bool) -> bool {
    node.get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| contains(t, type_filter, case_sensitive))
}

/// Check if a node matches the `--credential` filter.
fn node_matches_credential(node: &Value, cred_filter: &str, case_sensitive: bool) -> bool {
    let Some(credentials) = node.get("credentials").and_then(Value::as_object) else {
        return false;
    };
    for (key, value) in credentials {
        if contains(key, cred_filter, case_sensitive) {
            return true;
        }
        let mut pairs = Vec::new();
        walk_strings(value, "", &mut pairs);
        for (_, v) in pairs {
            if contains(&v, cred_filter, case_sensitive) {
                return true;
            }
        }
    }
    false
}

/// Check if a node matches the `--expression` filter by finding `={{...}}` strings.
fn node_matches_expression(node: &Value, expr_filter: &str, case_sensitive: bool) -> bool {
    let parameters = node.get("parameters").unwrap_or(&Value::Null);
    let mut pairs = Vec::new();
    walk_strings(parameters, "", &mut pairs);

    for (_, value) in pairs {
        if let Some(rest) = value.strip_prefix("={{")
            && let Some(expr_content) = rest.strip_suffix("}}")
            && contains(expr_content, expr_filter, case_sensitive)
        {
            return true;
        }
    }
    false
}

/// Collect all `.workflow.json` files from the workflows directory.
fn collect_workflow_files(wf_dir: &PathBuf) -> Result<Vec<PathBuf>, AppError> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(wf_dir).map_err(|err| {
        AppError::config(COMMAND, format!("Failed to read workflow directory: {err}"))
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| {
            AppError::config(COMMAND, format!("Failed to read directory entry: {err}"))
        })?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".workflow.json"))
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_search(context: &Context, args: SearchArgs) -> Result<(), AppError> {
    let has_filter = args.query.is_some()
        || args.node_type.is_some()
        || args.credential.is_some()
        || args.expression.is_some();

    if !has_filter {
        return Err(AppError::usage(
            COMMAND,
            "At least one of <QUERY>, --node-type, --credential, or --expression must be provided.",
        ));
    }

    let repo = load_loaded_repo(context)?;
    let wf_dir = workflow_dir(&repo.root, &repo.config);
    let files = collect_workflow_files(&wf_dir)?;

    let mut results: Vec<SearchResult> = Vec::new();

    for file_path in &files {
        let workflow = load_workflow_file(file_path, COMMAND)?;
        let workflow_name = workflow
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let workflow_id = workflow.get("id").and_then(Value::as_str).map(String::from);

        let nodes = workflow
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut workflow_matches: Vec<SearchMatch> = Vec::new();

        // Check if the workflow name matches the text query (independent of nodes)
        if let Some(ref query) = args.query
            && contains(&workflow_name, query, args.case_sensitive)
            && args.node_type.is_none()
            && args.credential.is_none()
            && args.expression.is_none()
        {
            workflow_matches.push(SearchMatch {
                node_name: String::new(),
                node_type: String::new(),
                field: "name".to_string(),
                value: workflow_name.clone(),
            });
        }

        for node in &nodes {
            // Check all filter flags (AND semantics)
            if let Some(ref type_filter) = args.node_type
                && !node_matches_type(node, type_filter, args.case_sensitive)
            {
                continue;
            }
            if let Some(ref cred_filter) = args.credential
                && !node_matches_credential(node, cred_filter, args.case_sensitive)
            {
                continue;
            }
            if let Some(ref expr_filter) = args.expression
                && !node_matches_expression(node, expr_filter, args.case_sensitive)
            {
                continue;
            }

            // If text query is provided, collect matching fields from the node
            if let Some(ref query) = args.query {
                let text_matches = node_matches_text(node, query, args.case_sensitive);
                if text_matches.is_empty() {
                    continue;
                }
                workflow_matches.extend(text_matches);
            } else {
                // No text query, but filter flags matched; emit the node itself
                let node_name = node
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let node_type = node
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                workflow_matches.push(SearchMatch {
                    node_name,
                    node_type: node_type.clone(),
                    field: "type".to_string(),
                    value: node_type,
                });
            }
        }

        if !workflow_matches.is_empty() {
            let rel_path = file_path
                .strip_prefix(&repo.root)
                .unwrap_or(file_path)
                .to_path_buf();

            results.push(SearchResult {
                workflow_path: rel_path,
                workflow_name,
                workflow_id,
                matches: workflow_matches,
            });
        }
    }

    let total_matches: usize = results.iter().map(|r| r.matches.len()).sum();
    let workflows_matched = results.len();

    if results.is_empty() {
        return Err(AppError::not_found(COMMAND, "No matches found."));
    }

    if context.json {
        emit_json(
            COMMAND,
            &SearchOutput {
                total_matches,
                workflows_matched,
                results,
            },
        )
    } else {
        for result in &results {
            println!("{}", result.workflow_path.display());
            println!("  {}", result.workflow_name);
            // Group matches by node
            let mut current_node: Option<(&str, &str)> = None;
            for m in &result.matches {
                if m.node_name.is_empty() && m.node_type.is_empty() {
                    // Workflow-level match
                    println!("    {}: \"{}\"", m.field, m.value);
                    continue;
                }
                let node_key = (m.node_name.as_str(), m.node_type.as_str());
                if current_node != Some(node_key) {
                    println!("  {} ({})", m.node_name, m.node_type);
                    current_node = Some(node_key);
                }
                println!("    {}: \"{}\"", m.field, m.value);
            }
        }
        println!("{total_matches} match(es) across {workflows_matched} workflow(s).");
        Ok(())
    }
}
