use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
        .ok_or_else(|| AppError::validation("pull", "Workflow payload is missing `id`."))?;
    let workflow_name = workflow_name(&canonical).unwrap_or_else(|| "workflow".to_string());

    fs::create_dir_all(workflow_dir(&repo.root, &repo.config)).map_err(|err| {
        AppError::config(
            "pull",
            format!(
                "Failed to create workflow directory {}: {err}",
                workflow_dir(&repo.root, &repo.config).display()
            ),
        )
    })?;

    let file_name = format!("{}--{}.workflow.json", slugify(&workflow_name), workflow_id);
    let target_path = workflow_dir(&repo.root, &repo.config).join(file_name);
    if let Some(existing) = find_existing_workflow_path(repo, &workflow_id) {
        if existing != target_path {
            let existing_meta = sidecar_path_for(&existing);
            let _ = fs::remove_file(&existing);
            let _ = fs::remove_file(existing_meta);
        }
    }

    let rendered_workflow = pretty_json(&canonical)?;
    fs::write(&target_path, rendered_workflow).map_err(|err| {
        AppError::validation(
            "pull",
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
            AppError::validation("pull", format!("Failed to serialize metadata: {err}"))
        })?,
    ))?;
    fs::write(&meta_path, meta_json).map_err(|err| {
        AppError::validation(
            "pull",
            format!("Failed to write {}: {err}", meta_path.display()),
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

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugify_keeps_names_stable() {
        assert_eq!(slugify("Order Alert"), "order-alert");
        assert_eq!(slugify("  !! "), "workflow");
    }
}
