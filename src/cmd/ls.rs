use owo_colors::OwoColorize;
use serde::Serialize;
use serde_json::json;

use crate::{
    api::ListOptions,
    cli::ListArgs,
    error::AppError,
    repo::{workflow_active, workflow_id, workflow_name, workflow_updated_at},
};

use super::common::{Context, emit_json, load_loaded_repo, remote_client, truncate, use_color};

#[derive(Debug, Serialize)]
struct WorkflowListRow {
    id: String,
    name: String,
    active: Option<bool>,
    updated_at: Option<String>,
}

pub(crate) async fn cmd_ls(context: &Context, args: ListArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "ls")?;
    let offset = args.offset as usize;
    // The offset is applied in-band, so the fetch must cover it as well as the
    // page itself. Fetching only `limit` rows made every page after the first
    // come back empty.
    let wanted = offset.saturating_add(args.limit as usize);
    let page = client
        .list_workflows(&ListOptions {
            max_results: Some(wanted),
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

    // The API stopped us short of every match; the in-band slice below can hide
    // rows too. Either way the caller must be told.
    let more_beyond_limit = page.truncated;
    let workflows = page.items;
    let total = workflows.len();

    // Apply offset in-band (the API returns from the start; we slice here).
    let paginated: Vec<WorkflowListRow> = workflows
        .into_iter()
        .skip(offset)
        .take(args.limit as usize)
        .map(|workflow| WorkflowListRow {
            id: workflow_id(&workflow).unwrap_or_default(),
            name: workflow_name(&workflow).unwrap_or_else(|| "<unnamed>".to_string()),
            active: workflow_active(&workflow),
            updated_at: workflow_updated_at(&workflow),
        })
        .collect();

    // Apply --fields filter if requested.
    let selected_fields: Option<Vec<String>> = args.fields.as_ref().map(|f| {
        f.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });

    if context.json {
        let items: Vec<serde_json::Value> = paginated
            .iter()
            .map(|row| {
                let full = json!({
                    "id": row.id,
                    "name": row.name,
                    "active": row.active,
                    "updated_at": row.updated_at,
                });
                if let Some(fields) = &selected_fields {
                    let obj = full.as_object().unwrap();
                    let filtered: serde_json::Map<String, serde_json::Value> = fields
                        .iter()
                        .filter_map(|f| obj.get_key_value(f.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    serde_json::Value::Object(filtered)
                } else {
                    full
                }
            })
            .collect();

        emit_json(
            "ls",
            &json!({
                "items": items,
                "total": total,
                "limit": args.limit,
                "offset": offset,
                "truncated": more_beyond_limit || (offset + paginated.len()) < total,
            }),
        )
    } else {
        let color = use_color();
        if color {
            println!(
                "{:<20} {:<8} {:<24} {}",
                "ID".bold(),
                "ACTIVE".bold(),
                "UPDATED".bold(),
                "NAME".bold()
            );
        } else {
            println!("{:<20} {:<8} {:<24} NAME", "ID", "ACTIVE", "UPDATED");
        }
        for row in &paginated {
            let id = truncate(&row.id, 20);
            let active_label = row
                .active
                .map(|value| if value { "true" } else { "false" })
                .unwrap_or("-");
            let updated = row.updated_at.as_deref().unwrap_or("-");
            if color {
                let id_padded = format!("{id:<20}");
                let active_padded = format!("{active_label:<8}");
                let updated_padded = format!("{updated:<24}");
                let active_colored: String = match row.active {
                    Some(true) => active_padded.green().to_string(),
                    Some(false) => active_padded.dimmed().to_string(),
                    None => active_padded,
                };
                println!(
                    "{} {} {} {}",
                    id_padded.cyan(),
                    active_colored,
                    updated_padded.dimmed(),
                    row.name
                );
            } else {
                println!(
                    "{:<20} {:<8} {:<24} {}",
                    id, active_label, updated, row.name
                );
            }
        }
        if offset > 0 || (offset + paginated.len()) < total {
            eprintln!(
                "Showing {}-{} of {} results. Use --offset and --limit to paginate.",
                offset,
                offset + paginated.len(),
                total
            );
        }
        if more_beyond_limit {
            eprintln!(
                "More workflows exist beyond --limit {}. Raise it to see them.",
                args.limit
            );
        }
        Ok(())
    }
}
