use serde_json::json;

use crate::{
    cli::{
        ConnAddArgs, ConnArgs, ConnCommand, ConnRemoveArgs, ExprArgs, ExprCommand, ExprSetArgs,
        NodeAddArgs, NodeArgs, NodeCommand, NodeListArgs, NodeRemoveArgs, NodeRenameArgs,
        NodeSetArgs, NodeSetRemoteArgs,
    },
    edit::{
        add_connection, add_node, apply_node_value, remove_connection, remove_node, rename_node,
        set_node_expression, set_node_value, workflow_id_string,
    },
    error::AppError,
    repo::{workflow_active, workflow_id},
};

use super::{
    common::{
        Context, emit_edit_result, emit_json, load_loaded_repo, parse_node_value, remote_client,
        resolve_local_file_path, workflow_update_payload,
    },
    workflow::{print_workflow_nodes, summarize_workflow_nodes},
};

use crate::canonical::{canonicalize_workflow, hash_value};
use crate::repo::load_workflow_file;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_node(context: &Context, args: NodeArgs) -> Result<(), AppError> {
    match args.command {
        NodeCommand::Ls(args) => cmd_node_ls(context, args).await,
        NodeCommand::Add(args) => cmd_node_add(context, args).await,
        NodeCommand::Set(args) => cmd_node_set(context, args).await,
        NodeCommand::SetRemote(args) => cmd_node_set_remote(context, args).await,
        NodeCommand::Rename(args) => cmd_node_rename(context, args).await,
        NodeCommand::Rm(args) => cmd_node_remove(context, args).await,
    }
}

/// Edits one node on a remote workflow, without making it a tracked artifact.
///
/// The write is guarded the way a hand-rolled `PUT` is not: the workflow is
/// re-read immediately before writing and refused if it changed underneath
/// (exit `12`), only the mutable fields are sent, and if the update clears
/// `active` the workflow is reactivated. A no-op edit issues no write at all.
async fn cmd_node_set_remote(context: &Context, args: NodeSetRemoteArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "node")?;

    let from_file = match args.value_file.as_deref() {
        Some(path) => Some(std::fs::read_to_string(path).map_err(|err| {
            AppError::config("node", format!("Cannot read {}: {err}", path.display()))
        })?),
        None => None,
    };
    let value = parse_node_value(
        "node",
        &args.mode,
        from_file.as_deref().or(args.value.as_deref()),
    )?;

    let before = client.resolve_workflow(&args.workflow).await?;
    let workflow_id = workflow_id(&before).ok_or_else(|| {
        AppError::api(
            "node",
            "api.invalid_response",
            format!("Workflow `{}` has no id.", args.workflow),
        )
    })?;
    let was_active = workflow_active(&before).unwrap_or(false);

    // The lease: what we read is what we are allowed to overwrite.
    let canonical_before = canonicalize_workflow(&before)?;
    let lease = hash_value(&canonical_before)?;

    let mut edited = canonical_before;
    apply_node_value(&mut edited, "node", &args.node, &args.path, value)?;
    let changed = hash_value(&edited)? != lease;

    if !changed || args.dry_run {
        return emit_set_remote_result(
            context,
            &workflow_id,
            &args,
            changed,
            was_active,
            false,
            args.dry_run,
        );
    }

    // Re-read right before writing. A concurrent editor is a conflict, not a
    // silent overwrite.
    let current = client.resolve_workflow(&workflow_id).await?;
    if hash_value(&canonicalize_workflow(&current)?)? != lease {
        return Err(AppError::conflict(
            "node",
            format!("Workflow `{workflow_id}` changed on the remote since it was read."),
        )
        .with_suggestion("Re-run the command to edit the current version."));
    }

    let (payload, _stripped) = workflow_update_payload(&edited)?;
    let updated = client.update_workflow(&workflow_id, &payload).await?;

    // n8n's update endpoint can return the workflow with `active` cleared.
    let mut reactivated = false;
    let mut active_now = workflow_active(&updated).unwrap_or(false);
    if was_active && !active_now {
        client.activate_workflow(&workflow_id).await?;
        let refetched = client.resolve_workflow(&workflow_id).await?;
        active_now = workflow_active(&refetched).unwrap_or(false);
        reactivated = true;
    }

    emit_set_remote_result(
        context,
        &workflow_id,
        &args,
        true,
        active_now,
        reactivated,
        false,
    )
}

fn emit_set_remote_result(
    context: &Context,
    workflow_id: &str,
    args: &NodeSetRemoteArgs,
    changed: bool,
    active: bool,
    reactivated: bool,
    dry_run: bool,
) -> Result<(), AppError> {
    if context.json {
        return emit_json(
            "node",
            &json!({
                "workflow_id": workflow_id,
                "node": args.node,
                "path": args.path,
                "changed": changed,
                "dry_run": dry_run,
                "active": active,
                "reactivated": reactivated,
            }),
        );
    }

    let verb = if dry_run {
        "Would update"
    } else if changed {
        "Updated"
    } else {
        "No change for"
    };
    println!("{verb} `{}` on remote workflow {workflow_id}", args.node);
    if reactivated {
        println!("Reactivated the workflow: the update had cleared `active`.");
    }
    Ok(())
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
    // A file value is read verbatim: multiline `jsCode` bodies must survive.
    let from_file = match args.value_file.as_deref() {
        Some(path) => Some(std::fs::read_to_string(path).map_err(|err| {
            AppError::config("node", format!("Cannot read {}: {err}", path.display()))
        })?),
        None => None,
    };
    let value = parse_node_value(
        "node",
        &args.mode,
        from_file.as_deref().or(args.value.as_deref()),
    )?;
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

pub(crate) async fn cmd_conn(context: &Context, args: ConnArgs) -> Result<(), AppError> {
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

pub(crate) async fn cmd_expr(context: &Context, args: ExprArgs) -> Result<(), AppError> {
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
