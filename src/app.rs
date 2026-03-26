use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use chrono::DateTime;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    api::{ApiClient, ExecutionListOptions, ListOptions},
    auth::{
        ensure_alias_exists, list_auth_statuses, read_token_from_stdin, remove_token,
        resolve_token, store_token,
    },
    canonical::{canonicalize_workflow, hash_value, pretty_json},
    cli::{
        AuthAddArgs, AuthAliasArgs, AuthArgs, AuthCommand, Cli, Command, DiffArgs, FmtArgs,
        GetArgs, IdArgs, InitArgs, ListArgs, PullArgs, PushArgs, RunsArgs, RunsCommand,
        RunsGetArgs, RunsListArgs, RunsWatchArgs, StatusArgs, TriggerArgs, ValidateArgs,
    },
    config::{
        InstanceConfig, LoadedRepo, RepoConfig, ensure_gitignore, ensure_repo_layout, load_repo,
        resolve_instance_alias, save_repo_config,
    },
    error::AppError,
    repo::{
        LocalWorkflowState, RemoteSyncState, build_local_diff, build_refreshed_diff,
        collect_json_targets, format_json_file, load_meta, load_workflow_file,
        refresh_local_status, scan_local_status, sidecar_path_for, store_workflow, workflow_active,
        workflow_id, workflow_name, workflow_updated_at,
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

#[derive(Debug, Clone, Serialize)]
struct ExecutionListRow {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stopped_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_till: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct ExecutionNodeRow {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_time_ms: Option<i64>,
    output_items: usize,
}

#[derive(Debug, Serialize)]
struct AuthListRow {
    alias: String,
    base_url: String,
    token_source: String,
}

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
        Command::Runs(args) => cmd_runs(&context, args).await,
        Command::Pull(args) => cmd_pull(&context, args).await,
        Command::Push(args) => cmd_push(&context, args).await,
        Command::Status(args) => cmd_status(&context, args).await,
        Command::Diff(args) => cmd_diff(&context, args).await,
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

async fn cmd_runs(context: &Context, args: RunsArgs) -> Result<(), AppError> {
    match args.command {
        RunsCommand::Ls(args) => cmd_runs_ls(context, args).await,
        RunsCommand::Get(args) => cmd_runs_get(context, args).await,
        RunsCommand::Watch(args) => cmd_runs_watch(context, args).await,
    }
}

async fn cmd_runs_ls(context: &Context, args: RunsListArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "runs")?;
    let workflow_id = resolve_execution_workflow_id(&client, args.workflow.as_deref()).await?;
    let rows = fetch_execution_rows(
        &client,
        workflow_id.as_deref(),
        args.status.as_deref(),
        args.limit,
    )
    .await?;

    if context.json {
        emit_json("runs", &json!({ "count": rows.len(), "executions": rows }))
    } else {
        print_execution_rows(&rows);
        Ok(())
    }
}

async fn cmd_runs_watch(context: &Context, args: RunsWatchArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "runs")?;
    let (client, _, _) = remote_client(&repo, Some(&alias), "runs")?;
    let workflow_id = resolve_execution_workflow_id(&client, args.workflow.as_deref()).await?;
    let mut known_ids = BTreeSet::new();
    let mut poll = 0u32;

    loop {
        poll += 1;
        let rows = fetch_execution_rows(
            &client,
            workflow_id.as_deref(),
            args.status.as_deref(),
            args.limit,
        )
        .await?;
        let new_rows = note_new_executions(&rows, &mut known_ids);
        let event = if poll == 1 {
            "snapshot"
        } else if new_rows.is_empty() {
            "heartbeat"
        } else {
            "update"
        };

        if context.json {
            emit_json_line(
                "runs",
                &json!({
                    "event": event,
                    "poll": poll,
                    "interval_seconds": args.interval.max(1),
                    "count": rows.len(),
                    "new_count": new_rows.len(),
                    "executions": rows,
                    "new_executions": new_rows,
                }),
            )?;
        } else if poll == 1 {
            println!(
                "Watching executions on `{alias}` every {}s. Press Ctrl-C to stop.",
                args.interval.max(1)
            );
            if let Some(workflow) = args.workflow.as_deref() {
                println!("Workflow filter: {workflow}");
            }
            if let Some(status) = args.status.as_deref() {
                println!("Status filter: {status}");
            }
            if rows.is_empty() {
                println!("No executions found.");
            } else {
                println!("Current executions:");
                print_execution_rows(&rows);
            }
        } else if !new_rows.is_empty() {
            println!();
            println!("New executions:");
            print_execution_rows(&new_rows);
        }

        if args.iterations.is_some_and(|iterations| poll >= iterations) {
            break;
        }

        thread::sleep(Duration::from_secs(args.interval.max(1)));
    }

    Ok(())
}

async fn cmd_runs_get(context: &Context, args: RunsGetArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "runs")?;
    let execution = client
        .get_execution(&args.execution_id, args.details)
        .await?
        .ok_or_else(|| {
            AppError::not_found(
                "runs",
                format!("Execution `{}` was not found.", args.execution_id),
            )
        })?;

    if context.json {
        emit_json("runs", &json!({ "execution": execution }))
    } else {
        let workflow_id = value_string(&execution, "workflowId");
        let workflow_name = workflow_name_for_execution(&client, &execution).await?;
        println!(
            "Execution: {}",
            value_string(&execution, "id").unwrap_or(args.execution_id)
        );
        if let Some(status) = value_string(&execution, "status") {
            println!("Status: {status}");
        }
        if let Some(mode) = value_string(&execution, "mode") {
            println!("Mode: {mode}");
        }
        match (workflow_name.as_deref(), workflow_id.as_deref()) {
            (Some(name), Some(id)) => println!("Workflow: {name} ({id})"),
            (Some(name), None) => println!("Workflow: {name}"),
            (None, Some(id)) => println!("Workflow ID: {id}"),
            (None, None) => {}
        }
        if let Some(started_at) = value_string(&execution, "startedAt") {
            println!("Started: {started_at}");
        }
        if let Some(stopped_at) = value_string(&execution, "stoppedAt") {
            println!("Stopped: {stopped_at}");
        }
        if let Some(wait_till) = value_string(&execution, "waitTill") {
            println!("Wait Till: {wait_till}");
        }
        if let Some(duration_ms) = execution_duration_ms(&execution) {
            println!("Duration: {}", format_duration(Some(duration_ms)));
        }

        if args.details {
            let nodes = execution_node_rows(&execution);
            if !nodes.is_empty() {
                println!();
                println!(
                    "{:<32} {:<10} {:<10} {}",
                    "NODE", "STATUS", "TIME", "OUTPUTS"
                );
                for node in nodes {
                    println!(
                        "{:<32} {:<10} {:<10} {}",
                        truncate(&node.name, 32),
                        truncate(node.status.as_deref().unwrap_or("-"), 10),
                        truncate(&format_duration(node.execution_time_ms), 10),
                        node.output_items
                    );
                }
            }
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

async fn cmd_status(context: &Context, args: StatusArgs) -> Result<(), AppError> {
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
                "{:<14} {:<14} {:<14} {:<20} {:<20} {}",
                "LOCAL", "SYNC", "INSTANCE", "ID", "LOCAL HASH", "FILE"
            );
        } else {
            println!(
                "{:<14} {:<14} {:<20} {:<20} {}",
                "STATE", "INSTANCE", "ID", "LOCAL HASH", "FILE"
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

async fn cmd_diff(context: &Context, args: DiffArgs) -> Result<(), AppError> {
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
    let diff = if args.refresh {
        let local = build_local_diff(&repo, &file)?;
        if let (Some(instance), Some(workflow_id)) = (
            local.status.instance.as_deref(),
            local.status.workflow_id.as_deref(),
        ) {
            let client = client_for_instance(&repo, instance, "diff", &mut BTreeMap::new())?;
            let remote = client.get_workflow_by_id(workflow_id).await?;
            let remote_workflow = remote
                .as_ref()
                .map(|value| value.get("data").unwrap_or(value));
            build_refreshed_diff("diff", &repo, &file, remote_workflow)?
        } else {
            local
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

async fn workflow_names_for_executions(
    client: &ApiClient,
    executions: &[Value],
) -> Result<BTreeMap<String, String>, AppError> {
    let mut names = BTreeMap::new();
    for workflow_id in executions
        .iter()
        .filter_map(|execution| value_string(execution, "workflowId"))
    {
        if names.contains_key(&workflow_id) {
            continue;
        }
        let Some(workflow) = client.get_workflow_by_id(&workflow_id).await? else {
            continue;
        };
        let workflow = workflow.get("data").unwrap_or(&workflow);
        if let Some(name) = workflow_name(workflow) {
            names.insert(workflow_id, name);
        }
    }
    Ok(names)
}

async fn resolve_execution_workflow_id(
    client: &ApiClient,
    workflow: Option<&str>,
) -> Result<Option<String>, AppError> {
    let Some(identifier) = workflow else {
        return Ok(None);
    };
    let workflow = client.resolve_workflow(identifier).await?;
    Ok(workflow_id(&workflow))
}

async fn fetch_execution_rows(
    client: &ApiClient,
    workflow_id: Option<&str>,
    status: Option<&str>,
    limit: u16,
) -> Result<Vec<ExecutionListRow>, AppError> {
    let executions = client
        .list_executions(&ExecutionListOptions {
            limit: limit.clamp(1, 250),
            workflow_id: workflow_id.map(ToOwned::to_owned),
            status: status.map(ToOwned::to_owned),
        })
        .await?;
    let workflow_names = workflow_names_for_executions(client, &executions).await?;

    Ok(executions
        .into_iter()
        .map(|execution| {
            let workflow_id = value_string(&execution, "workflowId");
            ExecutionListRow {
                id: value_string(&execution, "id").unwrap_or_default(),
                workflow_name: workflow_id
                    .as_ref()
                    .and_then(|id| workflow_names.get(id).cloned()),
                workflow_id,
                status: value_string(&execution, "status"),
                mode: value_string(&execution, "mode"),
                started_at: value_string(&execution, "startedAt"),
                stopped_at: value_string(&execution, "stoppedAt"),
                wait_till: value_string(&execution, "waitTill"),
                duration_ms: execution_duration_ms(&execution),
            }
        })
        .collect())
}

async fn workflow_name_for_execution(
    client: &ApiClient,
    execution: &Value,
) -> Result<Option<String>, AppError> {
    if let Some(name) = execution
        .get("workflowData")
        .and_then(|workflow| workflow.get("name"))
        .and_then(Value::as_str)
    {
        return Ok(Some(name.to_string()));
    }

    let Some(workflow_id) = value_string(execution, "workflowId") else {
        return Ok(None);
    };
    let Some(workflow) = client.get_workflow_by_id(&workflow_id).await? else {
        return Ok(None);
    };
    Ok(workflow_name(workflow.get("data").unwrap_or(&workflow)))
}

fn execution_node_rows(execution: &Value) -> Vec<ExecutionNodeRow> {
    let Some(run_data) = execution
        .get("data")
        .and_then(|data| data.get("resultData"))
        .and_then(|result| result.get("runData"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    for (name, runs) in run_data {
        let Some(last_run) = runs.as_array().and_then(|entries| entries.last()) else {
            continue;
        };
        rows.push(ExecutionNodeRow {
            name: name.clone(),
            status: value_string(last_run, "executionStatus")
                .or_else(|| value_string(last_run, "status")),
            execution_time_ms: last_run.get("executionTime").and_then(Value::as_i64),
            output_items: count_output_items(
                last_run
                    .get("data")
                    .and_then(|data| data.get("main"))
                    .unwrap_or(&Value::Null),
            ),
        });
    }
    rows
}

fn count_output_items(main: &Value) -> usize {
    main.as_array()
        .map(|branches| {
            branches
                .iter()
                .map(|branch| branch.as_array().map_or(0, Vec::len))
                .sum()
        })
        .unwrap_or(0)
}

fn execution_duration_ms(execution: &Value) -> Option<i64> {
    let started = value_string(execution, "startedAt")?;
    let stopped = value_string(execution, "stoppedAt")?;
    let started = DateTime::parse_from_rfc3339(&started).ok()?;
    let stopped = DateTime::parse_from_rfc3339(&stopped).ok()?;
    Some((stopped - started).num_milliseconds())
}

fn format_duration(duration_ms: Option<i64>) -> String {
    let Some(duration_ms) = duration_ms else {
        return "-".to_string();
    };
    if duration_ms < 1_000 {
        format!("{duration_ms}ms")
    } else if duration_ms < 60_000 {
        format!("{:.2}s", duration_ms as f64 / 1_000.0)
    } else {
        format!("{:.2}m", duration_ms as f64 / 60_000.0)
    }
}

fn execution_workflow_label(row: &ExecutionListRow) -> String {
    match (row.workflow_name.as_deref(), row.workflow_id.as_deref()) {
        (Some(name), Some(id)) => format!("{name} ({id})"),
        (Some(name), None) => name.to_string(),
        (None, Some(id)) => id.to_string(),
        (None, None) => "-".to_string(),
    }
}

fn print_execution_rows(rows: &[ExecutionListRow]) {
    println!(
        "{:<10} {:<10} {:<10} {:<10} {:<24} {}",
        "ID", "STATUS", "MODE", "DURATION", "STARTED", "WORKFLOW"
    );
    for row in rows {
        println!(
            "{:<10} {:<10} {:<10} {:<10} {:<24} {}",
            truncate(&row.id, 10),
            truncate(row.status.as_deref().unwrap_or("-"), 10),
            truncate(row.mode.as_deref().unwrap_or("-"), 10),
            truncate(&format_duration(row.duration_ms), 10),
            truncate(row.started_at.as_deref().unwrap_or("-"), 24),
            execution_workflow_label(row)
        );
    }
}

fn note_new_executions(
    rows: &[ExecutionListRow],
    known_ids: &mut BTreeSet<String>,
) -> Vec<ExecutionListRow> {
    let mut new_rows = Vec::new();
    for row in rows {
        if known_ids.insert(row.id.clone()) {
            new_rows.push(row.clone());
        }
    }
    new_rows
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
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

fn emit_json_line<T: Serialize>(command: &'static str, data: &T) -> Result<(), AppError> {
    let envelope = Envelope {
        ok: true,
        command,
        version: env!("CARGO_PKG_VERSION"),
        contract_version: 1,
        data,
    };
    let rendered = serde_json::to_string(&envelope).map_err(|err| {
        AppError::api(
            command,
            "output.serialize_failed",
            format!("Failed to serialize JSON output: {err}"),
        )
    })?;
    println!("{rendered}");
    Ok(())
}

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

async fn refresh_statuses(
    repo: &LoadedRepo,
    statuses: &[crate::repo::LocalStatusEntry],
    command: &'static str,
) -> Result<Vec<crate::repo::LocalStatusEntry>, AppError> {
    let mut clients = BTreeMap::new();
    let mut refreshed = Vec::with_capacity(statuses.len());

    for status in statuses {
        if !matches!(
            status.state,
            LocalWorkflowState::Clean | LocalWorkflowState::Modified
        ) {
            refreshed.push(status.clone());
            continue;
        }

        let Some(instance) = status.instance.as_deref() else {
            refreshed.push(status.clone());
            continue;
        };
        let Some(workflow_id) = status.workflow_id.as_deref() else {
            refreshed.push(status.clone());
            continue;
        };

        let client = client_for_instance(repo, instance, command, &mut clients)?;
        let remote = client.get_workflow_by_id(workflow_id).await?;
        let remote_workflow = remote
            .as_ref()
            .map(|value| value.get("data").unwrap_or(value));
        refreshed.push(refresh_local_status(command, status, remote_workflow)?);
    }

    Ok(refreshed)
}

fn client_for_instance(
    repo: &LoadedRepo,
    instance: &str,
    command: &'static str,
    clients: &mut BTreeMap<String, ApiClient>,
) -> Result<ApiClient, AppError> {
    if let Some(client) = clients.get(instance) {
        return Ok(client.clone());
    }

    let (client, _, _) = remote_client(repo, Some(instance), command)?;
    clients.insert(instance.to_string(), client.clone());
    Ok(client)
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use std::collections::BTreeSet;

    use super::{
        ExecutionListRow, execution_duration_ms, execution_node_rows, format_duration,
        note_new_executions,
    };

    #[test]
    fn execution_duration_uses_started_and_stopped_times() {
        let execution = json!({
            "startedAt": "2026-03-26T12:00:00.000Z",
            "stoppedAt": "2026-03-26T12:00:01.250Z"
        });

        assert_eq!(execution_duration_ms(&execution), Some(1_250));
        assert_eq!(format_duration(Some(1_250)), "1.25s");
    }

    #[test]
    fn execution_node_rows_summarize_last_run_data() {
        let execution = json!({
            "data": {
                "resultData": {
                    "runData": {
                        "First Node": [
                            {
                                "executionStatus": "success",
                                "executionTime": 12,
                                "data": {
                                    "main": [
                                        [{"json": {"ok": true}}, {"json": {"ok": true}}],
                                        []
                                    ]
                                }
                            }
                        ],
                        "Second Node": [
                            {
                                "executionStatus": "error",
                                "executionTime": 3,
                                "data": {
                                    "main": [
                                        [],
                                        [{"json": {"ok": false}}]
                                    ]
                                }
                            }
                        ]
                    }
                }
            }
        });

        let rows = execution_node_rows(&execution);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "First Node");
        assert_eq!(rows[0].status.as_deref(), Some("success"));
        assert_eq!(rows[0].execution_time_ms, Some(12));
        assert_eq!(rows[0].output_items, 2);
        assert_eq!(rows[1].name, "Second Node");
        assert_eq!(rows[1].status.as_deref(), Some("error"));
        assert_eq!(rows[1].execution_time_ms, Some(3));
        assert_eq!(rows[1].output_items, 1);
    }

    #[test]
    fn note_new_executions_only_returns_unseen_rows() {
        let rows = vec![
            ExecutionListRow {
                id: "101".to_string(),
                workflow_id: Some("wf-1".to_string()),
                workflow_name: Some("Alpha".to_string()),
                status: Some("success".to_string()),
                mode: Some("trigger".to_string()),
                started_at: Some("2026-03-26T12:00:00.000Z".to_string()),
                stopped_at: Some("2026-03-26T12:00:00.100Z".to_string()),
                wait_till: None,
                duration_ms: Some(100),
            },
            ExecutionListRow {
                id: "100".to_string(),
                workflow_id: Some("wf-1".to_string()),
                workflow_name: Some("Alpha".to_string()),
                status: Some("success".to_string()),
                mode: Some("trigger".to_string()),
                started_at: Some("2026-03-26T11:59:00.000Z".to_string()),
                stopped_at: Some("2026-03-26T11:59:00.100Z".to_string()),
                wait_till: None,
                duration_ms: Some(100),
            },
        ];
        let mut known_ids = BTreeSet::from(["100".to_string()]);

        let new_rows = note_new_executions(&rows, &mut known_ids);

        assert_eq!(new_rows.len(), 1);
        assert_eq!(new_rows[0].id, "101");
        assert!(known_ids.contains("100"));
        assert!(known_ids.contains("101"));
    }
}
