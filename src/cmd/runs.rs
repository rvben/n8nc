use std::collections::{BTreeMap, BTreeSet};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    api::{ApiClient, ExecutionListOptions},
    cli::{
        RunsArgs, RunsCommand, RunsGetArgs, RunsListArgs, RunsStatsArgs, RunsTimeArgs,
        RunsWatchArgs,
    },
    config::resolve_instance_alias,
    error::AppError,
    repo::{workflow_active, workflow_name},
};

use super::common::{
    Context, emit_json, emit_json_line, load_loaded_repo, remote_client, truncate, value_string,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_runs(context: &Context, args: RunsArgs) -> Result<(), AppError> {
    match args.command {
        RunsCommand::Ls(args) => cmd_runs_ls(context, args).await,
        RunsCommand::Get(args) => cmd_runs_get(context, args).await,
        RunsCommand::Watch(args) => cmd_runs_watch(context, args).await,
        RunsCommand::Stats(args) => cmd_runs_stats(context, args).await,
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
        since,
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
    let node_executions = args.details.then(|| execution_node_rows(&execution));
    let run_data = args.details.then(|| execution_run_data_value(&execution));

    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("execution".to_string(), execution);
        if let Some(run_data) = run_data {
            data.insert("run_data".to_string(), run_data);
        }
        if let Some(node_executions) = node_executions {
            data.insert("node_executions".to_string(), json!(node_executions));
        }
        emit_json("runs", &Value::Object(data))
    } else {
        let wf_id = value_string(&execution, "workflowId");
        let wf_name = workflow_name_for_execution(&client, &execution).await?;
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
        match (wf_name.as_deref(), wf_id.as_deref()) {
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
            let nodes = node_executions.unwrap_or_default();
            if !nodes.is_empty() {
                println!();
                println!("{:<32} {:<10} {:<10} OUTPUTS", "NODE", "STATUS", "TIME");
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

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct StatsOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_name: Option<String>,
    period: String,
    capped: bool,
    total: usize,
    succeeded: usize,
    failed: usize,
    running: usize,
    waiting: usize,
    success_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<DurationStats>,
}

#[derive(Debug, Clone, Serialize)]
struct DurationStats {
    min: i64,
    max: i64,
    avg: i64,
}

async fn cmd_runs_stats(context: &Context, args: RunsStatsArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "runs")?;

    // Resolve workflow: file path or ID/name
    let workflow_id = if let Some(ref identifier) = args.workflow {
        if identifier.contains('/') || identifier.ends_with(".workflow.json") {
            let content = std::fs::read_to_string(identifier).map_err(|err| {
                AppError::not_found("runs", format!("Cannot read workflow file: {err}"))
            })?;
            let wf: Value = serde_json::from_str(&content)
                .map_err(|err| AppError::usage("runs", format!("Invalid workflow JSON: {err}")))?;
            crate::repo::workflow_id(&wf)
        } else {
            resolve_execution_workflow_id(&client, Some(identifier)).await?
        }
    } else {
        None
    };

    // Fetch workflow name if we have an ID
    let wf_name = if let Some(ref wf_id) = workflow_id {
        match client.get_workflow_by_id(wf_id).await? {
            Some(wf) => workflow_name(wf.get("data").unwrap_or(&wf)),
            None => None,
        }
    } else {
        None
    };

    // Determine time window
    let time_filter = parse_runs_time_filter("runs", &args.time)?;
    let (since, period_label) = if time_filter.since.is_some() || time_filter.last.is_some() {
        let since = time_filter.effective_since();
        let label = if let Some(ref last) = time_filter.last_label {
            format!("last {last}")
        } else {
            format!("since {}", since.unwrap().to_rfc3339())
        };
        (since, label)
    } else {
        let since = Utc::now() - ChronoDuration::try_hours(24).unwrap();
        (Some(since), "last 24h".to_string())
    };

    // Fetch executions directly (not via fetch_execution_rows which clamps at 250)
    let executions = client
        .list_executions(&ExecutionListOptions {
            limit: 1000,
            workflow_id: workflow_id.clone(),
            status: None,
            since,
        })
        .await?;

    let capped = executions.len() == 1000;

    // Aggregate stats
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut running = 0usize;
    let mut waiting = 0usize;
    let mut durations = Vec::new();

    for execution in &executions {
        match value_string(execution, "status").as_deref() {
            Some("success") => succeeded += 1,
            Some("error") => failed += 1,
            Some("running") => running += 1,
            Some("waiting") => waiting += 1,
            _ => {}
        }
        if let Some(ms) = execution_duration_ms(execution) {
            durations.push(ms);
        }
    }

    let total = executions.len();
    let success_rate = if total > 0 {
        succeeded as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    let duration_stats = if durations.is_empty() {
        None
    } else {
        let min = *durations.iter().min().unwrap();
        let max = *durations.iter().max().unwrap();
        let avg = durations.iter().sum::<i64>() / durations.len() as i64;
        Some(DurationStats { min, max, avg })
    };

    let stats = StatsOutput {
        workflow_id: workflow_id.clone(),
        workflow_name: wf_name.clone(),
        period: period_label.clone(),
        capped,
        total,
        succeeded,
        failed,
        running,
        waiting,
        success_rate,
        duration_ms: duration_stats.clone(),
    };

    if context.json {
        emit_json("runs", &json!(stats))
    } else {
        match (wf_name.as_deref(), workflow_id.as_deref()) {
            (Some(name), Some(id)) => println!("Workflow: {name} ({id})"),
            (Some(name), None) => println!("Workflow: {name}"),
            (None, Some(id)) => println!("Workflow ID: {id}"),
            (None, None) => {}
        }
        println!("Period: {period_label}");
        if capped {
            println!("Note: results capped at 1000 executions");
        }
        println!();
        println!("Total:      {total}");
        if total > 0 {
            println!(
                "Succeeded:  {succeeded} ({:.1}%)",
                succeeded as f64 / total as f64 * 100.0
            );
            println!(
                "Failed:     {failed} ({:.1}%)",
                failed as f64 / total as f64 * 100.0
            );
            if running > 0 {
                println!(
                    "Running:    {running} ({:.1}%)",
                    running as f64 / total as f64 * 100.0
                );
            }
            if waiting > 0 {
                println!(
                    "Waiting:    {waiting} ({:.1}%)",
                    waiting as f64 / total as f64 * 100.0
                );
            }
            println!("Success rate: {success_rate:.1}%");
        }
        if let Some(ref ds) = duration_stats {
            println!();
            println!("Duration (completed executions):");
            println!("  Min: {}", format_duration(Some(ds.min)));
            println!("  Max: {}", format_duration(Some(ds.max)));
            println!("  Avg: {}", format_duration(Some(ds.avg)));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn workflow_names_for_executions(
    client: &ApiClient,
    executions: &[Value],
) -> Result<BTreeMap<String, String>, AppError> {
    let mut names = BTreeMap::new();
    for wf_id in executions
        .iter()
        .filter_map(|execution| value_string(execution, "workflowId"))
    {
        if names.contains_key(&wf_id) {
            continue;
        }
        let Some(workflow) = client.get_workflow_by_id(&wf_id).await? else {
            continue;
        };
        let workflow = workflow.get("data").unwrap_or(&workflow);
        if let Some(name) = workflow_name(workflow) {
            names.insert(wf_id, name);
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
    Ok(crate::repo::workflow_id(&workflow))
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
            let wf_id = value_string(&execution, "workflowId");
            ExecutionListRow {
                id: value_string(&execution, "id").unwrap_or_default(),
                workflow_name: wf_id
                    .as_ref()
                    .and_then(|id| workflow_names.get(id).cloned()),
                workflow_id: wf_id,
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

    let Some(wf_id) = value_string(execution, "workflowId") else {
        return Ok(None);
    };
    let Some(workflow) = client.get_workflow_by_id(&wf_id).await? else {
        return Ok(None);
    };
    Ok(workflow_name(workflow.get("data").unwrap_or(&workflow)))
}

fn execution_node_rows(execution: &Value) -> Vec<ExecutionNodeRow> {
    let Some(run_data) = execution_run_data_object(execution) else {
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

fn execution_run_data_object(execution: &Value) -> Option<&serde_json::Map<String, Value>> {
    execution
        .get("data")
        .and_then(|data| data.get("resultData"))
        .and_then(|result| result.get("runData"))
        .and_then(Value::as_object)
}

fn execution_run_data_value(execution: &Value) -> Value {
    execution
        .get("data")
        .and_then(|data| data.get("resultData"))
        .and_then(|result| result.get("runData"))
        .cloned()
        .unwrap_or(Value::Null)
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
        "{:<10} {:<10} {:<10} {:<10} {:<24} WORKFLOW",
        "ID", "STATUS", "MODE", "DURATION", "STARTED"
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::collections::BTreeSet;

    use crate::cli::RunsTimeArgs;

    use super::{
        ExecutionListRow, execution_duration_ms, execution_node_rows, format_duration,
        note_new_executions, parse_runs_time_filter, parse_time_window,
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
}
