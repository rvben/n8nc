use serde_json::json;

use crate::{
    cli::ValidateArgs,
    config::load_repo,
    error::AppError,
    repo::collect_json_targets,
    validate::{Severity, validate_workflow_path},
};

use super::common::{Context, emit_json};

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_validate(context: &Context, args: ValidateArgs) -> Result<(), AppError> {
    let repo = load_repo(context.repo_root.as_deref()).ok();
    let files = collect_json_targets(&args.paths, repo.as_ref())?;
    let workflow_files: Vec<_> = files
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|name| name.ends_with(".workflow.json"))
                .unwrap_or(false)
        })
        .collect();

    let mut diagnostics = Vec::new();
    for file in &workflow_files {
        diagnostics.extend(validate_workflow_path(file)?);
    }

    let error_count = diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Error)
        .count();
    let warning_count = diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Warning)
        .count();

    if context.json {
        if error_count > 0 {
            return Err(AppError::validation(
                "validate",
                format!("Validation failed with {error_count} error(s)."),
            )
            .with_json_data(json!({
                "files_checked": workflow_files.len(),
                "error_count": error_count,
                "warning_count": warning_count,
                "diagnostics": diagnostics,
            })));
        }

        emit_json(
            "validate",
            &json!({
                "files_checked": workflow_files.len(),
                "error_count": error_count,
                "warning_count": warning_count,
                "diagnostics": diagnostics,
            }),
        )?;
    } else if diagnostics.is_empty() {
        println!(
            "Validated {} workflow file(s): 0 errors, 0 warnings.",
            workflow_files.len()
        );
    } else {
        for diagnostic in &diagnostics {
            let path = diagnostic.path.as_deref().unwrap_or("-");
            println!(
                "[{}] {} {} {}",
                match diagnostic.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                },
                diagnostic.file,
                path,
                diagnostic.message
            );
        }
        println!(
            "Validated {} workflow file(s): {} error(s), {} warning(s).",
            workflow_files.len(),
            error_count,
            warning_count
        );
    }

    if error_count > 0 {
        Err(AppError::validation(
            "validate",
            format!("Validation failed with {error_count} error(s)."),
        ))
    } else {
        Ok(())
    }
}
