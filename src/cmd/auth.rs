use serde::Serialize;
use serde_json::json;

use crate::{
    api::{ApiClient, ListOptions},
    auth::{
        browser_id_env_var_name, ensure_alias_exists, list_auth_statuses,
        read_session_cookie_from_stdin, read_token_from_stdin, remove_browser_id,
        remove_session_cookie, remove_token, resolve_browser_id, resolve_session_cookie,
        session_cookie_env_var_name, store_browser_id, store_session_cookie, store_token,
    },
    cli::{
        AuthAddArgs, AuthAliasArgs, AuthArgs, AuthCommand, AuthSessionAddArgs, AuthSessionArgs,
        AuthSessionCommand,
    },
    error::AppError,
};

use super::common::{Context, emit_json, load_loaded_repo, remote_client};

#[derive(Debug, Serialize)]
pub(crate) struct AuthListRow {
    pub alias: String,
    pub base_url: String,
    pub token_source: String,
    pub session_cookie_source: String,
    pub browser_id_source: String,
    pub session_ready: bool,
}

pub(crate) async fn cmd_auth(context: &Context, args: AuthArgs) -> Result<(), AppError> {
    match args.command {
        AuthCommand::Add(args) => cmd_auth_add(context, args).await,
        AuthCommand::Test(args) => cmd_auth_test(context, args).await,
        AuthCommand::Session(args) => cmd_auth_session(context, args).await,
        AuthCommand::List => cmd_auth_list(context).await,
        AuthCommand::Remove(args) => cmd_auth_remove(context, args).await,
    }
}

async fn cmd_auth_session(context: &Context, args: AuthSessionArgs) -> Result<(), AppError> {
    match args.command {
        AuthSessionCommand::Add(args) => cmd_auth_session_add(context, args).await,
        AuthSessionCommand::Test(args) => cmd_auth_session_test(context, args).await,
        AuthSessionCommand::Remove(args) => cmd_auth_session_remove(context, args).await,
    }
}

async fn cmd_auth_add(context: &Context, args: AuthAddArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    let token = match (args.token, args.stdin) {
        (Some(token), false) => token,
        (None, true) => read_token_from_stdin()?,
        (None, false) => {
            return Err(AppError::usage(
                "auth",
                "Provide a token with `--token` or pipe it with `--stdin`.",
            ));
        }
        (Some(_), true) => unreachable!(),
    };

    store_token(&alias, &token)?;
    if context.json {
        emit_json("auth", &json!({"alias": alias, "stored": true}))
    } else {
        println!("Stored token for `{alias}`.");
        Ok(())
    }
}

async fn cmd_auth_test(context: &Context, args: AuthAliasArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    let (client, token_source, base_url) = remote_client(&repo, Some(&alias), "auth")?;
    let workflows = client
        .list_workflows(&ListOptions {
            limit: 1,
            active: None,
            name_filter: None,
        })
        .await?;

    let data = json!({
        "alias": alias,
        "base_url": base_url,
        "token_source": token_source,
        "reachable": true,
        "sample_count": workflows.len(),
    });
    if context.json {
        emit_json("auth", &data)
    } else {
        println!("Alias: {}", data["alias"].as_str().unwrap_or_default());
        println!(
            "Base URL: {}",
            data["base_url"].as_str().unwrap_or_default()
        );
        println!(
            "Token source: {}",
            data["token_source"].as_str().unwrap_or_default()
        );
        println!("API reachable: yes");
        Ok(())
    }
}

async fn cmd_auth_session_add(context: &Context, args: AuthSessionAddArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    let session_cookie = match (args.cookie, args.cookie_stdin) {
        (Some(cookie), false) => cookie,
        (None, true) => read_session_cookie_from_stdin()?,
        (None, false) => {
            return Err(AppError::usage(
                "auth",
                "Provide a session cookie with `--cookie` or pipe it with `--cookie-stdin`.",
            ));
        }
        (Some(_), true) => unreachable!(),
    };

    store_session_cookie(&alias, &session_cookie)?;
    store_browser_id(&alias, &args.browser_id)?;

    let data = json!({
        "alias": alias,
        "session_cookie_stored": true,
        "browser_id_stored": true,
        "session_ready": true,
    });
    if context.json {
        emit_json("auth", &data)
    } else {
        println!(
            "Stored browser-session auth for `{}`.",
            data["alias"].as_str().unwrap_or_default()
        );
        Ok(())
    }
}

async fn cmd_auth_session_test(context: &Context, args: AuthAliasArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    let instance =
        repo.config.instances.get(&alias).ok_or_else(|| {
            AppError::config("auth", format!("Unknown instance alias `{alias}`."))
        })?;
    let session_cookie = resolve_session_cookie(&alias, "auth")?.ok_or_else(|| {
        AppError::auth(
            "auth",
            format!("No session cookie configured for `{alias}`."),
        )
        .with_suggestion(session_auth_setup_hint(&alias))
    })?;
    let browser_id = resolve_browser_id(&alias, "auth")?.ok_or_else(|| {
        AppError::auth("auth", format!("No browser ID configured for `{alias}`."))
            .with_suggestion(session_auth_setup_hint(&alias))
    })?;
    let client = ApiClient::new("auth", instance, "session-probe".to_string())?;
    let credentials = client
        .list_credentials_rest_session(&session_cookie.value, &browser_id.value)
        .await?;

    let data = json!({
        "alias": alias,
        "base_url": instance.base_url,
        "session_cookie_source": session_cookie.source,
        "browser_id_source": browser_id.source,
        "reachable": true,
        "sample_count": credentials.len(),
    });
    if context.json {
        emit_json("auth", &data)
    } else {
        println!("Alias: {}", data["alias"].as_str().unwrap_or_default());
        println!(
            "Base URL: {}",
            data["base_url"].as_str().unwrap_or_default()
        );
        println!(
            "Session cookie source: {}",
            data["session_cookie_source"].as_str().unwrap_or_default()
        );
        println!(
            "Browser ID source: {}",
            data["browser_id_source"].as_str().unwrap_or_default()
        );
        println!("Internal REST reachable: yes");
        Ok(())
    }
}

async fn cmd_auth_list(context: &Context) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let rows: Vec<AuthListRow> = list_auth_statuses(&repo)?
        .into_iter()
        .map(|status| AuthListRow {
            alias: status.alias,
            base_url: status.base_url,
            token_source: status.token_source,
            session_cookie_source: status.session_cookie_source,
            browser_id_source: status.browser_id_source,
            session_ready: status.session_ready,
        })
        .collect();

    if context.json {
        emit_json("auth", &json!({ "instances": rows }))
    } else {
        println!("{:<16} {:<10} {:<22} BASE URL", "ALIAS", "TOKEN", "SESSION");
        for row in rows {
            println!(
                "{:<16} {:<10} {:<22} {}",
                row.alias,
                row.token_source,
                auth_session_status_label(&row),
                row.base_url
            );
        }
        Ok(())
    }
}

async fn cmd_auth_remove(context: &Context, args: AuthAliasArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    remove_token(&alias)?;
    if context.json {
        emit_json("auth", &json!({"alias": alias, "removed": true}))
    } else {
        println!("Removed token for `{alias}`.");
        Ok(())
    }
}

async fn cmd_auth_session_remove(context: &Context, args: AuthAliasArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let alias = ensure_alias_exists(&repo, &args.alias, "auth")?;
    remove_session_cookie(&alias)?;
    remove_browser_id(&alias)?;
    if context.json {
        emit_json(
            "auth",
            &json!({
                "alias": alias,
                "session_cookie_removed": true,
                "browser_id_removed": true,
            }),
        )
    } else {
        println!("Removed browser-session auth for `{alias}`.");
        Ok(())
    }
}

fn auth_session_status_label(row: &AuthListRow) -> String {
    if row.session_ready {
        format!("{}+{}", row.session_cookie_source, row.browser_id_source)
    } else if row.session_cookie_source != "missing" || row.browser_id_source != "missing" {
        format!(
            "partial({}+{})",
            row.session_cookie_source, row.browser_id_source
        )
    } else {
        "missing".to_string()
    }
}

pub(crate) fn session_auth_setup_hint(alias: &str) -> String {
    format!(
        "Run `n8nc auth session add {alias} --cookie <n8n-auth=...> --browser-id <browser-id>` or set {} and {}.",
        session_cookie_env_var_name(alias),
        browser_id_env_var_name(alias)
    )
}
