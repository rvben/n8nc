use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{
    api::{ApiClient, ListOptions},
    auth::{ensure_alias_exists, resolve_token},
    cli::DoctorArgs,
    cmd::credential::probe_credential_inventory_capability,
    config::LoadedRepo,
    error::AppError,
    execute::{execute_backend_setup_hint, probe_execute_backend},
    repo::collect_json_targets,
    validate::sensitive_data_diagnostics,
};

use super::common::{Context, emit_json, load_loaded_repo, truncate};

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

pub(crate) async fn cmd_doctor(context: &Context, args: DoctorArgs) -> Result<(), AppError> {
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

        match instance.execute.as_ref() {
            Some(execute_config) => {
                match probe_execute_backend(&repo.root, execute_config, "doctor") {
                    Ok(detail) => add_doctor_check(
                        &mut checks,
                        DoctorCheckStatus::Ok,
                        "instance",
                        Some(alias.clone()),
                        "workflow_execute",
                        detail,
                        None,
                    ),
                    Err(err) => add_doctor_check(
                        &mut checks,
                        DoctorCheckStatus::Fail,
                        "instance",
                        Some(alias.clone()),
                        "workflow_execute",
                        err.message,
                        Some(execute_backend_setup_hint(&alias).to_string()),
                    ),
                }
            }
            None => add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Skip,
                "instance",
                Some(alias.clone()),
                "workflow_execute",
                "No workflow execute backend configured. Non-webhook execution is unavailable, but `trigger` still works for webhook URLs.",
                None,
            ),
        }

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

        let api_reachable = match client
            .list_workflows(&ListOptions {
                limit: 1,
                active: None,
                name_filter: None,
            })
            .await
        {
            Ok(workflows) => {
                add_doctor_check(
                    &mut checks,
                    DoctorCheckStatus::Ok,
                    "instance",
                    Some(alias.clone()),
                    "api",
                    format!("API reachable (sample_count={}).", workflows.len()),
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
                    "api",
                    err.message,
                    err.suggestion,
                );
                false
            }
        };

        if !api_reachable {
            continue;
        }

        match probe_credential_inventory_capability(&alias, &client).await {
            Ok(detail) => add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Ok,
                "instance",
                Some(alias.clone()),
                "credential_inventory",
                detail,
                None,
            ),
            Err(err) => add_doctor_check(
                &mut checks,
                DoctorCheckStatus::Fail,
                "instance",
                Some(alias.clone()),
                "credential_inventory",
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
        "{:<8} {:<10} {:<16} {:<18} DETAIL",
        "STATUS", "SCOPE", "TARGET", "CHECK"
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

fn doctor_status_label(status: DoctorCheckStatus) -> &'static str {
    match status {
        DoctorCheckStatus::Ok => "ok",
        DoctorCheckStatus::Fail => "fail",
        DoctorCheckStatus::Skip => "skip",
    }
}
