use serde_json::{Value, json};

use crate::{
    cli::{SecretArgs, SecretCommand, SecretExtractArgs},
    edit::{attach_header_credential, read_inline_header_value, workflow_id_string},
    error::AppError,
};

use super::common::{
    Context, emit_json, load_loaded_repo, print_message, remote_client,
    resolve_tracked_workflow_file,
};

pub(crate) async fn cmd_secret(context: &Context, args: SecretArgs) -> Result<(), AppError> {
    match args.command {
        SecretCommand::Extract(args) => cmd_secret_extract(context, args).await,
    }
}

async fn cmd_secret_extract(context: &Context, args: SecretExtractArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let workflow_path = resolve_tracked_workflow_file(&repo, "secret", &args.file)?;
    let header_name = args.header.as_deref().unwrap_or("Authorization");
    let credential_type = args.credential_type.as_deref().unwrap_or("httpHeaderAuth");

    // Read the inline header value that will move into a credential. Do this
    // before any remote call so a missing node/header fails without side effects.
    let value = read_inline_header_value(&workflow_path, &args.node, header_name)?;
    let credential_name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("{} {}", args.node, header_name));

    // Create the credential on the instance.
    let (client, _, _) = remote_client(&repo, args.remote.instance.as_deref(), "secret")?;
    let data = json!({ "name": header_name, "value": value });
    let created = client
        .create_credential(&credential_name, credential_type, &data)
        .await?;
    let credential_id = created.get("id").and_then(Value::as_str).ok_or_else(|| {
        AppError::api(
            "secret",
            "credential.missing_id",
            "n8n did not return an id for the created credential.",
        )
    })?;

    // Rewrite the local workflow to use the credential and drop the inline header.
    let result = attach_header_credential(
        &workflow_path,
        &args.node,
        header_name,
        credential_type,
        credential_id,
        Some(&credential_name),
    )?;

    if context.json {
        emit_json(
            "secret",
            &json!({
                "workflow_id": workflow_id_string(&result.workflow),
                "workflow_path": result.path,
                "node": args.node,
                "header": header_name,
                "credential": {
                    "id": credential_id,
                    "name": credential_name,
                    "type": credential_type,
                },
                "changed": result.changed,
                "next_step": "Run `n8nc push` to apply the change on the remote.",
            }),
        )
    } else {
        print_message(
            context,
            &format!(
                "Created {credential_type} credential `{credential_name}` ({credential_id}) from the inline `{header_name}` header on node `{}`.",
                args.node
            ),
        );
        print_message(
            context,
            &format!("Updated local file: {}", result.path.display()),
        );
        print_message(
            context,
            "Run `n8nc push` to apply the change on the remote.",
        );
        Ok(())
    }
}
