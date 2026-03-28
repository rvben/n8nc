use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::{
    api::ApiClient,
    auth::{resolve_browser_id, resolve_session_cookie},
    canonical::pretty_json,
    cli::{
        CredentialArgs, CredentialCommand, CredentialListArgs, CredentialSchemaArgs,
        CredentialSetArgs, CredentialSource,
    },
    config::resolve_instance_alias,
    edit::{set_credential_reference, workflow_id_string},
    error::AppError,
};

use super::{
    auth::session_auth_setup_hint,
    common::{
        emit_edit_result, emit_json, load_loaded_repo, remote_client, resolve_local_file_path,
        truncate, Context,
    },
    workflow::{
        CredentialReferenceRow,
        summarize_credential_references,
    },
};

use serde::Serialize;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CredentialInventorySource {
    PublicApi,
    RestSession,
    WorkflowReferences,
}

#[derive(Debug, Clone, Serialize)]
struct CredentialListResult {
    requested_source: &'static str,
    resolved_source: CredentialInventorySource,
    coverage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback_reason: Option<String>,
    note: String,
    credentials: Vec<CredentialReferenceRow>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_credential(context: &Context, args: CredentialArgs) -> Result<(), AppError> {
    match args.command {
        CredentialCommand::Ls(args) => cmd_credential_list(context, args).await,
        CredentialCommand::Schema(args) => cmd_credential_schema(context, args).await,
        CredentialCommand::Set(args) => cmd_credential_set(context, args).await,
    }
}

async fn cmd_credential_list(context: &Context, args: CredentialListArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "credential")?;
    let (client, _, _) = remote_client(&repo, Some(&alias), "credential")?;
    let result = build_credential_list_result(&alias, &client, &args).await?;

    if context.json {
        emit_json(
            "credential",
            &json!({
                "instance": alias,
                "workflow_filter": args.workflow,
                "credential_type_filter": args.credential_type,
                "requested_source": result.requested_source,
                "resolved_source": result.resolved_source,
                "count": result.credentials.len(),
                "coverage": result.coverage,
                "fallback_reason": result.fallback_reason,
                "note": result.note,
                "credentials": result.credentials,
            }),
        )
    } else {
        println!("Credentials ({alias}):");
        println!("  requested source: {}", result.requested_source);
        println!(
            "  resolved source: {}",
            credential_inventory_source_label(result.resolved_source)
        );
        println!("  coverage: {}", result.coverage);
        if let Some(reason) = &result.fallback_reason {
            println!("  fallback: {reason}");
        }
        if result.credentials.is_empty() {
            println!("  none found");
            println!("  {}", result.note);
            return Ok(());
        }
        println!(
            "{:<24} {:<18} {:<28} {:<8} WORKFLOWS",
            "TYPE", "ID", "NAME", "USES"
        );
        for row in &result.credentials {
            let workflows = row
                .workflows
                .iter()
                .map(|usage| {
                    let name = usage
                        .workflow_name
                        .as_deref()
                        .or(usage.workflow_id.as_deref())
                        .unwrap_or("<unknown>");
                    if usage.nodes.is_empty() {
                        name.to_string()
                    } else {
                        format!("{name} [{}]", usage.nodes.join(", "))
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            println!(
                "{:<24} {:<18} {:<28} {:<8} {}",
                truncate(&row.credential_type, 24),
                truncate(row.credential_id.as_deref().unwrap_or("-"), 18),
                truncate(row.credential_name.as_deref().unwrap_or("-"), 28),
                row.usage_count,
                workflows
            );
        }
        println!("{}", result.note);
        Ok(())
    }
}

async fn cmd_credential_schema(
    context: &Context,
    args: CredentialSchemaArgs,
) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = resolve_instance_alias(&repo, args.remote.instance.as_deref(), "credential")?;
    let (client, _, _) = remote_client(&repo, Some(&alias), "credential")?;
    let schema = client.get_credential_schema(&args.credential_type).await?;

    if context.json {
        emit_json(
            "credential",
            &json!({
                "instance": alias,
                "credential_type": args.credential_type,
                "schema": schema,
            }),
        )
    } else {
        println!("Credential schema: {}", args.credential_type);
        println!("{}", pretty_json(&schema)?);
        Ok(())
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
            (
                "credential_discovery".to_string(),
                json!(
                    "Use `n8nc credential ls` to discover credential IDs with the best available source (`auto`, `public`, `rest-session`, or `workflow-refs`), `n8nc credential schema <type>` for the official schema, or source the ID from the n8n UI."
                ),
            ),
        ],
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn build_credential_list_result(
    alias: &str,
    client: &ApiClient,
    args: &CredentialListArgs,
) -> Result<CredentialListResult, AppError> {
    if args.workflow.is_some()
        && matches!(
            args.source,
            CredentialSource::Public | CredentialSource::RestSession
        )
    {
        return Err(AppError::usage(
            "credential",
            "The `--workflow` filter only works with `--source workflow-refs` or `--source auto`.",
        )
        .with_suggestion(
            "Use `n8nc credential ls --workflow <id-or-name> --source workflow-refs` for workflow-scoped usage, or remove `--workflow` to inspect instance-wide inventory.".to_string(),
        ));
    }

    if args.workflow.is_some() || args.source == CredentialSource::WorkflowRefs {
        return build_workflow_reference_credential_list(args, None, client).await;
    }

    let session_cookie = resolve_session_cookie(alias, "credential")?;
    let browser_id = resolve_browser_id(alias, "credential")?;

    match args.source {
        CredentialSource::Auto => match client.list_credentials_public().await {
            Ok(inventory) => {
                build_full_inventory_credential_list(
                    args,
                    client,
                    CredentialInventorySource::PublicApi,
                    inventory,
                    None,
                )
                .await
            }
            Err(public_err) if credential_inventory_fallback_allowed(&public_err) => {
                if let Some(cookie) = session_cookie.as_ref().map(|secret| secret.value.as_str()) {
                    if let Some(browser_id) =
                        browser_id.as_ref().map(|secret| secret.value.as_str())
                    {
                        match client
                            .list_credentials_rest_session(cookie, browser_id)
                            .await
                        {
                            Ok(inventory) => {
                                build_full_inventory_credential_list(
                                    args,
                                    client,
                                    CredentialInventorySource::RestSession,
                                    inventory,
                                    Some(format!(
                                        "Public credential inventory was unavailable: {}",
                                        public_err.message
                                    )),
                                )
                                .await
                            }
                            Err(rest_err) if credential_inventory_fallback_allowed(&rest_err) => {
                                build_workflow_reference_credential_list(
                                    args,
                                    Some(format!(
                                        "Public credential inventory was unavailable ({public_message}). Internal REST fallback was also unavailable ({rest_message}).",
                                        public_message = public_err.message,
                                        rest_message = rest_err.message,
                                    )),
                                    client,
                                )
                                .await
                            }
                            Err(rest_err) => Err(rest_err),
                        }
                    } else {
                        build_workflow_reference_credential_list(
                            args,
                            Some(format!(
                                "Public credential inventory was unavailable: {}. A session cookie is configured, but the matching browser ID is missing so the internal REST fallback cannot authenticate. {}",
                                public_err.message,
                                session_auth_setup_hint(alias)
                            )),
                            client,
                        )
                        .await
                    }
                } else {
                    build_workflow_reference_credential_list(
                        args,
                        Some(format!(
                            "Public credential inventory was unavailable: {}. No internal REST session auth is configured. {}",
                            public_err.message,
                            session_auth_setup_hint(alias)
                        )),
                        client,
                    )
                    .await
                }
            }
            Err(public_err) => Err(public_err),
        },
        CredentialSource::Public => {
            let inventory = client
                .list_credentials_public()
                .await
                .map_err(|err| forced_credential_source_error(alias, args.source, err))?;
            build_full_inventory_credential_list(
                args,
                client,
                CredentialInventorySource::PublicApi,
                inventory,
                None,
            )
            .await
        }
        CredentialSource::RestSession => {
            let cookie = session_cookie.ok_or_else(|| {
                AppError::auth(
                    "credential",
                    format!("No session cookie is configured for `{alias}`.",),
                )
                .with_suggestion(session_auth_setup_hint(alias))
            })?;
            let browser_id = browser_id.ok_or_else(|| {
                AppError::auth(
                    "credential",
                    format!("No browser ID is configured for `{alias}`."),
                )
                .with_suggestion(session_auth_setup_hint(alias))
            })?;
            let inventory = client
                .list_credentials_rest_session(&cookie.value, &browser_id.value)
                .await
                .map_err(|err| forced_credential_source_error(alias, args.source, err))?;
            build_full_inventory_credential_list(
                args,
                client,
                CredentialInventorySource::RestSession,
                inventory,
                None,
            )
            .await
        }
        CredentialSource::WorkflowRefs => unreachable!(),
    }
}

async fn build_full_inventory_credential_list(
    args: &CredentialListArgs,
    client: &ApiClient,
    source: CredentialInventorySource,
    inventory: Vec<Value>,
    fallback_reason: Option<String>,
) -> Result<CredentialListResult, AppError> {
    let workflows = load_workflows_for_credential_usage(client, None).await?;
    let usage = summarize_credential_references(&workflows, args.credential_type.as_deref());
    let credentials = merge_credential_inventory(inventory, usage, args.credential_type.as_deref());

    Ok(CredentialListResult {
        requested_source: credential_source_request_label(args.source),
        resolved_source: source,
        coverage: "full_instance",
        fallback_reason,
        note: full_inventory_note(source),
        credentials,
    })
}

async fn build_workflow_reference_credential_list(
    args: &CredentialListArgs,
    fallback_reason: Option<String>,
    client: &ApiClient,
) -> Result<CredentialListResult, AppError> {
    let workflows = load_workflows_for_credential_usage(client, args.workflow.as_deref()).await?;
    let credentials = summarize_credential_references(&workflows, args.credential_type.as_deref());

    Ok(CredentialListResult {
        requested_source: credential_source_request_label(args.source),
        resolved_source: CredentialInventorySource::WorkflowReferences,
        coverage: "workflow_references_only",
        fallback_reason,
        note: workflow_reference_inventory_note().to_string(),
        credentials,
    })
}

async fn load_workflows_for_credential_usage(
    client: &ApiClient,
    workflow_filter: Option<&str>,
) -> Result<Vec<Value>, AppError> {
    if let Some(identifier) = workflow_filter {
        return Ok(vec![client.resolve_workflow(identifier).await?]);
    }

    let listed = client
        .list_workflows(&crate::api::ListOptions {
            limit: 250,
            active: None,
            name_filter: None,
        })
        .await?;
    let mut workflows = Vec::new();
    for item in listed {
        let Some(wf_id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(workflow) = client.get_workflow_by_id(wf_id).await? {
            workflows.push(workflow.get("data").cloned().unwrap_or(workflow));
        }
    }
    Ok(workflows)
}

fn merge_credential_inventory(
    inventory: Vec<Value>,
    usage: Vec<CredentialReferenceRow>,
    credential_type_filter: Option<&str>,
) -> Vec<CredentialReferenceRow> {
    let mut usage_by_id = BTreeMap::new();
    let mut orphaned_usage = Vec::new();
    for row in usage {
        if let Some(credential_id) = row.credential_id.clone() {
            usage_by_id.insert((row.credential_type.clone(), credential_id), row);
        } else {
            orphaned_usage.push(row);
        }
    }

    let mut rows = Vec::new();
    for item in inventory {
        let Some(credential_type) = item
            .get("type")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        if let Some(filter) = credential_type_filter
            && credential_type != filter
        {
            continue;
        }

        let credential_id = item
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let credential_name = item
            .get("name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let mut row = credential_id
            .as_ref()
            .and_then(|id| usage_by_id.remove(&(credential_type.clone(), id.clone())))
            .unwrap_or(CredentialReferenceRow {
                credential_type: credential_type.clone(),
                credential_id: credential_id.clone(),
                credential_name: credential_name.clone(),
                usage_count: 0,
                workflow_count: 0,
                workflows: Vec::new(),
            });

        if row.credential_id.is_none() {
            row.credential_id = credential_id;
        }
        if row.credential_name.is_none() {
            row.credential_name = credential_name;
        }
        rows.push(row);
    }

    rows.extend(usage_by_id.into_values());
    rows.extend(orphaned_usage);
    sort_credential_rows(&mut rows);
    rows
}

fn sort_credential_rows(rows: &mut [CredentialReferenceRow]) {
    rows.sort_by(|left, right| {
        (
            left.credential_type.as_str(),
            left.credential_id.as_deref().unwrap_or(""),
            left.credential_name.as_deref().unwrap_or(""),
        )
            .cmp(&(
                right.credential_type.as_str(),
                right.credential_id.as_deref().unwrap_or(""),
                right.credential_name.as_deref().unwrap_or(""),
            ))
    });
}

fn credential_source_request_label(source: CredentialSource) -> &'static str {
    match source {
        CredentialSource::Auto => "auto",
        CredentialSource::Public => "public",
        CredentialSource::RestSession => "rest_session",
        CredentialSource::WorkflowRefs => "workflow_refs",
    }
}

fn credential_inventory_source_label(source: CredentialInventorySource) -> &'static str {
    match source {
        CredentialInventorySource::PublicApi => "public_api",
        CredentialInventorySource::RestSession => "rest_session",
        CredentialInventorySource::WorkflowReferences => "workflow_references",
    }
}

fn workflow_reference_inventory_note() -> &'static str {
    "Results come from credential references found in workflows. Unused credentials cannot be discovered in this mode."
}

fn full_inventory_note(source: CredentialInventorySource) -> String {
    match source {
        CredentialInventorySource::PublicApi => "Results include the full credential inventory from the public API. Workflow usage is derived from current workflow references; unused credentials show zero uses.".to_string(),
        CredentialInventorySource::RestSession => "Results include the full credential inventory from n8n's internal REST API using a browser session cookie plus the matching browser-id. This internal fallback is opt-in and may change across n8n upgrades.".to_string(),
        CredentialInventorySource::WorkflowReferences => workflow_reference_inventory_note().to_string(),
    }
}

fn credential_inventory_fallback_allowed(err: &AppError) -> bool {
    matches!(
        err.code.as_str(),
        "api.http_401" | "api.http_403" | "api.http_404" | "api.http_405"
    )
}

fn forced_credential_source_error(
    alias: &str,
    source: CredentialSource,
    err: AppError,
) -> AppError {
    let source_label = match source {
        CredentialSource::Public => "public credential inventory",
        CredentialSource::RestSession => "internal REST credential inventory",
        CredentialSource::Auto | CredentialSource::WorkflowRefs => "credential inventory",
    };
    let suggestion = match source {
        CredentialSource::Public => {
            "Use `n8nc credential ls --source auto` to allow fallback, or `--source workflow-refs` for partial discovery.".to_string()
        }
        CredentialSource::RestSession => session_auth_setup_hint(alias),
        CredentialSource::Auto | CredentialSource::WorkflowRefs => {
            "Use `n8nc credential ls --source auto`.".to_string()
        }
    };

    AppError::api(
        "credential",
        "credential.inventory_unavailable",
        format!(
            "The {source_label} path is not available for `{alias}`: {}",
            err.message
        ),
    )
    .with_suggestion(suggestion)
}

pub(crate) async fn probe_credential_inventory_capability(
    alias: &str,
    client: &ApiClient,
) -> Result<String, AppError> {
    match client.probe_credentials_public().await {
        Ok(()) => Ok("Full credential inventory is available via the public API.".to_string()),
        Err(public_err) if credential_inventory_fallback_allowed(&public_err) => {
            if let Some(cookie) = resolve_session_cookie(alias, "doctor")? {
                if let Some(browser_id) = resolve_browser_id(alias, "doctor")? {
                    match client
                        .probe_credentials_rest_session(&cookie.value, &browser_id.value)
                        .await
                    {
                        Ok(()) => Ok(format!(
                            "Public credential inventory is unavailable ({}). Full inventory is available via the internal REST fallback because browser-session auth is configured.",
                            public_err.message
                        )),
                        Err(rest_err) if credential_inventory_fallback_allowed(&rest_err) => {
                            Ok(format!(
                                "Credential discovery falls back to workflow references only. Public inventory is unavailable ({}). Internal REST inventory is also unavailable ({}).",
                                public_err.message, rest_err.message
                            ))
                        }
                        Err(rest_err) => Err(rest_err),
                    }
                } else {
                    Ok(format!(
                        "Credential discovery falls back to workflow references only. Public inventory is unavailable ({}). A session cookie is configured, but the matching browser ID is missing. {}",
                        public_err.message,
                        session_auth_setup_hint(alias)
                    ))
                }
            } else {
                Ok(format!(
                    "Credential discovery falls back to workflow references only. Public inventory is unavailable ({}). {}",
                    public_err.message,
                    session_auth_setup_hint(alias)
                ))
            }
        }
        Err(public_err) => Err(public_err),
    }
}
