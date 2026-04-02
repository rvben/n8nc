use serde_json::json;

use crate::{
    auth::{resolve_browser_id, resolve_session_cookie},
    cli::IdArgs,
    config::resolve_instance_alias,
    error::AppError,
    repo::{
        find_existing_workflow_path, load_meta, sidecar_path_for, store_workflow, workflow_active,
        workflow_id, workflow_name,
    },
};

use super::{
    auth::session_auth_setup_hint,
    common::{
        Context, emit_json, load_loaded_repo, print_message, remote_client,
        wait_for_workflow_active_state,
    },
    workflow::{print_workflow_webhooks, summarize_workflow_webhooks},
};

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_activation(
    context: &Context,
    args: IdArgs,
    active: bool,
) -> Result<(), AppError> {
    let command = if active { "activate" } else { "deactivate" };
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), command)?;
    let (client, _, base_url) = remote_client(&repo, Some(&alias), command)?;
    let workflow = client.resolve_workflow(&args.identifier).await?;
    let wf_id = workflow_id(&workflow).ok_or_else(|| {
        AppError::api(
            command,
            "api.invalid_response",
            "Workflow payload was missing `id`.",
        )
    })?;

    if active {
        client.activate_workflow(&wf_id).await?;
    } else {
        client.deactivate_workflow(&wf_id).await?;
    }
    let current = wait_for_workflow_active_state(&client, &wf_id, command, active).await?;
    if let Some(path) = find_existing_workflow_path(&repo, &wf_id) {
        let meta_path = sidecar_path_for(&path);
        if meta_path.exists() {
            let meta = load_meta(&meta_path, command)?;
            if meta.instance == alias {
                let _ = store_workflow(&repo, &alias, &current)?;
            }
        }
    }
    let active_state = workflow_active(&current).unwrap_or(active);
    let webhooks = summarize_workflow_webhooks(&current, Some(base_url.as_str()));

    if context.json {
        emit_json(
            command,
            &json!({"workflow_id": wf_id, "active": active_state, "webhooks": webhooks}),
        )
    } else {
        print_message(
            context,
            &format!(
                "{} {wf_id}.",
                if active_state { "Activated" } else { "Deactivated" }
            ),
        );
        if active_state {
            print_workflow_webhooks(&webhooks);
        }
        Ok(())
    }
}

pub(crate) async fn cmd_archive(
    context: &Context,
    args: IdArgs,
    archive: bool,
) -> Result<(), AppError> {
    let command = if archive { "archive" } else { "unarchive" };
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), command)?;
    let (client, _, _base_url) = remote_client(&repo, Some(&alias), command)?;

    let workflow = client.resolve_workflow(&args.identifier).await?;
    let wf_id = workflow_id(&workflow).ok_or_else(|| {
        AppError::api(
            command,
            "api.invalid_response",
            "Workflow payload was missing `id`.",
        )
    })?;
    let active_before = workflow_active(&workflow).unwrap_or(false);
    let workflow_name_str = workflow_name(&workflow).unwrap_or_else(|| "<unnamed>".to_string());

    let is_archived = workflow
        .get("isArchived")
        .and_then(serde_json::Value::as_bool);
    if archive && is_archived == Some(true) {
        if context.json {
            return emit_json(
                command,
                &json!({
                    "action": command,
                    "instance": alias,
                    "workflow_id": wf_id,
                    "workflow_name": workflow_name_str,
                    "active_before": active_before,
                    "active_after": active_before,
                    "already_archived": true,
                    "note": "Uses n8n internal API (session auth)"
                }),
            );
        } else {
            print_message(context, &format!("Already archived: \"{workflow_name_str}\" ({wf_id})"));
            return Ok(());
        }
    }
    if !archive && is_archived == Some(false) {
        if context.json {
            return emit_json(
                command,
                &json!({
                    "action": command,
                    "instance": alias,
                    "workflow_id": wf_id,
                    "workflow_name": workflow_name_str,
                    "active_before": active_before,
                    "active_after": active_before,
                    "already_unarchived": true,
                    "note": "Uses n8n internal API (session auth)"
                }),
            );
        } else {
            print_message(context, &format!("Already unarchived: \"{workflow_name_str}\" ({wf_id})"));
            return Ok(());
        }
    }

    let (session_cookie, browser_id) = require_session_auth(&alias, command)?;

    if archive {
        client
            .archive_workflow(&wf_id, &session_cookie, &browser_id)
            .await?;
    } else {
        client
            .unarchive_workflow(&wf_id, &session_cookie, &browser_id)
            .await?;
    }

    let (active_after, refetch_ok) = match client.get_workflow_by_id(&wf_id).await {
        Ok(Some(current)) => {
            let active = workflow_active(&current).unwrap_or(false);
            if let Some(path) = find_existing_workflow_path(&repo, &wf_id) {
                let meta_path = sidecar_path_for(&path);
                if meta_path.exists() {
                    let meta = load_meta(&meta_path, command)?;
                    if meta.instance == alias {
                        let _ = store_workflow(&repo, &alias, &current)?;
                    }
                }
            }
            (active, true)
        }
        Ok(None) if archive => (false, false),
        Ok(None) => {
            return Err(AppError::not_found(
                command,
                format!("Workflow {wf_id} not found after {command}."),
            ));
        }
        Err(_) if archive => (false, false),
        Err(err) => return Err(err),
    };

    if context.json {
        emit_json(
            command,
            &json!({
                "action": command,
                "instance": alias,
                "workflow_id": wf_id,
                "workflow_name": workflow_name_str,
                "active_before": active_before,
                "active_after": active_after,
                "note": "Uses n8n internal API (session auth)"
            }),
        )
    } else {
        let action_word = if archive { "Archived" } else { "Unarchived" };
        print_message(context, &format!("{action_word} \"{workflow_name_str}\" ({wf_id}) on {alias}"));
        if archive && active_before {
            print_message(context, "  Workflow was deactivated automatically");
        }
        print_message(context, "  Note: uses n8n internal API (session auth required)");
        if !refetch_ok {
            print_message(
                context,
                "  Warning: could not re-fetch workflow after archive (public API may not expose archived workflows)",
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_session_auth(alias: &str, command: &'static str) -> Result<(String, String), AppError> {
    let cookie = resolve_session_cookie(alias, command)?
        .ok_or_else(|| {
            AppError::auth(
                command,
                format!("Session auth required for {command}. Run `n8nc auth session add {alias}` to configure."),
            )
            .with_suggestion(session_auth_setup_hint(alias))
        })?;
    let browser_id = resolve_browser_id(alias, command)?
        .ok_or_else(|| {
            AppError::auth(
                command,
                format!("Browser ID required for {command}. Run `n8nc auth session add {alias}` to configure."),
            )
            .with_suggestion(session_auth_setup_hint(alias))
        })?;
    Ok((cookie.value, browser_id.value))
}
