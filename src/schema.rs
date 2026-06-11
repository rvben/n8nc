use serde_json::{Value, json};

fn arg_to_json(arg: &clap::Arg) -> Value {
    let mut obj = serde_json::Map::new();

    let id = arg.get_id().as_str();
    let name = if arg.is_positional() {
        id.to_string()
    } else {
        arg.get_long()
            .map(|l| format!("--{l}"))
            .unwrap_or_else(|| id.to_string())
    };
    obj.insert("name".into(), json!(name));

    let is_bool = !arg.get_action().takes_values();
    if is_bool {
        obj.insert("type".into(), json!("boolean"));
    } else {
        let possible: Vec<String> = arg
            .get_possible_values()
            .iter()
            .filter(|v| !v.is_hide_set())
            .map(|v| v.get_name().to_string())
            .collect();
        if !possible.is_empty() {
            obj.insert("type".into(), json!("string"));
            obj.insert("enum".into(), json!(possible));
        } else {
            obj.insert("type".into(), json!("string"));
        }
    }

    obj.insert("required".into(), json!(arg.is_required_set()));

    if let Some(default) = arg.get_default_values().first() {
        obj.insert("default".into(), json!(default.to_string_lossy()));
    }

    if let Some(help) = arg.get_help().map(|h| h.to_string()) {
        obj.insert("description".into(), json!(help));
    }

    Value::Object(obj)
}

/// Whether a command by flat path is a mutating operation.
///
/// Commands marked `true` modify state on the remote or local filesystem.
/// Commands marked `false` are read-only.
/// Commands absent from this map are read-only by default (marked false).
fn is_mutating(path: &str) -> bool {
    matches!(
        path,
        "init"
            | "pull"
            | "push"
            | "activate"
            | "deactivate"
            | "archive"
            | "unarchive"
            | "trigger"
            | "auth add"
            | "auth remove"
            | "auth session add"
            | "auth session remove"
            | "node add"
            | "node set"
            | "node rename"
            | "node rm"
            | "conn add"
            | "conn rm"
            | "expr set"
            | "credential set"
            | "workflow new"
            | "workflow create"
            | "workflow execute"
            | "workflow rm"
            | "fmt"
    )
}

/// Output fields for list/get commands.
fn output_fields_for(path: &str) -> Vec<Value> {
    match path {
        "ls" => vec![
            json!({"name": "id", "type": "string"}),
            json!({"name": "name", "type": "string"}),
            json!({"name": "active", "type": "boolean | null"}),
            json!({"name": "updated_at", "type": "string | null"}),
        ],
        "get" => vec![
            json!({"name": "id", "type": "string"}),
            json!({"name": "name", "type": "string"}),
            json!({"name": "active", "type": "boolean"}),
            json!({"name": "nodes", "type": "array"}),
            json!({"name": "connections", "type": "object"}),
        ],
        "status" => vec![
            json!({"name": "path", "type": "string"}),
            json!({"name": "id", "type": "string | null"}),
            json!({"name": "name", "type": "string | null"}),
            json!({"name": "state", "type": "string"}),
            json!({"name": "remote_active", "type": "boolean | null"}),
        ],
        "diff" => vec![
            json!({"name": "path", "type": "string"}),
            json!({"name": "diff", "type": "string"}),
        ],
        "runs ls" => vec![
            json!({"name": "id", "type": "string"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "workflow_name", "type": "string | null"}),
            json!({"name": "status", "type": "string | null"}),
            json!({"name": "mode", "type": "string | null"}),
            json!({"name": "started_at", "type": "string | null"}),
            json!({"name": "stopped_at", "type": "string | null"}),
            json!({"name": "duration_ms", "type": "integer | null"}),
        ],
        "runs get" => vec![
            json!({"name": "id", "type": "string"}),
            json!({"name": "status", "type": "string | null"}),
            json!({"name": "mode", "type": "string | null"}),
            json!({"name": "started_at", "type": "string | null"}),
            json!({"name": "stopped_at", "type": "string | null"}),
            json!({"name": "duration_ms", "type": "integer | null"}),
        ],
        "runs stats" => vec![
            json!({"name": "total", "type": "integer"}),
            json!({"name": "success", "type": "integer"}),
            json!({"name": "error", "type": "integer"}),
            json!({"name": "success_rate", "type": "number"}),
            json!({"name": "avg_duration_ms", "type": "number | null"}),
        ],
        "auth list" => vec![
            json!({"name": "alias", "type": "string"}),
            json!({"name": "url", "type": "string"}),
            json!({"name": "has_token", "type": "boolean"}),
        ],
        "auth test" => vec![
            json!({"name": "alias", "type": "string"}),
            json!({"name": "ok", "type": "boolean"}),
        ],
        "credential ls" => vec![
            json!({"name": "id", "type": "string | null"}),
            json!({"name": "name", "type": "string"}),
            json!({"name": "type", "type": "string | null"}),
        ],
        "node ls" => vec![
            json!({"name": "name", "type": "string"}),
            json!({"name": "node_type", "type": "string | null"}),
            json!({"name": "type_version", "type": "number | null"}),
            json!({"name": "disabled", "type": "boolean | null"}),
        ],
        "search" => vec![
            json!({"name": "file", "type": "string"}),
            json!({"name": "workflow_name", "type": "string | null"}),
            json!({"name": "match_type", "type": "string"}),
            json!({"name": "value", "type": "string"}),
        ],
        "workflow show" => vec![
            json!({"name": "name", "type": "string"}),
            json!({"name": "id", "type": "string | null"}),
            json!({"name": "active", "type": "boolean | null"}),
            json!({"name": "nodes", "type": "array"}),
            json!({"name": "connections", "type": "array"}),
            json!({"name": "webhooks", "type": "array"}),
        ],
        "doctor" => vec![
            json!({"name": "status", "type": "string"}),
            json!({"name": "checks", "type": "array"}),
        ],
        _ => vec![],
    }
}

fn walk_commands(cmd: &clap::Command, prefix: &str, out: &mut Vec<Value>) {
    let global_ids = ["help", "version", "output", "json", "quiet", "repo_root"];

    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        if name == "help" || sub.is_hide_set() {
            continue;
        }

        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix} {name}")
        };

        let has_subcommands = sub.get_subcommands().any(|s| s.get_name() != "help");
        if has_subcommands {
            walk_commands(sub, &path, out);
        } else {
            let mut entry = serde_json::Map::new();
            entry.insert("name".into(), json!(path));

            if let Some(about) = sub.get_about().map(|a| a.to_string()) {
                entry.insert("description".into(), json!(about));
            }

            entry.insert("mutating".into(), json!(is_mutating(&path)));

            let mut args = Vec::new();
            for arg in sub.get_arguments() {
                let arg_id = arg.get_id().as_str();
                if global_ids.contains(&arg_id) {
                    continue;
                }
                if arg.is_hide_set() {
                    continue;
                }
                args.push(arg_to_json(arg));
            }
            if !args.is_empty() {
                entry.insert("args".into(), json!(args));
            }

            let fields = output_fields_for(&path);
            if !fields.is_empty() {
                entry.insert("output_fields".into(), json!(fields));
            }

            out.push(Value::Object(entry));
        }
    }
}

pub fn generate(cmd: &clap::Command) -> Value {
    let mut commands: Vec<Value> = Vec::new();
    walk_commands(cmd, "", &mut commands);

    json!({
        "clispec": "0.2",
        "name": "n8nc",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Human- and agent-friendly CLI for n8n workflows",
        "global_args": [
            {
                "name": "--output",
                "type": "string",
                "enum": ["auto", "text", "json"],
                "default": "auto",
                "required": false,
                "description": "Output format; auto selects JSON when stdout is not a TTY"
            },
            {
                "name": "--quiet",
                "type": "boolean",
                "required": false,
                "default": false,
                "description": "Suppress non-data output (summary lines, confirmations)"
            },
            {
                "name": "--repo-root",
                "type": "string",
                "required": false,
                "description": "Override the repository root directory"
            }
        ],
        "commands": commands,
        "errors": [
            {
                "kind": "usage",
                "exit_code": 2,
                "retryable": false,
                "description": "Invalid arguments or usage error"
            },
            {
                "kind": "confirmation_required",
                "exit_code": 2,
                "retryable": false,
                "description": "Destructive command requires --yes confirmation when not on a TTY"
            },
            {
                "kind": "config",
                "exit_code": 3,
                "retryable": false,
                "description": "Configuration error (missing or invalid n8n.toml)"
            },
            {
                "kind": "auth",
                "exit_code": 4,
                "retryable": false,
                "description": "Authentication error (missing or invalid token)"
            },
            {
                "kind": "network",
                "exit_code": 5,
                "retryable": true,
                "description": "Network error (unreachable instance)"
            },
            {
                "kind": "api",
                "exit_code": 6,
                "retryable": false,
                "description": "API error (server rejected the request)"
            },
            {
                "kind": "validation",
                "exit_code": 10,
                "retryable": false,
                "description": "Validation error (local workflow is invalid)"
            },
            {
                "kind": "not_found",
                "exit_code": 11,
                "retryable": false,
                "description": "Resource not found (workflow or execution does not exist)"
            },
            {
                "kind": "conflict",
                "exit_code": 12,
                "retryable": false,
                "description": "Conflict: remote workflow changed since last pull"
            }
        ]
    })
}

pub fn print_schema() {
    use clap::CommandFactory;
    let cmd = crate::cli::Cli::command();
    let schema = generate(&cmd);
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).expect("serialize schema")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cmd() -> clap::Command {
        use clap::CommandFactory;
        crate::cli::Cli::command()
    }

    #[test]
    fn schema_has_required_v02_top_level_keys() {
        let schema = generate(&test_cmd());
        assert_eq!(schema.get("clispec").and_then(Value::as_str), Some("0.2"));
        assert!(schema.get("name").is_some());
        assert!(schema.get("version").is_some());
        assert!(schema.get("global_args").is_some());
        assert!(schema.get("commands").is_some());
        assert!(schema.get("errors").is_some());
    }

    #[test]
    fn schema_commands_is_array() {
        let schema = generate(&test_cmd());
        assert!(
            schema["commands"].is_array(),
            "commands must be an array for clispec v0.2"
        );
    }

    #[test]
    fn schema_global_args_is_array() {
        let schema = generate(&test_cmd());
        assert!(
            schema["global_args"].is_array(),
            "global_args must be an array for clispec v0.2"
        );
    }

    #[test]
    fn schema_errors_have_exit_codes() {
        let schema = generate(&test_cmd());
        let errors = schema["errors"].as_array().unwrap();
        assert!(!errors.is_empty());
        for err in errors {
            assert!(
                err.get("kind").is_some(),
                "error entry missing 'kind': {err}"
            );
            assert!(
                err.get("exit_code").is_some(),
                "error entry missing 'exit_code': {err}"
            );
        }
    }

    #[test]
    fn schema_all_commands_have_mutating() {
        let schema = generate(&test_cmd());
        let commands = schema["commands"].as_array().unwrap();
        assert!(!commands.is_empty());
        for cmd in commands {
            let name = cmd
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            assert!(
                cmd.get("mutating").is_some(),
                "command '{name}' is missing 'mutating' marker"
            );
        }
    }

    #[test]
    fn schema_includes_expected_commands() {
        let schema = generate(&test_cmd());
        let commands = schema["commands"].as_array().unwrap();
        let names: Vec<&str> = commands
            .iter()
            .filter_map(|c| c.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"ls"), "missing 'ls'");
        assert!(names.contains(&"pull"), "missing 'pull'");
        assert!(names.contains(&"push"), "missing 'push'");
        assert!(names.contains(&"runs ls"), "missing 'runs ls'");
        assert!(names.contains(&"runs get"), "missing 'runs get'");
        assert!(names.contains(&"workflow new"), "missing 'workflow new'");
        assert!(names.contains(&"auth add"), "missing 'auth add'");
        assert!(names.contains(&"node add"), "missing 'node add'");
    }

    #[test]
    fn schema_has_confirmation_required_and_conflict_error_kinds() {
        let schema = generate(&test_cmd());
        let errors = schema["errors"].as_array().unwrap();
        let kinds: Vec<&str> = errors
            .iter()
            .filter_map(|e| e.get("kind").and_then(Value::as_str))
            .collect();
        assert!(
            kinds.contains(&"confirmation_required"),
            "missing 'confirmation_required' error kind"
        );
        assert!(kinds.contains(&"conflict"), "missing 'conflict' error kind");
    }

    #[test]
    fn schema_ls_command_has_output_fields_and_pagination() {
        let schema = generate(&test_cmd());
        let commands = schema["commands"].as_array().unwrap();
        let ls = commands
            .iter()
            .find(|c| c.get("name").and_then(Value::as_str) == Some("ls"))
            .expect("ls command not found");
        assert!(
            ls.get("output_fields").is_some(),
            "ls command missing output_fields"
        );
        let empty = vec![];
        let args = ls["args"].as_array().unwrap_or(&empty);
        let arg_names: Vec<&str> = args
            .iter()
            .filter_map(|a| a.get("name").and_then(Value::as_str))
            .collect();
        assert!(arg_names.contains(&"--limit"), "ls missing --limit arg");
        assert!(arg_names.contains(&"--offset"), "ls missing --offset arg");
        assert!(arg_names.contains(&"--fields"), "ls missing --fields arg");
    }

    #[test]
    fn schema_error_envelope_is_clispec_v02_format() {
        let err = crate::error::AppError::not_found("test", "workflow XYZ not found");
        assert_eq!(err.kind, "not_found");
        assert_eq!(err.exit_code, 11);
    }

    #[test]
    fn schema_validates_against_clispec_v02_json_schema() {
        let schema_doc: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/clispec-v0.2.schema.json"))
                .expect("parse v0.2 JSON schema fixture");

        let output = generate(&test_cmd());
        let validator =
            jsonschema::validator_for(&schema_doc).expect("compile clispec v0.2 JSON schema");
        let errors: Vec<_> = validator.iter_errors(&output).collect();
        assert!(
            errors.is_empty(),
            "schema output failed validation against clispec v0.2: {:?}",
            errors
        );
    }
}
