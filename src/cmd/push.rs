use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    canonical::{canonicalize_workflow, hash_value},
    cli::PushArgs,
    config::{LoadedRepo, resolve_instance_alias},
    error::AppError,
    repo::{
        LocalWorkflowState, StoredWorkflow, load_meta, load_workflow_file, scan_local_status,
        sidecar_path_for, store_workflow, workflow_id,
    },
    validate::sensitive_data_diagnostics,
};

use super::common::{
    Context, absolutize, emit_json, fetch_workflow_required, is_zero, load_loaded_repo,
    print_message, print_sensitive_warning_summary, remote_client, unsupported_push_fields,
    workflow_update_payload,
};

#[derive(Debug, Serialize)]
struct BatchPushResult {
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

#[derive(Debug)]
enum PushOneResult {
    Pushed(Box<StoredWorkflow>),
    Unchanged,
}

pub(crate) async fn cmd_push(context: &Context, args: PushArgs) -> Result<(), AppError> {
    if args.all {
        return cmd_push_all(context, args).await;
    }
    let file = args
        .file
        .ok_or_else(|| AppError::usage("push", "Provide a workflow file or use --all."))?;

    let repo = load_loaded_repo(context)?;
    let workflow_path = absolutize(&repo.root, &file);
    let meta_path = sidecar_path_for(&workflow_path);
    let workflow = load_workflow_file(&workflow_path, "push")?;
    let canonical = canonicalize_workflow(&workflow)?;
    let local_id = workflow_id(&canonical)
        .ok_or_else(|| AppError::validation("push", "Workflow file is missing `id`."))?;
    let meta = load_meta(&meta_path, "push")?;
    if meta.workflow_id != local_id {
        return Err(AppError::validation(
            "push",
            format!(
                "Workflow ID `{local_id}` does not match metadata sidecar ID `{}`.",
                meta.workflow_id
            ),
        ));
    }

    let alias = resolve_instance_alias(
        &repo,
        args.remote.instance.as_deref().or(Some(&meta.instance)),
        "push",
    )?;
    if alias != meta.instance {
        return Err(AppError::config(
            "push",
            format!(
                "Workflow is tracked against `{}` but push was requested for `{alias}`.",
                meta.instance
            ),
        ));
    }

    let (client, _, _) = remote_client(&repo, Some(&alias), "push")?;
    let remote = client
        .get_workflow_by_id(&meta.workflow_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(
                "push",
                format!("Remote workflow `{}` was not found.", meta.workflow_id),
            )
        })?;
    let remote_workflow = remote.get("data").cloned().unwrap_or(remote);
    let remote_canonical = canonicalize_workflow(&remote_workflow)?;
    let remote_hash = hash_value(&remote_canonical)?;
    let local_hash = hash_value(&canonical)?;
    let unsupported_changes = unsupported_push_fields(&canonical, &remote_canonical);

    if remote_hash != meta.remote_hash {
        return Err(AppError::conflict(
            "push",
            format!(
                "Remote workflow changed since the last pull. local={}, recorded={}, remote={}",
                local_hash, meta.remote_hash, remote_hash
            ),
        )
        .with_suggestion("Run `n8nc pull <id>` again before pushing."));
    }

    if local_hash == meta.remote_hash {
        if context.json {
            return emit_json(
                "push",
                &json!({"workflow_id": meta.workflow_id, "changed": false}),
            );
        }
        print_message(
            context,
            &format!("No changes to push for {}.", meta.workflow_id),
        );
        return Ok(());
    }

    if !unsupported_changes.is_empty() {
        return Err(AppError::validation(
            "push",
            format!(
                "`push` only updates `name`, `nodes`, `connections`, and `settings`. Local changes also modified unsupported field(s): {}.",
                unsupported_changes.join(", ")
            ),
        )
        .with_suggestion(
            "Use `activate`/`deactivate` for workflow state changes, or re-pull after editing unsupported fields in n8n.",
        ));
    }

    let payload = workflow_update_payload(&workflow)?;
    client.update_workflow(&meta.workflow_id, &payload).await?;
    let updated = fetch_workflow_required(
        &client,
        &meta.workflow_id,
        "push",
        "could not be re-fetched after push",
    )
    .await?;
    let stored = store_workflow(&repo, &alias, &updated)?;
    let warnings = sensitive_data_diagnostics(&stored.workflow_path)?;
    let warning_count = warnings.len();

    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("workflow_id".to_string(), json!(meta.workflow_id));
        data.insert("changed".to_string(), json!(true));
        data.insert("workflow_path".to_string(), json!(stored.workflow_path));
        data.insert("meta_path".to_string(), json!(stored.meta_path));
        data.insert("warning_count".to_string(), json!(warning_count));
        if warning_count > 0 {
            data.insert("diagnostics".to_string(), json!(warnings));
        }
        emit_json("push", &Value::Object(data))
    } else {
        print_message(context, &format!("Pushed {}.", meta.workflow_id));
        print_message(
            context,
            &format!("Updated local file: {}", stored.workflow_path.display()),
        );
        print_sensitive_warning_summary(&stored.workflow_path, warning_count);
        Ok(())
    }
}

async fn cmd_push_all(context: &Context, args: PushArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let statuses = scan_local_status(&repo, &[])?;

    let mut results: Vec<BatchPushResult> = Vec::new();
    let mut pushed_count: usize = 0;
    let mut unchanged_count: usize = 0;
    let mut skipped_count: usize = 0;
    let mut failed_count: usize = 0;
    let mut total_warning_count: usize = 0;

    for entry in &statuses {
        let wf_id = entry.workflow_id.clone().unwrap_or_default();
        let wf_name = entry
            .name
            .clone()
            .unwrap_or_else(|| "<unnamed>".to_string());

        match entry.state {
            LocalWorkflowState::Modified => {}
            LocalWorkflowState::Clean => {
                print_message(
                    context,
                    &format!("Unchanged {} ({})", wf_id, entry.file.display()),
                );
                results.push(BatchPushResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "unchanged",
                    workflow_path: Some(entry.file.clone()),
                    meta_path: entry.sidecar.clone(),
                    warning_count: 0,
                    diagnostics: None,
                    error: None,
                });
                unchanged_count += 1;
                continue;
            }
            _ => {
                let reason = match entry.state {
                    LocalWorkflowState::Untracked => "untracked",
                    LocalWorkflowState::Invalid => "invalid",
                    LocalWorkflowState::OrphanedMeta => "orphaned metadata",
                    _ => "unknown",
                };
                print_message(
                    context,
                    &format!("Skipped {} ({}) — {}", wf_id, entry.file.display(), reason),
                );
                results.push(BatchPushResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "skipped",
                    workflow_path: Some(entry.file.clone()),
                    meta_path: None,
                    warning_count: 0,
                    diagnostics: None,
                    error: Some(format!("Workflow is {reason}")),
                });
                skipped_count += 1;
                continue;
            }
        }

        let meta_path = match &entry.sidecar {
            Some(path) => path.clone(),
            None => {
                print_message(context, &format!("Skipped {wf_id} — missing sidecar"));
                results.push(BatchPushResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "skipped",
                    workflow_path: Some(entry.file.clone()),
                    meta_path: None,
                    warning_count: 0,
                    diagnostics: None,
                    error: Some("Missing sidecar metadata".to_string()),
                });
                skipped_count += 1;
                continue;
            }
        };

        match push_one_workflow(
            &repo,
            &entry.file,
            &meta_path,
            args.remote.instance.as_deref(),
        )
        .await
        {
            Ok(PushOneResult::Pushed(stored)) => {
                let warnings =
                    sensitive_data_diagnostics(&stored.workflow_path).unwrap_or_default();
                let wc = warnings.len();
                total_warning_count += wc;

                print_message(
                    context,
                    &format!("Pushed {} -> {}", wf_id, stored.workflow_path.display()),
                );
                print_sensitive_warning_summary(&stored.workflow_path, wc);

                results.push(BatchPushResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "pushed",
                    workflow_path: Some(stored.workflow_path),
                    meta_path: Some(stored.meta_path),
                    warning_count: wc,
                    diagnostics: if wc > 0 { Some(json!(warnings)) } else { None },
                    error: None,
                });
                pushed_count += 1;
            }
            Ok(PushOneResult::Unchanged) => {
                print_message(
                    context,
                    &format!("Unchanged {} ({})", wf_id, entry.file.display()),
                );
                results.push(BatchPushResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "unchanged",
                    workflow_path: Some(entry.file.clone()),
                    meta_path: entry.sidecar.clone(),
                    warning_count: 0,
                    diagnostics: None,
                    error: None,
                });
                unchanged_count += 1;
            }
            Err(err) => {
                print_message(context, &format!("Failed {}: {}", wf_id, err.message));
                results.push(BatchPushResult {
                    workflow_id: wf_id,
                    name: wf_name,
                    status: "failed",
                    workflow_path: Some(entry.file.clone()),
                    meta_path: entry.sidecar.clone(),
                    warning_count: 0,
                    diagnostics: None,
                    error: Some(err.message),
                });
                failed_count += 1;
            }
        }
    }

    print_message(context, "---");
    print_message(
        context,
        &format!(
            "Pushed: {pushed_count}, Unchanged: {unchanged_count}, Skipped: {skipped_count}, Failed: {failed_count}"
        ),
    );

    let data = json!({
        "total": results.len(),
        "pushed": pushed_count,
        "unchanged": unchanged_count,
        "skipped": skipped_count,
        "failed": failed_count,
        "warning_count": total_warning_count,
        "results": results,
    });

    if failed_count > 0 {
        return Err(AppError::api(
            "push",
            "push.partial_failure",
            format!(
                "{failed_count} of {} workflow(s) failed to push.",
                results.len()
            ),
        )
        .with_json_data(data));
    }

    if context.json {
        emit_json("push", &data)
    } else {
        Ok(())
    }
}

async fn push_one_workflow(
    repo: &LoadedRepo,
    workflow_path: &Path,
    meta_path: &Path,
    instance_override: Option<&str>,
) -> Result<PushOneResult, AppError> {
    let workflow = load_workflow_file(workflow_path, "push")?;
    let canonical = canonicalize_workflow(&workflow)?;
    let local_id = workflow_id(&canonical)
        .ok_or_else(|| AppError::validation("push", "Workflow file is missing `id`."))?;
    let meta = load_meta(meta_path, "push")?;

    if meta.workflow_id != local_id {
        return Err(AppError::validation(
            "push",
            format!(
                "Workflow ID `{local_id}` does not match metadata sidecar ID `{}`.",
                meta.workflow_id
            ),
        ));
    }

    let alias = resolve_instance_alias(repo, instance_override.or(Some(&meta.instance)), "push")?;
    if alias != meta.instance {
        return Err(AppError::config(
            "push",
            format!(
                "Workflow is tracked against `{}` but push was requested for `{alias}`.",
                meta.instance
            ),
        ));
    }

    let (client, _, _) = remote_client(repo, Some(&alias), "push")?;
    let remote = client
        .get_workflow_by_id(&meta.workflow_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(
                "push",
                format!("Remote workflow `{}` was not found.", meta.workflow_id),
            )
        })?;
    let remote_workflow = remote.get("data").cloned().unwrap_or(remote);
    let remote_canonical = canonicalize_workflow(&remote_workflow)?;
    let remote_hash = hash_value(&remote_canonical)?;
    let local_hash = hash_value(&canonical)?;
    let unsupported_changes = unsupported_push_fields(&canonical, &remote_canonical);

    if remote_hash != meta.remote_hash {
        return Err(AppError::conflict(
            "push",
            format!(
                "Remote workflow changed since the last pull. local={}, recorded={}, remote={}",
                local_hash, meta.remote_hash, remote_hash
            ),
        )
        .with_suggestion("Run `n8nc pull <id>` again before pushing."));
    }

    if local_hash == meta.remote_hash {
        return Ok(PushOneResult::Unchanged);
    }

    if !unsupported_changes.is_empty() {
        return Err(AppError::validation(
            "push",
            format!(
                "`push` only updates `name`, `nodes`, `connections`, and `settings`. Local changes also modified unsupported field(s): {}.",
                unsupported_changes.join(", ")
            ),
        )
        .with_suggestion(
            "Use `activate`/`deactivate` for workflow state changes, or re-pull after editing unsupported fields in n8n.",
        ));
    }

    let payload = workflow_update_payload(&workflow)?;
    client.update_workflow(&meta.workflow_id, &payload).await?;
    let updated = fetch_workflow_required(
        &client,
        &meta.workflow_id,
        "push",
        "could not be re-fetched after push",
    )
    .await?;
    let stored = store_workflow(repo, &alias, &updated)?;
    Ok(PushOneResult::Pushed(Box::new(stored)))
}
