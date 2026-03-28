use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    config::{ExecuteBackend, ExecuteConfig},
    error::AppError,
};

#[derive(Debug, Clone)]
pub struct WorkflowExecuteInvocation {
    pub instance_alias: String,
    pub base_url: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub workflow_active: Option<bool>,
    pub input: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowExecuteOutput {
    pub backend: &'static str,
    pub program: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

pub fn execute_workflow(
    repo_root: &Path,
    config: &ExecuteConfig,
    invocation: &WorkflowExecuteInvocation,
    command: &'static str,
) -> Result<WorkflowExecuteOutput, AppError> {
    match config.backend {
        ExecuteBackend::Command => execute_command_backend(repo_root, config, invocation, command),
    }
}

pub fn probe_execute_backend(
    repo_root: &Path,
    config: &ExecuteConfig,
    command: &'static str,
) -> Result<String, AppError> {
    match config.backend {
        ExecuteBackend::Command => {
            let resolved_program = resolve_command_program(repo_root, &config.program, command)?;
            let cwd = config
                .cwd
                .as_ref()
                .map(|path| resolve_repo_path(repo_root, path));
            if let Some(cwd) = &cwd
                && !cwd.is_dir()
            {
                return Err(AppError::config(
                    command,
                    format!(
                        "Workflow execute backend working directory does not exist: {}",
                        cwd.display()
                    ),
                ));
            }

            let stdin_mode = if config.stdin_json {
                "stdin_json"
            } else {
                "no_stdin"
            };
            Ok(format!(
                "Workflow execute backend available via `{}` ({stdin_mode}).",
                resolved_program.display()
            ))
        }
    }
}

pub fn execute_backend_setup_hint(alias: &str) -> String {
    format!(
        "Configure `[instances.{alias}.execute]` in n8n.toml with `backend = \"command\"` and a runnable `program`, then rerun `n8nc doctor`."
    )
}

fn execute_command_backend(
    repo_root: &Path,
    config: &ExecuteConfig,
    invocation: &WorkflowExecuteInvocation,
    command: &'static str,
) -> Result<WorkflowExecuteOutput, AppError> {
    let resolved_program = resolve_command_program(repo_root, &config.program, command)?;
    let args = config
        .args
        .iter()
        .map(|value| expand_execute_placeholders(value, invocation))
        .collect::<Vec<_>>();
    let cwd = config
        .cwd
        .as_ref()
        .map(|path| resolve_repo_path(repo_root, path));

    if let Some(cwd) = &cwd
        && !cwd.is_dir()
    {
        return Err(AppError::config(
            command,
            format!(
                "Workflow execute backend working directory does not exist: {}",
                cwd.display()
            ),
        ));
    }

    let mut process = Command::new(&resolved_program);
    process.args(&args);
    if let Some(cwd) = &cwd {
        process.current_dir(cwd);
    }
    process.env("N8NC_EXECUTE_INSTANCE_ALIAS", &invocation.instance_alias);
    process.env("N8NC_EXECUTE_BASE_URL", &invocation.base_url);
    process.env("N8NC_EXECUTE_WORKFLOW_ID", &invocation.workflow_id);
    process.env("N8NC_EXECUTE_WORKFLOW_NAME", &invocation.workflow_name);
    if let Some(active) = invocation.workflow_active {
        process.env(
            "N8NC_EXECUTE_WORKFLOW_ACTIVE",
            if active { "true" } else { "false" },
        );
    }
    if let Some(input) = &invocation.input {
        let rendered = serde_json::to_string(input).map_err(|err| {
            AppError::api(
                command,
                "workflow.execute_input_invalid",
                format!("Failed to serialize workflow execute input: {err}"),
            )
        })?;
        process.env("N8NC_EXECUTE_INPUT_JSON", rendered);
    }

    let output = if config.stdin_json {
        let payload = serde_json::to_vec(&build_execute_request(invocation)).map_err(|err| {
            AppError::api(
                command,
                "workflow.execute_input_invalid",
                format!("Failed to serialize workflow execute request: {err}"),
            )
        })?;
        let mut child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                AppError::config(
                    command,
                    format!(
                        "Failed to start workflow execute backend `{}`: {err}",
                        resolved_program.display()
                    ),
                )
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&payload).map_err(|err| {
                AppError::api(
                    command,
                    "workflow.execute_failed",
                    format!("Failed to send workflow execute input to backend: {err}"),
                )
            })?;
        }
        child.wait_with_output().map_err(|err| {
            AppError::api(
                command,
                "workflow.execute_failed",
                format!("Failed to wait for workflow execute backend: {err}"),
            )
        })?
    } else {
        process.output().map_err(|err| {
            AppError::config(
                command,
                format!(
                    "Failed to start workflow execute backend `{}`: {err}",
                    resolved_program.display()
                ),
            )
        })?
    };

    let stdout = parse_process_output(&output.stdout);
    let stderr = parse_stderr(&output.stderr);
    if !output.status.success() {
        let status = output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string());
        return Err(AppError::api(
            command,
            "workflow.execute_failed",
            format!(
                "Workflow execute backend `{}` exited with status {status}.",
                resolved_program.display()
            ),
        )
        .with_suggestion(
            "Check the execute backend command in `n8n.toml`, run `n8nc doctor`, and inspect the attached stdout/stderr for adapter errors.".to_string(),
        )
        .with_json_data(json!({
            "backend": "command",
            "program": resolved_program,
            "args": args,
            "cwd": cwd,
            "stdout": stdout,
            "stderr": stderr,
        })));
    }

    Ok(WorkflowExecuteOutput {
        backend: "command",
        program: resolved_program.display().to_string(),
        args,
        cwd,
        output: stdout,
        stderr,
    })
}

fn build_execute_request(invocation: &WorkflowExecuteInvocation) -> Value {
    json!({
        "tool": "execute_workflow",
        "instance_alias": invocation.instance_alias,
        "base_url": invocation.base_url,
        "workflow": {
            "id": invocation.workflow_id,
            "name": invocation.workflow_name,
            "active": invocation.workflow_active,
        },
        "input": invocation.input,
    })
}

fn parse_process_output(bytes: &[u8]) -> Option<Value> {
    if bytes.is_empty() {
        return None;
    }
    let trimmed = String::from_utf8_lossy(bytes).trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed)
        .ok()
        .or_else(|| Some(Value::String(trimmed)))
}

fn parse_stderr(bytes: &[u8]) -> Option<String> {
    let rendered = String::from_utf8_lossy(bytes).trim().to_string();
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

fn expand_execute_placeholders(value: &str, invocation: &WorkflowExecuteInvocation) -> String {
    value
        .replace("{instance_alias}", &invocation.instance_alias)
        .replace("{base_url}", &invocation.base_url)
        .replace("{workflow_id}", &invocation.workflow_id)
        .replace("{workflow_name}", &invocation.workflow_name)
}

fn resolve_command_program(
    repo_root: &Path,
    program: &str,
    command: &'static str,
) -> Result<PathBuf, AppError> {
    if let Some(path) = resolve_command_path(repo_root, program) {
        return Ok(path);
    }

    Err(AppError::config(
        command,
        format!("Workflow execute backend program `{program}` could not be found."),
    )
    .with_suggestion(
        "Install the configured backend command or update `[instances.<alias>.execute]` in `n8n.toml`."
            .to_string(),
    ))
}

fn resolve_command_path(repo_root: &Path, program: &str) -> Option<PathBuf> {
    let program_path = PathBuf::from(program);
    if program_path.is_absolute()
        || program.contains(std::path::MAIN_SEPARATOR)
        || program.starts_with('.')
    {
        let resolved = resolve_repo_path(repo_root, &program_path);
        if resolved.is_file() {
            return Some(resolved);
        }
        return None;
    }

    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|path| path.join(program))
            .find(|candidate| candidate.is_file())
    })
}

fn resolve_repo_path(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{WorkflowExecuteInvocation, expand_execute_placeholders};

    #[test]
    fn expands_execute_placeholders() {
        let invocation = WorkflowExecuteInvocation {
            instance_alias: "prod".to_string(),
            base_url: "https://n8n.example.test".to_string(),
            workflow_id: "wf-123".to_string(),
            workflow_name: "Nightly Import".to_string(),
            workflow_active: Some(true),
            input: Some(json!({"hello":"world"})),
        };

        assert_eq!(
            expand_execute_placeholders(
                "run {workflow_id} {workflow_name} {instance_alias} {base_url}",
                &invocation
            ),
            "run wf-123 Nightly Import prod https://n8n.example.test"
        );
    }
}
