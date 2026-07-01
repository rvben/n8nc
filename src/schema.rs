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
            | "secret extract"
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
        "init" => vec![
            json!({"name": "repo_root", "type": "string"}),
            json!({"name": "config", "type": "string"}),
            json!({"name": "workflow_dir", "type": "string"}),
            json!({"name": "token_stored", "type": "boolean"}),
        ],
        "auth add" => vec![
            json!({"name": "alias", "type": "string"}),
            json!({"name": "stored", "type": "boolean"}),
        ],
        "auth session add" => vec![
            json!({"name": "alias", "type": "string"}),
            json!({"name": "session_cookie_stored", "type": "boolean"}),
            json!({"name": "browser_id_stored", "type": "boolean"}),
            json!({"name": "session_ready", "type": "boolean"}),
        ],
        "auth session test" => vec![
            json!({"name": "alias", "type": "string"}),
            json!({"name": "base_url", "type": "string"}),
            json!({"name": "session_cookie_source", "type": "string"}),
            json!({"name": "browser_id_source", "type": "string"}),
            json!({"name": "reachable", "type": "boolean"}),
            json!({"name": "sample_count", "type": "integer"}),
        ],
        "auth session remove" => vec![
            json!({"name": "alias", "type": "string"}),
            json!({"name": "session_cookie_removed", "type": "boolean"}),
            json!({"name": "browser_id_removed", "type": "boolean"}),
        ],
        "auth remove" => vec![
            json!({"name": "alias", "type": "string"}),
            json!({"name": "removed", "type": "boolean"}),
        ],
        // runs watch emits NDJSON lines; each line has these top-level fields
        "runs watch" => vec![
            json!({"name": "event", "type": "string"}),
            json!({"name": "poll", "type": "integer"}),
            json!({"name": "interval_seconds", "type": "integer"}),
            json!({"name": "count", "type": "integer"}),
            json!({"name": "new_count", "type": "integer"}),
            json!({"name": "executions", "type": "array"}),
            json!({"name": "new_executions", "type": "array"}),
        ],
        // pull: single-workflow path and --all path share common top-level fields
        "pull" => vec![
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "instance", "type": "string"}),
            json!({"name": "workflow_path", "type": "string | null"}),
            json!({"name": "meta_path", "type": "string | null"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "total", "type": "integer | null"}),
            json!({"name": "pulled", "type": "integer | null"}),
            json!({"name": "unchanged", "type": "integer | null"}),
            json!({"name": "failed", "type": "integer | null"}),
            json!({"name": "pruned", "type": "integer | null"}),
            json!({"name": "results", "type": "array | null"}),
            json!({"name": "pruned_results", "type": "array | null"}),
        ],
        // push: single-workflow path and --all path share common top-level fields
        "push" => vec![
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "workflow_path", "type": "string | null"}),
            json!({"name": "meta_path", "type": "string | null"}),
            json!({"name": "warning_count", "type": "integer | null"}),
            json!({"name": "total", "type": "integer | null"}),
            json!({"name": "pushed", "type": "integer | null"}),
            json!({"name": "unchanged", "type": "integer | null"}),
            json!({"name": "skipped", "type": "integer | null"}),
            json!({"name": "failed", "type": "integer | null"}),
            json!({"name": "results", "type": "array | null"}),
        ],
        // workflow new creates a local draft via emit_edit_result
        "workflow new" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
        ],
        "workflow create" => vec![
            json!({"name": "instance", "type": "string"}),
            json!({"name": "source_path", "type": "string"}),
            json!({"name": "source_removed", "type": "boolean"}),
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "meta_path", "type": "string"}),
            json!({"name": "workflow_id", "type": "string"}),
            json!({"name": "active", "type": "boolean | null"}),
            json!({"name": "webhooks", "type": "array"}),
            json!({"name": "warning_count", "type": "integer"}),
        ],
        "workflow execute" => vec![
            json!({"name": "action", "type": "string"}),
            json!({"name": "instance", "type": "string"}),
            json!({"name": "workflow_id", "type": "string"}),
            json!({"name": "workflow_name", "type": "string"}),
            json!({"name": "active", "type": "boolean | null"}),
            json!({"name": "execution", "type": "object"}),
        ],
        "workflow rm" => vec![
            json!({"name": "target", "type": "string"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "workflow_name", "type": "string | null"}),
            json!({"name": "instance", "type": "string | null"}),
            json!({"name": "remote_removed", "type": "boolean"}),
            json!({"name": "local_removed", "type": "boolean"}),
            json!({"name": "removed_paths", "type": "array"}),
        ],
        // node add/set/rename/rm all go through emit_edit_result plus extra fields
        "node add" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "node", "type": "string"}),
        ],
        "node set" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "node", "type": "string"}),
            json!({"name": "path", "type": "string"}),
        ],
        "node rename" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "from", "type": "string"}),
            json!({"name": "to", "type": "string"}),
        ],
        "node rm" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "node", "type": "string"}),
        ],
        "conn add" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "from", "type": "string"}),
            json!({"name": "to", "type": "string"}),
            json!({"name": "kind", "type": "string"}),
            json!({"name": "output_index", "type": "integer"}),
            json!({"name": "input_index", "type": "integer | null"}),
        ],
        "conn rm" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "from", "type": "string"}),
            json!({"name": "to", "type": "string"}),
            json!({"name": "kind", "type": "string"}),
            json!({"name": "output_index", "type": "integer"}),
            json!({"name": "input_index", "type": "integer | null"}),
        ],
        "expr set" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "node", "type": "string"}),
            json!({"name": "path", "type": "string"}),
        ],
        "credential schema" => vec![
            json!({"name": "instance", "type": "string"}),
            json!({"name": "credential_type", "type": "string"}),
            json!({"name": "schema", "type": "object"}),
        ],
        "credential set" => vec![
            json!({"name": "workflow_path", "type": "string"}),
            json!({"name": "changed", "type": "boolean"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "workflow_id", "type": "string | null"}),
            json!({"name": "node", "type": "string"}),
            json!({"name": "credential_type", "type": "string"}),
            json!({"name": "credential_discovery", "type": "string"}),
        ],
        "activate" | "deactivate" => vec![
            json!({"name": "workflow_id", "type": "string"}),
            json!({"name": "active", "type": "boolean"}),
            json!({"name": "webhooks", "type": "array"}),
        ],
        "archive" | "unarchive" => vec![
            json!({"name": "action", "type": "string"}),
            json!({"name": "instance", "type": "string"}),
            json!({"name": "workflow_id", "type": "string"}),
            json!({"name": "workflow_name", "type": "string"}),
            json!({"name": "active_before", "type": "boolean"}),
            json!({"name": "active_after", "type": "boolean"}),
            json!({"name": "note", "type": "string"}),
        ],
        "trigger" => vec![
            json!({"name": "status", "type": "integer"}),
            json!({"name": "headers", "type": "object"}),
            json!({"name": "body", "type": "object | array | string | null"}),
        ],
        "fmt" => vec![json!({"name": "changed", "type": "array"})],
        "validate" => vec![
            json!({"name": "files_checked", "type": "integer"}),
            json!({"name": "error_count", "type": "integer"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "diagnostics", "type": "array"}),
        ],
        "lint" => vec![
            json!({"name": "files_checked", "type": "integer"}),
            json!({"name": "error_count", "type": "integer"}),
            json!({"name": "warning_count", "type": "integer"}),
            json!({"name": "results", "type": "array"}),
        ],
        // completions outputs raw shell script to stdout (no JSON envelope)
        // schema outputs the clispec JSON document to stdout (no JSON envelope)
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
        ],
        "outcomes": [
            {
                "kind": "doctor_failed",
                "exit_code": 13,
                "retryable": false,
                "description": "Doctor checks ran but one or more checks failed; see the report for details"
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
    fn node_set_args_document_every_argument() {
        let schema = generate(&test_cmd());
        let command = schema["commands"]
            .as_array()
            .expect("commands array")
            .iter()
            .find(|cmd| cmd["name"] == json!("node set"))
            .expect("`node set` command present in schema");
        let args = command["args"].as_array().expect("`node set` args array");
        assert!(!args.is_empty(), "`node set` should declare arguments");
        for arg in args {
            let name = arg["name"].as_str().unwrap_or("<unknown>");
            let description = arg.get("description").and_then(Value::as_str).unwrap_or("");
            assert!(
                !description.trim().is_empty(),
                "arg `{name}` of `node set` must carry a schema description"
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
