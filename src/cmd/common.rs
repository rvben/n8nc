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

use owo_colors::OwoColorize;

use crate::{
    api::ApiClient,
    auth::resolve_token,
    canonical::{canonicalize_workflow, pretty_json},
    cli::ValueModeArgs,
    config::{LoadedRepo, load_repo, resolve_instance_alias, workflow_dir},
    edit::{EditResult, default_workflow_file_name, default_workflow_settings},
    error::AppError,
    repo::{
        find_tracked_workflows_by_id, find_tracked_workflows_by_slug, load_workflow_file,
        workflow_active,
    },
    validate::{Severity, sensitive_data_diagnostics, validate_workflow_path},
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Context {
    pub json: bool,
    pub quiet: bool,
    pub repo_root: Option<PathBuf>,
}

/// Print a human-readable message to stderr. Suppressed by `--quiet` or `--json`.
pub(crate) fn print_message(context: &Context, msg: &str) {
    if !context.quiet && !context.json {
        eprintln!("{msg}");
    }
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

/// Workflow `settings` keys that the n8n public API rejects on create/update.
///
/// The public API validates the request body's `settings` object with
/// `additionalProperties: false`, so any key it does not know about fails the
/// whole request with HTTP 400 `settings must NOT have additional properties`.
/// These keys are set through the n8n editor UI (they round-trip via `pull`)
/// but are not part of the public API schema, which turns an otherwise valid
/// `pull` -> edit -> `push` into a hard failure.
///
/// Omitting a settings key on update does not clear it: the server keeps the
/// stored value for any key absent from the payload. Stripping these before a
/// push is therefore lossless, and we report which keys were dropped.
///
/// This is deliberately a denylist of known-incompatible keys rather than an
/// allowlist of accepted ones, because n8n Cloud accepts several keys that are
/// absent from the published schema (for example `callerPolicy` and
/// `availableInMCP`); an allowlist would silently drop those.
pub(crate) const PUSH_INCOMPATIBLE_SETTINGS: &[&str] = &["binaryMode", "timeSavedMode"];

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

    if use_color() {
        println!(
            "{} found {} potential sensitive-data warning(s) in {}.",
            "Warning:".yellow().bold(),
            warning_count,
            workflow_path.display()
        );
    } else {
        println!(
            "Warning: found {} potential sensitive-data warning(s) in {}.",
            warning_count,
            workflow_path.display()
        );
    }
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

pub(crate) fn is_workflow_json_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|name| name.ends_with(".workflow.json"))
        .unwrap_or(false)
}

/// Resolve a `push`/`diff` target into the tracked `.workflow.json` file it
/// refers to. The target may be given as any of:
///
/// 1. a path to an existing `.workflow.json` file (absolute or repo-relative),
/// 2. a tracked workflow id (matched against `<slug>--<id>.workflow.json`), or
/// 3. a tracked workflow slug (the `<slug>` portion of the file name).
///
/// `pull` and `runs get` already accept a bare id; `push` and `diff` used to
/// require a file path, which surfaced as a confusing "No such file" error when
/// an id was passed. This resolver closes that gap.
pub(crate) fn resolve_tracked_workflow_file(
    repo: &LoadedRepo,
    command: &'static str,
    target: &Path,
) -> Result<PathBuf, AppError> {
    // 1. An existing path wins, and must be a `.workflow.json` file.
    let absolute = absolutize(&repo.root, target);
    if absolute.is_file() {
        if is_workflow_json_path(&absolute) {
            return Ok(absolute);
        }
        return Err(AppError::usage(
            command,
            format!("`{}` is not a `.workflow.json` file.", absolute.display()),
        ));
    }

    let raw = target
        .to_str()
        .ok_or_else(|| AppError::usage(command, "Workflow target must be valid UTF-8."))?;

    // A target that looks like a path (has a separator) or a workflow file name
    // but does not exist is a missing file, not an id/slug. Avoid silently
    // reinterpreting a mistyped path as a lookup key.
    if raw.contains('/')
        || raw.contains(std::path::MAIN_SEPARATOR)
        || raw.ends_with(".workflow.json")
    {
        return Err(AppError::not_found(
            command,
            format!("Workflow file not found: {}", absolute.display()),
        ));
    }

    // 2/3. Match a tracked workflow by id or slug. Both lookups are pooled (and
    //    each gathers every tracked match, not just the first walk hit) so that
    //    an untracked copy cannot shadow the real workflow and an id/slug
    //    namespace collision is reported as ambiguous rather than silently
    //    resolving to one interpretation.
    let mut candidates = find_tracked_workflows_by_id(repo, raw);
    for path in find_tracked_workflows_by_slug(repo, raw) {
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }

    match pick_unique_tracked(command, raw, candidates)? {
        Some(path) => Ok(path),
        None => Err(AppError::not_found(
            command,
            format!("No tracked workflow matches `{raw}`."),
        )
        .with_suggestion(
            "Pass a `.workflow.json` path, or a tracked workflow id or slug (see `n8nc status`).",
        )),
    }
}

/// Collapse the tracked candidates into a single result: `None` when there is no
/// match, `Some(path)` for exactly one, and an ambiguity error for more than one.
fn pick_unique_tracked(
    command: &'static str,
    raw: &str,
    mut candidates: Vec<PathBuf>,
) -> Result<Option<PathBuf>, AppError> {
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(Some(candidates.remove(0))),
        _ => {
            let names: Vec<String> = candidates
                .iter()
                .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
                .map(ToOwned::to_owned)
                .collect();
            Err(AppError::usage(
                command,
                format!(
                    "`{raw}` matches multiple tracked workflows: {}. Use the workflow id or file path.",
                    names.join(", ")
                ),
            ))
        }
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

pub(crate) fn workflow_create_payload(path: &Path) -> Result<(Value, Vec<String>), AppError> {
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
    let stripped = strip_push_incompatible_settings(object);

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
    Ok((canonicalize_workflow(&payload)?, stripped))
}

/// Remove workflow `settings` keys the n8n public API rejects (see
/// [`PUSH_INCOMPATIBLE_SETTINGS`]). Returns the removed keys, sorted, so callers
/// can report what was omitted. The server retains stored values for omitted
/// keys, so this is lossless.
pub(crate) fn strip_push_incompatible_settings(
    object: &mut serde_json::Map<String, Value>,
) -> Vec<String> {
    let Some(settings) = object.get_mut("settings").and_then(Value::as_object_mut) else {
        return Vec::new();
    };
    let mut stripped = Vec::new();
    for key in PUSH_INCOMPATIBLE_SETTINGS {
        if settings.remove(*key).is_some() {
            stripped.push((*key).to_string());
        }
    }
    stripped.sort();
    stripped
}

/// Incompatible settings keys (see [`PUSH_INCOMPATIBLE_SETTINGS`]) whose local
/// value differs from the remote value. Because these keys are dropped from the
/// update payload, a local change to one of them cannot be applied by push;
/// callers reject such a change instead of silently discarding it.
pub(crate) fn changed_incompatible_settings(local: &Value, remote: &Value) -> Vec<String> {
    let local_settings = local.get("settings");
    let remote_settings = remote.get("settings");
    PUSH_INCOMPATIBLE_SETTINGS
        .iter()
        .filter(|key| {
            local_settings.and_then(|settings| settings.get(**key))
                != remote_settings.and_then(|settings| settings.get(**key))
        })
        .map(|key| (*key).to_string())
        .collect()
}

/// Human-readable note describing settings omitted from a create/push because
/// the n8n public API rejects them. `retained_by_server` is true for updates
/// (the server keeps the stored value for omitted keys) and false for creates
/// (the new workflow simply will not have the key). Returns `None` when nothing
/// was stripped.
pub(crate) fn stripped_settings_note(
    stripped: &[String],
    retained_by_server: bool,
) -> Option<String> {
    if stripped.is_empty() {
        return None;
    }
    let tail = if retained_by_server {
        "the server keeps its stored values"
    } else {
        "the created workflow will not have them"
    };
    Some(format!(
        "Note: omitted API-incompatible setting(s): {} ({tail}).",
        stripped.join(", ")
    ))
}

pub(crate) fn workflow_update_payload(workflow: &Value) -> Result<(Value, Vec<String>), AppError> {
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

    let stripped = strip_push_incompatible_settings(&mut out);
    Ok((canonicalize_workflow(&Value::Object(out))?, stripped))
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

pub(crate) fn use_color() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

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
    use std::{collections::BTreeMap, fs, path::Path};

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use crate::config::{InstanceConfig, LoadedRepo, RepoConfig};

    use super::{
        changed_incompatible_settings, resolve_tracked_workflow_file, unsupported_push_fields,
        workflow_update_payload,
    };

    fn fixture_repo(root: &Path) -> LoadedRepo {
        fs::create_dir_all(root.join("workflows")).expect("workflow dir");
        let mut instances = BTreeMap::new();
        instances.insert(
            "prod".to_string(),
            InstanceConfig {
                base_url: "https://example.n8n.cloud".to_string(),
                api_version: "v1".to_string(),
                execute: None,
            },
        );
        LoadedRepo {
            root: root.to_path_buf(),
            config: RepoConfig {
                schema_version: 1,
                default_instance: "prod".to_string(),
                workflow_dir: "workflows".into(),
                instances,
                lint: None,
            },
        }
    }

    fn write_untracked_workflow_file(repo: &LoadedRepo, file_name: &str) -> std::path::PathBuf {
        let path = repo.root.join("workflows").join(file_name);
        fs::write(&path, "{}").expect("write workflow file");
        path
    }

    /// A tracked workflow has both the `.workflow.json` file and its `.meta.json`
    /// sidecar on disk.
    fn write_tracked_workflow_file(repo: &LoadedRepo, file_name: &str) -> std::path::PathBuf {
        let path = write_untracked_workflow_file(repo, file_name);
        fs::write(crate::repo::sidecar_path_for(&path), "{}").expect("write sidecar");
        path
    }

    #[test]
    fn workflow_update_payload_only_keeps_mutable_fields() {
        let (payload, stripped) = workflow_update_payload(&json!({
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

        assert!(stripped.is_empty());
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
    fn workflow_update_payload_strips_api_incompatible_settings() {
        let (payload, stripped) = workflow_update_payload(&json!({
            "id": "wf-1",
            "name": "Example",
            "nodes": [],
            "connections": {},
            "settings": {
                "executionOrder": "v1",
                "callerPolicy": "workflowsFromSameOwner",
                "binaryMode": "separate",
                "timeSavedMode": "fixed"
            }
        }))
        .expect("update payload");

        assert_eq!(
            stripped,
            vec!["binaryMode".to_string(), "timeSavedMode".to_string()]
        );
        let settings = payload
            .get("settings")
            .and_then(Value::as_object)
            .expect("settings object");
        assert!(!settings.contains_key("binaryMode"));
        assert!(!settings.contains_key("timeSavedMode"));
        // Keys the public API accepts must be preserved.
        assert_eq!(
            settings.get("callerPolicy").and_then(Value::as_str),
            Some("workflowsFromSameOwner")
        );
        assert_eq!(
            settings.get("executionOrder").and_then(Value::as_str),
            Some("v1")
        );
    }

    #[test]
    fn changed_incompatible_settings_flags_only_changed_keys() {
        let remote = json!({
            "settings": { "binaryMode": "separate", "timeSavedMode": "fixed", "executionOrder": "v1" }
        });
        let local = json!({
            "settings": { "binaryMode": "default", "timeSavedMode": "fixed", "executionOrder": "v1" }
        });

        // Only the changed incompatible key is reported.
        assert_eq!(
            changed_incompatible_settings(&local, &remote),
            vec!["binaryMode".to_string()]
        );
        // An unchanged round-trip reports nothing.
        assert!(changed_incompatible_settings(&remote, &remote).is_empty());
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

    #[test]
    fn resolve_tracked_workflow_file_matches_id_slug_and_path() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());
        let path = write_tracked_workflow_file(&repo, "offerte--WoupM9pnmtSDPnBC.workflow.json");

        assert_eq!(
            resolve_tracked_workflow_file(&repo, "push", Path::new("WoupM9pnmtSDPnBC"))
                .expect("resolve by id"),
            path
        );
        assert_eq!(
            resolve_tracked_workflow_file(&repo, "push", Path::new("offerte"))
                .expect("resolve by slug"),
            path
        );
        assert_eq!(
            resolve_tracked_workflow_file(
                &repo,
                "push",
                Path::new("workflows/offerte--WoupM9pnmtSDPnBC.workflow.json"),
            )
            .expect("resolve by path"),
            path
        );
    }

    #[test]
    fn resolve_tracked_workflow_file_ignores_untracked_slug_and_id_matches() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());
        let tracked = write_tracked_workflow_file(&repo, "offerte--WoupM9pnmtSDPnBC.workflow.json");
        // An untracked draft sharing the slug must not shadow the tracked file.
        write_untracked_workflow_file(&repo, "offerte--draft-1-2.workflow.json");

        assert_eq!(
            resolve_tracked_workflow_file(&repo, "push", Path::new("offerte"))
                .expect("slug resolves to the tracked workflow"),
            tracked
        );

        // A bare id that only matches an untracked file is treated as unknown.
        let err = resolve_tracked_workflow_file(&repo, "push", Path::new("draft-1-2"))
            .expect_err("untracked id is not resolvable");
        assert!(err.message.contains("No tracked workflow matches"));
    }

    #[test]
    fn resolve_tracked_workflow_file_prefers_tracked_over_untracked_duplicate_id() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());
        let tracked = write_tracked_workflow_file(&repo, "offerte--WoupM9pnmtSDPnBC.workflow.json");
        // An untracked copy carrying the same id must not shadow or hide the
        // tracked file regardless of directory-walk order.
        write_untracked_workflow_file(&repo, "copy-offerte--WoupM9pnmtSDPnBC.workflow.json");

        assert_eq!(
            resolve_tracked_workflow_file(&repo, "push", Path::new("WoupM9pnmtSDPnBC"))
                .expect("id resolves to the tracked workflow despite an untracked duplicate"),
            tracked
        );
    }

    #[test]
    fn resolve_tracked_workflow_file_reports_unknown_target() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());
        let err = resolve_tracked_workflow_file(&repo, "push", Path::new("no-such-workflow"))
            .expect_err("unknown target");
        assert!(err.message.contains("No tracked workflow matches"));
    }

    #[test]
    fn resolve_tracked_workflow_file_flags_ambiguous_slug() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());
        write_tracked_workflow_file(&repo, "offerte--aaaaaaaaaaaaaaaa.workflow.json");
        write_tracked_workflow_file(&repo, "offerte--bbbbbbbbbbbbbbbb.workflow.json");
        let err = resolve_tracked_workflow_file(&repo, "push", Path::new("offerte"))
            .expect_err("ambiguous slug");
        assert!(err.message.contains("matches multiple"));
    }

    #[test]
    fn resolve_tracked_workflow_file_flags_id_slug_collision() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());
        // `collide` is the id of one workflow and the slug of another.
        write_tracked_workflow_file(&repo, "alpha--collide.workflow.json");
        write_tracked_workflow_file(&repo, "collide--zzzzzzzzzzzzzzzz.workflow.json");
        let err = resolve_tracked_workflow_file(&repo, "push", Path::new("collide"))
            .expect_err("id/slug collision");
        assert!(err.message.contains("matches multiple"));
    }
}
