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
        RunsWatchArgs, StatsGroupBy,
    },
    config::resolve_instance_alias,
    error::AppError,
    repo::{workflow_active, workflow_name},
};

use owo_colors::OwoColorize;

use super::common::{
    Context, emit_json, emit_json_line, load_loaded_repo, print_message, remote_client, truncate,
    use_color, value_string,
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
    /// Populated only by `--explain`, which costs one detail request per row.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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
    let offset = args.offset as usize;
    let limit = args.limit as usize;
    // The offset is applied in-band, so the fetch must cover it as well as the
    // page itself. Fetching only `limit` rows made every page after the first
    // come back empty, while still reporting success.
    let wanted = offset.saturating_add(limit);
    let (rows, more_beyond_limit) = fetch_execution_rows(
        &client,
        workflow_id.as_deref(),
        args.status.as_deref(),
        since,
        wanted,
    )
    .await?;
    let note = execution_history_note(&client, workflow_id.as_deref(), &rows).await?;

    let total = rows.len();

    // Apply in-band offset/limit so callers can paginate the result set.
    let mut page: Vec<_> = rows.iter().skip(offset).take(limit).cloned().collect();
    if args.explain {
        explain_execution_rows(&client, &mut page).await?;
    }

    if context.json {
        let mut data = serde_json::Map::new();
        data.insert("items".to_string(), json!(page));
        data.insert("total".to_string(), json!(total));
        data.insert("limit".to_string(), json!(limit));
        data.insert("offset".to_string(), json!(offset));
        data.insert(
            "truncated".to_string(),
            json!(more_beyond_limit || (offset + page.len()) < total),
        );
        if let Some(note) = note {
            data.insert("note".to_string(), json!(note));
        }
        emit_json("runs", &Value::Object(data))
    } else {
        if let Some(workflow) = args.workflow.as_deref() {
            print_message(context, &format!("Workflow filter: {workflow}"));
        }
        if let Some(status) = args.status.as_deref() {
            print_message(context, &format!("Status filter: {status}"));
        }
        if let Some(since_label) = time_filter.describe(&since) {
            print_message(context, &since_label);
        }
        if let Some(note) = note {
            print_message(context, &format!("Note: {note}"));
        }
        print_execution_rows(&page);
        if offset > 0 || (offset + page.len()) < total {
            eprintln!(
                "Showing {}-{} of {} results. Use --offset and --limit to paginate.",
                offset,
                offset + page.len(),
                total
            );
        }
        if more_beyond_limit {
            eprintln!("More executions exist beyond --limit {limit}. Raise it to see them.");
        }
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
        let (rows, _) = fetch_execution_rows(
            &client,
            workflow_id.as_deref(),
            args.status.as_deref(),
            since,
            args.limit as usize,
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
            print_message(
                context,
                &format!(
                    "Watching executions on `{alias}` every {}s. Press Ctrl-C to stop.",
                    args.interval.max(1)
                ),
            );
            if let Some(workflow) = args.workflow.as_deref() {
                print_message(context, &format!("Workflow filter: {workflow}"));
            }
            if let Some(status) = args.status.as_deref() {
                print_message(context, &format!("Status filter: {status}"));
            }
            if let Some(since_label) = time_filter.describe(&since) {
                print_message(context, &since_label);
            }
            if rows.is_empty() {
                print_message(context, "No executions found.");
            } else {
                print_message(context, "Current executions:");
                print_execution_rows(&rows);
            }
        } else if !new_rows.is_empty() {
            print_message(context, "");
            print_message(context, "New executions:");
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

    // `--summary` and `--node` need the run data fetched, but never emit it.
    let node_filter = args.node.as_deref();
    let want_data = args.details || args.summary || node_filter.is_some();
    let mut execution = client
        .get_execution(&args.execution_id, want_data)
        .await?
        .ok_or_else(|| {
            AppError::not_found(
                "runs",
                format!("Execution `{}` was not found.", args.execution_id),
            )
        })?;

    if let Some(node) = node_filter {
        let selected = execution_node_output(&execution, node).ok_or_else(|| {
            AppError::not_found(
                "runs",
                format!(
                    "Node `{node}` did not run in execution `{}`.",
                    args.execution_id
                ),
            )
        })?;
        strip_execution_payload(&mut execution);
        if context.json {
            let mut data = serde_json::Map::new();
            data.insert("execution".to_string(), execution);
            data.insert("node".to_string(), selected);
            return emit_json("runs", &Value::Object(data));
        }
        let rendered = serde_json::to_string_pretty(&selected).map_err(|err| {
            AppError::api(
                "runs",
                "runs.render_failed",
                format!("Failed to render node output: {err}"),
            )
        })?;
        println!("{rendered}");
        return Ok(());
    }

    let node_executions = (args.details || args.summary).then(|| execution_node_rows(&execution));
    let run_data = args.details.then(|| execution_run_data_value(&execution));
    if args.summary {
        strip_execution_payload(&mut execution);
    }

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
        let color = use_color();
        let wf_id = value_string(&execution, "workflowId");
        let wf_name = workflow_name_for_execution(&client, &execution).await?;
        println!(
            "Execution: {}",
            value_string(&execution, "id").unwrap_or(args.execution_id)
        );
        if let Some(status) = value_string(&execution, "status") {
            let status_display: String = if color {
                colorize_execution_status(&status)
            } else {
                status
            };
            println!("Status: {status_display}");
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

        if args.details || args.summary {
            let nodes = node_executions.unwrap_or_default();
            if !nodes.is_empty() {
                println!();
                if color {
                    println!(
                        "{:<32} {:<10} {:<10} {}",
                        "NODE".bold(),
                        "STATUS".bold(),
                        "TIME".bold(),
                        "OUTPUTS".bold()
                    );
                } else {
                    println!("{:<32} {:<10} {:<10} OUTPUTS", "NODE", "STATUS", "TIME");
                }
                for node in nodes {
                    let node_status = node.status.as_deref().unwrap_or("-");
                    let node_status_padded = format!("{:<10}", truncate(node_status, 10));
                    let node_status_display: String = if color {
                        match node_status {
                            "success" => node_status_padded.green().to_string(),
                            "error" | "crashed" => node_status_padded.red().to_string(),
                            "running" => node_status_padded.cyan().to_string(),
                            _ => node_status_padded,
                        }
                    } else {
                        node_status_padded
                    };
                    println!(
                        "{:<32} {} {:<10} {}",
                        truncate(&node.name, 32),
                        node_status_display,
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
    /// Retained for the v1 contract. Always false: stats now walk every page in
    /// the window, so the aggregate can no longer be computed from a sample.
    capped: bool,
    /// Cadence over the window. `None` when nothing ran: an absent execution and
    /// a gap of zero are different facts.
    #[serde(skip_serializing_if = "Option::is_none")]
    first: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last: Option<String>,
    /// Largest interval between consecutive executions, and its endpoints.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_gap_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_gap_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_gap_to: Option<String>,
    /// Present only with `--by workflow`.
    #[serde(skip_serializing_if = "Option::is_none")]
    groups: Option<Vec<StatsGroup>>,
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

#[derive(Debug, Clone, Serialize)]
struct StatsGroup {
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_name: Option<String>,
    total: usize,
    succeeded: usize,
    failed: usize,
    running: usize,
    waiting: usize,
    success_rate: f64,
}

/// First and last execution in the window, plus the largest interval between
/// consecutive ones. A monitoring workflow on a fixed cadence turns this into an
/// outage detector: a heartbeat every 5 minutes with a 100-minute gap means the
/// instance was down.
#[derive(Debug, Clone, Default)]
struct Cadence {
    first: Option<String>,
    last: Option<String>,
    max_gap_ms: Option<i64>,
    max_gap_from: Option<String>,
    max_gap_to: Option<String>,
}

fn compute_cadence(executions: &[Value]) -> Cadence {
    let mut stamps: Vec<(DateTime<Utc>, String)> = executions
        .iter()
        .filter_map(|execution| {
            let raw = value_string(execution, "startedAt")?;
            let parsed = DateTime::parse_from_rfc3339(&raw).ok()?;
            Some((parsed.with_timezone(&Utc), raw))
        })
        .collect();
    if stamps.is_empty() {
        return Cadence::default();
    }
    stamps.sort_by_key(|(when, _)| *when);

    let mut cadence = Cadence {
        first: Some(stamps[0].1.clone()),
        last: Some(stamps[stamps.len() - 1].1.clone()),
        ..Cadence::default()
    };

    for pair in stamps.windows(2) {
        let gap = (pair[1].0 - pair[0].0).num_milliseconds();
        if cadence.max_gap_ms.is_none_or(|current| gap > current) {
            cadence.max_gap_ms = Some(gap);
            cadence.max_gap_from = Some(pair[0].1.clone());
            cadence.max_gap_to = Some(pair[1].1.clone());
        }
    }

    cadence
}

/// Groups the window per workflow, failures first: that is the triage question.
fn group_by_workflow(executions: &[Value], names: &BTreeMap<String, String>) -> Vec<StatsGroup> {
    let mut buckets: BTreeMap<String, [usize; 4]> = BTreeMap::new();
    for execution in executions {
        let wf_id = value_string(execution, "workflowId").unwrap_or_default();
        let counts = buckets.entry(wf_id).or_insert([0; 4]);
        match value_string(execution, "status").as_deref() {
            Some("success") => counts[0] += 1,
            Some("error") => counts[1] += 1,
            Some("running") => counts[2] += 1,
            Some("waiting") => counts[3] += 1,
            _ => {}
        }
    }

    let mut groups: Vec<StatsGroup> = buckets
        .into_iter()
        .map(|(wf_id, [succeeded, failed, running, waiting])| {
            let total = succeeded + failed + running + waiting;
            StatsGroup {
                workflow_name: names.get(&wf_id).cloned(),
                workflow_id: (!wf_id.is_empty()).then_some(wf_id),
                total,
                succeeded,
                failed,
                running,
                waiting,
                success_rate: if total > 0 {
                    succeeded as f64 / total as f64 * 100.0
                } else {
                    0.0
                },
            }
        })
        .collect();

    groups.sort_by(|a, b| {
        b.failed
            .cmp(&a.failed)
            .then(b.total.cmp(&a.total))
            .then(a.workflow_id.cmp(&b.workflow_id))
    });
    groups
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

    // Every execution in the window: stats over a truncated sample would be a
    // plausible-looking lie. Pagination keeps each request within the API cap.
    let executions = client
        .list_executions(&ExecutionListOptions {
            max_results: None,
            workflow_id: workflow_id.clone(),
            status: args.status.clone(),
            since,
        })
        .await?
        .items;

    let cadence = compute_cadence(&executions);
    let groups = match args.by {
        Some(StatsGroupBy::Workflow) => {
            let names = workflow_names_for_executions(&client, &executions).await?;
            Some(group_by_workflow(&executions, &names))
        }
        None => None,
    };

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
        capped: false,
        first: cadence.first.clone(),
        last: cadence.last.clone(),
        max_gap_ms: cadence.max_gap_ms,
        max_gap_from: cadence.max_gap_from.clone(),
        max_gap_to: cadence.max_gap_to.clone(),
        groups: groups.clone(),
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
        if let Some(status) = args.status.as_deref() {
            println!("Status filter: {status}");
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
        if let (Some(first), Some(last)) = (cadence.first.as_deref(), cadence.last.as_deref()) {
            println!();
            println!("Cadence:");
            println!("  First: {first}");
            println!("  Last:  {last}");
            if let Some(gap) = cadence.max_gap_ms {
                println!(
                    "  Largest gap: {} ({} -> {})",
                    format_duration(Some(gap)),
                    cadence.max_gap_from.as_deref().unwrap_or("?"),
                    cadence.max_gap_to.as_deref().unwrap_or("?")
                );
            }
        }
        if let Some(groups) = groups.as_ref() {
            println!();
            println!(
                "{:<34} {:>6} {:>8} {:>7}",
                "WORKFLOW", "TOTAL", "FAILED", "OK%"
            );
            for group in groups {
                let label = group
                    .workflow_name
                    .clone()
                    .or_else(|| group.workflow_id.clone())
                    .unwrap_or_else(|| "<unknown>".to_string());
                println!(
                    "{:<34} {:>6} {:>8} {:>6.1}%",
                    truncate(&label, 34),
                    group.total,
                    group.failed,
                    group.success_rate
                );
            }
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

/// Fetches up to `max_results` executions, following `nextCursor` across pages.
/// The bool reports whether the API still had rows when the cap stopped the walk.
async fn fetch_execution_rows(
    client: &ApiClient,
    workflow_id: Option<&str>,
    status: Option<&str>,
    since: Option<DateTime<Utc>>,
    max_results: usize,
) -> Result<(Vec<ExecutionListRow>, bool), AppError> {
    let page = client
        .list_executions(&ExecutionListOptions {
            max_results: Some(max_results),
            workflow_id: workflow_id.map(ToOwned::to_owned),
            status: status.map(ToOwned::to_owned),
            since,
        })
        .await?;
    let truncated = page.truncated;
    let executions = page.items;
    let workflow_names = workflow_names_for_executions(client, &executions).await?;

    let rows = executions
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
                last_node: None,
                error: None,
            }
        })
        .collect();

    Ok((rows, truncated))
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

/// Fills in `last_node` and `error` for each row, one detail request per row.
/// Only the failure cause is kept; the megabytes of run data are discarded.
async fn explain_execution_rows(
    client: &ApiClient,
    rows: &mut [ExecutionListRow],
) -> Result<(), AppError> {
    for row in rows.iter_mut() {
        let Some(execution) = client.get_execution(&row.id, true).await? else {
            continue;
        };
        let result_data = execution
            .get("data")
            .and_then(|data| data.get("resultData"));
        let Some(result_data) = result_data else {
            continue;
        };
        row.last_node = result_data
            .get("lastNodeExecuted")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        row.error = result_data
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    Ok(())
}

fn execution_run_data_value(execution: &Value) -> Value {
    execution
        .get("data")
        .and_then(|data| data.get("resultData"))
        .and_then(|result| result.get("runData"))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Drops the two heavy blobs once whatever we needed has been projected out:
/// `data` (every node's inputs and outputs) and `workflowData` (the whole
/// workflow definition). Both are megabyte-scale on a real workflow and neither
/// is what `--summary` or `--node` was asked for.
fn strip_execution_payload(execution: &mut Value) {
    if let Some(object) = execution.as_object_mut() {
        object.remove("data");
        object.remove("workflowData");
    }
}

/// One node's output items plus its status, without the rest of the run.
fn execution_node_output(execution: &Value, node: &str) -> Option<Value> {
    let runs = execution
        .get("data")?
        .get("resultData")?
        .get("runData")?
        .get(node)?
        .as_array()?;

    let mut items: Vec<Value> = Vec::new();
    let mut status: Option<String> = None;
    let mut execution_time_ms: Option<i64> = None;

    for run in runs {
        if status.is_none() {
            status = run
                .get("executionStatus")
                .or_else(|| run.get("status"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        if execution_time_ms.is_none() {
            execution_time_ms = run.get("executionTime").and_then(Value::as_i64);
        }
        let Some(main) = run
            .get("data")
            .and_then(|data| data.get("main"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for branch in main {
            if let Some(branch_items) = branch.as_array() {
                items.extend(branch_items.iter().cloned());
            }
        }
    }

    Some(json!({
        "name": node,
        "status": status,
        "execution_time_ms": execution_time_ms,
        "output_items": items.len(),
        "items": items,
    }))
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

fn colorize_execution_status(status: &str) -> String {
    match status {
        "success" => status.green().to_string(),
        "error" | "crashed" => status.red().to_string(),
        "running" | "new" => status.cyan().to_string(),
        "waiting" => status.yellow().to_string(),
        _ => status.to_string(),
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
    let color = use_color();
    if color {
        println!(
            "{:<10} {:<10} {:<10} {:<10} {:<24} {}",
            "ID".bold(),
            "STATUS".bold(),
            "MODE".bold(),
            "DURATION".bold(),
            "STARTED".bold(),
            "WORKFLOW".bold()
        );
    } else {
        println!(
            "{:<10} {:<10} {:<10} {:<10} {:<24} WORKFLOW",
            "ID", "STATUS", "MODE", "DURATION", "STARTED"
        );
    }
    for row in rows {
        let status_str = row.status.as_deref().unwrap_or("-");
        let status_padded = format!("{:<10}", truncate(status_str, 10));
        let status_display: String = if color {
            match status_str {
                "success" => status_padded.green().to_string(),
                "error" | "crashed" => status_padded.red().to_string(),
                "running" | "new" => status_padded.cyan().to_string(),
                "waiting" => status_padded.yellow().to_string(),
                _ => status_padded,
            }
        } else {
            status_padded
        };
        let id_display: String = if color {
            format!("{:<10}", truncate(&row.id, 10)).cyan().to_string()
        } else {
            format!("{:<10}", truncate(&row.id, 10))
        };
        println!(
            "{} {} {:<10} {:<10} {:<24} {}",
            id_display,
            status_display,
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
                last_node: None,
                error: None,
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
                last_node: None,
                error: None,
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

        assert_eq!(err.kind, "usage");
        assert!(
            err.message
                .contains("`--since` must be an RFC3339 timestamp")
        );
    }
}
