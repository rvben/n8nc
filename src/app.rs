use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    api::{ApiClient, ListOptions},
    auth::{
        ensure_alias_exists, list_auth_statuses, read_token_from_stdin, remove_token,
        resolve_token, store_token,
    },
    canonical::{canonicalize_workflow, hash_value, pretty_json},
    cli::{
        AuthAddArgs, AuthAliasArgs, AuthArgs, AuthCommand, Cli, Command, FmtArgs, GetArgs, IdArgs,
        InitArgs, ListArgs, PullArgs, PushArgs, TriggerArgs, ValidateArgs,
    },
    config::{
        InstanceConfig, LoadedRepo, RepoConfig, ensure_gitignore, ensure_repo_layout, load_repo,
        resolve_instance_alias, save_repo_config,
    },
    error::AppError,
    repo::{
        collect_json_targets, format_json_file, load_meta, load_workflow_file, sidecar_path_for,
        store_workflow, workflow_active, workflow_id, workflow_name, workflow_updated_at,
    },
    validate::validate_workflow_path,
};

#[derive(Debug, Clone)]
struct Context {
    json: bool,
    repo_root: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct Envelope<T: Serialize> {
    ok: bool,
    command: &'static str,
    version: &'static str,
    contract_version: u32,
    data: T,
}

#[derive(Debug, Serialize)]
struct WorkflowListRow {
    id: String,
    name: String,
    active: Option<bool>,
    updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct AuthListRow {
    alias: String,
    base_url: String,
    token_source: String,
}

pub async fn run(cli: Cli) -> Result<(), AppError> {
    let context = Context {
        json: cli.json,
        repo_root: cli.repo_root,
    };

    match cli.command {
        Command::Init(args) => cmd_init(&context, args).await,
        Command::Auth(args) => cmd_auth(&context, args).await,
        Command::Ls(args) => cmd_ls(&context, args).await,
        Command::Get(args) => cmd_get(&context, args).await,
        Command::Pull(args) => cmd_pull(&context, args).await,
        Command::Push(args) => cmd_push(&context, args).await,
        Command::Activate(args) => cmd_activation(&context, args, true).await,
        Command::Deactivate(args) => cmd_activation(&context, args, false).await,
        Command::Trigger(args) => cmd_trigger(&context, args).await,
        Command::Fmt(args) => cmd_fmt(&context, args).await,
        Command::Validate(args) => cmd_validate(&context, args).await,
    }
}

async fn cmd_init(context: &Context, args: InitArgs) -> Result<(), AppError> {
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

    let mut instances = std::collections::BTreeMap::new();
    instances.insert(
        args.instance.clone(),
        InstanceConfig {
            base_url: args.url.trim_end_matches('/').to_string(),
            api_version: "v1".to_string(),
        },
    );
    let config = RepoConfig {
        schema_version: 1,
        default_instance: args.instance,
        workflow_dir: args.workflow_dir,
        instances,
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
        println!("Initialized n8n repo at {}", root.display());
        println!("Config: {}", root.join("n8n.toml").display());
        println!("Workflow dir: {}", root.join(config.workflow_dir).display());
        Ok(())
    }
}

async fn cmd_auth(context: &Context, args: AuthArgs) -> Result<(), AppError> {
    match args.command {
        AuthCommand::Add(args) => cmd_auth_add(context, args).await,
        AuthCommand::Test(args) => cmd_auth_test(context, args).await,
        AuthCommand::List => cmd_auth_list(context).await,
        AuthCommand::Remove(args) => cmd_auth_remove(context, args).await,
    }
}

async fn cmd_auth_add(context: &Context, args: AuthAddArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    let token = match (args.token, args.stdin) {
        (Some(token), false) => token,
        (None, true) => read_token_from_stdin()?,
        (None, false) => {
            return Err(AppError::usage(
                "auth",
                "Provide a token with `--token` or pipe it with `--stdin`.",
            ));
        }
        (Some(_), true) => unreachable!(),
    };

    store_token(&alias, &token)?;
    if context.json {
        emit_json("auth", &json!({"alias": alias, "stored": true}))
    } else {
        println!("Stored token for `{alias}`.");
        Ok(())
    }
}

async fn cmd_auth_test(context: &Context, args: AuthAliasArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    let (client, token_source, base_url) = remote_client(&repo, Some(&alias), "auth")?;
    let workflows = client
        .list_workflows(&ListOptions {
            limit: 1,
            active: None,
            name_filter: None,
        })
        .await?;

    let data = json!({
        "alias": alias,
        "base_url": base_url,
        "token_source": token_source,
        "reachable": true,
        "sample_count": workflows.len(),
    });
    if context.json {
        emit_json("auth", &data)
    } else {
        println!("Alias: {}", data["alias"].as_str().unwrap_or_default());
        println!(
            "Base URL: {}",
            data["base_url"].as_str().unwrap_or_default()
        );
        println!(
            "Token source: {}",
            data["token_source"].as_str().unwrap_or_default()
        );
        println!("API reachable: yes");
        Ok(())
    }
}

async fn cmd_auth_list(context: &Context) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let rows: Vec<AuthListRow> = list_auth_statuses(&repo)
        .into_iter()
        .map(|status| AuthListRow {
            alias: status.alias,
            base_url: status.base_url,
            token_source: status.token_source,
        })
        .collect();

    if context.json {
        emit_json("auth", &json!({ "instances": rows }))
    } else {
        println!("{:<16} {:<10} {}", "ALIAS", "TOKEN", "BASE URL");
        for row in rows {
            println!(
                "{:<16} {:<10} {}",
                row.alias, row.token_source, row.base_url
            );
        }
        Ok(())
    }
}

async fn cmd_auth_remove(context: &Context, args: AuthAliasArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    remove_token(&alias)?;
    if context.json {
        emit_json("auth", &json!({"alias": alias, "removed": true}))
    } else {
        println!("Removed token for `{alias}`.");
        Ok(())
    }
}

async fn cmd_ls(context: &Context, args: ListArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "ls")?;
    let workflows = client
        .list_workflows(&ListOptions {
            limit: args.limit.min(250),
            active: if args.active {
                Some(true)
            } else if args.inactive {
                Some(false)
            } else {
                None
            },
            name_filter: args.name,
        })
        .await?;

    let rows: Vec<WorkflowListRow> = workflows
        .into_iter()
        .map(|workflow| WorkflowListRow {
            id: workflow_id(&workflow).unwrap_or_default(),
            name: workflow_name(&workflow).unwrap_or_else(|| "<unnamed>".to_string()),
            active: workflow_active(&workflow),
            updated_at: workflow_updated_at(&workflow),
        })
        .collect();

    if context.json {
        emit_json("ls", &json!({ "count": rows.len(), "workflows": rows }))
    } else {
        println!("{:<20} {:<8} {:<24} {}", "ID", "ACTIVE", "UPDATED", "NAME");
        for row in rows {
            println!(
                "{:<20} {:<8} {:<24} {}",
                truncate(&row.id, 20),
                row.active
                    .map(|value| if value { "true" } else { "false" })
                    .unwrap_or("-"),
                row.updated_at.unwrap_or_else(|| "-".to_string()),
                row.name
            );
        }
        Ok(())
    }
}

async fn cmd_get(context: &Context, args: GetArgs) -> Result<(), AppError> {
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

async fn cmd_pull(context: &Context, args: PullArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "pull")?;
    let (client, _, _) = remote_client(&repo, Some(&alias), "pull")?;
    let workflow = client.resolve_workflow(&args.identifier).await?;
    let stored = store_workflow(&repo, &alias, &workflow)?;

    if context.json {
        emit_json(
            "pull",
            &json!({
                "instance": alias,
                "workflow_path": stored.workflow_path,
                "meta_path": stored.meta_path,
                "workflow_id": stored.meta.workflow_id,
            }),
        )
    } else {
        println!(
            "Pulled {} -> {}",
            stored.meta.workflow_id,
            stored.workflow_path.display()
        );
        println!("Metadata: {}", stored.meta_path.display());
        Ok(())
    }
}

async fn cmd_push(context: &Context, args: PushArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let workflow_path = absolutize(&repo.root, &args.file);
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
    let remote_hash = hash_value(&canonicalize_workflow(&remote_workflow)?)?;
    let local_hash = hash_value(&canonical)?;

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
        println!("No changes to push for {}.", meta.workflow_id);
        return Ok(());
    }

    let updated = client
        .update_workflow(&meta.workflow_id, &canonical)
        .await?;
    let stored = store_workflow(&repo, &alias, &updated)?;

    if context.json {
        emit_json(
            "push",
            &json!({
                "workflow_id": meta.workflow_id,
                "changed": true,
                "workflow_path": stored.workflow_path,
                "meta_path": stored.meta_path,
            }),
        )
    } else {
        println!("Pushed {}.", meta.workflow_id);
        println!("Updated local file: {}", stored.workflow_path.display());
        Ok(())
    }
}

async fn cmd_activation(context: &Context, args: IdArgs, active: bool) -> Result<(), AppError> {
    let command = if active { "activate" } else { "deactivate" };
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), command)?;
    let workflow = client.resolve_workflow(&args.identifier).await?;
    let workflow_id = workflow_id(&workflow).ok_or_else(|| {
        AppError::api(
            command,
            "api.invalid_response",
            "Workflow payload was missing `id`.",
        )
    })?;

    if active {
        client.activate_workflow(&workflow_id).await?;
    } else {
        client.deactivate_workflow(&workflow_id).await?;
    }

    if context.json {
        emit_json(
            command,
            &json!({"workflow_id": workflow_id, "active": active}),
        )
    } else {
        println!(
            "{} {}.",
            if active { "Activated" } else { "Deactivated" },
            workflow_id
        );
        Ok(())
    }
}

async fn cmd_trigger(context: &Context, args: TriggerArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "trigger")?;
    let headers = parse_pairs("trigger", "header", &args.headers, ':')?;
    let query = parse_pairs("trigger", "query", &args.query, '=')?;
    let body = read_request_body(args.data, args.data_file, args.stdin)?;
    let response = client
        .trigger(&args.target, &args.method, &headers, &query, body)
        .await?;

    if context.json {
        emit_json("trigger", &response)
    } else {
        println!("HTTP {}", response.status);
        print_response_body(&response.body)?;
        Ok(())
    }
}

async fn cmd_fmt(context: &Context, args: FmtArgs) -> Result<(), AppError> {
    let repo = load_repo(context.repo_root.as_deref()).ok();
    let files = collect_json_targets(&args.paths, repo.as_ref())?;
    let mut changed = Vec::new();
    for file in files {
        let formatted = format_json_file(&file)?;
        let current = fs::read_to_string(&file).map_err(|err| {
            AppError::validation("fmt", format!("Failed to read {}: {err}", file.display()))
        })?;
        if current != formatted {
            changed.push(file.clone());
            if !args.check {
                fs::write(&file, formatted).map_err(|err| {
                    AppError::validation(
                        "fmt",
                        format!("Failed to write {}: {err}", file.display()),
                    )
                })?;
            }
        }
    }

    if args.check && !changed.is_empty() {
        return Err(AppError::validation(
            "fmt",
            format!("{} file(s) would be reformatted.", changed.len()),
        ));
    }

    if context.json {
        emit_json(
            "fmt",
            &json!({"changed": changed.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>() }),
        )
    } else {
        if changed.is_empty() {
            println!("All files are already formatted.");
        } else if args.check {
            println!("{} file(s) would be reformatted.", changed.len());
        } else {
            println!("Formatted {} file(s).", changed.len());
        }
        Ok(())
    }
}

async fn cmd_validate(context: &Context, args: ValidateArgs) -> Result<(), AppError> {
    let repo = load_repo(context.repo_root.as_deref()).ok();
    let files = collect_json_targets(&args.paths, repo.as_ref())?;
    let workflow_files: Vec<_> = files
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|name| name.ends_with(".workflow.json"))
                .unwrap_or(false)
        })
        .collect();

    let mut diagnostics = Vec::new();
    for file in &workflow_files {
        diagnostics.extend(validate_workflow_path(file)?);
    }

    let error_count = diagnostics
        .iter()
        .filter(|diag| diag.severity == crate::validate::Severity::Error)
        .count();

    if context.json {
        if error_count > 0 {
            return Err(AppError::validation(
                "validate",
                format!("Validation failed with {error_count} error(s)."),
            )
            .with_json_data(json!({
                "files_checked": workflow_files.len(),
                "error_count": error_count,
                "diagnostics": diagnostics,
            })));
        }

        emit_json(
            "validate",
            &json!({
                "files_checked": workflow_files.len(),
                "error_count": error_count,
                "diagnostics": diagnostics,
            }),
        )?;
    } else if diagnostics.is_empty() {
        println!(
            "Validated {} workflow file(s): 0 errors.",
            workflow_files.len()
        );
    } else {
        for diagnostic in &diagnostics {
            let path = diagnostic.path.as_deref().unwrap_or("-");
            println!(
                "[error] {} {} {}",
                diagnostic.file, path, diagnostic.message
            );
        }
        println!(
            "Validated {} workflow file(s): {} error(s).",
            workflow_files.len(),
            error_count
        );
    }

    if error_count > 0 {
        Err(AppError::validation(
            "validate",
            format!("Validation failed with {error_count} error(s)."),
        ))
    } else {
        Ok(())
    }
}

fn load_loaded_repo(context: &Context) -> Result<LoadedRepo, AppError> {
    load_repo(context.repo_root.as_deref())
}

fn remote_client(
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

fn parse_pairs(
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

fn read_request_body(
    data: Option<String>,
    data_file: Option<PathBuf>,
    stdin: bool,
) -> Result<Option<Vec<u8>>, AppError> {
    if let Some(data) = data {
        return Ok(Some(data.into_bytes()));
    }
    if let Some(path) = data_file {
        return fs::read(&path).map(Some).map_err(|err| {
            AppError::usage(
                "trigger",
                format!("Failed to read {}: {err}", path.display()),
            )
        });
    }
    if stdin {
        let mut buffer = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buffer)
            .map_err(|err| AppError::usage("trigger", format!("Failed to read stdin: {err}")))?;
        return Ok(Some(buffer));
    }
    Ok(None)
}

fn emit_json<T: Serialize>(command: &'static str, data: &T) -> Result<(), AppError> {
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

fn truncate(input: &str, width: usize) -> String {
    if input.len() <= width {
        input.to_string()
    } else {
        format!("{}...", &input[..width.saturating_sub(3)])
    }
}

fn print_response_body(value: &Value) -> Result<(), AppError> {
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

fn absolutize(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}
