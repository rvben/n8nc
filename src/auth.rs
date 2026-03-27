use std::{env, io::Read};

#[cfg(not(target_os = "macos"))]
use keyring::{Entry, Error as KeyringError};
use serde::Serialize;
#[cfg(target_os = "macos")]
use std::process::{Command, Output};

use crate::{
    config::{LoadedRepo, resolve_instance_alias},
    error::AppError,
};

const SERVICE_NAME: &str = "n8nc";

#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub alias: String,
    pub base_url: String,
    pub token_source: String,
    pub session_cookie_source: String,
    pub browser_id_source: String,
    pub session_ready: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedSecret {
    pub value: String,
    pub source: String,
}

pub fn env_var_name(alias: &str) -> String {
    let mut out = String::from("N8NC_TOKEN_");
    normalize_alias_suffix(alias, &mut out);
    out
}

pub fn session_cookie_env_var_name(alias: &str) -> String {
    let mut out = String::from("N8NC_SESSION_COOKIE_");
    normalize_alias_suffix(alias, &mut out);
    out
}

pub fn browser_id_env_var_name(alias: &str) -> String {
    let mut out = String::from("N8NC_BROWSER_ID_");
    normalize_alias_suffix(alias, &mut out);
    out
}

fn normalize_alias_suffix(alias: &str, out: &mut String) {
    for ch in alias.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn entry(alias: &str) -> Result<Entry, AppError> {
    Entry::new(SERVICE_NAME, alias).map_err(|err| {
        AppError::auth(
            "auth",
            format!("Failed to access keychain entry for `{alias}`: {err}"),
        )
    })
}

fn session_cookie_entry_name(alias: &str) -> String {
    format!("session-cookie:{alias}")
}

fn browser_id_entry_name(alias: &str) -> String {
    format!("browser-id:{alias}")
}

#[cfg(target_os = "macos")]
fn security_output_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("security exited with status {}.", output.status)
}

#[cfg(target_os = "macos")]
fn security_store_secret(
    account: &str,
    label: &str,
    alias: &str,
    value: &str,
) -> Result<(), AppError> {
    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            account,
            "-s",
            SERVICE_NAME,
            "-w",
            value,
        ])
        .output()
        .map_err(|err| {
            AppError::auth(
                "auth",
                format!("Failed to store {label} for `{alias}`: {err}"),
            )
        })?;

    if output.status.success() {
        Ok(())
    } else {
        Err(AppError::auth(
            "auth",
            format!(
                "Failed to store {label} for `{alias}`: {}",
                security_output_message(&output)
            ),
        ))
    }
}

#[cfg(target_os = "macos")]
fn security_read_secret(
    account: &str,
    alias: &str,
    command: &'static str,
    label: &str,
) -> Result<Option<String>, AppError> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            account,
            "-s",
            SERVICE_NAME,
            "-w",
        ])
        .output()
        .map_err(|err| {
            AppError::auth(
                command,
                format!("Failed to read {label} for `{alias}`: {err}"),
            )
        })?;

    match output.status.code() {
        Some(0) => {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
        Some(44) => Ok(None),
        _ => Err(AppError::auth(
            command,
            format!(
                "Failed to read {label} for `{alias}`: {}",
                security_output_message(&output)
            ),
        )),
    }
}

#[cfg(target_os = "macos")]
fn security_remove_secret(account: &str, label: &str, alias: &str) -> Result<(), AppError> {
    let output = Command::new("security")
        .args(["delete-generic-password", "-a", account, "-s", SERVICE_NAME])
        .output()
        .map_err(|err| {
            AppError::auth(
                "auth",
                format!("Failed to remove {label} for `{alias}`: {err}"),
            )
        })?;

    match output.status.code() {
        Some(0) | Some(44) => Ok(()),
        _ => Err(AppError::auth(
            "auth",
            format!(
                "Failed to remove {label} for `{alias}`: {}",
                security_output_message(&output)
            ),
        )),
    }
}

#[cfg(target_os = "macos")]
fn store_secret(account: &str, alias: &str, label: &str, value: &str) -> Result<(), AppError> {
    security_store_secret(account, label, alias, value)
}

#[cfg(not(target_os = "macos"))]
fn store_secret(account: &str, alias: &str, label: &str, value: &str) -> Result<(), AppError> {
    entry(account)?.set_password(value).map_err(|err| {
        AppError::auth(
            "auth",
            format!("Failed to store {label} for `{alias}`: {err}"),
        )
    })
}

#[cfg(target_os = "macos")]
fn remove_secret(account: &str, alias: &str, label: &str) -> Result<(), AppError> {
    security_remove_secret(account, label, alias)
}

#[cfg(not(target_os = "macos"))]
fn remove_secret(account: &str, alias: &str, label: &str) -> Result<(), AppError> {
    let item = entry(account)?;
    match item.delete_credential() {
        Ok(()) => Ok(()),
        Err(KeyringError::NoEntry) => Ok(()),
        Err(err) => Err(AppError::auth(
            "auth",
            format!("Failed to remove {label} for `{alias}`: {err}"),
        )),
    }
}

#[cfg(target_os = "macos")]
fn resolve_optional_secret(
    account: &str,
    alias: &str,
    command: &'static str,
    env_var_name: String,
    label: &str,
) -> Result<Option<ResolvedSecret>, AppError> {
    if let Ok(value) = env::var(&env_var_name) {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(Some(ResolvedSecret {
                value,
                source: "env".to_string(),
            }));
        }
    }

    Ok(
        security_read_secret(account, alias, command, label)?.map(|value| ResolvedSecret {
            value,
            source: "keychain".to_string(),
        }),
    )
}

#[cfg(not(target_os = "macos"))]
fn resolve_optional_secret(
    account: &str,
    alias: &str,
    command: &'static str,
    env_var_name: String,
    label: &str,
) -> Result<Option<ResolvedSecret>, AppError> {
    if let Ok(value) = env::var(&env_var_name) {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(Some(ResolvedSecret {
                value,
                source: "env".to_string(),
            }));
        }
    }

    match entry(account)?.get_password() {
        Ok(value) => {
            let value = value.trim().to_string();
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(ResolvedSecret {
                    value,
                    source: "keychain".to_string(),
                }))
            }
        }
        Err(KeyringError::NoEntry) => Ok(None),
        Err(err) => Err(AppError::auth(
            command,
            format!("Failed to read {label} for `{alias}`: {err}"),
        )),
    }
}

fn secret_source(
    account: &str,
    alias: &str,
    command: &'static str,
    env_var_name: String,
    label: &str,
) -> Result<String, AppError> {
    Ok(
        resolve_optional_secret(account, alias, command, env_var_name, label)?
            .map(|secret| secret.source)
            .unwrap_or_else(|| "missing".to_string()),
    )
}

pub fn store_token(alias: &str, token: &str) -> Result<(), AppError> {
    store_secret(alias, alias, "token", token)
}

pub fn remove_token(alias: &str) -> Result<(), AppError> {
    remove_secret(alias, alias, "token")
}

pub fn resolve_token(alias: &str, command: &'static str) -> Result<(String, String), AppError> {
    resolve_optional_secret(alias, alias, command, env_var_name(alias), "token")?
        .map(|secret| (secret.value, secret.source))
        .ok_or_else(|| {
            AppError::auth(command, format!("No token configured for `{alias}`.")).with_suggestion(
                format!(
                    "Run `n8nc auth add {alias} --token <api_key>` or set {}.",
                    env_var_name(alias)
                ),
            )
        })
}

pub fn store_session_cookie(alias: &str, session_cookie: &str) -> Result<(), AppError> {
    store_secret(
        &session_cookie_entry_name(alias),
        alias,
        "session cookie",
        session_cookie,
    )
}

pub fn remove_session_cookie(alias: &str) -> Result<(), AppError> {
    remove_secret(&session_cookie_entry_name(alias), alias, "session cookie")
}

pub fn resolve_session_cookie(
    alias: &str,
    command: &'static str,
) -> Result<Option<ResolvedSecret>, AppError> {
    resolve_optional_secret(
        &session_cookie_entry_name(alias),
        alias,
        command,
        session_cookie_env_var_name(alias),
        "session cookie",
    )
}

pub fn store_browser_id(alias: &str, browser_id: &str) -> Result<(), AppError> {
    store_secret(
        &browser_id_entry_name(alias),
        alias,
        "browser ID",
        browser_id,
    )
}

pub fn remove_browser_id(alias: &str) -> Result<(), AppError> {
    remove_secret(&browser_id_entry_name(alias), alias, "browser ID")
}

pub fn resolve_browser_id(
    alias: &str,
    command: &'static str,
) -> Result<Option<ResolvedSecret>, AppError> {
    resolve_optional_secret(
        &browser_id_entry_name(alias),
        alias,
        command,
        browser_id_env_var_name(alias),
        "browser ID",
    )
}

fn read_value_from_stdin(label: &str) -> Result<String, AppError> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|err| {
            AppError::auth("auth", format!("Failed to read {label} from stdin: {err}"))
        })?;
    let value = buffer.trim().to_string();
    if value.is_empty() {
        return Err(AppError::usage(
            "auth",
            format!("The {label} read from stdin was empty."),
        ));
    }
    Ok(value)
}

pub fn read_token_from_stdin() -> Result<String, AppError> {
    read_value_from_stdin("token")
}

pub fn read_session_cookie_from_stdin() -> Result<String, AppError> {
    read_value_from_stdin("session cookie")
}

pub fn list_auth_statuses(repo: &LoadedRepo) -> Result<Vec<AuthStatus>, AppError> {
    repo.config
        .instances
        .iter()
        .map(|(alias, instance)| {
            let token_source = secret_source(alias, alias, "auth", env_var_name(alias), "token")?;
            let session_cookie_source = secret_source(
                &session_cookie_entry_name(alias),
                alias,
                "auth",
                session_cookie_env_var_name(alias),
                "session cookie",
            )?;
            let browser_id_source = secret_source(
                &browser_id_entry_name(alias),
                alias,
                "auth",
                browser_id_env_var_name(alias),
                "browser ID",
            )?;
            Ok(AuthStatus {
                alias: alias.clone(),
                base_url: instance.base_url.clone(),
                token_source,
                session_cookie_source: session_cookie_source.clone(),
                browser_id_source: browser_id_source.clone(),
                session_ready: session_cookie_source != "missing" && browser_id_source != "missing",
            })
        })
        .collect()
}

pub fn ensure_alias_exists(
    repo: &LoadedRepo,
    alias: &str,
    command: &'static str,
) -> Result<String, AppError> {
    resolve_instance_alias(repo, Some(alias), command)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{browser_id_env_var_name, env_var_name, session_cookie_env_var_name};

    #[test]
    fn env_var_names_are_normalized() {
        assert_eq!(env_var_name("prod"), "N8NC_TOKEN_PROD");
        assert_eq!(env_var_name("eu-west-1"), "N8NC_TOKEN_EU_WEST_1");
        assert_eq!(
            session_cookie_env_var_name("prod"),
            "N8NC_SESSION_COOKIE_PROD"
        );
        assert_eq!(
            session_cookie_env_var_name("eu-west-1"),
            "N8NC_SESSION_COOKIE_EU_WEST_1"
        );
        assert_eq!(browser_id_env_var_name("prod"), "N8NC_BROWSER_ID_PROD");
        assert_eq!(
            browser_id_env_var_name("eu-west-1"),
            "N8NC_BROWSER_ID_EU_WEST_1"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_secret_storage_round_trips_across_fresh_lookups() {
        use super::{
            remove_browser_id, remove_session_cookie, resolve_browser_id, resolve_session_cookie,
            store_browser_id, store_session_cookie,
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("timestamp")
            .as_nanos();
        let alias = format!("session-storage-test-{unique}");

        store_session_cookie(&alias, "n8n-auth=test-cookie").expect("store session cookie");
        store_browser_id(&alias, "browser-test").expect("store browser id");

        let session_cookie = resolve_session_cookie(&alias, "auth")
            .expect("resolve session cookie")
            .expect("session cookie present");
        let browser_id = resolve_browser_id(&alias, "auth")
            .expect("resolve browser id")
            .expect("browser id present");

        assert_eq!(session_cookie.value, "n8n-auth=test-cookie");
        assert_eq!(session_cookie.source, "keychain");
        assert_eq!(browser_id.value, "browser-test");
        assert_eq!(browser_id.source, "keychain");

        remove_session_cookie(&alias).expect("remove session cookie");
        remove_browser_id(&alias).expect("remove browser id");
    }
}
