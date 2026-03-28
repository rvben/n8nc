use std::fs;

use serde_json::json;

use crate::{
    cli::FmtArgs,
    config::load_repo,
    error::AppError,
    repo::{collect_json_targets, format_json_file},
};

use super::common::{emit_json, Context};

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_fmt(context: &Context, args: FmtArgs) -> Result<(), AppError> {
    let repo = load_repo(context.repo_root.as_deref()).ok();
    let files = collect_json_targets(&args.paths, repo.as_ref())?;
    let mut changed = Vec::new();
    for file in files {
        let formatted = format_json_file(&file)?;
        let current = fs::read_to_string(&file).map_err(|err| {
            AppError::validation("fmt", format!("Failed to read {}: {err}", file.display()))
        })?;
        if current != formatted {
            changed.push(file.clone());
            if !args.check {
                fs::write(&file, formatted).map_err(|err| {
                    AppError::validation(
                        "fmt",
                        format!("Failed to write {}: {err}", file.display()),
                    )
                })?;
            }
        }
    }

    if args.check && !changed.is_empty() {
        return Err(AppError::validation(
            "fmt",
            format!("{} file(s) would be reformatted.", changed.len()),
        ));
    }

    if context.json {
        emit_json(
            "fmt",
            &json!({"changed": changed.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>() }),
        )
    } else {
        if changed.is_empty() {
            println!("All files are already formatted.");
        } else if args.check {
            println!("{} file(s) would be reformatted.", changed.len());
        } else {
            println!("Formatted {} file(s).", changed.len());
        }
        Ok(())
    }
}
