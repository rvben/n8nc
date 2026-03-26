use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
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
        AuthAddArgs, AuthAliasArgs, AuthArgs, AuthCommand, Cli, Command, ConnAddArgs, ConnArgs,
        ConnCommand, ConnRemoveArgs, CredentialArgs, CredentialCommand, CredentialSetArgs,
        DiffArgs, DoctorArgs, ExprArgs, ExprCommand, ExprSetArgs, FmtArgs, GetArgs, IdArgs,
        InitArgs, ListArgs, NodeAddArgs, NodeArgs, NodeCommand, NodeListArgs, NodeRemoveArgs,
        NodeRenameArgs, NodeSetArgs, PullArgs, PushArgs, RunsArgs, RunsCommand, RunsGetArgs,
        RunsListArgs, RunsTimeArgs, RunsWatchArgs, StatusArgs, TriggerArgs, ValidateArgs,
        ValueModeArgs, WorkflowArgs, WorkflowCommand, WorkflowCreateArgs, WorkflowNewArgs,
        WorkflowShowArgs,
    },
    config::{
        InstanceConfig, LoadedRepo, RepoConfig, ensure_gitignore, ensure_repo_layout, load_repo,
        resolve_instance_alias, save_repo_config, workflow_dir,
    },
    edit::{
        EditResult, add_connection, add_node, create_workflow, default_workflow_file_name,
        default_workflow_settings, remove_connection, remove_node, rename_node,
        set_credential_reference, set_node_expression, set_node_value, workflow_id_string,
    },
    error::AppError,
    repo::{
        LocalWorkflowState, RemoteSyncState, build_local_diff, build_refreshed_diff,
        collect_json_targets, format_json_file, load_meta, load_workflow_file,
        refresh_local_status, scan_local_status, sidecar_path_for, store_workflow, workflow_active,
        workflow_id, workflow_name, workflow_updated_at,
    },
    validate::{Severity, sensitive_data_diagnostics, validate_workflow_path},
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
struct WorkflowNodeRow {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    type_version: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowConnectionRow {
    from: String,
    kind: String,
    output_index: usize,
    to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowWebhookRow {
    node: String,
    methods: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    webhook_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    production_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_url: Option<String>,
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

#[derive(Debug, Clone)]
struct RunsTimeFilter {
    since: Option<DateTime<Utc>>,
    last: Option<ChronoDuration>,
    last_label: Option<String>,
}

#[derive(Debug, Serialize)]
struct AuthListRow {
    alias: String,
    base_url: String,
    token_source: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DoctorCheckStatus {
    Ok,
    Fail,
    Skip,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    status: DoctorCheckStatus,
    scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    name: String,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestion: Option<String>,
}

#[derive(Debug, Serialize)]
struct DoctorSummary {
    ok: usize,
    fail: usize,
    skip: usize,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    repo_root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_instance: Option<String>,
    checks: Vec<DoctorCheck>,
    summary: DoctorSummary,
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

const WEBHOOK_NODE_TYPE: &str = "n8n-nodes-base.webhook";

pub async fn run(cli: Cli) -> Result<(), AppError> {
    let context = Context {
        json: cli.json,
        repo_root: cli.repo_root,
    };

    match cli.command {
        Command::Init(args) => cmd_init(&context, args).await,
        Command::Doctor(args) => cmd_doctor(&context, args).await,
        Command::Auth(args) => cmd_auth(&context, args).await,
        Command::Ls(args) => cmd_ls(&context, args).await,
        Command::Get(args) => cmd_get(&context, args).await,
        Command::Runs(args) => cmd_runs(&context, args).await,
        Command::Pull(args) => cmd_pull(&context, args).await,
        Command::Push(args) => cmd_push(&context, args).await,
        Command::Workflow(args) => cmd_workflow(&context, args).await,
        Command::Node(args) => cmd_node(&context, args).await,
        Command::Conn(args) => cmd_conn(&context, args).await,
        Command::Expr(args) => cmd_expr(&context, args).await,
        Command::Credential(args) => cmd_credential(&context, args).await,
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

async fn cmd_doctor(context: &Context, args: DoctorArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let report = build_doctor_report(&repo, &args).await?;

    if report.summary.fail == 0 {
        if context.json {
            emit_json("doctor", &report)
        } else {
            print_doctor_report(&report);
            Ok(())
        }
    } else {
        if !context.json {
            print_doctor_report(&report);
        }
        Err(doctor_failed_error(&report)?)
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

async fn build_doctor_report(
    repo: &LoadedRepo,
    args: &DoctorArgs,
) -> Result<DoctorReport, AppError> {
    let selected_instance = args
        .remote
        .instance
        .as_deref()
        .map(|alias| ensure_alias_exists(repo, alias, "doctor"))
        .transpose()?;
    let workflow_dir = repo.root.join(&repo.config.workflow_dir);
    let cache_dir = repo.root.join(".n8n").join("cache");
    let mut checks = Vec::new();

    add_doctor_check(
        &mut checks,
        if repo.root.join("n8n.toml").is_file() {
            DoctorCheckStatus::Ok
        } else {
            DoctorCheckStatus::Fail
        },
        "repo",
        None,
        "config_file",
        format!("Config path: {}", repo.root.join("n8n.toml").display()),
        Some(
            "Run `n8nc init --instance <alias> --url <base_url>` to create the repo config."
                .to_string(),
        ),
    );
    add_doctor_check(
        &mut checks,
        if workflow_dir.is_dir() {
            DoctorCheckStatus::Ok
        } else {
            DoctorCheckStatus::Fail
        },
        "repo",
        None,
        "workflow_dir",
        format!("Workflow directory: {}", workflow_dir.display()),
        Some("Create the workflow directory or rerun `n8nc init`.".to_string()),
    );
    add_doctor_check(
        &mut checks,
        if cache_dir.is_dir() {
            DoctorCheckStatus::Ok
        } else {
            DoctorCheckStatus::Fail
        },
        "repo",
        None,
        "cache_dir",
        format!("Cache directory: {}", cache_dir.display()),
        Some("Create `.n8n/cache` or rerun `n8nc init`.".to_string()),
    );
    add_doctor_check(
        &mut checks,
        if repo.config.instances.is_empty() {
            DoctorCheckStatus::Fail
        } else {
            DoctorCheckStatus::Ok
        },
        "repo",
        None,
        "instances",
        format!("Configured instances: {}", repo.config.instances.len()),
        Some("Add at least one instance to `n8n.toml`.".to_string()),
    );
    add_doctor_check(
        &mut checks,
        if repo
            .config
            .instances
            .contains_key(&repo.config.default_instance)
        {
            DoctorCheckStatus::Ok
        } else {
            DoctorCheckStatus::Fail
        },
        "repo",
        None,
        "default_instance",
        format!("Default instance: {}", repo.config.default_instance),
        Some("Set `default_instance` to one of the configured aliases.".to_string()),
    );

    if workflow_dir.is_dir() {
        match scan_repo_for_sensitive_workflows(&workflow_dir) {
            Ok((workflow_count, warning_count, sample_files)) => {
                if workflow_count == 0 {
                    add_doctor_check(
                        &mut checks,
                        DoctorCheckStatus::Ok,
                        "repo",
                        None,
                        "sensitive_data",
                        "No tracked workflow files to scan for sensitive data.".to_string(),
                        None,
                    );
                } else if warning_count == 0 {
                    add_doctor_check(
                        &mut checks,
                        DoctorCheckStatus::Ok,
                        "repo",
                        None,
                        "sensitive_data",
                        format!(
                            "Scanned {workflow_count} workflow file(s), no sensitive literals detected."
                        ),
                        None,
                    );
                } else {
                    let sample_suffix = if sample_files.is_empty() {
                        String::new()
                    } else {
                        format!(" Example file(s): {}.", sample_files.join(", "))
                    };
                    add_doctor_check(
                        &mut checks,
                        DoctorCheckStatus::Fail,
                        "repo",
                        None,
                        "sensitive_data",
                        format!(
                            "Found {warning_count} potential sensitive-data warning(s) across {workflow_count} workflow file(s).{sample_suffix}"
                        ),
                        Some(
                            "Run `n8nc validate` to inspect the findings and move secrets into credentials or env-backed expressions."
                                .to_string(),
                        ),
                    );
                }
            }
            Err(err) => add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Fail,
                "repo",
                None,
                "sensitive_data",
                err.message,
                err.suggestion,
            ),
        }
    } else {
        add_doctor_check(
            &mut checks,
            DoctorCheckStatus::Skip,
            "repo",
            None,
            "sensitive_data",
            "Skipped sensitive-data scan because the workflow directory is missing.".to_string(),
            None,
        );
    }

    let instance_aliases: Vec<String> = match selected_instance.clone() {
        Some(alias) => vec![alias],
        None => repo.config.instances.keys().cloned().collect(),
    };

    for alias in instance_aliases {
        let Some(instance) = repo.config.instances.get(&alias) else {
            continue;
        };

        let client_ready = match ApiClient::new("doctor", instance, "doctor-probe".to_string()) {
            Ok(_) => {
                add_doctor_check(
                    &mut checks,
                    DoctorCheckStatus::Ok,
                    "instance",
                    Some(alias.clone()),
                    "config",
                    format!(
                        "Base URL {} using API version {}.",
                        instance.base_url, instance.api_version
                    ),
                    None,
                );
                true
            }
            Err(err) => {
                add_doctor_check(
                    &mut checks,
                    DoctorCheckStatus::Fail,
                    "instance",
                    Some(alias.clone()),
                    "config",
                    err.message,
                    err.suggestion,
                );
                false
            }
        };

        let token = match resolve_token(&alias, "doctor") {
            Ok((token, source)) => {
                add_doctor_check(
                    &mut checks,
                    DoctorCheckStatus::Ok,
                    "instance",
                    Some(alias.clone()),
                    "token",
                    format!("Token available via {source}."),
                    None,
                );
                Some(token)
            }
            Err(err) => {
                add_doctor_check(
                    &mut checks,
                    DoctorCheckStatus::Fail,
                    "instance",
                    Some(alias.clone()),
                    "token",
                    err.message,
                    err.suggestion,
                );
                None
            }
        };

        if args.skip_network {
            add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Skip,
                "instance",
                Some(alias.clone()),
                "api",
                "Skipped live API check because `--skip-network` was requested.",
                None,
            );
            continue;
        }

        if !client_ready {
            add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Skip,
                "instance",
                Some(alias.clone()),
                "api",
                "Skipped live API check because the instance configuration is invalid.",
                None,
            );
            continue;
        }

        let Some(token) = token else {
            add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Skip,
                "instance",
                Some(alias.clone()),
                "api",
                "Skipped live API check because no token is configured.",
                None,
            );
            continue;
        };

        let client = match ApiClient::new("doctor", instance, token) {
            Ok(client) => client,
            Err(err) => {
                add_doctor_check(
                    &mut checks,
                    DoctorCheckStatus::Fail,
                    "instance",
                    Some(alias.clone()),
                    "api",
                    err.message,
                    err.suggestion,
                );
                continue;
            }
        };

        match client
            .list_workflows(&ListOptions {
                limit: 1,
                active: None,
                name_filter: None,
            })
            .await
        {
            Ok(workflows) => add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Ok,
                "instance",
                Some(alias.clone()),
                "api",
                format!("API reachable (sample_count={}).", workflows.len()),
                None,
            ),
            Err(err) => add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Fail,
                "instance",
                Some(alias.clone()),
                "api",
                err.message,
                err.suggestion,
            ),
        }
    }

    let summary = summarize_doctor_checks(&checks);
    Ok(DoctorReport {
        repo_root: repo.root.clone(),
        selected_instance,
        checks,
        summary,
    })
}

fn add_doctor_check(
    checks: &mut Vec<DoctorCheck>,
    status: DoctorCheckStatus,
    scope: &'static str,
    target: Option<String>,
    name: impl Into<String>,
    detail: impl Into<String>,
    suggestion: Option<String>,
) {
    let suggestion = match status {
        DoctorCheckStatus::Fail => suggestion,
        DoctorCheckStatus::Ok | DoctorCheckStatus::Skip => None,
    };
    checks.push(DoctorCheck {
        status,
        scope,
        target,
        name: name.into(),
        detail: detail.into(),
        suggestion,
    });
}

fn summarize_doctor_checks(checks: &[DoctorCheck]) -> DoctorSummary {
    let mut summary = DoctorSummary {
        ok: 0,
        fail: 0,
        skip: 0,
    };
    for check in checks {
        match check.status {
            DoctorCheckStatus::Ok => summary.ok += 1,
            DoctorCheckStatus::Fail => summary.fail += 1,
            DoctorCheckStatus::Skip => summary.skip += 1,
        }
    }
    summary
}

fn doctor_failed_error(report: &DoctorReport) -> Result<AppError, AppError> {
    let plural = if report.summary.fail == 1 { "" } else { "s" };
    let data = serde_json::to_value(report).map_err(|err| {
        AppError::api(
            "doctor",
            "output.serialize_failed",
            format!("Failed to serialize doctor report: {err}"),
        )
    })?;
    Ok(AppError::new(
        13,
        "doctor",
        "doctor.failed",
        format!(
            "Doctor found {} failing check{}.",
            report.summary.fail, plural
        ),
    )
    .with_suggestion("Fix the failing checks and rerun `n8nc doctor`.")
    .with_json_data(data))
}

fn print_doctor_report(report: &DoctorReport) {
    println!("Repo root: {}", report.repo_root.display());
    if let Some(alias) = report.selected_instance.as_deref() {
        println!("Selected instance: {alias}");
    }
    println!(
        "{:<8} {:<10} {:<16} {:<18} {}",
        "STATUS", "SCOPE", "TARGET", "CHECK", "DETAIL"
    );
    for check in &report.checks {
        println!(
            "{:<8} {:<10} {:<16} {:<18} {}",
            doctor_status_label(check.status),
            check.scope,
            check.target.as_deref().unwrap_or("-"),
            truncate(&check.name, 18),
            check.detail,
        );
        if let Some(suggestion) = &check.suggestion {
            println!("  {}", suggestion);
        }
    }
    println!(
        "Summary: ok={}, fail={}, skip={}",
        report.summary.ok, report.summary.fail, report.summary.skip
    );
}

fn scan_repo_for_sensitive_workflows(
    workflow_dir: &Path,
) -> Result<(usize, usize, Vec<String>), AppError> {
    let workflow_files: Vec<PathBuf> = collect_json_targets(&[workflow_dir.to_path_buf()], None)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|name| name.ends_with(".workflow.json"))
                .unwrap_or(false)
        })
        .collect();

    let mut warning_count = 0usize;
    let mut sample_files = Vec::new();
    for file in &workflow_files {
        let warnings = sensitive_data_diagnostics(file)?;
        if !warnings.is_empty() {
            warning_count += warnings.len();
            if sample_files.len() < 3 {
                sample_files.push(
                    file.strip_prefix(workflow_dir)
                        .unwrap_or(file)
                        .display()
                        .to_string(),
                );
            }
        }
    }

    Ok((workflow_files.len(), warning_count, sample_files))
}

fn print_sensitive_warning_summary(workflow_path: &Path, warning_count: usize) {
    if warning_count == 0 {
        return;
    }

    println!(
        "Warning: found {} potential sensitive-data warning(s) in {}.",
        warning_count,
        workflow_path.display()
    );
    println!(
        "Run `n8nc validate {}` to inspect the findings.",
        workflow_path.display()
    );
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
    let time_filter = parse_runs_time_filter("runs", &args.time)?;
    let since = time_filter.effective_since();
    let rows = fetch_execution_rows(
        &client,
        workflow_id.as_deref(),
        args.status.as_deref(),
        since.clone(),
        args.limit,
    )
    .await?;
    let note = execution_history_note(&client, workflow_id.as_deref(), &rows).await?;

    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("count".to_string(), json!(rows.len()));
        data.insert("executions".to_string(), json!(rows));
        if let Some(note) = note {
            data.insert("note".to_string(), json!(note));
        }
        emit_json("runs", &Value::Object(data))
    } else {
        if let Some(workflow) = args.workflow.as_deref() {
            println!("Workflow filter: {workflow}");
        }
        if let Some(status) = args.status.as_deref() {
            println!("Status filter: {status}");
        }
        if let Some(since_label) = time_filter.describe(&since) {
            println!("{since_label}");
        }
        if let Some(note) = note {
            println!("Note: {note}");
        }
        print_execution_rows(&rows);
        Ok(())
    }
}

async fn cmd_runs_watch(context: &Context, args: RunsWatchArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "runs")?;
    let (client, _, _) = remote_client(&repo, Some(&alias), "runs")?;
    let workflow_id = resolve_execution_workflow_id(&client, args.workflow.as_deref()).await?;
    let time_filter = parse_runs_time_filter("runs", &args.time)?;
    let mut known_ids = BTreeSet::new();
    let mut poll = 0u32;

    loop {
        poll += 1;
        let since = time_filter.effective_since();
        let rows = fetch_execution_rows(
            &client,
            workflow_id.as_deref(),
            args.status.as_deref(),
            since,
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
            if let Some(since_label) = time_filter.describe(&since) {
                println!("{since_label}");
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
        println!("Pushed {}.", meta.workflow_id);
        println!("Updated local file: {}", stored.workflow_path.display());
        print_sensitive_warning_summary(&stored.workflow_path, warning_count);
        Ok(())
    }
}

async fn cmd_workflow(context: &Context, args: WorkflowArgs) -> Result<(), AppError> {
    match args.command {
        WorkflowCommand::New(args) => cmd_workflow_new(context, args).await,
        WorkflowCommand::Create(args) => cmd_workflow_create(context, args).await,
        WorkflowCommand::Show(args) => cmd_workflow_show(context, args).await,
    }
}

async fn cmd_workflow_new(context: &Context, args: WorkflowNewArgs) -> Result<(), AppError> {
    let workflow_id = args.id.unwrap_or_else(|| {
        format!(
            "draft-{}-{}",
            Utc::now().timestamp_millis(),
            std::process::id()
        )
    });
    let target_path = resolve_new_workflow_path(
        context,
        args.path.as_deref(),
        &args.name,
        Some(&workflow_id),
    )?;
    let result = create_workflow(&target_path, &args.name, Some(&workflow_id), args.active)?;
    emit_edit_result(
        context,
        "workflow",
        "Created",
        &result,
        vec![(
            "workflow_id".to_string(),
            json!(workflow_id_string(&result.workflow)),
        )],
    )
}

async fn cmd_workflow_create(context: &Context, args: WorkflowCreateArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "workflow")?;
    let source_path = resolve_local_file_path(context, &args.file)?;
    let source_meta_path = sidecar_path_for(&source_path);
    if source_meta_path.exists() {
        return Err(AppError::validation(
            "workflow",
            format!(
                "{} already has a metadata sidecar. Use `n8nc push` for tracked workflows.",
                source_path.display()
            ),
        ));
    }

    let payload = workflow_create_payload(&source_path)?;
    let (client, _, base_url) = remote_client(&repo, Some(&alias), "workflow")?;
    let mut created = client.create_workflow(&payload).await?;
    let created_id = workflow_id(&created).ok_or_else(|| {
        AppError::api(
            "workflow",
            "api.invalid_response",
            "Created workflow response was missing `id`.",
        )
    })?;

    if args.activate {
        client.activate_workflow(&created_id).await?;
        let Some(remote_workflow) = client.get_workflow_by_id(&created_id).await? else {
            return Err(AppError::not_found(
                "workflow",
                format!("Workflow `{created_id}` was created but could not be re-fetched."),
            ));
        };
        created = remote_workflow
            .get("data")
            .cloned()
            .unwrap_or(remote_workflow);
    }

    let stored = store_workflow(&repo, &alias, &created)?;
    let (source_removed, cleanup_warning) =
        finalize_created_workflow_source(&repo, &source_path, &stored.workflow_path);

    let warnings = sensitive_data_diagnostics(&stored.workflow_path)?;
    let warning_count = warnings.len();
    let webhooks = summarize_workflow_webhooks(&created, Some(base_url.as_str()));
    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("instance".to_string(), json!(alias));
        data.insert("source_path".to_string(), json!(source_path));
        data.insert("source_removed".to_string(), json!(source_removed));
        data.insert("workflow_path".to_string(), json!(stored.workflow_path));
        data.insert("meta_path".to_string(), json!(stored.meta_path));
        data.insert("workflow_id".to_string(), json!(stored.meta.workflow_id));
        data.insert("active".to_string(), json!(workflow_active(&created)));
        data.insert("webhooks".to_string(), json!(webhooks));
        data.insert("warning_count".to_string(), json!(warning_count));
        if warning_count > 0 {
            data.insert("diagnostics".to_string(), json!(warnings));
        }
        if let Some(cleanup_warning) = cleanup_warning {
            data.insert("cleanup_warning".to_string(), json!(cleanup_warning));
        }
        emit_json("workflow", &Value::Object(data))
    } else {
        println!(
            "Created remote workflow {} -> {}",
            stored.meta.workflow_id,
            stored.workflow_path.display()
        );
        println!("Metadata: {}", stored.meta_path.display());
        if source_removed {
            println!("Removed original draft: {}", source_path.display());
        } else if source_path != stored.workflow_path {
            println!("Original local file kept at {}", source_path.display());
        }
        if let Some(cleanup_warning) = cleanup_warning {
            println!("{cleanup_warning}");
        }
        print_workflow_webhooks(&webhooks);
        print_sensitive_warning_summary(&stored.workflow_path, warning_count);
        Ok(())
    }
}

async fn cmd_workflow_show(context: &Context, args: WorkflowShowArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let workflow = canonicalize_workflow(&load_workflow_file(&file, "workflow")?)?;
    let instance = resolve_workflow_show_instance(context, &file, args.remote.instance.as_deref())?;
    let base_url = resolve_instance_base_url(context, instance.as_deref())?;
    let nodes = summarize_workflow_nodes(&workflow);
    let connections = summarize_workflow_connections(&workflow);
    let webhooks = summarize_workflow_webhooks(&workflow, base_url.as_deref());

    if context.json {
        emit_json(
            "workflow",
            &json!({
                "workflow_path": file,
                "workflow_id": workflow_id(&workflow),
                "name": workflow_name(&workflow),
                "active": workflow_active(&workflow),
                "instance": instance,
                "node_count": nodes.len(),
                "connection_count": connections.len(),
                "nodes": nodes,
                "connections": connections,
                "webhooks": webhooks,
            }),
        )
    } else {
        println!(
            "Workflow: {}",
            workflow_name(&workflow).unwrap_or_else(|| "<unnamed>".to_string())
        );
        println!("File: {}", file.display());
        if let Some(workflow_id) = workflow_id(&workflow) {
            println!("ID: {workflow_id}");
        }
        println!("Active: {}", workflow_active(&workflow).unwrap_or(false));
        if let Some(instance) = &instance {
            println!("Instance: {instance}");
        }
        print_workflow_nodes(&nodes);
        print_workflow_connections(&connections);
        print_workflow_webhooks(&webhooks);
        Ok(())
    }
}

async fn cmd_node(context: &Context, args: NodeArgs) -> Result<(), AppError> {
    match args.command {
        NodeCommand::Ls(args) => cmd_node_ls(context, args).await,
        NodeCommand::Add(args) => cmd_node_add(context, args).await,
        NodeCommand::Set(args) => cmd_node_set(context, args).await,
        NodeCommand::Rename(args) => cmd_node_rename(context, args).await,
        NodeCommand::Rm(args) => cmd_node_remove(context, args).await,
    }
}

async fn cmd_node_ls(context: &Context, args: NodeListArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let workflow = canonicalize_workflow(&load_workflow_file(&file, "node")?)?;
    let nodes = summarize_workflow_nodes(&workflow);

    if context.json {
        emit_json(
            "node",
            &json!({
                "workflow_path": file,
                "workflow_id": workflow_id(&workflow),
                "count": nodes.len(),
                "nodes": nodes,
            }),
        )
    } else {
        println!("Workflow: {}", file.display());
        print_workflow_nodes(&nodes);
        Ok(())
    }
}

async fn cmd_node_add(context: &Context, args: NodeAddArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let result = add_node(
        &file,
        &args.name,
        &args.node_type,
        args.type_version,
        args.x,
        args.y,
        args.disabled,
    )?;
    emit_edit_result(
        context,
        "node",
        if result.changed {
            "Added node to"
        } else {
            "No node changes for"
        },
        &result,
        vec![
            (
                "workflow_id".to_string(),
                json!(workflow_id_string(&result.workflow)),
            ),
            ("node".to_string(), json!(args.name)),
        ],
    )
}

async fn cmd_node_set(context: &Context, args: NodeSetArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let value = parse_node_value("node", &args.mode, args.value.as_deref())?;
    let result = set_node_value(&file, &args.node, &args.path, value)?;
    emit_edit_result(
        context,
        "node",
        if result.changed {
            "Updated node in"
        } else {
            "No node changes for"
        },
        &result,
        vec![
            (
                "workflow_id".to_string(),
                json!(workflow_id_string(&result.workflow)),
            ),
            ("node".to_string(), json!(args.node)),
            ("path".to_string(), json!(args.path)),
        ],
    )
}

async fn cmd_node_rename(context: &Context, args: NodeRenameArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let result = rename_node(&file, &args.current_name, &args.new_name)?;
    emit_edit_result(
        context,
        "node",
        if result.changed {
            "Renamed node in"
        } else {
            "No node changes for"
        },
        &result,
        vec![
            (
                "workflow_id".to_string(),
                json!(workflow_id_string(&result.workflow)),
            ),
            ("from".to_string(), json!(args.current_name)),
            ("to".to_string(), json!(args.new_name)),
        ],
    )
}

async fn cmd_node_remove(context: &Context, args: NodeRemoveArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let result = remove_node(&file, &args.node)?;
    emit_edit_result(
        context,
        "node",
        if result.changed {
            "Removed node from"
        } else {
            "No node changes for"
        },
        &result,
        vec![
            (
                "workflow_id".to_string(),
                json!(workflow_id_string(&result.workflow)),
            ),
            ("node".to_string(), json!(args.node)),
        ],
    )
}

async fn cmd_conn(context: &Context, args: ConnArgs) -> Result<(), AppError> {
    match args.command {
        ConnCommand::Add(args) => cmd_conn_add(context, args).await,
        ConnCommand::Rm(args) => cmd_conn_remove(context, args).await,
    }
}

async fn cmd_conn_add(context: &Context, args: ConnAddArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let result = add_connection(
        &file,
        &args.from,
        &args.to,
        &args.kind,
        args.target_kind.as_deref(),
        args.output_index,
        args.input_index,
    )?;
    emit_edit_result(
        context,
        "conn",
        if result.changed {
            "Updated connections in"
        } else {
            "No connection changes for"
        },
        &result,
        vec![
            (
                "workflow_id".to_string(),
                json!(workflow_id_string(&result.workflow)),
            ),
            ("from".to_string(), json!(args.from)),
            ("to".to_string(), json!(args.to)),
            ("kind".to_string(), json!(args.kind)),
            ("output_index".to_string(), json!(args.output_index)),
            ("input_index".to_string(), json!(args.input_index)),
        ],
    )
}

async fn cmd_conn_remove(context: &Context, args: ConnRemoveArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let result = remove_connection(
        &file,
        &args.from,
        &args.to,
        &args.kind,
        args.target_kind.as_deref(),
        args.output_index,
        args.input_index,
    )?;
    emit_edit_result(
        context,
        "conn",
        if result.changed {
            "Removed connections from"
        } else {
            "No connection changes for"
        },
        &result,
        vec![
            (
                "workflow_id".to_string(),
                json!(workflow_id_string(&result.workflow)),
            ),
            ("from".to_string(), json!(args.from)),
            ("to".to_string(), json!(args.to)),
            ("kind".to_string(), json!(args.kind)),
            ("output_index".to_string(), json!(args.output_index)),
            ("input_index".to_string(), json!(args.input_index)),
        ],
    )
}

async fn cmd_expr(context: &Context, args: ExprArgs) -> Result<(), AppError> {
    match args.command {
        ExprCommand::Set(args) => cmd_expr_set(context, args).await,
    }
}

async fn cmd_expr_set(context: &Context, args: ExprSetArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let result = set_node_expression(&file, &args.node, &args.path, &args.expression)?;
    emit_edit_result(
        context,
        "expr",
        if result.changed {
            "Updated expression in"
        } else {
            "No expression changes for"
        },
        &result,
        vec![
            (
                "workflow_id".to_string(),
                json!(workflow_id_string(&result.workflow)),
            ),
            ("node".to_string(), json!(args.node)),
            ("path".to_string(), json!(args.path)),
        ],
    )
}

async fn cmd_credential(context: &Context, args: CredentialArgs) -> Result<(), AppError> {
    match args.command {
        CredentialCommand::Set(args) => cmd_credential_set(context, args).await,
    }
}

async fn cmd_credential_set(context: &Context, args: CredentialSetArgs) -> Result<(), AppError> {
    let file = resolve_local_file_path(context, &args.file)?;
    let result = set_credential_reference(
        &file,
        &args.node,
        &args.credential_type,
        &args.credential_id,
        args.name.as_deref(),
    )?;
    emit_edit_result(
        context,
        "credential",
        if result.changed {
            "Updated credential reference in"
        } else {
            "No credential changes for"
        },
        &result,
        vec![
            (
                "workflow_id".to_string(),
                json!(workflow_id_string(&result.workflow)),
            ),
            ("node".to_string(), json!(args.node)),
            ("credential_type".to_string(), json!(args.credential_type)),
        ],
    )
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

async fn cmd_activation(context: &Context, args: IdArgs, active: bool) -> Result<(), AppError> {
    let command = if active { "activate" } else { "deactivate" };
    let repo = load_loaded_repo(context)?;
    let (client, _, base_url) = remote_client(&repo, args.remote.instance.as_deref(), command)?;
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
    let current = client
        .get_workflow_by_id(&workflow_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(
                command,
                format!("Workflow `{workflow_id}` could not be re-fetched after {command}."),
            )
        })?
        .get("data")
        .cloned()
        .unwrap_or_else(|| workflow.clone());
    let active_state = workflow_active(&current).unwrap_or(active);
    let webhooks = summarize_workflow_webhooks(&current, Some(base_url.as_str()));

    if context.json {
        emit_json(
            command,
            &json!({"workflow_id": workflow_id, "active": active_state, "webhooks": webhooks}),
        )
    } else {
        println!(
            "{} {}.",
            if active_state {
                "Activated"
            } else {
                "Deactivated"
            },
            workflow_id
        );
        if active_state {
            print_workflow_webhooks(&webhooks);
        }
        Ok(())
    }
}

async fn cmd_trigger(context: &Context, args: TriggerArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, base_url) = remote_client(&repo, args.remote.instance.as_deref(), "trigger")?;
    let headers = parse_pairs("trigger", "header", &args.headers, ':')?;
    let query = parse_pairs("trigger", "query", &args.query, '=')?;
    let body = read_request_body(args.data, args.data_file, args.stdin)?;
    let response = client
        .trigger(&args.target, &args.method, &headers, &query, body)
        .await
        .map_err(|err| enrich_trigger_error(err, &base_url, &args.target))?;

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
        .filter(|diag| diag.severity == Severity::Error)
        .count();
    let warning_count = diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Warning)
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
                "warning_count": warning_count,
                "diagnostics": diagnostics,
            })));
        }

        emit_json(
            "validate",
            &json!({
                "files_checked": workflow_files.len(),
                "error_count": error_count,
                "warning_count": warning_count,
                "diagnostics": diagnostics,
            }),
        )?;
    } else if diagnostics.is_empty() {
        println!(
            "Validated {} workflow file(s): 0 errors, 0 warnings.",
            workflow_files.len()
        );
    } else {
        for diagnostic in &diagnostics {
            let path = diagnostic.path.as_deref().unwrap_or("-");
            println!(
                "[{}] {} {} {}",
                match diagnostic.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                },
                diagnostic.file,
                path,
                diagnostic.message
            );
        }
        println!(
            "Validated {} workflow file(s): {} error(s), {} warning(s).",
            workflow_files.len(),
            error_count,
            warning_count
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
    since: Option<DateTime<Utc>>,
    limit: u16,
) -> Result<Vec<ExecutionListRow>, AppError> {
    let executions = client
        .list_executions(&ExecutionListOptions {
            limit: limit.clamp(1, 250),
            workflow_id: workflow_id.map(ToOwned::to_owned),
            status: status.map(ToOwned::to_owned),
            since,
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

async fn execution_history_note(
    client: &ApiClient,
    workflow_id: Option<&str>,
    rows: &[ExecutionListRow],
) -> Result<Option<String>, AppError> {
    if !rows.is_empty() {
        return Ok(None);
    }
    let Some(workflow_id) = workflow_id else {
        return Ok(None);
    };
    let Some(workflow) = client.get_workflow_by_id(workflow_id).await? else {
        return Ok(None);
    };
    let workflow = workflow.get("data").unwrap_or(&workflow);
    if !workflow_active(workflow).unwrap_or(false) {
        return Ok(None);
    }

    let save_success = workflow
        .get("settings")
        .and_then(Value::as_object)
        .and_then(|settings| settings.get("saveDataSuccessExecution"))
        .and_then(Value::as_str);

    if save_success == Some("all") {
        return Ok(None);
    }

    Ok(Some(format!(
        "Workflow settings do not explicitly save successful production executions (`saveDataSuccessExecution = {}`). Successful runs may not appear in `runs ls`.",
        save_success.unwrap_or("unset"),
    )))
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

fn parse_runs_time_filter(
    command: &'static str,
    args: &RunsTimeArgs,
) -> Result<RunsTimeFilter, AppError> {
    let since = args
        .since
        .as_deref()
        .map(|value| parse_rfc3339_timestamp(command, "--since", value))
        .transpose()?;
    let last = args
        .last
        .as_deref()
        .map(|value| parse_time_window(command, value))
        .transpose()?;

    Ok(RunsTimeFilter {
        since,
        last,
        last_label: args.last.clone(),
    })
}

fn parse_rfc3339_timestamp(
    command: &'static str,
    flag: &'static str,
    value: &str,
) -> Result<DateTime<Utc>, AppError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|err| {
            AppError::usage(
                command,
                format!("`{flag}` must be an RFC3339 timestamp: {err}"),
            )
        })
}

fn parse_time_window(command: &'static str, value: &str) -> Result<ChronoDuration, AppError> {
    if value.len() < 2 {
        return Err(AppError::usage(
            command,
            "`--last` must use an integer and a unit like `15m`, `2h`, or `1d`.",
        ));
    }

    let (amount, unit) = value.split_at(value.len() - 1);
    let amount: i64 = amount.parse().map_err(|_| {
        AppError::usage(
            command,
            "`--last` must start with a whole number, for example `15m` or `2h`.",
        )
    })?;
    if amount <= 0 {
        return Err(AppError::usage(
            command,
            "`--last` must be greater than zero.",
        ));
    }

    let duration = match unit.to_ascii_lowercase().as_str() {
        "s" => ChronoDuration::try_seconds(amount),
        "m" => ChronoDuration::try_minutes(amount),
        "h" => ChronoDuration::try_hours(amount),
        "d" => ChronoDuration::try_days(amount),
        _ => None,
    };

    duration.ok_or_else(|| {
        AppError::usage(
            command,
            "`--last` must use one of these units: `s`, `m`, `h`, `d`.",
        )
    })
}

impl RunsTimeFilter {
    fn effective_since(&self) -> Option<DateTime<Utc>> {
        self.since
            .as_ref()
            .cloned()
            .or_else(|| self.last.map(|window| Utc::now() - window))
    }

    fn describe(&self, since: &Option<DateTime<Utc>>) -> Option<String> {
        if let Some(since) = self.since.as_ref() {
            return Some(format!("Since: {}", since.to_rfc3339()));
        }
        if let Some(last) = self.last_label.as_deref() {
            return Some(format!("Window: last {last}"));
        }
        since
            .as_ref()
            .map(|value| format!("Since: {}", value.to_rfc3339()))
    }
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

fn parse_node_value(
    command: &'static str,
    mode: &ValueModeArgs,
    value: Option<&str>,
) -> Result<Value, AppError> {
    if mode.null {
        return Ok(Value::Null);
    }

    let Some(value) = value else {
        return Err(AppError::usage(
            command,
            "A value is required unless `--null` is used.",
        ));
    };

    if mode.json_value {
        return serde_json::from_str(value).map_err(|err| {
            AppError::usage(command, format!("`--json-value` must be valid JSON: {err}"))
        });
    }

    if mode.number {
        let number = serde_json::Number::from_f64(value.parse::<f64>().map_err(|err| {
            AppError::usage(command, format!("`--number` value must be numeric: {err}"))
        })?)
        .ok_or_else(|| AppError::usage(command, "`--number` value must be finite."))?;
        return Ok(Value::Number(number));
    }

    if mode.bool_value {
        let parsed = match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => {
                return Err(AppError::usage(
                    command,
                    "`--bool` value must be one of: true, false, 1, 0, yes, no.",
                ));
            }
        };
        return Ok(Value::Bool(parsed));
    }

    Ok(Value::String(value.to_string()))
}

fn workflow_create_payload(path: &Path) -> Result<Value, AppError> {
    let diagnostics = validate_workflow_path(path)?;
    let error_count = diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Error)
        .count();
    let warning_count = diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Warning)
        .count();
    if error_count > 0 {
        return Err(AppError::validation(
            "workflow",
            format!(
                "Local workflow file has {error_count} validation error(s) and cannot be created remotely."
            ),
        )
        .with_json_data(json!({
            "files_checked": 1,
            "error_count": error_count,
            "warning_count": warning_count,
            "diagnostics": diagnostics,
        })));
    }

    let workflow = load_workflow_file(path, "workflow")?;
    let mut payload = canonicalize_workflow(&workflow)?;
    let object = payload.as_object_mut().ok_or_else(|| {
        AppError::validation("workflow", "Workflow payload must be a JSON object.")
    })?;
    object.remove("id");
    object.remove("active");
    apply_default_workflow_settings(object)?;

    let has_name = object
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if !has_name {
        return Err(AppError::validation(
            "workflow",
            "Workflow file must include a non-empty `name` before it can be created remotely.",
        ));
    }
    if !matches!(object.get("nodes"), Some(Value::Array(_))) {
        return Err(AppError::validation(
            "workflow",
            "Workflow file must include a `nodes` array before it can be created remotely.",
        ));
    }
    if !matches!(object.get("connections"), Some(Value::Object(_))) {
        return Err(AppError::validation(
            "workflow",
            "Workflow file must include a `connections` object before it can be created remotely.",
        ));
    }

    normalize_remote_create_payload(&mut payload)?;
    canonicalize_workflow(&payload)
}

fn apply_default_workflow_settings(
    object: &mut serde_json::Map<String, Value>,
) -> Result<(), AppError> {
    let settings = object
        .entry("settings".to_string())
        .or_insert_with(default_workflow_settings);
    let settings_object = settings.as_object_mut().ok_or_else(|| {
        AppError::validation("workflow", "Workflow `settings` field must be an object.")
    })?;

    for (key, value) in default_workflow_settings()
        .as_object()
        .into_iter()
        .flatten()
    {
        settings_object
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    Ok(())
}

fn normalize_remote_create_payload(payload: &mut Value) -> Result<(), AppError> {
    let Some(nodes) = payload.get_mut("nodes").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for node in nodes {
        normalize_remote_create_node(node)?;
    }
    Ok(())
}

fn normalize_remote_create_node(node: &mut Value) -> Result<(), AppError> {
    if node.get("type").and_then(Value::as_str) != Some(WEBHOOK_NODE_TYPE) {
        return Ok(());
    }
    let node_object = node.as_object_mut().ok_or_else(|| {
        AppError::validation("workflow", "Workflow node entry must be a JSON object.")
    })?;
    let parameters = node_object
        .entry("parameters".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let parameters = parameters.as_object_mut().ok_or_else(|| {
        AppError::validation("workflow", "Webhook node `parameters` must be an object.")
    })?;
    let normalized_path = parameters
        .get("path")
        .and_then(Value::as_str)
        .map(normalize_webhook_path)
        .filter(|path| !path.is_empty());
    if let Some(path) = normalized_path {
        parameters.insert("path".to_string(), Value::String(path.clone()));
        let existing_webhook_id = node_object
            .get("webhookId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if existing_webhook_id.is_none() {
            node_object.insert("webhookId".to_string(), Value::String(path));
        }
    }
    let type_version = node_object.get("typeVersion").and_then(Value::as_f64);
    if type_version.is_none_or(|version| version < 2.0) {
        node_object.insert("typeVersion".to_string(), json!(2));
    }
    Ok(())
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

fn emit_edit_result(
    context: &Context,
    command: &'static str,
    action: &str,
    result: &EditResult,
    extra: Vec<(String, Value)>,
) -> Result<(), AppError> {
    let warnings = sensitive_data_diagnostics(&result.path)?;
    let warning_count = warnings.len();
    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("workflow_path".to_string(), json!(result.path));
        data.insert("changed".to_string(), json!(result.changed));
        data.insert("warning_count".to_string(), json!(warning_count));
        for (key, value) in extra {
            data.insert(key, value);
        }
        if warning_count > 0 {
            data.insert("diagnostics".to_string(), json!(warnings));
        }
        emit_json(command, &Value::Object(data))
    } else {
        println!("{action} {}.", result.path.display());
        print_sensitive_warning_summary(&result.path, warning_count);
        Ok(())
    }
}

fn resolve_local_file_path(context: &Context, path: &Path) -> Result<PathBuf, AppError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    if let Ok(repo) = load_repo(context.repo_root.as_deref()) {
        return Ok(repo.root.join(path));
    }

    Ok(context_root(context)?.join(path))
}

fn resolve_new_workflow_path(
    context: &Context,
    explicit: Option<&Path>,
    name: &str,
    workflow_id: Option<&str>,
) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit {
        return resolve_local_file_path(context, path);
    }

    let workflow_id = workflow_id.map(ToOwned::to_owned).unwrap_or_else(|| {
        format!(
            "draft-{}-{}",
            Utc::now().timestamp_millis(),
            std::process::id()
        )
    });
    let file_name = default_workflow_file_name(name, &workflow_id);

    if let Ok(repo) = load_repo(context.repo_root.as_deref()) {
        return Ok(workflow_dir(&repo.root, &repo.config).join(file_name));
    }

    Ok(context_root(context)?.join(file_name))
}

fn context_root(context: &Context) -> Result<PathBuf, AppError> {
    if let Some(path) = &context.repo_root {
        Ok(path.clone())
    } else {
        std::env::current_dir().map_err(|err| {
            AppError::config(
                "config",
                format!("Failed to resolve current directory: {err}"),
            )
        })
    }
}

fn finalize_created_workflow_source(
    repo: &LoadedRepo,
    source_path: &Path,
    tracked_path: &Path,
) -> (bool, Option<String>) {
    if source_path == tracked_path {
        return (false, None);
    }

    let workflow_root = workflow_dir(&repo.root, &repo.config);
    if !source_path.starts_with(&workflow_root) {
        return (false, None);
    }

    match fs::remove_file(source_path) {
        Ok(()) => (true, None),
        Err(err) => (
            false,
            Some(format!(
                "Warning: failed to remove original draft {}: {err}",
                source_path.display()
            )),
        ),
    }
}

fn resolve_workflow_show_instance(
    context: &Context,
    file: &Path,
    explicit_alias: Option<&str>,
) -> Result<Option<String>, AppError> {
    if let Some(alias) = explicit_alias {
        return Ok(Some(alias.to_string()));
    }

    let meta_path = sidecar_path_for(file);
    if meta_path.exists() {
        let meta = load_meta(&meta_path, "workflow")?;
        return Ok(Some(meta.instance));
    }

    let _ = context;
    Ok(None)
}

fn resolve_instance_base_url(
    context: &Context,
    alias: Option<&str>,
) -> Result<Option<String>, AppError> {
    let Some(alias) = alias else {
        return Ok(None);
    };
    let repo = load_loaded_repo(context)?;
    let instance = repo.config.instances.get(alias).ok_or_else(|| {
        AppError::config("workflow", format!("Unknown instance alias `{alias}`."))
    })?;
    Ok(Some(instance.base_url.clone()))
}

fn summarize_workflow_nodes(workflow: &Value) -> Vec<WorkflowNodeRow> {
    let mut rows = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|node| WorkflowNodeRow {
            name: node
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>")
                .to_string(),
            node_id: value_string(node, "id"),
            node_type: value_string(node, "type"),
            type_version: node.get("typeVersion").and_then(Value::as_f64),
            position: node.get("position").and_then(parse_position),
            disabled: node.get("disabled").and_then(Value::as_bool),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

fn summarize_workflow_connections(workflow: &Value) -> Vec<WorkflowConnectionRow> {
    let mut rows = Vec::new();
    let Some(connections) = workflow.get("connections").and_then(Value::as_object) else {
        return rows;
    };

    for (from, kinds) in connections {
        let Some(kinds) = kinds.as_object() else {
            continue;
        };
        for (kind, branches) in kinds {
            let Some(branches) = branches.as_array() else {
                continue;
            };
            for (output_index, branch) in branches.iter().enumerate() {
                let Some(entries) = branch.as_array() else {
                    continue;
                };
                for entry in entries {
                    let Some(to) = entry.get("node").and_then(Value::as_str) else {
                        continue;
                    };
                    rows.push(WorkflowConnectionRow {
                        from: from.clone(),
                        kind: kind.clone(),
                        output_index,
                        to: to.to_string(),
                        target_kind: value_string(entry, "type"),
                        input_index: entry
                            .get("index")
                            .and_then(Value::as_u64)
                            .map(|value| value as usize),
                    });
                }
            }
        }
    }

    rows.sort_by(|left, right| {
        (
            left.from.as_str(),
            left.kind.as_str(),
            left.output_index,
            left.to.as_str(),
            left.input_index.unwrap_or(usize::MAX),
        )
            .cmp(&(
                right.from.as_str(),
                right.kind.as_str(),
                right.output_index,
                right.to.as_str(),
                right.input_index.unwrap_or(usize::MAX),
            ))
    });
    rows
}

fn summarize_workflow_webhooks(
    workflow: &Value,
    base_url: Option<&str>,
) -> Vec<WorkflowWebhookRow> {
    let mut rows = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|node| node.get("type").and_then(Value::as_str) == Some(WEBHOOK_NODE_TYPE))
        .map(|node| {
            let node_name = node
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>")
                .to_string();
            let path = node
                .get("parameters")
                .and_then(Value::as_object)
                .and_then(|parameters| parameters.get("path"))
                .and_then(Value::as_str)
                .map(normalize_webhook_path)
                .filter(|path| !path.is_empty());
            let methods = webhook_methods(node);
            let production_url = path
                .as_deref()
                .zip(base_url)
                .map(|(path, base)| format!("{}/webhook/{}", base.trim_end_matches('/'), path));
            let test_url = path.as_deref().zip(base_url).map(|(path, base)| {
                format!("{}/webhook-test/{}", base.trim_end_matches('/'), path)
            });

            WorkflowWebhookRow {
                node: node_name,
                methods,
                path,
                webhook_id: value_string(node, "webhookId"),
                production_url,
                test_url,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.node.cmp(&right.node));
    rows
}

fn parse_position(value: &Value) -> Option<Vec<i64>> {
    let position = value.as_array()?;
    let coords = position
        .iter()
        .map(|entry| entry.as_i64())
        .collect::<Option<Vec<_>>>()?;
    if coords.is_empty() {
        None
    } else {
        Some(coords)
    }
}

fn webhook_methods(node: &Value) -> Vec<String> {
    let Some(parameters) = node.get("parameters").and_then(Value::as_object) else {
        return Vec::new();
    };
    if let Some(method) = parameters.get("httpMethod").and_then(Value::as_str) {
        return vec![method.to_string()];
    }
    parameters
        .get("httpMethods")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn print_workflow_nodes(rows: &[WorkflowNodeRow]) {
    if rows.is_empty() {
        println!("Nodes: none");
        return;
    }

    println!("Nodes:");
    println!(
        "{:<24} {:<28} {:<10} {:<14} {}",
        "NAME", "TYPE", "VERSION", "POSITION", "DISABLED"
    );
    for row in rows {
        let position = row
            .position
            .as_ref()
            .map(|coords| {
                coords
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|| "-".to_string());
        let version = row
            .type_version
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<24} {:<28} {:<10} {:<14} {}",
            truncate(&row.name, 24),
            truncate(row.node_type.as_deref().unwrap_or("-"), 28),
            truncate(&version, 10),
            truncate(&position, 14),
            row.disabled.unwrap_or(false)
        );
    }
}

fn print_workflow_connections(rows: &[WorkflowConnectionRow]) {
    if rows.is_empty() {
        println!("Connections: none");
        return;
    }

    println!("Connections:");
    println!(
        "{:<24} {:<10} {:<6} {:<24} {:<12} {}",
        "FROM", "KIND", "OUT", "TO", "TARGET", "IN"
    );
    for row in rows {
        println!(
            "{:<24} {:<10} {:<6} {:<24} {:<12} {}",
            truncate(&row.from, 24),
            truncate(&row.kind, 10),
            row.output_index,
            truncate(&row.to, 24),
            truncate(row.target_kind.as_deref().unwrap_or("-"), 12),
            row.input_index
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
}

fn print_workflow_webhooks(rows: &[WorkflowWebhookRow]) {
    if rows.is_empty() {
        return;
    }

    println!("Webhooks:");
    for row in rows {
        let methods = if row.methods.is_empty() {
            "-".to_string()
        } else {
            row.methods.join(",")
        };
        println!(
            "  {} [{}] path={} webhook_id={}",
            row.node,
            methods,
            row.path.as_deref().unwrap_or("-"),
            row.webhook_id.as_deref().unwrap_or("-"),
        );
        if let Some(url) = &row.production_url {
            println!("  production: {url}");
        }
        if let Some(url) = &row.test_url {
            println!("  test: {url}");
        }
    }
}

fn normalize_webhook_path(path: &str) -> String {
    path.trim_matches('/').to_string()
}

fn enrich_trigger_error(mut err: AppError, base_url: &str, target: &str) -> AppError {
    if err.command != "trigger" || !err.code.starts_with("trigger.http_404") {
        return err;
    }

    let resolved_path = resolve_trigger_path(base_url, target);
    if let Some(path) = &resolved_path {
        if path.starts_with("/webhook-test/") {
            err.suggestion = Some(
                "Test webhook URLs only work while the workflow is listening in test mode in n8n. Use the editor test listener or call the production `/webhook/...` URL for active workflows.".to_string(),
            );
        } else if path.starts_with("/webhook/") {
            err.suggestion = Some(
                "Production webhook 404s usually mean the path is wrong, the workflow is inactive, or n8n has not registered the webhook yet. Check `n8nc workflow show <file>` for the expected URL and re-activate the workflow if needed.".to_string(),
            );
        }
    }
    err
}

fn resolve_trigger_path(base_url: &str, target: &str) -> Option<String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        reqwest::Url::parse(target)
            .ok()
            .map(|url| url.path().to_string())
    } else {
        reqwest::Url::parse(base_url)
            .ok()
            .and_then(|base| base.join(target.trim_start_matches('/')).ok())
            .map(|url| url.path().to_string())
    }
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

fn doctor_status_label(status: DoctorCheckStatus) -> &'static str {
    match status {
        DoctorCheckStatus::Ok => "ok",
        DoctorCheckStatus::Fail => "fail",
        DoctorCheckStatus::Skip => "skip",
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

fn client_for_instance(
    repo: &LoadedRepo,
    instance: &str,
    command: &'static str,
    clients: &mut BTreeMap<String, Result<ApiClient, AppError>>,
) -> Result<ApiClient, AppError> {
    if let Some(client) = clients.get(instance) {
        return client.clone();
    }

    let resolved = remote_client(repo, Some(instance), command).map(|(client, _, _)| client);
    clients.insert(instance.to_string(), resolved.clone());
    resolved
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

    use crate::cli::RunsTimeArgs;
    use crate::repo::{LocalStatusEntry, LocalWorkflowState, RemoteSyncState};

    use super::{
        ExecutionListRow, execution_duration_ms, execution_node_rows, format_duration,
        note_new_executions, parse_runs_time_filter, parse_time_window, summarize_sync_states,
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

    #[test]
    fn parse_time_window_accepts_supported_units() {
        assert_eq!(
            parse_time_window("runs", "15m").expect("15m").num_minutes(),
            15
        );
        assert_eq!(parse_time_window("runs", "2h").expect("2h").num_hours(), 2);
        assert_eq!(parse_time_window("runs", "1d").expect("1d").num_days(), 1);
    }

    #[test]
    fn parse_runs_time_filter_rejects_invalid_since() {
        let err = parse_runs_time_filter(
            "runs",
            &RunsTimeArgs {
                since: Some("tomorrow morning".to_string()),
                last: None,
            },
        )
        .expect_err("invalid since should fail");

        assert_eq!(err.code, "usage.invalid");
        assert!(
            err.message
                .contains("`--since` must be an RFC3339 timestamp")
        );
    }

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
