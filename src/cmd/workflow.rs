use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    canonical::{canonicalize_workflow, pretty_json},
    cli::{
        GetArgs, WorkflowArgs, WorkflowCommand, WorkflowCreateArgs, WorkflowExecuteArgs,
        WorkflowNewArgs, WorkflowRemoveArgs, WorkflowShowArgs,
    },
    config::{load_repo, resolve_instance_alias},
    edit::{create_workflow, workflow_id_string},
    error::AppError,
    execute::{
        WorkflowExecuteInvocation, execute_backend_setup_hint, execute_workflow,
        probe_execute_backend,
    },
    repo::{
        cache_snapshot_path, find_existing_workflow_path, load_meta, load_workflow_file,
        sidecar_path_for, store_workflow, workflow_active, workflow_id, workflow_name,
    },
    tree,
    validate::sensitive_data_diagnostics,
};

use super::common::{
    Context, WEBHOOK_NODE_TYPE, emit_edit_result, emit_json, fetch_workflow_required,
    finalize_created_workflow_source, load_loaded_repo, normalize_webhook_path,
    parse_workflow_execute_input, print_message, print_response_body,
    print_sensitive_warning_summary, read_request_body, remote_client,
    resolve_existing_workflow_path, resolve_local_file_path, resolve_new_workflow_path, truncate,
    value_string, wait_for_workflow_active_state, workflow_create_payload,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkflowNodeRow {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_version: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub credentials: Vec<NodeCredentialRow>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkflowConnectionRow {
    pub from: String,
    pub kind: String,
    pub output_index: usize,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkflowWebhookRow {
    pub node: String,
    pub methods: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct NodeCredentialRow {
    pub credential_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialWorkflowUsageRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialReferenceRow {
    pub credential_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_name: Option<String>,
    pub usage_count: usize,
    pub workflow_count: usize,
    pub workflows: Vec<CredentialWorkflowUsageRow>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowRemoveResult {
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance: Option<String>,
    remote_removed: bool,
    local_removed: bool,
    removed_paths: Vec<PathBuf>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_get(context: &Context, args: GetArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "get")?;
    let workflow = client.resolve_workflow(&args.identifier).await?;
    let canonical = canonicalize_workflow(&workflow)?;

    if context.json {
        emit_json("get", &json!({ "workflow": canonical }))
    } else {
        print!("{}", pretty_json(&canonical)?);
        Ok(())
    }
}

pub(crate) async fn cmd_workflow(context: &Context, args: WorkflowArgs) -> Result<(), AppError> {
    match args.command {
        WorkflowCommand::New(args) => cmd_workflow_new(context, args).await,
        WorkflowCommand::Create(args) => cmd_workflow_create(context, args).await,
        WorkflowCommand::Execute(args) => cmd_workflow_execute(context, args).await,
        WorkflowCommand::Show(args) => cmd_workflow_show(context, args).await,
        WorkflowCommand::Rm(args) => cmd_workflow_remove(context, args).await,
    }
}

async fn cmd_workflow_new(context: &Context, args: WorkflowNewArgs) -> Result<(), AppError> {
    let workflow_id = args.id.unwrap_or_else(|| {
        format!(
            "draft-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            std::process::id()
        )
    });
    let target_path = resolve_new_workflow_path(
        context,
        args.path.as_deref(),
        &args.name,
        Some(&workflow_id),
    )?;
    let result = create_workflow(&target_path, &args.name, Some(&workflow_id), args.active)?;
    emit_edit_result(
        context,
        "workflow",
        "Created",
        &result,
        vec![(
            "workflow_id".to_string(),
            json!(workflow_id_string(&result.workflow)),
        )],
    )
}

async fn cmd_workflow_create(context: &Context, args: WorkflowCreateArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "workflow")?;
    let source_path = resolve_local_file_path(context, &args.file)?;
    let source_meta_path = sidecar_path_for(&source_path);
    if source_meta_path.exists() {
        return Err(AppError::validation(
            "workflow",
            format!(
                "{} already has a metadata sidecar. Use `n8nc push` for tracked workflows.",
                source_path.display()
            ),
        ));
    }

    let payload = workflow_create_payload(&source_path)?;
    let (client, _, base_url) = remote_client(&repo, Some(&alias), "workflow")?;
    let created = client.create_workflow(&payload).await?;
    let created_id = workflow_id(&created).ok_or_else(|| {
        AppError::api(
            "workflow",
            "api.invalid_response",
            "Created workflow response was missing `id`.",
        )
    })?;

    let created = if args.activate {
        client.activate_workflow(&created_id).await?;
        wait_for_workflow_active_state(&client, &created_id, "workflow", true).await?
    } else {
        fetch_workflow_required(
            &client,
            &created_id,
            "workflow",
            "was created but could not be re-fetched",
        )
        .await?
    };

    let stored = store_workflow(&repo, &alias, &created)?;
    let (source_removed, cleanup_warning) =
        finalize_created_workflow_source(&repo, &source_path, &stored.workflow_path);

    let warnings = sensitive_data_diagnostics(&stored.workflow_path)?;
    let warning_count = warnings.len();
    let webhooks = summarize_workflow_webhooks(&created, Some(base_url.as_str()));
    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("instance".to_string(), json!(alias));
        data.insert("source_path".to_string(), json!(source_path));
        data.insert("source_removed".to_string(), json!(source_removed));
        data.insert("workflow_path".to_string(), json!(stored.workflow_path));
        data.insert("meta_path".to_string(), json!(stored.meta_path));
        data.insert("workflow_id".to_string(), json!(stored.meta.workflow_id));
        data.insert("active".to_string(), json!(workflow_active(&created)));
        data.insert("webhooks".to_string(), json!(webhooks));
        data.insert("warning_count".to_string(), json!(warning_count));
        if warning_count > 0 {
            data.insert("diagnostics".to_string(), json!(warnings));
        }
        if let Some(cleanup_warning) = cleanup_warning {
            data.insert("cleanup_warning".to_string(), json!(cleanup_warning));
        }
        emit_json("workflow", &Value::Object(data))
    } else {
        print_message(
            context,
            &format!(
                "Created remote workflow {} -> {}",
                stored.meta.workflow_id,
                stored.workflow_path.display()
            ),
        );
        print_message(
            context,
            &format!("Metadata: {}", stored.meta_path.display()),
        );
        if source_removed {
            print_message(
                context,
                &format!("Removed original draft: {}", source_path.display()),
            );
        } else if source_path != stored.workflow_path {
            print_message(
                context,
                &format!("Original local file kept at {}", source_path.display()),
            );
        }
        if let Some(cleanup_warning) = cleanup_warning {
            print_message(context, &cleanup_warning);
        }
        print_workflow_webhooks(&webhooks);
        print_sensitive_warning_summary(&stored.workflow_path, warning_count);
        Ok(())
    }
}

async fn cmd_workflow_execute(
    context: &Context,
    args: WorkflowExecuteArgs,
) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "workflow")?;
    let execute_config = repo
        .config
        .instances
        .get(&alias)
        .and_then(|instance| instance.execute.as_ref())
        .ok_or_else(|| {
            AppError::config(
                "workflow",
                format!("No workflow execute backend is configured for `{alias}`."),
            )
            .with_suggestion(format!(
                "{} Use `n8nc trigger <webhook-url>` for webhook-triggered workflows.",
                execute_backend_setup_hint(&alias)
            ))
        })?;
    probe_execute_backend(&repo.root, execute_config, "workflow").map_err(|err| {
        AppError::config(
            "workflow",
            format!(
                "Workflow execute backend for `{alias}` is not runnable: {}",
                err.message
            ),
        )
        .with_suggestion(format!(
            "{} Use `n8nc doctor` to verify the local backend wiring.",
            execute_backend_setup_hint(&alias)
        ))
    })?;

    let (client, _, base_url) = remote_client(&repo, Some(&alias), "workflow")?;
    let workflow = client.resolve_workflow(&args.identifier).await?;
    let wf_id = workflow_id(&workflow).ok_or_else(|| {
        AppError::api(
            "workflow",
            "api.invalid_response",
            "Resolved workflow response was missing `id`.",
        )
    })?;
    let wf_name = workflow_name(&workflow).ok_or_else(|| {
        AppError::api(
            "workflow",
            "api.invalid_response",
            "Resolved workflow response was missing `name`.",
        )
    })?;
    let input = parse_workflow_execute_input(read_request_body(
        "workflow",
        args.input,
        args.input_file,
        args.stdin,
    )?)?;
    let result = execute_workflow(
        &repo.root,
        execute_config,
        &WorkflowExecuteInvocation {
            instance_alias: alias.clone(),
            base_url: base_url.clone(),
            workflow_id: wf_id.clone(),
            workflow_name: wf_name.clone(),
            workflow_active: workflow_active(&workflow),
            input,
        },
        "workflow",
    )?;

    if context.json {
        emit_json(
            "workflow",
            &json!({
                "action": "execute",
                "instance": alias,
                "workflow_id": wf_id,
                "workflow_name": wf_name,
                "active": workflow_active(&workflow),
                "execution": result,
            }),
        )
    } else {
        println!("Executed workflow {wf_name} ({wf_id})");
        println!("Backend: {}", result.program);
        if let Some(output) = &result.output {
            print_response_body(output)?;
        }
        if let Some(stderr) = &result.stderr {
            eprintln!("{stderr}");
        }
        Ok(())
    }
}

async fn cmd_workflow_show(context: &Context, args: WorkflowShowArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let workflow = canonicalize_workflow(&load_workflow_file(&file, "workflow")?)?;
    let instance = resolve_workflow_show_instance(context, &file, args.remote.instance.as_deref())?;
    let base_url = resolve_instance_base_url(context, instance.as_deref())?;
    let nodes = summarize_workflow_nodes(&workflow);
    let connections = summarize_workflow_connections(&workflow);
    let webhooks = summarize_workflow_webhooks(&workflow, base_url.as_deref());
    let credentials = summarize_credential_references(std::slice::from_ref(&workflow), None);

    let tree_data = if args.tree {
        let raw_nodes_by_name: std::collections::HashMap<String, &Value> = workflow
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|n| {
                n.get("name")
                    .and_then(Value::as_str)
                    .map(|name| (name.to_string(), n))
            })
            .collect();

        let tree_nodes: Vec<tree::TreeNode> = nodes
            .iter()
            .map(|n| {
                let detail = raw_nodes_by_name
                    .get(&n.name)
                    .and_then(|raw| extract_node_detail(n.node_type.as_deref().unwrap_or(""), raw));
                tree::TreeNode {
                    name: n.name.clone(),
                    node_type: n.node_type.clone().unwrap_or_default(),
                    credentials: n
                        .credentials
                        .iter()
                        .filter_map(|c| {
                            c.credential_name
                                .clone()
                                .or_else(|| Some(c.credential_type.clone()))
                        })
                        .collect(),
                    disabled: n.disabled.unwrap_or(false),
                    detail,
                }
            })
            .collect();
        let tree_conns: Vec<(String, String, String, usize)> = connections
            .iter()
            .map(|c| (c.from.clone(), c.to.clone(), c.kind.clone(), c.output_index))
            .collect();
        Some((tree_nodes, tree_conns))
    } else {
        None
    };

    if context.json {
        let mut data = json!({
            "workflow_path": file,
            "workflow_id": workflow_id(&workflow),
            "name": workflow_name(&workflow),
            "active": workflow_active(&workflow),
            "instance": instance,
            "node_count": nodes.len(),
            "connection_count": connections.len(),
            "credential_count": credentials.len(),
            "nodes": nodes,
            "connections": connections,
            "credentials": credentials,
            "webhooks": webhooks,
        });
        if let Some((tree_nodes, tree_conns)) = &tree_data {
            data.as_object_mut().unwrap().insert(
                "tree".to_string(),
                serde_json::to_value(tree::build_tree_data(tree_nodes, tree_conns)).unwrap(),
            );
        }
        emit_json("workflow", &data)
    } else {
        println!(
            "Workflow: {}",
            workflow_name(&workflow).unwrap_or_else(|| "<unnamed>".to_string())
        );
        println!("File: {}", file.display());
        if let Some(wf_id) = workflow_id(&workflow) {
            println!("ID: {wf_id}");
        }
        println!("Active: {}", workflow_active(&workflow).unwrap_or(false));
        if let Some(instance) = &instance {
            println!("Instance: {instance}");
        }
        if let Some((tree_nodes, tree_conns)) = &tree_data {
            let colorize = !args.no_color
                && std::env::var_os("NO_COLOR").is_none()
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            println!();
            println!("{}", tree::render_tree(tree_nodes, tree_conns, colorize));
        } else {
            print_workflow_nodes(&nodes);
            print_workflow_connections(&connections);
        }
        print_credential_references(&credentials);
        print_workflow_webhooks(&webhooks);
        Ok(())
    }
}

async fn cmd_workflow_remove(context: &Context, args: WorkflowRemoveArgs) -> Result<(), AppError> {
    let target_path = resolve_existing_workflow_path(context, &args.target);
    let result = if let Some(path) = target_path {
        remove_workflow_by_path(context, &args, &path).await?
    } else {
        remove_workflow_by_identifier(context, &args).await?
    };

    if context.json {
        emit_json("workflow", &result)
    } else {
        if result.remote_removed {
            print_message(
                context,
                &format!(
                    "Deleted remote workflow {}{}.",
                    result
                        .workflow_id
                        .as_deref()
                        .unwrap_or(result.target.as_str()),
                    result
                        .workflow_name
                        .as_deref()
                        .map(|name| format!(" ({name})"))
                        .unwrap_or_default()
                ),
            );
        }
        if result.local_removed {
            print_message(context, "Removed local artifacts:");
            for path in &result.removed_paths {
                print_message(context, &format!("  {}", path.display()));
            }
        } else if args.keep_local {
            print_message(context, "Kept local artifacts.");
        } else if !result.remote_removed {
            print_message(context, "Removed local workflow file.");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_workflow_show_instance(
    context: &Context,
    file: &Path,
    explicit_alias: Option<&str>,
) -> Result<Option<String>, AppError> {
    if let Some(alias) = explicit_alias {
        return Ok(Some(alias.to_string()));
    }

    let meta_path = sidecar_path_for(file);
    if meta_path.exists() {
        let meta = load_meta(&meta_path, "workflow")?;
        return Ok(Some(meta.instance));
    }

    if let Ok(repo) = load_repo(context.repo_root.as_deref()) {
        return Ok(Some(repo.config.default_instance));
    }

    Ok(None)
}

fn resolve_instance_base_url(
    context: &Context,
    alias: Option<&str>,
) -> Result<Option<String>, AppError> {
    let Some(alias) = alias else {
        return Ok(None);
    };
    let repo = load_loaded_repo(context)?;
    let instance = repo.config.instances.get(alias).ok_or_else(|| {
        AppError::config("workflow", format!("Unknown instance alias `{alias}`."))
    })?;
    Ok(Some(instance.base_url.clone()))
}

async fn remove_workflow_by_path(
    context: &Context,
    args: &WorkflowRemoveArgs,
    path: &Path,
) -> Result<WorkflowRemoveResult, AppError> {
    let workflow = canonicalize_workflow(&load_workflow_file(path, "workflow")?)?;
    let meta_path = sidecar_path_for(path);
    let meta = if meta_path.exists() {
        Some(load_meta(&meta_path, "workflow")?)
    } else {
        None
    };
    let wf_id = meta
        .as_ref()
        .map(|meta| meta.workflow_id.clone())
        .or_else(|| workflow_id(&workflow));
    let wf_name = workflow_name(&workflow);
    let instance = args
        .remote
        .instance
        .clone()
        .or_else(|| meta.as_ref().map(|meta| meta.instance.clone()));

    let remote_removed = if args.local_only || meta.is_none() && args.remote.instance.is_none() {
        false
    } else {
        let wf_id = wf_id.clone().ok_or_else(|| {
            AppError::validation(
                "workflow",
                format!(
                    "Cannot delete remote workflow for {} because the local file is missing `id`.",
                    path.display()
                ),
            )
        })?;
        let repo = load_loaded_repo(context)?;
        let (client, _, _) = remote_client(&repo, instance.as_deref(), "workflow")?;
        client.delete_workflow(&wf_id).await?;
        true
    };

    let removed_paths = if args.keep_local {
        Vec::new()
    } else {
        remove_local_workflow_artifacts(
            context.repo_root.as_deref(),
            path,
            meta.as_ref().map(|meta| meta.instance.as_str()),
            wf_id.as_deref(),
        )?
    };

    Ok(WorkflowRemoveResult {
        target: path.display().to_string(),
        workflow_id: wf_id,
        workflow_name: wf_name,
        instance,
        remote_removed,
        local_removed: !removed_paths.is_empty(),
        removed_paths,
    })
}

async fn remove_workflow_by_identifier(
    context: &Context,
    args: &WorkflowRemoveArgs,
) -> Result<WorkflowRemoveResult, AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "workflow")?;
    let (client, _, _) = remote_client(&repo, Some(&alias), "workflow")?;
    let workflow = client.resolve_workflow(&args.target).await?;
    let wf_id = workflow_id(&workflow).ok_or_else(|| {
        AppError::api(
            "workflow",
            "api.invalid_response",
            "Workflow payload was missing `id`.",
        )
    })?;
    let wf_name = workflow_name(&workflow);
    client.delete_workflow(&wf_id).await?;

    let removed_paths = if args.keep_local {
        Vec::new()
    } else if let Some(path) = find_existing_workflow_path(&repo, &wf_id) {
        let meta_path = sidecar_path_for(&path);
        let meta_instance = if meta_path.exists() {
            Some(load_meta(&meta_path, "workflow")?.instance)
        } else {
            Some(alias.clone())
        };
        remove_local_workflow_artifacts(
            Some(&repo.root),
            &path,
            meta_instance.as_deref(),
            Some(&wf_id),
        )?
    } else {
        Vec::new()
    };

    Ok(WorkflowRemoveResult {
        target: args.target.clone(),
        workflow_id: Some(wf_id),
        workflow_name: wf_name,
        instance: Some(alias),
        remote_removed: true,
        local_removed: !removed_paths.is_empty(),
        removed_paths,
    })
}

fn remove_local_workflow_artifacts(
    repo_root: Option<&Path>,
    workflow_path: &Path,
    instance: Option<&str>,
    wf_id: Option<&str>,
) -> Result<Vec<PathBuf>, AppError> {
    let mut removed = Vec::new();

    if workflow_path.exists() {
        fs::remove_file(workflow_path).map_err(|err| {
            AppError::validation(
                "workflow",
                format!("Failed to remove {}: {err}", workflow_path.display()),
            )
        })?;
        removed.push(workflow_path.to_path_buf());
    }

    let meta_path = sidecar_path_for(workflow_path);
    if meta_path.exists() {
        fs::remove_file(&meta_path).map_err(|err| {
            AppError::validation(
                "workflow",
                format!("Failed to remove {}: {err}", meta_path.display()),
            )
        })?;
        removed.push(meta_path);
    }

    if let (Some(root), Some(instance), Some(wf_id)) = (repo_root, instance, wf_id) {
        let cache_path = cache_snapshot_path(root, instance, wf_id);
        if cache_path.exists() {
            fs::remove_file(&cache_path).map_err(|err| {
                AppError::validation(
                    "workflow",
                    format!("Failed to remove {}: {err}", cache_path.display()),
                )
            })?;
            removed.push(cache_path);
        }
    }

    Ok(removed)
}

pub(crate) fn summarize_workflow_nodes(workflow: &Value) -> Vec<WorkflowNodeRow> {
    let mut rows = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|node| WorkflowNodeRow {
            name: node
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>")
                .to_string(),
            node_id: value_string(node, "id"),
            node_type: value_string(node, "type"),
            type_version: node.get("typeVersion").and_then(Value::as_f64),
            position: node.get("position").and_then(parse_position),
            disabled: node.get("disabled").and_then(Value::as_bool),
            credentials: summarize_node_credentials(node),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

pub(crate) fn summarize_node_credentials(node: &Value) -> Vec<NodeCredentialRow> {
    let mut rows = node
        .get("credentials")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(credential_type, credential)| {
            let credential = credential.as_object()?;
            Some(NodeCredentialRow {
                credential_type: credential_type.clone(),
                credential_id: credential
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                credential_name: credential
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (
            left.credential_type.as_str(),
            left.credential_id.as_deref().unwrap_or(""),
            left.credential_name.as_deref().unwrap_or(""),
        )
            .cmp(&(
                right.credential_type.as_str(),
                right.credential_id.as_deref().unwrap_or(""),
                right.credential_name.as_deref().unwrap_or(""),
            ))
    });
    rows
}

/// Extract a key parameter summary from a raw workflow node JSON value.
/// Returns a short string for display in tree output: webhook path, HTTP method+URL, set field count.
pub(crate) fn extract_node_detail(node_type: &str, raw_node: &Value) -> Option<String> {
    let params = raw_node.get("parameters")?;
    match node_type {
        "n8n-nodes-base.webhook" => {
            let path = params.get("path").and_then(Value::as_str)?;
            Some(format!("path=/{path}"))
        }
        "n8n-nodes-base.httpRequest" => {
            let method = params
                .get("method")
                .or_else(|| params.get("requestMethod"))
                .and_then(Value::as_str)
                .unwrap_or("GET");
            let url = params.get("url").and_then(Value::as_str).unwrap_or("?");
            Some(format!("{method} {url}"))
        }
        "n8n-nodes-base.set" => {
            let count = params
                .get("assignments")
                .and_then(|a| a.get("assignments"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .or_else(|| {
                    params.get("values").and_then(|v| {
                        let obj = v.as_object()?;
                        let total: usize =
                            obj.values().filter_map(Value::as_array).map(Vec::len).sum();
                        Some(total)
                    })
                });
            count.map(|c| {
                if c == 1 {
                    "1 field".to_string()
                } else {
                    format!("{c} fields")
                }
            })
        }
        _ => None,
    }
}

pub(crate) fn summarize_credential_references(
    workflows: &[Value],
    credential_type_filter: Option<&str>,
) -> Vec<CredentialReferenceRow> {
    let mut entries = BTreeMap::<
        (String, Option<String>, Option<String>),
        BTreeMap<(Option<String>, Option<String>), std::collections::BTreeSet<String>>,
    >::new();

    for workflow in workflows {
        let wf_id = workflow_id(workflow);
        let wf_name = workflow_name(workflow);
        for node in workflow
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let node_name = node
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>")
                .to_string();
            for credential in summarize_node_credentials(node) {
                if credential_type_filter.is_some_and(|filter| credential.credential_type != filter)
                {
                    continue;
                }
                entries
                    .entry((
                        credential.credential_type.clone(),
                        credential.credential_id.clone(),
                        credential.credential_name.clone(),
                    ))
                    .or_default()
                    .entry((wf_id.clone(), wf_name.clone()))
                    .or_default()
                    .insert(node_name.clone());
            }
        }
    }

    let mut rows = entries
        .into_iter()
        .map(
            |((credential_type, credential_id, credential_name), workflow_map)| {
                let mut usage_count = 0usize;
                let mut workflows = workflow_map
                    .into_iter()
                    .map(|((wf_id, wf_name), nodes)| {
                        let mut nodes = nodes.into_iter().collect::<Vec<_>>();
                        nodes.sort();
                        usage_count += nodes.len();
                        CredentialWorkflowUsageRow {
                            workflow_id: wf_id,
                            workflow_name: wf_name,
                            nodes,
                        }
                    })
                    .collect::<Vec<_>>();
                workflows.sort_by(|left, right| {
                    (
                        left.workflow_name.as_deref().unwrap_or(""),
                        left.workflow_id.as_deref().unwrap_or(""),
                    )
                        .cmp(&(
                            right.workflow_name.as_deref().unwrap_or(""),
                            right.workflow_id.as_deref().unwrap_or(""),
                        ))
                });

                CredentialReferenceRow {
                    credential_type,
                    credential_id,
                    credential_name,
                    usage_count,
                    workflow_count: workflows.len(),
                    workflows,
                }
            },
        )
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (
            left.credential_type.as_str(),
            left.credential_name.as_deref().unwrap_or(""),
            left.credential_id.as_deref().unwrap_or(""),
        )
            .cmp(&(
                right.credential_type.as_str(),
                right.credential_name.as_deref().unwrap_or(""),
                right.credential_id.as_deref().unwrap_or(""),
            ))
    });
    rows
}

pub(crate) fn summarize_workflow_connections(workflow: &Value) -> Vec<WorkflowConnectionRow> {
    let mut rows = Vec::new();
    let Some(connections) = workflow.get("connections").and_then(Value::as_object) else {
        return rows;
    };

    for (from, kinds) in connections {
        let Some(kinds) = kinds.as_object() else {
            continue;
        };
        for (kind, branches) in kinds {
            let Some(branches) = branches.as_array() else {
                continue;
            };
            for (output_index, branch) in branches.iter().enumerate() {
                let Some(entries) = branch.as_array() else {
                    continue;
                };
                for entry in entries {
                    let Some(to) = entry.get("node").and_then(Value::as_str) else {
                        continue;
                    };
                    rows.push(WorkflowConnectionRow {
                        from: from.clone(),
                        kind: kind.clone(),
                        output_index,
                        to: to.to_string(),
                        target_kind: value_string(entry, "type"),
                        input_index: entry
                            .get("index")
                            .and_then(Value::as_u64)
                            .map(|value| value as usize),
                    });
                }
            }
        }
    }

    rows.sort_by(|left, right| {
        (
            left.from.as_str(),
            left.kind.as_str(),
            left.output_index,
            left.to.as_str(),
            left.input_index.unwrap_or(usize::MAX),
        )
            .cmp(&(
                right.from.as_str(),
                right.kind.as_str(),
                right.output_index,
                right.to.as_str(),
                right.input_index.unwrap_or(usize::MAX),
            ))
    });
    rows
}

pub(crate) fn summarize_workflow_webhooks(
    workflow: &Value,
    base_url: Option<&str>,
) -> Vec<WorkflowWebhookRow> {
    let mut rows = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|node| node.get("type").and_then(Value::as_str) == Some(WEBHOOK_NODE_TYPE))
        .map(|node| {
            let node_name = node
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>")
                .to_string();
            let path = node
                .get("parameters")
                .and_then(Value::as_object)
                .and_then(|parameters| parameters.get("path"))
                .and_then(Value::as_str)
                .map(normalize_webhook_path)
                .filter(|path| !path.is_empty());
            let methods = webhook_methods(node);
            let production_url = path
                .as_deref()
                .zip(base_url)
                .map(|(path, base)| format!("{}/webhook/{}", base.trim_end_matches('/'), path));
            let test_url = path.as_deref().zip(base_url).map(|(path, base)| {
                format!("{}/webhook-test/{}", base.trim_end_matches('/'), path)
            });

            WorkflowWebhookRow {
                node: node_name,
                methods,
                path,
                webhook_id: value_string(node, "webhookId"),
                production_url,
                test_url,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.node.cmp(&right.node));
    rows
}

fn parse_position(value: &Value) -> Option<Vec<i64>> {
    let position = value.as_array()?;
    let coords = position
        .iter()
        .map(|entry| entry.as_i64())
        .collect::<Option<Vec<_>>>()?;
    if coords.is_empty() {
        None
    } else {
        Some(coords)
    }
}

fn webhook_methods(node: &Value) -> Vec<String> {
    let Some(parameters) = node.get("parameters").and_then(Value::as_object) else {
        return Vec::new();
    };
    if let Some(method) = parameters.get("httpMethod").and_then(Value::as_str) {
        return vec![method.to_string()];
    }
    parameters
        .get("httpMethods")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

pub(crate) fn print_workflow_nodes(rows: &[WorkflowNodeRow]) {
    if rows.is_empty() {
        println!("Nodes: none");
        return;
    }

    println!("Nodes:");
    println!(
        "{:<24} {:<28} {:<10} {:<14} {:<8} CREDS",
        "NAME", "TYPE", "VERSION", "POSITION", "DISABLED"
    );
    for row in rows {
        let position = row
            .position
            .as_ref()
            .map(|coords| {
                coords
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|| "-".to_string());
        let version = row
            .type_version
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let credentials = if row.credentials.is_empty() {
            "-".to_string()
        } else {
            row.credentials
                .iter()
                .map(format_node_credential)
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "{:<24} {:<28} {:<10} {:<14} {:<8} {}",
            truncate(&row.name, 24),
            truncate(row.node_type.as_deref().unwrap_or("-"), 28),
            truncate(&version, 10),
            truncate(&position, 14),
            row.disabled.unwrap_or(false),
            credentials
        );
    }
}

fn format_node_credential(row: &NodeCredentialRow) -> String {
    match (row.credential_id.as_deref(), row.credential_name.as_deref()) {
        (Some(id), Some(name)) => format!("{}:{id} ({name})", row.credential_type),
        (Some(id), None) => format!("{}:{id}", row.credential_type),
        (None, Some(name)) => format!("{} ({name})", row.credential_type),
        (None, None) => row.credential_type.clone(),
    }
}

pub(crate) fn print_credential_references(rows: &[CredentialReferenceRow]) {
    if rows.is_empty() {
        return;
    }

    println!("Credentials:");
    println!(
        "{:<24} {:<18} {:<28} {:<8} WORKFLOWS",
        "TYPE", "ID", "NAME", "USES"
    );
    for row in rows {
        let workflows = row
            .workflows
            .iter()
            .map(|usage| {
                let name = usage
                    .workflow_name
                    .as_deref()
                    .or(usage.workflow_id.as_deref())
                    .unwrap_or("<unknown>");
                if usage.nodes.is_empty() {
                    name.to_string()
                } else {
                    format!("{name} [{}]", usage.nodes.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        println!(
            "{:<24} {:<18} {:<28} {:<8} {}",
            truncate(&row.credential_type, 24),
            truncate(row.credential_id.as_deref().unwrap_or("-"), 18),
            truncate(row.credential_name.as_deref().unwrap_or("-"), 28),
            row.usage_count,
            workflows
        );
    }
}

pub(crate) fn print_workflow_connections(rows: &[WorkflowConnectionRow]) {
    if rows.is_empty() {
        println!("Connections: none");
        return;
    }

    println!("Connections:");
    println!(
        "{:<24} {:<10} {:<6} {:<24} {:<12} IN",
        "FROM", "KIND", "OUT", "TO", "TARGET"
    );
    for row in rows {
        println!(
            "{:<24} {:<10} {:<6} {:<24} {:<12} {}",
            truncate(&row.from, 24),
            truncate(&row.kind, 10),
            row.output_index,
            truncate(&row.to, 24),
            truncate(row.target_kind.as_deref().unwrap_or("-"), 12),
            row.input_index
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
}

pub(crate) fn print_workflow_webhooks(rows: &[WorkflowWebhookRow]) {
    if rows.is_empty() {
        return;
    }

    println!("Webhooks:");
    for row in rows {
        let methods = if row.methods.is_empty() {
            "-".to_string()
        } else {
            row.methods.join(",")
        };
        println!(
            "  {} [{}] path={} webhook_id={}",
            row.node,
            methods,
            row.path.as_deref().unwrap_or("-"),
            row.webhook_id.as_deref().unwrap_or("-"),
        );
        if let Some(url) = &row.production_url {
            println!("  production: {url}");
        }
        if let Some(url) = &row.test_url {
            println!("  test: {url}");
        }
    }
}
