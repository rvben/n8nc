use std::{collections::BTreeSet, fs, path::PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    api::{ApiClient, ListOptions},
    canonical::{canonicalize_workflow, hash_value},
    cli::PullArgs,
    config::{LoadedRepo, resolve_instance_alias},
    error::AppError,
    repo::{
        LocalWorkflowState, StoredWorkflow, cache_snapshot_path, find_existing_workflow_path,
        load_meta, scan_local_status, sidecar_path_for, store_workflow, workflow_id, workflow_name,
    },
    validate::sensitive_data_diagnostics,
};

use super::common::{
    Context, emit_json, is_zero, load_loaded_repo, print_sensitive_warning_summary, remote_client,
};

#[derive(Debug, Serialize)]
struct BatchPullResult {
    workflow_id: String,
    name: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "is_zero")]
    warning_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct BatchPruneResult {
    workflow_id: String,
    name: String,
    instance: String,
    workflow_path: PathBuf,
}

#[derive(Debug)]
enum PullOneResult {
    Pulled(StoredWorkflow),
    Unchanged(PathBuf),
}

pub(crate) async fn cmd_pull(context: &Context, args: PullArgs) -> Result<(), AppError> {
    if args.all {
        return cmd_pull_all(context, args).await;
    }

    let identifier = args
        .identifier
        .ok_or_else(|| AppError::usage("pull", "Provide a workflow identifier or use --all."))?;

    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "pull")?;
    let (client, _, _) = remote_client(&repo, Some(&alias), "pull")?;
    let workflow = client.resolve_workflow(&identifier).await?;
    let stored = store_workflow(&repo, &alias, &workflow)?;
    let warnings = sensitive_data_diagnostics(&stored.workflow_path)?;
    let warning_count = warnings.len();

    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("instance".to_string(), json!(alias));
        data.insert("workflow_path".to_string(), json!(stored.workflow_path));
        data.insert("meta_path".to_string(), json!(stored.meta_path));
        data.insert("workflow_id".to_string(), json!(stored.meta.workflow_id));
        data.insert("warning_count".to_string(), json!(warning_count));
        if warning_count > 0 {
            data.insert("diagnostics".to_string(), json!(warnings));
        }
        emit_json("pull", &Value::Object(data))
    } else {
        println!(
            "Pulled {} -> {}",
            stored.meta.workflow_id,
            stored.workflow_path.display()
        );
        println!("Metadata: {}", stored.meta_path.display());
        print_sensitive_warning_summary(&stored.workflow_path, warning_count);
        Ok(())
    }
}

async fn cmd_pull_all(context: &Context, args: PullArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "pull")?;
    let (client, _, _) = remote_client(&repo, Some(&alias), "pull")?;

    let active_filter = if args.active {
        Some(true)
    } else if args.inactive {
        Some(false)
    } else {
        None
    };

    let workflows = client
        .list_workflows(&ListOptions {
            limit: 250,
            active: active_filter,
            name_filter: None,
        })
        .await?;

    let mut results: Vec<BatchPullResult> = Vec::new();
    let mut pulled_count: usize = 0;
    let mut unchanged_count: usize = 0;
    let mut failed_count: usize = 0;
    let mut total_warning_count: usize = 0;

    for list_entry in &workflows {
        let wf_id = workflow_id(list_entry).unwrap_or_default();
        let wf_name = workflow_name(list_entry).unwrap_or_else(|| "<unnamed>".to_string());

        match pull_one_workflow(&repo, &client, &alias, &wf_id).await {
            Ok(PullOneResult::Pulled(stored)) => {
                let warnings =
                    sensitive_data_diagnostics(&stored.workflow_path).unwrap_or_default();
                let wc = warnings.len();
                total_warning_count += wc;

                if !context.json {
                    println!("Pulled {} -> {}", wf_id, stored.workflow_path.display());
                    print_sensitive_warning_summary(&stored.workflow_path, wc);
                }

                results.push(BatchPullResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "pulled",
                    workflow_path: Some(stored.workflow_path),
                    meta_path: Some(stored.meta_path),
                    warning_count: wc,
                    diagnostics: if wc > 0 { Some(json!(warnings)) } else { None },
                    error: None,
                });
                pulled_count += 1;
            }
            Ok(PullOneResult::Unchanged(path)) => {
                if !context.json {
                    println!("Unchanged {} ({})", wf_id, path.display());
                }

                results.push(BatchPullResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "unchanged",
                    workflow_path: Some(path),
                    meta_path: None,
                    warning_count: 0,
                    diagnostics: None,
                    error: None,
                });
                unchanged_count += 1;
            }
            Err(err) => {
                if !context.json {
                    println!("Failed {}: {}", wf_id, err.message);
                }

                results.push(BatchPullResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "failed",
                    workflow_path: None,
                    meta_path: None,
                    warning_count: 0,
                    diagnostics: None,
                    error: Some(err.message),
                });
                failed_count += 1;
            }
        }
    }

    // Prune local tracked workflows that no longer exist on the remote
    let mut pruned_results: Vec<BatchPruneResult> = Vec::new();
    if args.prune {
        let remote_ids: BTreeSet<String> = workflows.iter().filter_map(workflow_id).collect();

        let local_statuses = scan_local_status(&repo, &[])?;
        for entry in &local_statuses {
            if entry.state == LocalWorkflowState::Untracked
                || entry.state == LocalWorkflowState::OrphanedMeta
            {
                continue;
            }

            let Some(ref wf_id) = entry.workflow_id else {
                continue;
            };
            let Some(ref instance) = entry.instance else {
                continue;
            };

            if instance != &alias {
                continue;
            }

            if remote_ids.contains(wf_id) {
                continue;
            }

            let wf_name = entry
                .name
                .clone()
                .unwrap_or_else(|| "<unnamed>".to_string());
            let workflow_path = entry.file.clone();
            let meta_path = sidecar_path_for(&workflow_path);
            let cache_path = cache_snapshot_path(&repo.root, &alias, wf_id);

            let _ = fs::remove_file(&workflow_path);
            let _ = fs::remove_file(&meta_path);
            let _ = fs::remove_file(&cache_path);

            if !context.json {
                println!("Pruned {} ({})", wf_id, workflow_path.display());
            }

            pruned_results.push(BatchPruneResult {
                workflow_id: wf_id.clone(),
                name: wf_name,
                instance: alias.clone(),
                workflow_path,
            });
        }
    }
    let pruned_count = pruned_results.len();

    if !context.json {
        println!("---");
        println!(
            "Pulled: {}, Unchanged: {}, Failed: {}, Pruned: {}",
            pulled_count, unchanged_count, failed_count, pruned_count
        );
    }

    let data = json!({
        "instance": alias,
        "total": results.len(),
        "pulled": pulled_count,
        "unchanged": unchanged_count,
        "failed": failed_count,
        "pruned": pruned_count,
        "warning_count": total_warning_count,
        "results": results,
        "pruned_results": pruned_results,
    });

    if failed_count > 0 {
        return Err(AppError::api(
            "pull",
            "pull.partial_failure",
            format!(
                "{failed_count} of {} workflow(s) failed to pull.",
                results.len()
            ),
        )
        .with_json_data(data));
    }

    if context.json {
        emit_json("pull", &data)
    } else {
        Ok(())
    }
}

async fn pull_one_workflow(
    repo: &LoadedRepo,
    client: &ApiClient,
    alias: &str,
    wf_id: &str,
) -> Result<PullOneResult, AppError> {
    let response = client
        .get_workflow_by_id(wf_id)
        .await?
        .ok_or_else(|| AppError::not_found("pull", format!("Workflow `{wf_id}` was not found.")))?;
    let workflow = response.get("data").cloned().unwrap_or(response);
    let canonical = canonicalize_workflow(&workflow)?;
    let remote_hash = hash_value(&canonical)?;

    if let Some(existing_path) = find_existing_workflow_path(repo, wf_id) {
        let meta_path = sidecar_path_for(&existing_path);
        if let Ok(meta) = load_meta(&meta_path, "pull")
            && meta.remote_hash == remote_hash
        {
            return Ok(PullOneResult::Unchanged(existing_path));
        }
    }

    let stored = store_workflow(repo, alias, &workflow)?;
    Ok(PullOneResult::Pulled(stored))
}
