use std::collections::BTreeMap;

use owo_colors::OwoColorize;
use serde_json::json;

use crate::{
    cli::LintArgs,
    config::load_repo,
    error::AppError,
    lint::lint_workflow,
    repo::{collect_json_targets, load_workflow_file},
};

use super::common::{Context, emit_json, print_message, use_color};

pub(crate) async fn cmd_lint(context: &Context, args: LintArgs) -> Result<(), AppError> {
    let repo = load_repo(context.repo_root.as_deref()).ok();

    let lint_config: BTreeMap<String, String> = repo
        .as_ref()
        .and_then(|r| r.config.lint.clone())
        .unwrap_or_default();

    let files = collect_json_targets(&args.paths, repo.as_ref())?;
    let workflow_files: Vec<_> = files
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|v| v.to_str())
                .is_some_and(|name| name.ends_with(".workflow.json"))
        })
        .collect();

    let mut all_results = Vec::new();
    for file in &workflow_files {
        let workflow = load_workflow_file(file, "lint")?;
        let diagnostics = lint_workflow(&workflow, &lint_config, args.rule.as_deref());
        if !diagnostics.is_empty() {
            all_results.push(json!({
                "file": file,
                "diagnostics": diagnostics,
            }));
        }
    }

    let error_count: usize = all_results
        .iter()
        .flat_map(|r| r["diagnostics"].as_array().into_iter().flatten())
        .filter(|d| d["severity"] == "error")
        .count();
    let warning_count: usize = all_results
        .iter()
        .flat_map(|r| r["diagnostics"].as_array().into_iter().flatten())
        .filter(|d| d["severity"] == "warn")
        .count();

    let summary = json!({
        "files_checked": workflow_files.len(),
        "error_count": error_count,
        "warning_count": warning_count,
        "results": all_results,
    });

    if context.json {
        if error_count > 0 {
            return Err(AppError::validation(
                "lint",
                format!("Lint failed with {error_count} error(s)."),
            )
            .with_json_data(summary));
        }
        emit_json("lint", &summary)?;
    } else {
        let color = use_color();
        for result in &all_results {
            let file = result["file"].as_str().unwrap_or("-");
            if let Some(diags) = result["diagnostics"].as_array() {
                for diag in diags {
                    let severity = diag["severity"].as_str().unwrap_or("warn");
                    let rule = diag["rule"].as_str().unwrap_or("unknown");
                    let node = diag["node"].as_str().unwrap_or("");
                    let message = diag["message"].as_str().unwrap_or("");
                    let severity_display: String = if color {
                        match severity {
                            "error" => format!("[{}]", severity.red().bold()),
                            "warn" => format!("[{}]", severity.yellow()),
                            _ => format!("[{severity}]"),
                        }
                    } else {
                        format!("[{severity}]")
                    };
                    if node.is_empty() {
                        println!("{severity_display} {file} ({rule}) {message}");
                    } else {
                        println!("{severity_display} {file} node={node} ({rule}) {message}");
                    }
                }
            }
        }
        print_message(
            context,
            &format!(
                "{} file(s), {} warning(s), {} error(s).",
                workflow_files.len(),
                warning_count,
                error_count
            ),
        );
    }

    if error_count > 0 {
        Err(AppError::validation(
            "lint",
            format!("Lint failed with {error_count} error(s)."),
        ))
    } else {
        Ok(())
    }
}
