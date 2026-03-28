use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    api::ApiClient,
    auth::resolve_token,
    canonical::{canonicalize_workflow, pretty_json},
    cli::ValueModeArgs,
    config::{LoadedRepo, load_repo, resolve_instance_alias, workflow_dir},
    edit::{EditResult, default_workflow_file_name, default_workflow_settings},
    error::AppError,
    repo::{load_workflow_file, workflow_active},
    validate::{Severity, sensitive_data_diagnostics, validate_workflow_path},
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Context {
    pub json: bool,
    pub repo_root: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Envelope<T: Serialize> {
    pub ok: bool,
    pub command: &'static str,
    pub version: &'static str,
    pub contract_version: u32,
    pub data: T,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) fn is_zero(value: &usize) -> bool {
    *value == 0
}

pub(crate) const WORKFLOW_UPDATE_MUTABLE_FIELDS: &[&str] =
    &["name", "nodes", "connections", "settings"];
pub(crate) const ACTIVATION_POLL_ATTEMPTS: usize = 8;
pub(crate) const ACTIVATION_POLL_INTERVAL_MS: u64 = 250;
pub(crate) const WEBHOOK_NODE_TYPE: &str = "n8n-nodes-base.webhook";

// ---------------------------------------------------------------------------
// JSON output helpers
// ---------------------------------------------------------------------------

pub(crate) fn emit_json<T: Serialize>(command: &'static str, data: &T) -> Result<(), AppError> {
    let envelope = Envelope {
        ok: true,
        command,
        version: env!("CARGO_PKG_VERSION"),
        contract_version: 1,
        data,
    };
    let rendered = serde_json::to_string_pretty(&envelope).map_err(|err| {
        AppError::api(
            command,
            "output.serialize_failed",
            format!("Failed to serialize JSON output: {err}"),
        )
    })?;
    println!("{rendered}");
    Ok(())
}

pub(crate) fn emit_json_line<T: Serialize>(
    command: &'static str,
    data: &T,
) -> Result<(), AppError> {
    let envelope = Envelope {
        ok: true,
        command,
        version: env!("CARGO_PKG_VERSION"),
        contract_version: 1,
        data,
    };
    let rendered = serde_json::to_string(&envelope).map_err(|err| {
        AppError::api(
            command,
            "output.serialize_failed",
            format!("Failed to serialize JSON output: {err}"),
        )
    })?;
    println!("{rendered}");
    Ok(())
}

pub(crate) fn emit_edit_result(
    context: &Context,
    command: &'static str,
    action: &str,
    result: &EditResult,
    extra: Vec<(String, Value)>,
) -> Result<(), AppError> {
    let warnings = sensitive_data_diagnostics(&result.path)?;
    let warning_count = warnings.len();
    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("workflow_path".to_string(), json!(result.path));
        data.insert("changed".to_string(), json!(result.changed));
        data.insert("warning_count".to_string(), json!(warning_count));
        for (key, value) in extra {
            data.insert(key, value);
        }
        if warning_count > 0 {
            data.insert("diagnostics".to_string(), json!(warnings));
        }
        emit_json(command, &Value::Object(data))
    } else {
        println!("{action} {}.", result.path.display());
        print_sensitive_warning_summary(&result.path, warning_count);
        Ok(())
    }
}

pub(crate) fn print_sensitive_warning_summary(workflow_path: &Path, warning_count: usize) {
    if warning_count == 0 {
        return;
    }

    println!(
        "Warning: found {} potential sensitive-data warning(s) in {}.",
        warning_count,
        workflow_path.display()
    );
    println!(
        "Run `n8nc validate {}` to inspect the findings.",
        workflow_path.display()
    );
}

// ---------------------------------------------------------------------------
// Repo / client helpers
// ---------------------------------------------------------------------------

pub(crate) fn load_loaded_repo(context: &Context) -> Result<LoadedRepo, AppError> {
    load_repo(context.repo_root.as_deref())
}

pub(crate) fn remote_client(
    repo: &LoadedRepo,
    alias: Option<&str>,
    command: &'static str,
) -> Result<(ApiClient, String, String), AppError> {
    let alias = resolve_instance_alias(repo, alias, command)?;
    let instance =
        repo.config.instances.get(&alias).ok_or_else(|| {
            AppError::config(command, format!("Unknown instance alias `{alias}`."))
        })?;
    let (token, source) = resolve_token(&alias, command)?;
    let client = ApiClient::new(command, instance, token)?;
    Ok((client, source, instance.base_url.clone()))
}

pub(crate) fn client_for_instance(
    repo: &LoadedRepo,
    instance: &str,
    command: &'static str,
    clients: &mut BTreeMap<String, Result<ApiClient, AppError>>,
) -> Result<ApiClient, AppError> {
    if let Some(client) = clients.get(instance) {
        return client.clone();
    }

    let resolved = remote_client(repo, Some(instance), command).map(|(client, _, _)| client);
    clients.insert(instance.to_string(), resolved.clone());
    resolved
}

// ---------------------------------------------------------------------------
// Path resolution helpers
// ---------------------------------------------------------------------------

pub(crate) fn context_root(context: &Context) -> Result<PathBuf, AppError> {
    if let Some(path) = &context.repo_root {
        Ok(path.clone())
    } else {
        std::env::current_dir().map_err(|err| {
            AppError::config(
                "config",
                format!("Failed to resolve current directory: {err}"),
            )
        })
    }
}

pub(crate) fn resolve_local_file_path(context: &Context, path: &Path) -> Result<PathBuf, AppError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    if let Ok(repo) = load_repo(context.repo_root.as_deref()) {
        return Ok(repo.root.join(path));
    }

    Ok(context_root(context)?.join(path))
}

pub(crate) fn resolve_existing_workflow_path(context: &Context, target: &str) -> Option<PathBuf> {
    let raw = Path::new(target);
    let resolved = resolve_local_file_path(context, raw).ok()?;
    if resolved.is_file() {
        Some(resolved)
    } else {
        None
    }
}

pub(crate) fn resolve_new_workflow_path(
    context: &Context,
    explicit: Option<&Path>,
    name: &str,
    workflow_id: Option<&str>,
) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit {
        return resolve_local_file_path(context, path);
    }

    let workflow_id = workflow_id.map(ToOwned::to_owned).unwrap_or_else(|| {
        format!(
            "draft-{}-{}",
            Utc::now().timestamp_millis(),
            std::process::id()
        )
    });
    let file_name = default_workflow_file_name(name, &workflow_id);

    if let Ok(repo) = load_repo(context.repo_root.as_deref()) {
        return Ok(workflow_dir(&repo.root, &repo.config).join(file_name));
    }

    Ok(context_root(context)?.join(file_name))
}

pub(crate) fn finalize_created_workflow_source(
    repo: &LoadedRepo,
    source_path: &Path,
    tracked_path: &Path,
) -> (bool, Option<String>) {
    if source_path == tracked_path {
        return (false, None);
    }

    let workflow_root = workflow_dir(&repo.root, &repo.config);
    if !source_path.starts_with(&workflow_root) {
        return (false, None);
    }

    match fs::remove_file(source_path) {
        Ok(()) => (true, None),
        Err(err) => (
            false,
            Some(format!(
                "Warning: failed to remove original draft {}: {err}",
                source_path.display()
            )),
        ),
    }
}

// ---------------------------------------------------------------------------
// Workflow fetch / poll helpers
// ---------------------------------------------------------------------------

pub(crate) async fn fetch_workflow_required(
    client: &ApiClient,
    workflow_id: &str,
    command: &'static str,
    context: &'static str,
) -> Result<Value, AppError> {
    let remote = client
        .get_workflow_by_id(workflow_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(command, format!("Workflow `{workflow_id}` {context}."))
        })?;
    Ok(remote.get("data").cloned().unwrap_or(remote))
}

pub(crate) async fn wait_for_workflow_active_state(
    client: &ApiClient,
    workflow_id: &str,
    command: &'static str,
    desired_active: bool,
) -> Result<Value, AppError> {
    let mut last_workflow = None;
    let mut observed_active = None;

    for attempt in 0..ACTIVATION_POLL_ATTEMPTS {
        let current = fetch_workflow_required(
            client,
            workflow_id,
            command,
            "could not be re-fetched after the state change",
        )
        .await?;
        observed_active = workflow_active(&current);
        if observed_active == Some(desired_active) {
            return Ok(current);
        }
        last_workflow = Some(current);

        if attempt + 1 < ACTIVATION_POLL_ATTEMPTS {
            thread::sleep(Duration::from_millis(ACTIVATION_POLL_INTERVAL_MS));
        }
    }

    Err(AppError::api(
        command,
        "workflow.state_not_converged",
        format!(
            "Workflow `{workflow_id}` did not report `{}` after `{command}`.",
            if desired_active { "active" } else { "inactive" }
        ),
    )
    .with_suggestion("Re-run the command or inspect the workflow with `n8nc get <id>`.")
    .with_json_data(json!({
        "workflow_id": workflow_id,
        "expected_active": desired_active,
        "observed_active": observed_active,
        "last_workflow": last_workflow,
    })))
}

// ---------------------------------------------------------------------------
// Workflow payload builders
// ---------------------------------------------------------------------------

pub(crate) fn workflow_create_payload(path: &Path) -> Result<Value, AppError> {
    let diagnostics = validate_workflow_path(path)?;
    let error_count = diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Error)
        .count();
    let warning_count = diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Warning)
        .count();
    if error_count > 0 {
        return Err(AppError::validation(
            "workflow",
            format!(
                "Local workflow file has {error_count} validation error(s) and cannot be created remotely."
            ),
        )
        .with_json_data(json!({
            "files_checked": 1,
            "error_count": error_count,
            "warning_count": warning_count,
            "diagnostics": diagnostics,
        })));
    }

    let workflow = load_workflow_file(path, "workflow")?;
    let mut payload = canonicalize_workflow(&workflow)?;
    let object = payload.as_object_mut().ok_or_else(|| {
        AppError::validation("workflow", "Workflow payload must be a JSON object.")
    })?;
    object.remove("id");
    object.remove("active");
    apply_default_workflow_settings(object)?;

    let has_name = object
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if !has_name {
        return Err(AppError::validation(
            "workflow",
            "Workflow file must include a non-empty `name` before it can be created remotely.",
        ));
    }
    if !matches!(object.get("nodes"), Some(Value::Array(_))) {
        return Err(AppError::validation(
            "workflow",
            "Workflow file must include a `nodes` array before it can be created remotely.",
        ));
    }
    if !matches!(object.get("connections"), Some(Value::Object(_))) {
        return Err(AppError::validation(
            "workflow",
            "Workflow file must include a `connections` object before it can be created remotely.",
        ));
    }

    normalize_remote_create_payload(&mut payload)?;
    canonicalize_workflow(&payload)
}

pub(crate) fn workflow_update_payload(workflow: &Value) -> Result<Value, AppError> {
    let mut payload = canonicalize_workflow(workflow)?;
    let object = payload
        .as_object_mut()
        .ok_or_else(|| AppError::validation("push", "Workflow payload must be a JSON object."))?;
    apply_default_workflow_settings(object)?;

    let has_name = object
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if !has_name {
        return Err(AppError::validation(
            "push",
            "Workflow file must include a non-empty `name` before it can be pushed.",
        ));
    }
    if !matches!(object.get("nodes"), Some(Value::Array(_))) {
        return Err(AppError::validation(
            "push",
            "Workflow file must include a `nodes` array before it can be pushed.",
        ));
    }
    if !matches!(object.get("connections"), Some(Value::Object(_))) {
        return Err(AppError::validation(
            "push",
            "Workflow file must include a `connections` object before it can be pushed.",
        ));
    }

    normalize_remote_create_payload(&mut payload)?;
    let payload_object = payload
        .as_object()
        .ok_or_else(|| AppError::validation("push", "Workflow payload must be a JSON object."))?;
    let mut out = serde_json::Map::new();
    for field in WORKFLOW_UPDATE_MUTABLE_FIELDS {
        if let Some(value) = payload_object.get(*field) {
            out.insert((*field).to_string(), value.clone());
        }
    }

    canonicalize_workflow(&Value::Object(out))
}

pub(crate) fn unsupported_push_fields(local: &Value, remote: &Value) -> Vec<String> {
    let Some(local_object) = local.as_object() else {
        return Vec::new();
    };
    let Some(remote_object) = remote.as_object() else {
        return Vec::new();
    };

    let supported: BTreeSet<&str> = WORKFLOW_UPDATE_MUTABLE_FIELDS.iter().copied().collect();
    let mut keys = BTreeSet::new();
    for key in local_object.keys() {
        keys.insert(key.clone());
    }
    for key in remote_object.keys() {
        keys.insert(key.clone());
    }

    keys.into_iter()
        .filter(|key| key != "id" && !supported.contains(key.as_str()))
        .filter(|key| local_object.get(key) != remote_object.get(key))
        .collect()
}

pub(crate) fn apply_default_workflow_settings(
    object: &mut serde_json::Map<String, Value>,
) -> Result<(), AppError> {
    let settings = object
        .entry("settings".to_string())
        .or_insert_with(default_workflow_settings);
    let settings_object = settings.as_object_mut().ok_or_else(|| {
        AppError::validation("workflow", "Workflow `settings` field must be an object.")
    })?;

    for (key, value) in default_workflow_settings()
        .as_object()
        .into_iter()
        .flatten()
    {
        settings_object
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    Ok(())
}

pub(crate) fn normalize_remote_create_payload(payload: &mut Value) -> Result<(), AppError> {
    let Some(nodes) = payload.get_mut("nodes").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for node in nodes {
        normalize_remote_create_node(node)?;
    }
    Ok(())
}

pub(crate) fn normalize_remote_create_node(node: &mut Value) -> Result<(), AppError> {
    if node.get("type").and_then(Value::as_str) != Some(WEBHOOK_NODE_TYPE) {
        return Ok(());
    }
    let node_object = node.as_object_mut().ok_or_else(|| {
        AppError::validation("workflow", "Workflow node entry must be a JSON object.")
    })?;
    let parameters = node_object
        .entry("parameters".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let parameters = parameters.as_object_mut().ok_or_else(|| {
        AppError::validation("workflow", "Webhook node `parameters` must be an object.")
    })?;
    let normalized_path = parameters
        .get("path")
        .and_then(Value::as_str)
        .map(normalize_webhook_path)
        .filter(|path| !path.is_empty());
    if let Some(path) = normalized_path {
        parameters.insert("path".to_string(), Value::String(path.clone()));
        let existing_webhook_id = node_object
            .get("webhookId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if existing_webhook_id.is_none() {
            node_object.insert("webhookId".to_string(), Value::String(path));
        }
    }
    let type_version = node_object.get("typeVersion").and_then(Value::as_f64);
    if type_version.is_none_or(|version| version < 2.0) {
        node_object.insert("typeVersion".to_string(), json!(2));
    }
    Ok(())
}

pub(crate) fn normalize_webhook_path(path: &str) -> String {
    path.trim_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

pub(crate) fn parse_pairs(
    command: &'static str,
    field_name: &'static str,
    values: &[String],
    separator: char,
) -> Result<Vec<(String, String)>, AppError> {
    values
        .iter()
        .map(|item| {
            item.split_once(separator)
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .ok_or_else(|| {
                    AppError::usage(
                        command,
                        format!("`{field_name}` values must use `{separator}` separators."),
                    )
                })
        })
        .collect()
}

pub(crate) fn parse_node_value(
    command: &'static str,
    mode: &ValueModeArgs,
    value: Option<&str>,
) -> Result<Value, AppError> {
    if mode.null {
        return Ok(Value::Null);
    }

    let Some(value) = value else {
        return Err(AppError::usage(
            command,
            "A value is required unless `--null` is used.",
        ));
    };

    if mode.json_value {
        return serde_json::from_str(value).map_err(|err| {
            AppError::usage(command, format!("`--json-value` must be valid JSON: {err}"))
        });
    }

    if mode.number {
        let number = serde_json::Number::from_f64(value.parse::<f64>().map_err(|err| {
            AppError::usage(command, format!("`--number` value must be numeric: {err}"))
        })?)
        .ok_or_else(|| AppError::usage(command, "`--number` value must be finite."))?;
        return Ok(Value::Number(number));
    }

    if mode.bool_value {
        let parsed = match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => {
                return Err(AppError::usage(
                    command,
                    "`--bool` value must be one of: true, false, 1, 0, yes, no.",
                ));
            }
        };
        return Ok(Value::Bool(parsed));
    }

    Ok(Value::String(value.to_string()))
}

pub(crate) fn read_request_body(
    command: &'static str,
    data: Option<String>,
    data_file: Option<PathBuf>,
    stdin: bool,
) -> Result<Option<Vec<u8>>, AppError> {
    if let Some(data) = data {
        return Ok(Some(data.into_bytes()));
    }
    if let Some(path) = data_file {
        return fs::read(&path).map(Some).map_err(|err| {
            AppError::usage(command, format!("Failed to read {}: {err}", path.display()))
        });
    }
    if stdin {
        let mut buffer = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buffer)
            .map_err(|err| AppError::usage(command, format!("Failed to read stdin: {err}")))?;
        return Ok(Some(buffer));
    }
    Ok(None)
}

pub(crate) fn parse_workflow_execute_input(
    body: Option<Vec<u8>>,
) -> Result<Option<Value>, AppError> {
    let Some(body) = body else {
        return Ok(None);
    };
    let rendered = String::from_utf8(body).map_err(|err| {
        AppError::usage(
            "workflow",
            format!("Workflow execute input must be valid UTF-8 text or JSON: {err}"),
        )
    })?;
    let trimmed = rendered.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(serde_json::from_str::<Value>(trimmed)
        .ok()
        .or_else(|| Some(Value::String(trimmed.to_string()))))
}

// ---------------------------------------------------------------------------
// Display / formatting helpers
// ---------------------------------------------------------------------------

pub(crate) fn print_response_body(value: &Value) -> Result<(), AppError> {
    match value {
        Value::String(text) => {
            if !text.is_empty() {
                println!("{text}");
            }
        }
        other => {
            print!("{}", pretty_json(other)?);
        }
    }
    Ok(())
}

pub(crate) fn truncate(input: &str, width: usize) -> String {
    if input.len() <= width {
        input.to_string()
    } else {
        format!("{}...", &input[..width.saturating_sub(3)])
    }
}

pub(crate) fn absolutize(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub(crate) fn value_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{unsupported_push_fields, workflow_update_payload};

    #[test]
    fn workflow_update_payload_only_keeps_mutable_fields() {
        let payload = workflow_update_payload(&json!({
            "id": "wf-1",
            "name": "Example",
            "active": true,
            "description": "ignored",
            "tags": [{"id": "tag-1"}],
            "nodes": [],
            "connections": {},
            "settings": {},
            "meta": {"foo": "bar"}
        }))
        .expect("update payload");

        assert_eq!(
            payload,
            json!({
                "name": "Example",
                "settings": {
                    "executionOrder": "v1",
                    "saveDataErrorExecution": "all",
                    "saveDataSuccessExecution": "all",
                    "saveExecutionProgress": true,
                    "saveManualExecutions": true
                },
                "nodes": [],
                "connections": {}
            })
        );
    }

    #[test]
    fn unsupported_push_fields_only_reports_non_mutable_differences() {
        let local = json!({
            "id": "wf-1",
            "name": "Example",
            "active": true,
            "nodes": [{"name": "Webhook"}],
            "connections": {},
            "settings": {}
        });
        let remote = json!({
            "id": "wf-1",
            "name": "Example",
            "active": false,
            "nodes": [{"name": "Webhook"}],
            "connections": {},
            "settings": {}
        });

        assert_eq!(unsupported_push_fields(&local, &remote), vec!["active"]);
    }
}
