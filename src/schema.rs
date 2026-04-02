use std::collections::HashMap;

use serde_json::{Value, json};

/// Metadata that clap doesn't know — manually maintained per command path.
struct CommandMeta {
    mutating: bool,
    idempotent: bool,
    dangerous: bool,
    output_fields: &'static [&'static str],
}

impl Default for CommandMeta {
    fn default() -> Self {
        Self {
            mutating: false,
            idempotent: true,
            dangerous: false,
            output_fields: &[],
        }
    }
}

/// Walk a clap `Arg` and produce a JSON description.
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

    if let Some(help) = arg.get_help().map(|h| h.to_string()) {
        obj.insert("description".into(), json!(help));
    }

    let is_bool = !arg.get_action().takes_values();
    if is_bool {
        obj.insert("type".into(), json!("bool"));
    } else {
        let possible: Vec<String> = arg
            .get_possible_values()
            .iter()
            .map(|v| v.get_name().to_string())
            .collect();
        if !possible.is_empty() {
            obj.insert("type".into(), json!("string"));
            obj.insert("enum".into(), json!(possible));
        } else {
            obj.insert("type".into(), json!("string"));
        }
    }

    if arg.is_positional() {
        obj.insert("required".into(), json!(arg.is_required_set()));
    }

    if let Some(default) = arg.get_default_values().first() {
        obj.insert("default".into(), json!(default.to_string_lossy()));
    }

    if let Some(short) = arg.get_short() {
        obj.insert("short".into(), json!(format!("-{short}")));
    }

    Value::Object(obj)
}

/// Recursively walk the clap command tree and emit leaf commands.
fn walk_commands(
    cmd: &clap::Command,
    prefix: &str,
    metadata: &HashMap<&str, CommandMeta>,
    out: &mut serde_json::Map<String, Value>,
) {
    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        if name == "help" {
            continue;
        }

        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix} {name}")
        };

        let has_subcommands = sub.get_subcommands().any(|s| s.get_name() != "help");

        if has_subcommands {
            walk_commands(sub, &path, metadata, out);
        } else {
            let mut entry = serde_json::Map::new();

            if let Some(about) = sub.get_about().map(|a| a.to_string()) {
                entry.insert("description".into(), json!(about));
            }

            let global_ids = [
                "help", "version", "json", "quiet", "repo_root",
            ];
            let mut args = Vec::new();
            let mut flags = Vec::new();

            for arg in sub.get_arguments() {
                let id = arg.get_id().as_str();
                if global_ids.contains(&id) {
                    continue;
                }
                if arg.is_positional() {
                    args.push(arg_to_json(arg));
                } else {
                    flags.push(arg_to_json(arg));
                }
            }

            if !args.is_empty() {
                entry.insert("args".into(), json!(args));
            }
            if !flags.is_empty() {
                entry.insert("flags".into(), json!(flags));
            }

            let meta = metadata.get(path.as_str());
            entry.insert("mutating".into(), json!(meta.is_some_and(|m| m.mutating)));
            entry.insert(
                "idempotent".into(),
                json!(meta.is_none_or(|m| m.idempotent)),
            );
            entry.insert("dangerous".into(), json!(meta.is_some_and(|m| m.dangerous)));

            if let Some(m) = meta
                && !m.output_fields.is_empty()
            {
                entry.insert("output_fields".into(), json!(m.output_fields));
            }

            out.insert(path, Value::Object(entry));
        }
    }
}

/// Generate the complete agent introspection schema.
pub fn generate(cmd: &clap::Command) -> Value {
    let metadata = build_metadata();

    let mut commands = serde_json::Map::new();
    walk_commands(cmd, "", &metadata, &mut commands);

    json!({
        "name": "n8nc",
        "version": env!("CARGO_PKG_VERSION"),
        "contract_version": 1,
        "description": "CLI for n8n — sync workflows with Git, manage remote instances",
        "usage": "n8nc [OPTIONS] <COMMAND> [SUBCOMMAND] [ARGS]",
        "global_flags": {
            "--json": {"type": "bool", "description": "Output as JSON (auto-enabled when stdout is not a terminal)"},
            "--quiet": {"type": "bool", "description": "Suppress non-data output (summary lines, confirmations)"},
            "--repo-root": {"type": "string", "description": "Override the repository root directory"}
        },
        "exit_codes": {
            "0": "success",
            "2": "usage error (invalid arguments)",
            "3": "configuration error (missing or invalid n8n.toml)",
            "4": "authentication error (missing or invalid token)",
            "5": "network error (unreachable instance)",
            "6": "API error (server rejected the request)",
            "10": "validation error (local workflow is invalid)",
            "11": "resource not found (workflow/execution does not exist)",
            "12": "conflict (remote workflow changed since last pull)",
            "13": "doctor check failed"
        },
        "notes": {
            "auto_json": "JSON output is automatic when stdout is piped (not a TTY). Use --json to force on a TTY.",
            "contract_version": "All JSON envelopes include contract_version:1. Breaking envelope changes will increment this.",
            "instance_resolution": "Commands needing a remote instance use --instance to select one, falling back to default_instance in n8n.toml.",
            "error_envelopes": "Error responses use the same envelope format with ok:false and an error object containing code, message, and optional suggestion."
        },
        "commands": commands
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

fn build_metadata() -> HashMap<&'static str, CommandMeta> {
    let mut m = HashMap::new();

    macro_rules! meta {
        ($path:expr, $($field:ident: $val:expr),* $(,)?) => {
            #[allow(clippy::needless_update)]
            { m.insert($path, CommandMeta { $($field: $val,)* ..Default::default() }); }
        };
    }

    // Init / Doctor
    meta!("init", mutating: true, idempotent: false, output_fields: &["repo_root", "config", "workflow_dir"]);
    meta!("doctor", output_fields: &["repo_root", "selected_instance", "checks", "summary"]);

    // Auth
    meta!("auth add", mutating: true, idempotent: true, output_fields: &["alias"]);
    meta!("auth test", output_fields: &["alias"]);
    meta!("auth session add", mutating: true, idempotent: true, output_fields: &["alias"]);
    meta!("auth session test", output_fields: &["alias"]);
    meta!("auth session remove", mutating: true, idempotent: false, output_fields: &["alias"]);
    meta!("auth list", output_fields: &["aliases"]);
    meta!("auth remove", mutating: true, idempotent: false, output_fields: &["alias"]);

    // Listing / reading
    meta!("ls", output_fields: &["count", "workflows"]);
    meta!("get", output_fields: &["workflow"]);

    // Runs
    meta!("runs ls", output_fields: &["count", "executions"]);
    meta!("runs get", output_fields: &["execution"]);
    meta!("runs watch", output_fields: &["event", "executions"]);
    meta!("runs stats", output_fields: &["total", "succeeded", "failed", "success_rate"]);

    // Pull / Push
    meta!("pull", mutating: true, idempotent: true, output_fields: &["workflow_path", "meta_path", "workflow_id"]);
    meta!("push", mutating: true, idempotent: false, output_fields: &["workflow_id", "pushed_at"]);

    // Workflow
    meta!("workflow new", mutating: true, idempotent: false, output_fields: &["workflow_path", "workflow_id"]);
    meta!("workflow create", mutating: true, idempotent: false, output_fields: &["workflow_id", "workflow_path", "active"]);
    meta!("workflow execute", mutating: true, idempotent: false, output_fields: &["workflow_id", "execution"]);
    meta!("workflow show", output_fields: &["nodes", "connections", "webhooks"]);
    meta!("workflow rm", mutating: true, idempotent: false, dangerous: true, output_fields: &["workflow_id", "remote_removed", "local_removed"]);

    // Node
    meta!("node ls", output_fields: &["nodes"]);
    meta!("node add", mutating: true, idempotent: false, output_fields: &["workflow_path", "changed"]);
    meta!("node set", mutating: true, idempotent: true, output_fields: &["workflow_path", "changed"]);
    meta!("node rename", mutating: true, idempotent: false, output_fields: &["workflow_path", "changed"]);
    meta!("node rm", mutating: true, idempotent: false, output_fields: &["workflow_path", "changed"]);

    // Conn
    meta!("conn add", mutating: true, idempotent: false, output_fields: &["workflow_path", "changed"]);
    meta!("conn rm", mutating: true, idempotent: false, output_fields: &["workflow_path", "changed"]);

    // Expr
    meta!("expr set", mutating: true, idempotent: true, output_fields: &["workflow_path", "changed"]);

    // Credential
    meta!("credential ls", output_fields: &["credentials"]);
    meta!("credential schema", output_fields: &["credential_type", "schema"]);
    meta!("credential set", mutating: true, idempotent: true, output_fields: &["workflow_path", "changed"]);

    // Status / Diff
    meta!("status", output_fields: &["summary", "workflows"]);
    meta!("diff", output_fields: &["status", "patch"]);

    // Activation / Archival
    meta!("activate", mutating: true, idempotent: true, output_fields: &["workflow_id", "active"]);
    meta!("deactivate", mutating: true, idempotent: true, output_fields: &["workflow_id", "active"]);
    meta!("archive", mutating: true, idempotent: true, output_fields: &["workflow_id", "action"]);
    meta!("unarchive", mutating: true, idempotent: true, output_fields: &["workflow_id", "action"]);

    // Trigger
    meta!("trigger", mutating: true, idempotent: false, output_fields: &["status", "body", "headers"]);

    // Format / Validate / Lint / Search
    meta!("fmt", mutating: true, idempotent: true, output_fields: &["changed"]);
    meta!("validate", output_fields: &["files_checked", "error_count", "diagnostics"]);
    meta!("lint", output_fields: &["files_checked", "error_count", "diagnostics"]);
    meta!("search", output_fields: &["total_matches", "results"]);

    // Meta
    meta!("schema", output_fields: &[]);
    meta!("completions", output_fields: &[]);

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cmd() -> clap::Command {
        use clap::CommandFactory;
        crate::cli::Cli::command()
    }

    #[test]
    fn schema_has_required_top_level_keys() {
        let schema = generate(&test_cmd());
        assert!(schema.get("name").is_some());
        assert!(schema.get("version").is_some());
        assert!(schema.get("contract_version").is_some());
        assert!(schema.get("global_flags").is_some());
        assert!(schema.get("exit_codes").is_some());
        assert!(schema.get("commands").is_some());
        assert!(schema.get("notes").is_some());
    }

    #[test]
    fn schema_includes_leaf_commands() {
        let schema = generate(&test_cmd());
        let commands = schema["commands"].as_object().unwrap();
        assert!(commands.contains_key("ls"));
        assert!(commands.contains_key("pull"));
        assert!(commands.contains_key("push"));
        assert!(commands.contains_key("runs ls"));
        assert!(commands.contains_key("runs get"));
        assert!(commands.contains_key("workflow new"));
        assert!(commands.contains_key("auth add"));
        assert!(commands.contains_key("node add"));
    }

    #[test]
    fn schema_marks_push_as_mutating() {
        let schema = generate(&test_cmd());
        assert_eq!(schema["commands"]["push"]["mutating"], true);
    }

    #[test]
    fn schema_marks_workflow_rm_as_dangerous() {
        let schema = generate(&test_cmd());
        assert_eq!(schema["commands"]["workflow rm"]["dangerous"], true);
    }

    #[test]
    fn schema_exit_codes_are_documented() {
        let schema = generate(&test_cmd());
        let codes = schema["exit_codes"].as_object().unwrap();
        assert!(codes.contains_key("0"));
        assert!(codes.contains_key("3"));
        assert!(codes.contains_key("4"));
        assert!(codes.contains_key("10"));
        assert!(codes.contains_key("11"));
        assert!(codes.contains_key("12"));
    }

    #[test]
    fn all_leaf_commands_have_metadata() {
        let cmd = test_cmd();
        let metadata = build_metadata();

        // Collect all leaf command paths the same way walk_commands does.
        fn collect_leaf_paths(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
            for sub in cmd.get_subcommands() {
                let name = sub.get_name();
                if name == "help" {
                    continue;
                }
                let path = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix} {name}")
                };
                let has_subcommands = sub.get_subcommands().any(|s| s.get_name() != "help");
                if has_subcommands {
                    collect_leaf_paths(sub, &path, out);
                } else {
                    out.push(path);
                }
            }
        }

        let mut leaf_paths = Vec::new();
        collect_leaf_paths(&cmd, "", &mut leaf_paths);

        let mut missing = Vec::new();
        for path in &leaf_paths {
            // "schema" and "completions" are meta-commands; exclude them from the check.
            if path == "schema" || path == "completions" || path == "help" {
                continue;
            }
            if !metadata.contains_key(path.as_str()) {
                missing.push(path.clone());
            }
        }

        assert!(
            missing.is_empty(),
            "The following leaf commands are missing from the schema metadata map in schema.rs — add a `meta!(...)` entry for each:\n  {}",
            missing.join("\n  ")
        );
    }
}
