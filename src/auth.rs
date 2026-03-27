use std::{env, io::Read};

use keyring::{Entry, Error as KeyringError};
use serde::Serialize;

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

fn normalize_alias_suffix(alias: &str, out: &mut String) {
    for ch in alias.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
}

fn entry(alias: &str) -> Result<Entry, AppError> {
    Entry::new(SERVICE_NAME, alias).map_err(|err| {
        AppError::auth(
            "auth",
            format!("Failed to access keychain entry for `{alias}`: {err}"),
        )
    })
}

pub fn store_token(alias: &str, token: &str) -> Result<(), AppError> {
    entry(alias)?.set_password(token).map_err(|err| {
        AppError::auth(
            "auth",
            format!("Failed to store token for `{alias}`: {err}"),
        )
    })
}

pub fn remove_token(alias: &str) -> Result<(), AppError> {
    let item = entry(alias)?;
    match item.delete_credential() {
        Ok(()) => Ok(()),
        Err(KeyringError::NoEntry) => Ok(()),
        Err(err) => Err(AppError::auth(
            "auth",
            format!("Failed to remove token for `{alias}`: {err}"),
        )),
    }
}

pub fn resolve_token(alias: &str, command: &'static str) -> Result<(String, String), AppError> {
    if let Ok(value) = env::var(env_var_name(alias)) {
        if !value.trim().is_empty() {
            return Ok((value, "env".to_string()));
        }
    }

    match entry(alias)?.get_password() {
        Ok(value) if !value.trim().is_empty() => Ok((value, "keychain".to_string())),
        Ok(_) | Err(KeyringError::NoEntry) => Err(AppError::auth(
            command,
            format!("No token configured for `{alias}`."),
        )
        .with_suggestion(format!(
            "Run `n8nc auth add {alias} --token <api_key>` or set {}.",
            env_var_name(alias)
        ))),
        Err(err) => Err(AppError::auth(
            command,
            format!("Failed to read token for `{alias}`: {err}"),
        )),
    }
}

pub fn resolve_session_cookie(alias: &str) -> Option<String> {
    env::var(session_cookie_env_var_name(alias))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn read_token_from_stdin() -> Result<String, AppError> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|err| AppError::auth("auth", format!("Failed to read token from stdin: {err}")))?;
    let token = buffer.trim().to_string();
    if token.is_empty() {
        return Err(AppError::usage(
            "auth",
            "The token read from stdin was empty.",
        ));
    }
    Ok(token)
}

pub fn list_auth_statuses(repo: &LoadedRepo) -> Vec<AuthStatus> {
    repo.config
        .instances
        .iter()
        .map(|(alias, instance)| {
            let token_source = if env::var(env_var_name(alias))
                .ok()
                .filter(|value| !value.is_empty())
                .is_some()
            {
                "env".to_string()
            } else if let Ok(item) = entry(alias).and_then(|item| {
                item.get_password()
                    .map_err(|err| AppError::auth("auth", err.to_string()))
            }) {
                if item.is_empty() {
                    "missing".to_string()
                } else {
                    "keychain".to_string()
                }
            } else {
                "missing".to_string()
            };
            AuthStatus {
                alias: alias.clone(),
                base_url: instance.base_url.clone(),
                token_source,
            }
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
    use super::{env_var_name, session_cookie_env_var_name};

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
    }
}
