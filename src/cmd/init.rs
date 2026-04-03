use std::collections::BTreeMap;
use std::io::IsTerminal;

use owo_colors::OwoColorize;
use serde_json::json;

use crate::{
    api::{ApiClient, ListOptions},
    auth::store_token,
    cli::InitArgs,
    config::{InstanceConfig, RepoConfig, ensure_gitignore, ensure_repo_layout, save_repo_config},
    error::AppError,
};

use super::common::{Context, emit_json, use_color};

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

    // Resolve instance, URL, and token — from flags or interactive prompts.
    let (instance, url, token) = if args.instance.is_some() && args.url.is_some() {
        // Non-interactive: all required values provided via flags.
        let instance = args.instance.unwrap();
        let url = args.url.unwrap();
        (instance, url, args.token)
    } else if context.json {
        // JSON mode requires explicit flags — no interactive prompts.
        return Err(AppError::usage(
            "init",
            "`--instance` and `--url` are required in non-interactive / JSON mode.",
        ));
    } else if !std::io::stdin().is_terminal() {
        return Err(AppError::usage(
            "init",
            "Run `n8nc init` in an interactive terminal, or pass `--instance` and `--url`.",
        ));
    } else {
        init_interactive(args.instance, args.url, args.token).await?
    };

    let base_url = url.trim_end_matches('/').to_string();

    // Validate credentials if a token was provided.
    if let Some(ref token) = token {
        validate_credentials(&base_url, token).await?;
    }

    // Store the token in the OS keychain.
    if let Some(ref token) = token {
        store_token(&instance, token)?;
    }

    let mut instances = BTreeMap::new();
    instances.insert(
        instance.clone(),
        InstanceConfig {
            base_url: base_url.clone(),
            api_version: "v1".to_string(),
            execute: None,
        },
    );
    let config = RepoConfig {
        schema_version: 1,
        default_instance: instance,
        workflow_dir: args.workflow_dir,
        instances,
        lint: None,
    };

    save_repo_config(&root, &config)?;
    ensure_repo_layout(&root, &config)?;
    ensure_gitignore(&root)?;

    let saved_path = root.join("n8n.toml");
    if context.json {
        let data = json!({
            "repo_root": root,
            "config": saved_path,
            "workflow_dir": root.join(&config.workflow_dir),
            "token_stored": token.is_some(),
        });
        emit_json("init", &data)
    } else {
        eprintln!();
        eprintln!(
            "  {} Configuration saved to {}",
            sym_ok(),
            saved_path.display()
        );
        eprintln!();
        eprintln!("  Next steps:");
        eprintln!("    n8nc ls                 # list workflows");
        eprintln!("    n8nc runs ls            # show recent executions");
        eprintln!("    n8nc completions zsh    # shell completions");
        eprintln!();
        Ok(())
    }
}

/// Run the interactive init flow, prompting for instance alias, URL, and API key.
async fn init_interactive(
    prefill_instance: Option<String>,
    prefill_url: Option<String>,
    prefill_token: Option<String>,
) -> Result<(String, String, Option<String>), AppError> {
    let sep = sym_dim("──────────────");
    eprintln!("n8nc Setup");
    eprintln!("{sep}");
    eprintln!();

    // Instance alias
    let instance = if let Some(val) = prefill_instance {
        val
    } else {
        prompt_required(
            "Instance alias",
            sym_dim("e.g., prod").as_str(),
            Some("default"),
        )?
    };

    // URL
    let url = if let Some(val) = prefill_url {
        val
    } else {
        prompt_required(
            "n8n URL",
            sym_dim("e.g., https://n8n.example.com").as_str(),
            None,
        )?
    };

    // API key
    let token = if let Some(val) = prefill_token {
        Some(val)
    } else {
        eprintln!(
            "  {}",
            sym_dim("Get your API key from n8n -> Settings -> API -> Create API Key")
        );
        let raw = prompt("API key", "", None)?;
        if raw.trim().is_empty() {
            None
        } else {
            Some(raw)
        }
    };

    eprintln!();
    Ok((instance, url, token))
}

/// Validate credentials by fetching one workflow from the n8n API.
async fn validate_credentials(base_url: &str, token: &str) -> Result<(), AppError> {
    use std::io::Write;

    let is_tty = std::io::stderr().is_terminal();
    if is_tty {
        eprint!("  Verifying credentials...");
        std::io::stderr().flush().ok();
    }

    let instance = InstanceConfig {
        base_url: base_url.to_string(),
        api_version: "v1".to_string(),
        execute: None,
    };

    let client = ApiClient::new("init", &instance, token.to_string())?;
    let result = client
        .list_workflows(&ListOptions {
            limit: 1,
            active: None,
            name_filter: None,
        })
        .await;

    match result {
        Ok(workflows) => {
            if is_tty {
                eprintln!(
                    " {} Connected ({} workflow(s) accessible)",
                    sym_ok(),
                    workflows.len()
                );
            }
            Ok(())
        }
        Err(err) => {
            if is_tty {
                eprintln!(" {} {}", sym_fail(), err.message);
                eprintln!();
                let save = prompt("Save config anyway?", sym_dim("[y/N]").as_str(), Some("n"))?;
                if !save.trim().eq_ignore_ascii_case("y") {
                    return Err(AppError::auth(
                        "init",
                        "Credential verification failed and config was not saved.",
                    ));
                }
            } else {
                return Err(err);
            }
            Ok(())
        }
    }
}

// ── Prompt helpers ────────────────────────────────────────────────────────

fn prompt(label: &str, hint: &str, default: Option<&str>) -> Result<String, AppError> {
    use std::io::{self, Write};
    let hint_part = if hint.is_empty() {
        String::new()
    } else {
        format!("  {hint}")
    };
    let default_part = match default {
        Some(d) if !d.is_empty() => format!(" [{d}]"),
        _ => String::new(),
    };
    eprint!("{} {label}{hint_part}{default_part}: ", sym_q());
    io::stderr()
        .flush()
        .map_err(|err| AppError::config("init", format!("Failed to flush stderr: {err}")))?;
    let mut buf = String::new();
    io::stdin()
        .read_line(&mut buf)
        .map_err(|err| AppError::config("init", format!("Failed to read input: {err}")))?;
    let trimmed = buf.trim().to_owned();
    if trimmed.is_empty() {
        Ok(default.unwrap_or("").to_owned())
    } else {
        Ok(trimmed)
    }
}

fn prompt_required(label: &str, hint: &str, default: Option<&str>) -> Result<String, AppError> {
    loop {
        let value = prompt(label, hint, default)?;
        if !value.trim().is_empty() {
            return Ok(value);
        }
        eprintln!("  {} {label} is required.", sym_fail());
    }
}

// ── Color / symbol helpers ────────────────────────────────────────────────

fn sym_q() -> String {
    if use_color() {
        "?".green().bold().to_string()
    } else {
        "?".to_owned()
    }
}

fn sym_ok() -> String {
    if use_color() {
        "\u{2714}".green().to_string()
    } else {
        "\u{2714}".to_owned()
    }
}

fn sym_fail() -> String {
    if use_color() {
        "\u{2716}".red().to_string()
    } else {
        "\u{2716}".to_owned()
    }
}

fn sym_dim(s: &str) -> String {
    if use_color() {
        s.dimmed().to_string()
    } else {
        s.to_owned()
    }
}
