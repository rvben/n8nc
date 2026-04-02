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

fn walk_commands(cmd: &clap::Command, prefix: &str, out: &mut serde_json::Map<String, Value>) {
    let global_ids = ["help", "version", "json", "quiet", "repo_root"];

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
            walk_commands(sub, &path, out);
        } else {
            let mut entry = serde_json::Map::new();

            if let Some(about) = sub.get_about().map(|a| a.to_string()) {
                entry.insert("description".into(), json!(about));
            }

            let mut args = Vec::new();
            let mut flags = Vec::new();
            for arg in sub.get_arguments() {
                if global_ids.contains(&arg.get_id().as_str()) {
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

            out.insert(path, Value::Object(entry));
        }
    }
}

pub fn generate(cmd: &clap::Command) -> Value {
    let mut commands = serde_json::Map::new();
    walk_commands(cmd, "", &mut commands);

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
}
