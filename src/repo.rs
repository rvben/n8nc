use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::TextDiff;
use walkdir::WalkDir;

use crate::{
    canonical::{
        CANONICAL_VERSION, HASH_ALGORITHM, canonicalize_generic_json, canonicalize_workflow,
        hash_value, pretty_json,
    },
    config::{LoadedRepo, workflow_dir},
    error::AppError,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMeta {
    pub schema_version: u32,
    pub canonical_version: u32,
    pub hash_algorithm: String,
    pub instance: String,
    pub workflow_id: String,
    pub local_relpath: String,
    pub pulled_at: String,
    pub remote_updated_at: Option<String>,
    pub remote_hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredWorkflow {
    pub workflow_path: PathBuf,
    pub meta_path: PathBuf,
    pub meta: WorkflowMeta,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalWorkflowState {
    Clean,
    Modified,
    Untracked,
    Invalid,
    OrphanedMeta,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSyncState {
    Clean,
    Modified,
    Drifted,
    Conflict,
    MissingRemote,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalStatusEntry {
    pub state: LocalWorkflowState,
    pub file: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_state: Option<RemoteSyncState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalDiff {
    pub status: LocalStatusEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_hash: Option<String>,
    pub base_snapshot_available: bool,
    pub changed_sections: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    pub remote_comparison_available: bool,
    pub remote_changed_sections: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_patch: Option<String>,
}

pub fn workflow_id(workflow: &Value) -> Option<String> {
    workflow
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub fn workflow_name(workflow: &Value) -> Option<String> {
    workflow
        .get("name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub fn workflow_active(workflow: &Value) -> Option<bool> {
    workflow.get("active").and_then(Value::as_bool)
}

pub fn workflow_updated_at(workflow: &Value) -> Option<String> {
    workflow
        .get("updatedAt")
        .or_else(|| workflow.get("updated_at"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub fn sidecar_path_for(workflow_path: &Path) -> PathBuf {
    PathBuf::from(
        workflow_path
            .to_string_lossy()
            .replace(".workflow.json", ".meta.json"),
    )
}

pub fn cache_snapshot_path(root: &Path, instance: &str, workflow_id: &str) -> PathBuf {
    root.join(".n8n")
        .join("cache")
        .join(format!("{instance}--{workflow_id}.workflow.json"))
}

pub fn load_workflow_file(path: &Path, command: &'static str) -> Result<Value, AppError> {
    let raw = fs::read_to_string(path).map_err(|err| {
        AppError::validation(command, format!("Failed to read {}: {err}", path.display()))
    })?;
    serde_json::from_str(&raw).map_err(|err| {
        AppError::validation(
            command,
            format!("Failed to parse {}: {err}", path.display()),
        )
    })
}

pub fn load_meta(path: &Path, command: &'static str) -> Result<WorkflowMeta, AppError> {
    let raw = fs::read_to_string(path).map_err(|err| {
        AppError::validation(command, format!("Failed to read {}: {err}", path.display()))
    })?;
    serde_json::from_str(&raw).map_err(|err| {
        AppError::validation(
            command,
            format!("Failed to parse {}: {err}", path.display()),
        )
    })
}

pub fn format_json_file(path: &Path) -> Result<String, AppError> {
    let value = load_workflow_file(path, "fmt")?;
    let formatted = if path.extension().and_then(|value| value.to_str()) == Some("json")
        && path
            .file_name()
            .and_then(|value| value.to_str())
            .map(|name| name.ends_with(".workflow.json"))
            .unwrap_or(false)
    {
        pretty_json(&canonicalize_workflow(&value)?)?
    } else {
        pretty_json(&canonicalize_generic_json(&value))?
    };
    Ok(formatted)
}

pub fn collect_json_targets(
    paths: &[PathBuf],
    repo: Option<&LoadedRepo>,
) -> Result<Vec<PathBuf>, AppError> {
    let explicit: Vec<PathBuf> = if paths.is_empty() {
        if let Some(repo) = repo {
            vec![workflow_dir(&repo.root, &repo.config)]
        } else {
            vec![PathBuf::from(".")]
        }
    } else {
        paths.to_vec()
    };

    let mut files = Vec::new();
    for path in explicit {
        if path.is_file() {
            files.push(path);
            continue;
        }
        for entry in WalkDir::new(&path) {
            let entry = entry.map_err(|err| {
                AppError::validation(
                    "validate",
                    format!("Failed to walk {}: {err}", path.display()),
                )
            })?;
            if entry.file_type().is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

pub fn collect_workflow_artifacts(
    paths: &[PathBuf],
    repo: &LoadedRepo,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), AppError> {
    let explicit: Vec<PathBuf> = if paths.is_empty() {
        vec![workflow_dir(&repo.root, &repo.config)]
    } else {
        paths.to_vec()
    };

    let mut workflow_files = Vec::new();
    let mut meta_files = Vec::new();

    for path in explicit {
        if path.is_file() {
            classify_artifact_path(&path, &mut workflow_files, &mut meta_files);
            continue;
        }

        for entry in WalkDir::new(&path) {
            let entry = entry.map_err(|err| {
                AppError::validation(
                    "status",
                    format!("Failed to walk {}: {err}", path.display()),
                )
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            classify_artifact_path(entry.path(), &mut workflow_files, &mut meta_files);
        }
    }

    workflow_files.sort();
    workflow_files.dedup();
    meta_files.sort();
    meta_files.dedup();
    Ok((workflow_files, meta_files))
}

pub fn find_existing_workflow_path(repo: &LoadedRepo, workflow_id: &str) -> Option<PathBuf> {
    let workflow_dir = workflow_dir(&repo.root, &repo.config);
    WalkDir::new(workflow_dir)
        .into_iter()
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if name.ends_with(&format!("--{workflow_id}.workflow.json")) {
                Some(path.to_path_buf())
            } else {
                None
            }
        })
}

pub fn store_workflow(
    repo: &LoadedRepo,
    instance: &str,
    workflow: &Value,
) -> Result<StoredWorkflow, AppError> {
    let canonical = canonicalize_workflow(workflow)?;
    let workflow_id = workflow_id(&canonical)
        .ok_or_else(|| AppError::validation("store", "Workflow payload is missing `id`."))?;
    let workflow_name = workflow_name(&canonical).unwrap_or_else(|| "workflow".to_string());

    fs::create_dir_all(workflow_dir(&repo.root, &repo.config)).map_err(|err| {
        AppError::config(
            "store",
            format!(
                "Failed to create workflow directory {}: {err}",
                workflow_dir(&repo.root, &repo.config).display()
            ),
        )
    })?;

    let file_name = format!("{}--{}.workflow.json", slugify(&workflow_name), workflow_id);
    let target_path = workflow_dir(&repo.root, &repo.config).join(file_name);
    if let Some(existing) = find_existing_workflow_path(repo, &workflow_id)
        && existing != target_path
    {
        let existing_meta = sidecar_path_for(&existing);
        let _ = fs::remove_file(&existing);
        let _ = fs::remove_file(existing_meta);
    }

    let rendered_workflow = pretty_json(&canonical)?;
    fs::write(&target_path, rendered_workflow).map_err(|err| {
        AppError::validation(
            "store",
            format!("Failed to write {}: {err}", target_path.display()),
        )
    })?;

    let relpath = target_path
        .strip_prefix(&repo.root)
        .unwrap_or(&target_path)
        .to_string_lossy()
        .to_string();
    let meta = WorkflowMeta {
        schema_version: 1,
        canonical_version: CANONICAL_VERSION,
        hash_algorithm: HASH_ALGORITHM.to_string(),
        instance: instance.to_string(),
        workflow_id: workflow_id.clone(),
        local_relpath: relpath,
        pulled_at: Utc::now().to_rfc3339(),
        remote_updated_at: workflow_updated_at(workflow),
        remote_hash: hash_value(&canonical)?,
    };

    let meta_path = sidecar_path_for(&target_path);
    let meta_json = pretty_json(&canonicalize_generic_json(
        &serde_json::to_value(&meta).map_err(|err| {
            AppError::validation("store", format!("Failed to serialize metadata: {err}"))
        })?,
    ))?;
    fs::write(&meta_path, meta_json).map_err(|err| {
        AppError::validation(
            "store",
            format!("Failed to write {}: {err}", meta_path.display()),
        )
    })?;

    let cache_path = cache_snapshot_path(&repo.root, instance, &workflow_id);
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::config(
                "store",
                format!(
                    "Failed to create cache directory {}: {err}",
                    parent.display()
                ),
            )
        })?;
    }
    fs::write(&cache_path, pretty_json(&canonical)?).map_err(|err| {
        AppError::validation(
            "store",
            format!("Failed to write {}: {err}", cache_path.display()),
        )
    })?;

    Ok(StoredWorkflow {
        workflow_path: target_path,
        meta_path,
        meta,
    })
}

pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;

    for ch in input.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' {
            if !last_dash && !out.is_empty() {
                out.push('-');
            }
            last_dash = true;
        } else {
            out.push(mapped);
            last_dash = false;
        }
    }

    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "workflow".to_string()
    } else {
        out
    }
}

pub fn scan_local_status(
    repo: &LoadedRepo,
    paths: &[PathBuf],
) -> Result<Vec<LocalStatusEntry>, AppError> {
    let (workflow_files, meta_files) = collect_workflow_artifacts(paths, repo)?;
    let mut statuses = Vec::new();

    for workflow_path in &workflow_files {
        statuses.push(classify_workflow_status(workflow_path)?);
    }

    for meta_path in meta_files {
        let workflow_path = PathBuf::from(
            meta_path
                .to_string_lossy()
                .replace(".meta.json", ".workflow.json"),
        );
        if !workflow_path.exists() {
            statuses.push(classify_orphaned_meta_status(&meta_path));
        }
    }

    statuses.sort_by(|left, right| left.file.cmp(&right.file));
    Ok(statuses)
}

pub fn build_local_diff(repo: &LoadedRepo, workflow_path: &Path) -> Result<LocalDiff, AppError> {
    let status = classify_workflow_status(workflow_path)?;
    let Some(workflow_id) = status.workflow_id.clone() else {
        return Ok(LocalDiff {
            status,
            base_hash: None,
            base_snapshot_available: false,
            changed_sections: Vec::new(),
            patch: None,
            remote_comparison_available: false,
            remote_changed_sections: Vec::new(),
            remote_patch: None,
        });
    };
    let Some(instance) = status.instance.clone() else {
        return Ok(LocalDiff {
            status,
            base_hash: None,
            base_snapshot_available: false,
            changed_sections: Vec::new(),
            patch: None,
            remote_comparison_available: false,
            remote_changed_sections: Vec::new(),
            remote_patch: None,
        });
    };

    let cache_path = cache_snapshot_path(&repo.root, &instance, &workflow_id);
    if !cache_path.exists() {
        return Ok(LocalDiff {
            status,
            base_hash: None,
            base_snapshot_available: false,
            changed_sections: Vec::new(),
            patch: None,
            remote_comparison_available: false,
            remote_changed_sections: Vec::new(),
            remote_patch: None,
        });
    }

    let local_workflow = canonicalize_workflow(&load_workflow_file(workflow_path, "diff")?)?;
    let base_workflow = canonicalize_workflow(&load_workflow_file(&cache_path, "diff")?)?;
    let base_hash = hash_value(&base_workflow)?;
    let changed_sections = diff_sections(&base_workflow, &local_workflow);

    let patch = if base_hash == status.local_hash.clone().unwrap_or_default() {
        None
    } else {
        let base_text = pretty_json(&base_workflow)?;
        let local_text = pretty_json(&local_workflow)?;
        Some(
            TextDiff::from_lines(&base_text, &local_text)
                .unified_diff()
                .context_radius(3)
                .header("base", "local")
                .to_string(),
        )
    };

    Ok(LocalDiff {
        status,
        base_hash: Some(base_hash),
        base_snapshot_available: true,
        changed_sections,
        patch,
        remote_comparison_available: false,
        remote_changed_sections: Vec::new(),
        remote_patch: None,
    })
}

pub fn refresh_local_status(
    command: &'static str,
    status: &LocalStatusEntry,
    remote_workflow: Option<&Value>,
) -> Result<LocalStatusEntry, AppError> {
    let mut refreshed = status.clone();
    if !matches!(
        refreshed.state,
        LocalWorkflowState::Clean | LocalWorkflowState::Modified
    ) {
        return Ok(refreshed);
    }

    let Some(recorded_hash) = refreshed.recorded_hash.clone() else {
        return Ok(refreshed);
    };

    let Some(remote_workflow) = remote_workflow else {
        refreshed.sync_state = Some(RemoteSyncState::MissingRemote);
        refreshed.remote_hash = None;
        refreshed.remote_updated_at = None;
        refreshed.remote_detail = Some("Remote workflow was not found.".to_string());
        return Ok(refreshed);
    };

    let remote_canonical = canonicalize_workflow(remote_workflow).map_err(|err| {
        AppError::api(
            command,
            "api.invalid_response",
            format!(
                "Remote workflow payload could not be canonicalized: {}",
                err.message
            ),
        )
    })?;
    let remote_hash = hash_value(&remote_canonical)?;

    refreshed.remote_hash = Some(remote_hash.clone());
    refreshed.remote_updated_at = workflow_updated_at(remote_workflow);
    refreshed.remote_detail = None;
    refreshed.sync_state = Some(match refreshed.state {
        LocalWorkflowState::Clean => {
            if remote_hash == recorded_hash {
                RemoteSyncState::Clean
            } else {
                RemoteSyncState::Drifted
            }
        }
        LocalWorkflowState::Modified => {
            if remote_hash == recorded_hash {
                RemoteSyncState::Modified
            } else {
                RemoteSyncState::Conflict
            }
        }
        _ => unreachable!(),
    });

    Ok(refreshed)
}

pub fn build_refreshed_diff(
    command: &'static str,
    repo: &LoadedRepo,
    workflow_path: &Path,
    remote_workflow: Option<&Value>,
) -> Result<LocalDiff, AppError> {
    let mut diff = build_local_diff(repo, workflow_path)?;
    diff.status = refresh_local_status(command, &diff.status, remote_workflow)?;

    let Some(remote_workflow) = remote_workflow else {
        return Ok(diff);
    };

    if !matches!(
        diff.status.state,
        LocalWorkflowState::Clean | LocalWorkflowState::Modified
    ) {
        return Ok(diff);
    }

    let local_workflow = canonicalize_workflow(&load_workflow_file(workflow_path, "diff")?)?;
    let remote_canonical = canonicalize_workflow(remote_workflow).map_err(|err| {
        AppError::api(
            command,
            "api.invalid_response",
            format!(
                "Remote workflow payload could not be canonicalized: {}",
                err.message
            ),
        )
    })?;
    let remote_hash = hash_value(&remote_canonical)?;
    let local_hash = diff.status.local_hash.clone().unwrap_or_default();

    diff.remote_comparison_available = true;
    diff.remote_changed_sections = diff_sections(&remote_canonical, &local_workflow);
    diff.remote_patch = if remote_hash == local_hash {
        None
    } else {
        let remote_text = pretty_json(&remote_canonical)?;
        let local_text = pretty_json(&local_workflow)?;
        Some(
            TextDiff::from_lines(&remote_text, &local_text)
                .unified_diff()
                .context_radius(3)
                .header("remote", "local")
                .to_string(),
        )
    };

    Ok(diff)
}

fn classify_artifact_path(
    path: &Path,
    workflow_files: &mut Vec<PathBuf>,
    meta_files: &mut Vec<PathBuf>,
) {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return;
    };
    if name.ends_with(".workflow.json") {
        workflow_files.push(path.to_path_buf());
    } else if name.ends_with(".meta.json") {
        meta_files.push(path.to_path_buf());
    }
}

fn classify_workflow_status(workflow_path: &Path) -> Result<LocalStatusEntry, AppError> {
    let sidecar = sidecar_path_for(workflow_path);
    let workflow = match load_workflow_file(workflow_path, "status") {
        Ok(value) => value,
        Err(err) => {
            return Ok(LocalStatusEntry {
                state: LocalWorkflowState::Invalid,
                file: workflow_path.to_path_buf(),
                sidecar: sidecar.exists().then_some(sidecar),
                workflow_id: None,
                name: None,
                instance: None,
                local_hash: None,
                recorded_hash: None,
                detail: Some(err.message),
                sync_state: None,
                remote_hash: None,
                remote_updated_at: None,
                remote_detail: None,
            });
        }
    };

    let canonical = match canonicalize_workflow(&workflow) {
        Ok(value) => value,
        Err(err) => {
            return Ok(LocalStatusEntry {
                state: LocalWorkflowState::Invalid,
                file: workflow_path.to_path_buf(),
                sidecar: sidecar.exists().then_some(sidecar),
                workflow_id: workflow_id(&workflow),
                name: workflow_name(&workflow),
                instance: None,
                local_hash: None,
                recorded_hash: None,
                detail: Some(err.message),
                sync_state: None,
                remote_hash: None,
                remote_updated_at: None,
                remote_detail: None,
            });
        }
    };

    let local_hash = hash_value(&canonical)?;
    let workflow_id = workflow_id(&canonical);
    let name = workflow_name(&canonical);

    if !sidecar.exists() {
        return Ok(LocalStatusEntry {
            state: LocalWorkflowState::Untracked,
            file: workflow_path.to_path_buf(),
            sidecar: None,
            workflow_id,
            name,
            instance: None,
            local_hash: Some(local_hash),
            recorded_hash: None,
            detail: Some("No metadata sidecar found.".to_string()),
            sync_state: None,
            remote_hash: None,
            remote_updated_at: None,
            remote_detail: None,
        });
    }

    let meta = match load_meta(&sidecar, "status") {
        Ok(value) => value,
        Err(err) => {
            return Ok(LocalStatusEntry {
                state: LocalWorkflowState::Invalid,
                file: workflow_path.to_path_buf(),
                sidecar: Some(sidecar),
                workflow_id,
                name,
                instance: None,
                local_hash: Some(local_hash),
                recorded_hash: None,
                detail: Some(err.message),
                sync_state: None,
                remote_hash: None,
                remote_updated_at: None,
                remote_detail: None,
            });
        }
    };

    if meta.canonical_version != CANONICAL_VERSION {
        return Ok(LocalStatusEntry {
            state: LocalWorkflowState::Invalid,
            file: workflow_path.to_path_buf(),
            sidecar: Some(sidecar),
            workflow_id,
            name,
            instance: Some(meta.instance),
            local_hash: Some(local_hash),
            recorded_hash: Some(meta.remote_hash),
            detail: Some(format!(
                "Unsupported canonical version {} in metadata sidecar.",
                meta.canonical_version
            )),
            sync_state: None,
            remote_hash: None,
            remote_updated_at: None,
            remote_detail: None,
        });
    }

    if meta.hash_algorithm != HASH_ALGORITHM {
        return Ok(LocalStatusEntry {
            state: LocalWorkflowState::Invalid,
            file: workflow_path.to_path_buf(),
            sidecar: Some(sidecar),
            workflow_id,
            name,
            instance: Some(meta.instance),
            local_hash: Some(local_hash),
            recorded_hash: Some(meta.remote_hash),
            detail: Some(format!(
                "Unsupported hash algorithm `{}` in metadata sidecar.",
                meta.hash_algorithm
            )),
            sync_state: None,
            remote_hash: None,
            remote_updated_at: None,
            remote_detail: None,
        });
    }

    let diagnostics = crate::validate::validate_workflow_path(workflow_path)?;
    if let Some(diagnostic) = diagnostics.first() {
        return Ok(LocalStatusEntry {
            state: LocalWorkflowState::Invalid,
            file: workflow_path.to_path_buf(),
            sidecar: Some(sidecar),
            workflow_id,
            name,
            instance: Some(meta.instance),
            local_hash: Some(local_hash),
            recorded_hash: Some(meta.remote_hash),
            detail: Some(diagnostic.message.clone()),
            sync_state: None,
            remote_hash: None,
            remote_updated_at: None,
            remote_detail: None,
        });
    }

    let state = if local_hash == meta.remote_hash {
        LocalWorkflowState::Clean
    } else {
        LocalWorkflowState::Modified
    };

    Ok(LocalStatusEntry {
        state,
        file: workflow_path.to_path_buf(),
        sidecar: Some(sidecar),
        workflow_id,
        name,
        instance: Some(meta.instance),
        local_hash: Some(local_hash),
        recorded_hash: Some(meta.remote_hash),
        detail: None,
        sync_state: None,
        remote_hash: None,
        remote_updated_at: None,
        remote_detail: None,
    })
}

fn classify_orphaned_meta_status(meta_path: &Path) -> LocalStatusEntry {
    match load_meta(meta_path, "status") {
        Ok(meta) => LocalStatusEntry {
            state: LocalWorkflowState::OrphanedMeta,
            file: meta_path.to_path_buf(),
            sidecar: None,
            workflow_id: Some(meta.workflow_id),
            name: None,
            instance: Some(meta.instance),
            local_hash: None,
            recorded_hash: Some(meta.remote_hash),
            detail: Some("Metadata sidecar has no matching workflow file.".to_string()),
            sync_state: None,
            remote_hash: None,
            remote_updated_at: None,
            remote_detail: None,
        },
        Err(err) => LocalStatusEntry {
            state: LocalWorkflowState::Invalid,
            file: meta_path.to_path_buf(),
            sidecar: None,
            workflow_id: None,
            name: None,
            instance: None,
            local_hash: None,
            recorded_hash: None,
            detail: Some(err.message),
            sync_state: None,
            remote_hash: None,
            remote_updated_at: None,
            remote_detail: None,
        },
    }
}

fn diff_sections(base: &Value, local: &Value) -> Vec<String> {
    const CANDIDATE_SECTIONS: &[&str] =
        &["name", "active", "tags", "settings", "nodes", "connections"];
    CANDIDATE_SECTIONS
        .iter()
        .filter_map(|key| {
            let base_value = base.get(*key);
            let local_value = local.get(*key);
            if base_value != local_value {
                Some((*key).to_string())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::Path};

    use serde_json::json;
    use tempfile::tempdir;

    use crate::config::{InstanceConfig, LoadedRepo, RepoConfig};

    use super::{
        LocalWorkflowState, RemoteSyncState, build_local_diff, build_refreshed_diff,
        cache_snapshot_path, refresh_local_status, scan_local_status, slugify, store_workflow,
    };

    #[test]
    fn slugify_keeps_names_stable() {
        assert_eq!(slugify("Order Alert"), "order-alert");
        assert_eq!(slugify("  !! "), "workflow");
    }

    #[test]
    fn scan_local_status_reports_local_states() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());

        store_workflow(
            &repo,
            "prod",
            &json!({
                "id": "wf-clean",
                "name": "Clean Workflow",
                "active": false,
                "nodes": [],
                "connections": {}
            }),
        )
        .expect("store clean");

        let modified = store_workflow(
            &repo,
            "prod",
            &json!({
                "id": "wf-modified",
                "name": "Modified Workflow",
                "active": false,
                "nodes": [],
                "connections": {}
            }),
        )
        .expect("store modified");
        fs::write(
            &modified.workflow_path,
            r#"{
  "id": "wf-modified",
  "name": "Modified Workflow",
  "active": true,
  "nodes": [],
  "connections": {}
}
"#,
        )
        .expect("write modified workflow");

        fs::write(
            repo.root
                .join("workflows")
                .join("untracked--wf-untracked.workflow.json"),
            r#"{
  "id": "wf-untracked",
  "name": "Untracked Workflow",
  "nodes": [],
  "connections": {}
}
"#,
        )
        .expect("write untracked workflow");

        fs::write(
            repo.root
                .join("workflows")
                .join("orphaned--wf-orphaned.meta.json"),
            r#"{
  "schema_version": 1,
  "canonical_version": 1,
  "hash_algorithm": "sha256",
  "instance": "prod",
  "workflow_id": "wf-orphaned",
  "local_relpath": "workflows/orphaned--wf-orphaned.workflow.json",
  "pulled_at": "2026-03-26T10:31:54Z",
  "remote_updated_at": null,
  "remote_hash": "sha256:test"
}
"#,
        )
        .expect("write orphaned meta");

        let statuses = scan_local_status(&repo, &[]).expect("scan statuses");
        assert!(
            statuses
                .iter()
                .any(|status| status.workflow_id.as_deref() == Some("wf-clean")
                    && status.state == LocalWorkflowState::Clean)
        );
        assert!(statuses.iter().any(
            |status| status.workflow_id.as_deref() == Some("wf-modified")
                && status.state == LocalWorkflowState::Modified
        ));
        assert!(statuses.iter().any(|status| status.workflow_id.as_deref()
            == Some("wf-untracked")
            && status.state == LocalWorkflowState::Untracked));
        assert!(statuses.iter().any(
            |status| status.workflow_id.as_deref() == Some("wf-orphaned")
                && status.state == LocalWorkflowState::OrphanedMeta
        ));
    }

    #[test]
    fn build_local_diff_uses_cached_snapshot() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());

        let stored = store_workflow(
            &repo,
            "prod",
            &json!({
                "id": "wf-diff",
                "name": "Diff Workflow",
                "active": false,
                "settings": {"timezone": "UTC"},
                "nodes": [],
                "connections": {}
            }),
        )
        .expect("store workflow");

        let cache_path = cache_snapshot_path(&repo.root, "prod", "wf-diff");
        assert!(cache_path.exists());

        fs::write(
            &stored.workflow_path,
            r#"{
  "id": "wf-diff",
  "name": "Diff Workflow",
  "active": true,
  "settings": {
    "timezone": "Europe/Amsterdam"
  },
  "nodes": [],
  "connections": {}
}
"#,
        )
        .expect("write local changes");

        let diff = build_local_diff(&repo, &stored.workflow_path).expect("build diff");
        assert!(diff.base_snapshot_available);
        assert_eq!(diff.status.state, LocalWorkflowState::Modified);
        assert!(diff.changed_sections.contains(&"active".to_string()));
        assert!(diff.changed_sections.contains(&"settings".to_string()));
        assert!(diff.patch.expect("patch").contains("--- base"));
    }

    #[test]
    fn refresh_local_status_classifies_remote_sync_states() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());

        store_workflow(
            &repo,
            "prod",
            &json!({
                "id": "wf-clean-remote",
                "name": "Clean Remote Workflow",
                "active": false,
                "nodes": [],
                "connections": {}
            }),
        )
        .expect("store clean workflow");

        let modified = store_workflow(
            &repo,
            "prod",
            &json!({
                "id": "wf-modified-remote",
                "name": "Modified Remote Workflow",
                "active": false,
                "settings": {"timezone": "UTC"},
                "nodes": [],
                "connections": {}
            }),
        )
        .expect("store modified workflow");
        fs::write(
            &modified.workflow_path,
            r#"{
  "id": "wf-modified-remote",
  "name": "Modified Remote Workflow",
  "active": true,
  "settings": {
    "timezone": "Europe/Amsterdam"
  },
  "nodes": [],
  "connections": {}
}
"#,
        )
        .expect("write local modification");

        let statuses = scan_local_status(&repo, &[]).expect("scan statuses");
        let clean_status = statuses
            .iter()
            .find(|status| status.workflow_id.as_deref() == Some("wf-clean-remote"))
            .expect("clean status");
        let modified_status = statuses
            .iter()
            .find(|status| status.workflow_id.as_deref() == Some("wf-modified-remote"))
            .expect("modified status");

        let clean_remote = json!({
            "id": "wf-clean-remote",
            "name": "Clean Remote Workflow",
            "active": false,
            "updatedAt": "2026-03-26T10:32:00Z",
            "nodes": [],
            "connections": {}
        });
        let clean_refreshed =
            refresh_local_status("status", clean_status, Some(&clean_remote)).expect("clean");
        assert_eq!(clean_refreshed.sync_state, Some(RemoteSyncState::Clean));
        assert_eq!(
            clean_refreshed.remote_updated_at.as_deref(),
            Some("2026-03-26T10:32:00Z")
        );
        assert!(clean_refreshed.remote_hash.is_some());

        let drifted_remote = json!({
            "id": "wf-clean-remote",
            "name": "Clean Remote Workflow",
            "active": true,
            "nodes": [],
            "connections": {}
        });
        let drifted =
            refresh_local_status("status", clean_status, Some(&drifted_remote)).expect("drifted");
        assert_eq!(drifted.sync_state, Some(RemoteSyncState::Drifted));

        let recorded_remote = json!({
            "id": "wf-modified-remote",
            "name": "Modified Remote Workflow",
            "active": false,
            "settings": {"timezone": "UTC"},
            "nodes": [],
            "connections": {}
        });
        let modified_refreshed =
            refresh_local_status("status", modified_status, Some(&recorded_remote))
                .expect("modified");
        assert_eq!(
            modified_refreshed.sync_state,
            Some(RemoteSyncState::Modified)
        );

        let conflict_remote = json!({
            "id": "wf-modified-remote",
            "name": "Modified Remote Workflow",
            "active": false,
            "settings": {"timezone": "Asia/Tokyo"},
            "nodes": [],
            "connections": {}
        });
        let conflict = refresh_local_status("status", modified_status, Some(&conflict_remote))
            .expect("conflict");
        assert_eq!(conflict.sync_state, Some(RemoteSyncState::Conflict));

        let missing = refresh_local_status("status", clean_status, None).expect("missing remote");
        assert_eq!(missing.sync_state, Some(RemoteSyncState::MissingRemote));
        assert_eq!(
            missing.remote_detail.as_deref(),
            Some("Remote workflow was not found.")
        );
    }

    #[test]
    fn build_refreshed_diff_compares_remote_against_local() {
        let temp = tempdir().expect("tempdir");
        let repo = fixture_repo(temp.path());

        let stored = store_workflow(
            &repo,
            "prod",
            &json!({
                "id": "wf-remote-diff",
                "name": "Remote Diff Workflow",
                "active": false,
                "settings": {"timezone": "UTC"},
                "nodes": [],
                "connections": {}
            }),
        )
        .expect("store workflow");

        fs::write(
            &stored.workflow_path,
            r#"{
  "id": "wf-remote-diff",
  "name": "Remote Diff Workflow",
  "active": true,
  "settings": {
    "timezone": "Europe/Amsterdam"
  },
  "nodes": [],
  "connections": {}
}
"#,
        )
        .expect("write local changes");

        let remote_workflow = json!({
            "id": "wf-remote-diff",
            "name": "Remote Diff Workflow",
            "active": false,
            "settings": {"timezone": "Asia/Tokyo"},
            "updatedAt": "2026-03-26T10:33:00Z",
            "nodes": [],
            "connections": {}
        });
        let diff =
            build_refreshed_diff("diff", &repo, &stored.workflow_path, Some(&remote_workflow))
                .expect("build refreshed diff");

        assert_eq!(diff.status.state, LocalWorkflowState::Modified);
        assert_eq!(diff.status.sync_state, Some(RemoteSyncState::Conflict));
        assert!(diff.remote_comparison_available);
        assert!(diff.remote_changed_sections.contains(&"active".to_string()));
        assert!(
            diff.remote_changed_sections
                .contains(&"settings".to_string())
        );
        assert_eq!(
            diff.status.remote_updated_at.as_deref(),
            Some("2026-03-26T10:33:00Z")
        );
        assert!(
            diff.remote_patch
                .expect("remote patch")
                .contains("--- remote")
        );
    }

    fn fixture_repo(root: &Path) -> LoadedRepo {
        fs::create_dir_all(root.join("workflows")).expect("workflow dir");
        fs::create_dir_all(root.join(".n8n").join("cache")).expect("cache dir");

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
}
