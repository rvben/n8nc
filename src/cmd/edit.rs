use serde_json::json;

use crate::{
    cli::{
        ConnAddArgs, ConnArgs, ConnCommand, ConnRemoveArgs, ExprArgs, ExprCommand, ExprSetArgs,
        NodeAddArgs, NodeArgs, NodeCommand, NodeListArgs, NodeRemoveArgs, NodeRenameArgs,
        NodeSetArgs,
    },
    edit::{
        add_connection, add_node, remove_connection, remove_node, rename_node, set_node_expression,
        set_node_value, workflow_id_string,
    },
    error::AppError,
    repo::workflow_id,
};

use super::{
    common::{Context, emit_edit_result, emit_json, parse_node_value, resolve_local_file_path},
    workflow::{print_workflow_nodes, summarize_workflow_nodes},
};

use crate::canonical::canonicalize_workflow;
use crate::repo::load_workflow_file;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_node(context: &Context, args: NodeArgs) -> Result<(), AppError> {
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
