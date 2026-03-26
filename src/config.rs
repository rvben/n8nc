use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub schema_version: u32,
    pub default_instance: String,
    #[serde(default = "default_workflow_dir")]
    pub workflow_dir: PathBuf,
    pub instances: BTreeMap<String, InstanceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub base_url: String,
    #[serde(default = "default_api_version")]
    pub api_version: String,
}

#[derive(Debug, Clone)]
pub struct LoadedRepo {
    pub root: PathBuf,
    pub config: RepoConfig,
}

fn default_workflow_dir() -> PathBuf {
    PathBuf::from("workflows")
}

fn default_api_version() -> String {
    "v1".to_string()
}

pub fn discover_repo_root(explicit: Option<&Path>) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    let mut current = env::current_dir().map_err(|err| {
        AppError::config(
            "config",
            format!("Failed to resolve current directory: {err}"),
        )
    })?;

    loop {
        if current.join("n8n.toml").exists() {
            return Ok(current);
        }

        if !current.pop() {
            return Err(AppError::config(
                "config",
                "Could not find n8n.toml in the current directory or any parent directory.",
            )
            .with_suggestion("Run `n8nc init --instance <alias> --url <base_url>` first."));
        }
    }
}

pub fn load_repo(explicit_root: Option<&Path>) -> Result<LoadedRepo, AppError> {
    let root = discover_repo_root(explicit_root)?;
    let raw = fs::read_to_string(root.join("n8n.toml")).map_err(|err| {
        AppError::config(
            "config",
            format!("Failed to read {}: {err}", root.join("n8n.toml").display()),
        )
    })?;
    let config: RepoConfig = toml::from_str(&raw)
        .map_err(|err| AppError::config("config", format!("Failed to parse n8n.toml: {err}")))?;
    Ok(LoadedRepo { root, config })
}

pub fn save_repo_config(root: &Path, config: &RepoConfig) -> Result<(), AppError> {
    let serialized = toml::to_string_pretty(config)
        .map_err(|err| AppError::config("init", format!("Failed to serialize n8n.toml: {err}")))?;
    fs::write(root.join("n8n.toml"), serialized).map_err(|err| {
        AppError::config(
            "init",
            format!("Failed to write {}: {err}", root.join("n8n.toml").display()),
        )
    })
}

pub fn resolve_instance_alias(
    repo: &LoadedRepo,
    alias: Option<&str>,
    command: &'static str,
) -> Result<String, AppError> {
    let alias = alias.unwrap_or(&repo.config.default_instance).to_string();
    if repo.config.instances.contains_key(&alias) {
        Ok(alias)
    } else {
        Err(AppError::config(
            command,
            format!("Unknown instance alias `{alias}`."),
        ))
    }
}

pub fn workflow_dir(root: &Path, config: &RepoConfig) -> PathBuf {
    root.join(&config.workflow_dir)
}

pub fn ensure_repo_layout(root: &Path, config: &RepoConfig) -> Result<(), AppError> {
    fs::create_dir_all(workflow_dir(root, config)).map_err(|err| {
        AppError::config(
            "init",
            format!(
                "Failed to create workflow directory {}: {err}",
                workflow_dir(root, config).display()
            ),
        )
    })?;
    fs::create_dir_all(root.join(".n8n").join("cache")).map_err(|err| {
        AppError::config(
            "init",
            format!(
                "Failed to create cache directory {}: {err}",
                root.join(".n8n/cache").display()
            ),
        )
    })?;
    Ok(())
}

pub fn ensure_gitignore(root: &Path) -> Result<(), AppError> {
    let path = root.join(".gitignore");
    let existing = match fs::read_to_string(&path) {
        Ok(value) => value,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(AppError::config(
                "init",
                format!("Failed to read {}: {err}", path.display()),
            ));
        }
    };

    if existing.contains("/.n8n/") || existing.contains(".n8n/") {
        return Ok(());
    }

    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str("/.n8n/\n");
    fs::write(&path, next).map_err(|err| {
        AppError::config(
            "init",
            format!("Failed to update {}: {err}", path.display()),
        )
    })
}
