use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::{
    api::ApiClient,
    cli::{DiffArgs, StatusArgs},
    config::LoadedRepo,
    error::AppError,
    repo::{
        LocalWorkflowState, RemoteSyncState, build_local_diff, build_refreshed_diff,
        refresh_local_status, scan_local_status,
    },
};

use super::common::{
    absolutize, client_for_instance, emit_json, load_loaded_repo, truncate, Context,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct StatusSummary {
    clean: usize,
    modified: usize,
    untracked: usize,
    invalid: usize,
    orphaned_meta: usize,
}

#[derive(Debug, Serialize)]
struct SyncSummary {
    clean: usize,
    modified: usize,
    drifted: usize,
    conflict: usize,
    missing_remote: usize,
    unavailable: usize,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_status(context: &Context, args: StatusArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let statuses = scan_local_status(&repo, &args.paths)?;
    let statuses = if args.refresh {
        refresh_statuses(&repo, &statuses, "status").await?
    } else {
        statuses
    };
    let summary = summarize_statuses(&statuses);
    let sync_summary = args.refresh.then(|| summarize_sync_states(&statuses));

    if context.json {
        let mut data = serde_json::Map::new();
        data.insert(
            "summary".to_string(),
            serde_json::to_value(&summary).map_err(|err| {
                AppError::api(
                    "status",
                    "output.serialize_failed",
                    format!("Failed to serialize status summary: {err}"),
                )
            })?,
        );
        if let Some(sync_summary) = &sync_summary {
            data.insert(
                "sync_summary".to_string(),
                serde_json::to_value(sync_summary).map_err(|err| {
                    AppError::api(
                        "status",
                        "output.serialize_failed",
                        format!("Failed to serialize sync summary: {err}"),
                    )
                })?,
            );
        }
        data.insert(
            "workflows".to_string(),
            serde_json::to_value(&statuses).map_err(|err| {
                AppError::api(
                    "status",
                    "output.serialize_failed",
                    format!("Failed to serialize status entries: {err}"),
                )
            })?,
        );
        emit_json("status", &Value::Object(data))
    } else {
        if args.refresh {
            println!(
                "{:<14} {:<14} {:<14} {:<20} {:<20} FILE",
                "LOCAL", "SYNC", "INSTANCE", "ID", "LOCAL HASH"
            );
        } else {
            println!(
                "{:<14} {:<14} {:<20} {:<20} FILE",
                "STATE", "INSTANCE", "ID", "LOCAL HASH"
            );
        }
        for status in &statuses {
            if args.refresh {
                println!(
                    "{:<14} {:<14} {:<14} {:<20} {:<20} {}",
                    local_status_label(status.state),
                    status.sync_state.map(sync_status_label).unwrap_or("-"),
                    status.instance.as_deref().unwrap_or("-"),
                    truncate(status.workflow_id.as_deref().unwrap_or("-"), 20),
                    truncate(status.local_hash.as_deref().unwrap_or("-"), 20),
                    status.file.display(),
                );
            } else {
                println!(
                    "{:<14} {:<14} {:<20} {:<20} {}",
                    local_status_label(status.state),
                    status.instance.as_deref().unwrap_or("-"),
                    truncate(status.workflow_id.as_deref().unwrap_or("-"), 20),
                    truncate(status.local_hash.as_deref().unwrap_or("-"), 20),
                    status.file.display(),
                );
            }
            if let Some(detail) = &status.detail {
                println!("  {}", detail);
            }
            if let Some(detail) = &status.remote_detail {
                println!("  {}", detail);
            }
        }
        println!(
            "Local summary: clean={}, modified={}, untracked={}, invalid={}, orphaned_meta={}",
            summary.clean,
            summary.modified,
            summary.untracked,
            summary.invalid,
            summary.orphaned_meta
        );
        if let Some(sync_summary) = sync_summary {
            println!(
                "Sync summary: clean={}, modified={}, drifted={}, conflict={}, missing_remote={}, unavailable={}",
                sync_summary.clean,
                sync_summary.modified,
                sync_summary.drifted,
                sync_summary.conflict,
                sync_summary.missing_remote,
                sync_summary.unavailable
            );
        }
        Ok(())
    }
}

pub(crate) async fn cmd_diff(context: &Context, args: DiffArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let file = absolutize(&repo.root, &args.file);
    let is_workflow_file = file
        .file_name()
        .and_then(|value| value.to_str())
        .map(|name| name.ends_with(".workflow.json"))
        .unwrap_or(false);
    if !is_workflow_file {
        return Err(AppError::usage(
            "diff",
            "Diff expects a `.workflow.json` file path.",
        ));
    }
    if !file.exists() {
        return Err(AppError::not_found(
            "diff",
            format!("File not found: {}", file.display()),
        ));
    }
    let diff = if args.refresh {
        let local = build_local_diff(&repo, &file)?;
        if !is_refreshable_remote_status(&local.status) {
            local
        } else {
            match (
                local.status.instance.clone(),
                local.status.workflow_id.clone(),
            ) {
                (Some(instance), Some(workflow_id)) => {
                    match client_for_instance(&repo, &instance, "diff", &mut BTreeMap::new()) {
                        Ok(client) => match client.get_workflow_by_id(&workflow_id).await {
                            Ok(remote) => {
                                let remote_workflow = remote
                                    .as_ref()
                                    .map(|value| value.get("data").unwrap_or(value));
                                match build_refreshed_diff("diff", &repo, &file, remote_workflow) {
                                    Ok(diff) => diff,
                                    Err(err) => {
                                        return_diff_with_refresh_error(local, Some(&instance), err)
                                    }
                                }
                            }
                            Err(err) => return_diff_with_refresh_error(local, Some(&instance), err),
                        },
                        Err(err) => return_diff_with_refresh_error(local, Some(&instance), err),
                    }
                }
                (None, _) => with_remote_refresh_unavailable_diff(
                    local,
                    "Remote refresh unavailable: tracked workflow is missing an instance alias."
                        .to_string(),
                ),
                (_, None) => with_remote_refresh_unavailable_diff(
                    local,
                    "Remote refresh unavailable: tracked workflow is missing a workflow ID."
                        .to_string(),
                ),
            }
        }
    } else {
        build_local_diff(&repo, &file)?
    };

    if context.json {
        emit_json("diff", &diff)
    } else {
        println!("Local state: {}", local_status_label(diff.status.state));
        if args.refresh {
            println!(
                "Sync state: {}",
                diff.status.sync_state.map(sync_status_label).unwrap_or("-"),
            );
        }
        println!("File: {}", diff.status.file.display());
        if let Some(workflow_id) = &diff.status.workflow_id {
            println!("Workflow ID: {workflow_id}");
        }
        if let Some(local_hash) = &diff.status.local_hash {
            println!("Local hash: {local_hash}");
        }
        if let Some(recorded_hash) = &diff.status.recorded_hash {
            println!("Recorded hash: {recorded_hash}");
        }
        if let Some(base_hash) = &diff.base_hash {
            println!("Base hash: {base_hash}");
        }
        if let Some(remote_hash) = &diff.status.remote_hash {
            println!("Remote hash: {remote_hash}");
        }
        if let Some(remote_updated_at) = &diff.status.remote_updated_at {
            println!("Remote updated at: {remote_updated_at}");
        }
        if let Some(detail) = &diff.status.detail {
            println!("Detail: {detail}");
        }
        if let Some(detail) = &diff.status.remote_detail {
            println!("Remote detail: {detail}");
        }

        if !diff.changed_sections.is_empty() {
            println!("Base/local sections: {}", diff.changed_sections.join(", "));
        } else if diff.base_snapshot_available {
            println!("Base/local sections: none");
        } else {
            println!("Base/local sections: unavailable (no cached base snapshot)");
        }

        if let Some(patch) = &diff.patch {
            println!("Base vs local:");
            print!("{patch}");
        } else if diff.base_snapshot_available {
            println!("No local changes relative to the cached base snapshot.");
        } else {
            println!(
                "No cached base snapshot available. Re-pull the workflow to seed local diff data."
            );
        }

        if args.refresh {
            if !diff.remote_changed_sections.is_empty() {
                println!(
                    "Remote/local sections: {}",
                    diff.remote_changed_sections.join(", ")
                );
            } else if diff.remote_comparison_available {
                println!("Remote/local sections: none");
            } else {
                println!("Remote/local sections: unavailable");
            }

            if let Some(patch) = &diff.remote_patch {
                println!("Remote vs local:");
                print!("{patch}");
            } else if diff.remote_comparison_available {
                println!("No local changes relative to the current remote workflow.");
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn summarize_statuses(statuses: &[crate::repo::LocalStatusEntry]) -> StatusSummary {
    let mut summary = StatusSummary {
        clean: 0,
        modified: 0,
        untracked: 0,
        invalid: 0,
        orphaned_meta: 0,
    };
    for status in statuses {
        match status.state {
            LocalWorkflowState::Clean => summary.clean += 1,
            LocalWorkflowState::Modified => summary.modified += 1,
            LocalWorkflowState::Untracked => summary.untracked += 1,
            LocalWorkflowState::Invalid => summary.invalid += 1,
            LocalWorkflowState::OrphanedMeta => summary.orphaned_meta += 1,
        }
    }
    summary
}

fn summarize_sync_states(statuses: &[crate::repo::LocalStatusEntry]) -> SyncSummary {
    let mut summary = SyncSummary {
        clean: 0,
        modified: 0,
        drifted: 0,
        conflict: 0,
        missing_remote: 0,
        unavailable: 0,
    };
    for status in statuses {
        if !is_refreshable_remote_status(status) {
            continue;
        }
        match status.sync_state {
            Some(RemoteSyncState::Clean) => summary.clean += 1,
            Some(RemoteSyncState::Modified) => summary.modified += 1,
            Some(RemoteSyncState::Drifted) => summary.drifted += 1,
            Some(RemoteSyncState::Conflict) => summary.conflict += 1,
            Some(RemoteSyncState::MissingRemote) => summary.missing_remote += 1,
            None => summary.unavailable += 1,
        }
    }
    summary
}

fn local_status_label(state: LocalWorkflowState) -> &'static str {
    match state {
        LocalWorkflowState::Clean => "clean",
        LocalWorkflowState::Modified => "modified",
        LocalWorkflowState::Untracked => "untracked",
        LocalWorkflowState::Invalid => "invalid",
        LocalWorkflowState::OrphanedMeta => "orphaned_meta",
    }
}

fn sync_status_label(state: RemoteSyncState) -> &'static str {
    match state {
        RemoteSyncState::Clean => "clean",
        RemoteSyncState::Modified => "modified",
        RemoteSyncState::Drifted => "drifted",
        RemoteSyncState::Conflict => "conflict",
        RemoteSyncState::MissingRemote => "missing_remote",
    }
}

fn is_refreshable_remote_status(status: &crate::repo::LocalStatusEntry) -> bool {
    matches!(
        status.state,
        LocalWorkflowState::Clean | LocalWorkflowState::Modified
    )
}

fn remote_refresh_detail(instance: Option<&str>, message: &str) -> String {
    match instance {
        Some(instance) => format!("Remote refresh unavailable for `{instance}`: {message}"),
        None => format!("Remote refresh unavailable: {message}"),
    }
}

fn with_remote_refresh_unavailable_status(
    status: &crate::repo::LocalStatusEntry,
    detail: String,
) -> crate::repo::LocalStatusEntry {
    let mut status = status.clone();
    status.sync_state = None;
    status.remote_hash = None;
    status.remote_updated_at = None;
    status.remote_detail = Some(detail);
    status
}

fn with_remote_refresh_unavailable_diff(
    mut diff: crate::repo::LocalDiff,
    detail: String,
) -> crate::repo::LocalDiff {
    diff.status = with_remote_refresh_unavailable_status(&diff.status, detail);
    diff.remote_comparison_available = false;
    diff.remote_changed_sections.clear();
    diff.remote_patch = None;
    diff
}

fn return_diff_with_refresh_error(
    diff: crate::repo::LocalDiff,
    instance: Option<&str>,
    err: AppError,
) -> crate::repo::LocalDiff {
    with_remote_refresh_unavailable_diff(diff, remote_refresh_detail(instance, &err.message))
}

async fn refresh_statuses(
    repo: &LoadedRepo,
    statuses: &[crate::repo::LocalStatusEntry],
    command: &'static str,
) -> Result<Vec<crate::repo::LocalStatusEntry>, AppError> {
    let mut clients: BTreeMap<String, Result<ApiClient, AppError>> = BTreeMap::new();
    let mut refreshed = Vec::with_capacity(statuses.len());

    for status in statuses {
        if !is_refreshable_remote_status(status) {
            refreshed.push(status.clone());
            continue;
        }

        let Some(instance) = status.instance.as_deref() else {
            refreshed.push(with_remote_refresh_unavailable_status(
                status,
                "Remote refresh unavailable: tracked workflow is missing an instance alias."
                    .to_string(),
            ));
            continue;
        };
        let Some(workflow_id) = status.workflow_id.as_deref() else {
            refreshed.push(with_remote_refresh_unavailable_status(
                status,
                "Remote refresh unavailable: tracked workflow is missing a workflow ID."
                    .to_string(),
            ));
            continue;
        };

        let client = match client_for_instance(repo, instance, command, &mut clients) {
            Ok(client) => client,
            Err(err) => {
                refreshed.push(with_remote_refresh_unavailable_status(
                    status,
                    remote_refresh_detail(Some(instance), &err.message),
                ));
                continue;
            }
        };
        let remote = match client.get_workflow_by_id(workflow_id).await {
            Ok(remote) => remote,
            Err(err) => {
                refreshed.push(with_remote_refresh_unavailable_status(
                    status,
                    remote_refresh_detail(Some(instance), &err.message),
                ));
                continue;
            }
        };
        let remote_workflow = remote
            .as_ref()
            .map(|value| value.get("data").unwrap_or(value));
        match refresh_local_status(command, status, remote_workflow) {
            Ok(status) => refreshed.push(status),
            Err(err) => refreshed.push(with_remote_refresh_unavailable_status(
                status,
                remote_refresh_detail(Some(instance), &err.message),
            )),
        }
    }

    Ok(refreshed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::repo::{LocalStatusEntry, LocalWorkflowState, RemoteSyncState};

    use super::summarize_sync_states;

    #[test]
    fn summarize_sync_states_only_counts_refreshable_workflows() {
        let statuses = vec![
            LocalStatusEntry {
                state: LocalWorkflowState::Clean,
                file: "workflows/clean.workflow.json".into(),
                sidecar: None,
                workflow_id: Some("wf-clean".to_string()),
                name: Some("Clean".to_string()),
                instance: Some("prod".to_string()),
                local_hash: None,
                recorded_hash: None,
                detail: None,
                sync_state: Some(RemoteSyncState::Clean),
                remote_hash: None,
                remote_updated_at: None,
                remote_detail: None,
            },
            LocalStatusEntry {
                state: LocalWorkflowState::Modified,
                file: "workflows/modified.workflow.json".into(),
                sidecar: None,
                workflow_id: Some("wf-modified".to_string()),
                name: Some("Modified".to_string()),
                instance: Some("prod".to_string()),
                local_hash: None,
                recorded_hash: None,
                detail: None,
                sync_state: None,
                remote_hash: None,
                remote_updated_at: None,
                remote_detail: Some("Remote refresh unavailable".to_string()),
            },
            LocalStatusEntry {
                state: LocalWorkflowState::Untracked,
                file: "workflows/untracked.workflow.json".into(),
                sidecar: None,
                workflow_id: Some("wf-untracked".to_string()),
                name: Some("Untracked".to_string()),
                instance: None,
                local_hash: None,
                recorded_hash: None,
                detail: Some("No metadata sidecar found.".to_string()),
                sync_state: None,
                remote_hash: None,
                remote_updated_at: None,
                remote_detail: None,
            },
            LocalStatusEntry {
                state: LocalWorkflowState::OrphanedMeta,
                file: "workflows/orphaned.meta.json".into(),
                sidecar: None,
                workflow_id: Some("wf-orphaned".to_string()),
                name: None,
                instance: Some("prod".to_string()),
                local_hash: None,
                recorded_hash: None,
                detail: Some("Metadata sidecar has no matching workflow file.".to_string()),
                sync_state: None,
                remote_hash: None,
                remote_updated_at: None,
                remote_detail: None,
            },
        ];

        let summary = summarize_sync_states(&statuses);

        assert_eq!(summary.clean, 1);
        assert_eq!(summary.unavailable, 1);
        assert_eq!(summary.modified, 0);
        assert_eq!(summary.drifted, 0);
        assert_eq!(summary.conflict, 0);
        assert_eq!(summary.missing_remote, 0);
    }
}
