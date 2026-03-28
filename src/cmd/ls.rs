use serde::Serialize;
use serde_json::json;

use crate::{
    api::ListOptions,
    cli::ListArgs,
    error::AppError,
    repo::{workflow_active, workflow_id, workflow_name, workflow_updated_at},
};

use super::common::{Context, emit_json, load_loaded_repo, remote_client, truncate};

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
        println!("{:<20} {:<8} {:<24} NAME", "ID", "ACTIVE", "UPDATED");
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
