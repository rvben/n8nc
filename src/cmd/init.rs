use std::collections::BTreeMap;

use serde_json::json;

use crate::{
    cli::InitArgs,
    config::{InstanceConfig, RepoConfig, ensure_gitignore, ensure_repo_layout, save_repo_config},
    error::AppError,
};

use super::common::{Context, emit_json, print_message};

pub(crate) async fn cmd_init(context: &Context, args: InitArgs) -> Result<(), AppError> {
    let root = if let Some(path) = &context.repo_root {
        path.clone()
    } else {
        std::env::current_dir().map_err(|err| {
            AppError::config(
                "init",
                format!("Failed to resolve current directory: {err}"),
            )
        })?
    };
    let config_path = root.join("n8n.toml");
    if config_path.exists() && !args.force {
        return Err(
            AppError::config("init", format!("{} already exists.", config_path.display()))
                .with_suggestion("Use `--force` to overwrite it."),
        );
    }

    let mut instances = BTreeMap::new();
    instances.insert(
        args.instance.clone(),
        InstanceConfig {
            base_url: args.url.trim_end_matches('/').to_string(),
            api_version: "v1".to_string(),
            execute: None,
        },
    );
    let config = RepoConfig {
        schema_version: 1,
        default_instance: args.instance,
        workflow_dir: args.workflow_dir,
        instances,
        lint: None,
    };

    save_repo_config(&root, &config)?;
    ensure_repo_layout(&root, &config)?;
    ensure_gitignore(&root)?;

    let data = json!({
        "repo_root": root,
        "config": root.join("n8n.toml"),
        "workflow_dir": root.join(&config.workflow_dir),
    });
    if context.json {
        emit_json("init", &data)
    } else {
        print_message(
            context,
            &format!("Initialized n8n repo at {}", root.display()),
        );
        print_message(
            context,
            &format!("Config: {}", root.join("n8n.toml").display()),
        );
        print_message(
            context,
            &format!("Workflow dir: {}", root.join(config.workflow_dir).display()),
        );
        Ok(())
    }
}
