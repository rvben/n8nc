use crate::{cli::TriggerArgs, error::AppError};

use super::common::{
    Context, emit_json, load_loaded_repo, parse_pairs, print_response_body, read_request_body,
    remote_client,
};

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn cmd_trigger(context: &Context, args: TriggerArgs) -> Result<(), AppError> {
    let repo = load_loaded_repo(context)?;
    let (client, _, base_url) = remote_client(&repo, args.remote.instance.as_deref(), "trigger")?;
    let headers = parse_pairs("trigger", "header", &args.headers, ':')?;
    let query = parse_pairs("trigger", "query", &args.query, '=')?;
    let body = read_request_body("trigger", args.data, args.data_file, args.stdin)?;
    let response = client
        .trigger(&args.target, &args.method, &headers, &query, body)
        .await
        .map_err(|err| enrich_trigger_error(err, &base_url, &args.target))?;

    if context.json {
        emit_json("trigger", &response)
    } else {
        println!("HTTP {}", response.status);
        print_response_body(&response.body)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn enrich_trigger_error(mut err: AppError, base_url: &str, target: &str) -> AppError {
    if err.command != "trigger" || !err.code.starts_with("trigger.http_404") {
        return err;
    }

    let resolved_path = resolve_trigger_path(base_url, target);
    if let Some(path) = &resolved_path {
        if path.starts_with("/webhook-test/") {
            err.suggestion = Some(
                "Test webhook URLs only work while the workflow is listening in test mode in n8n. Use the editor test listener or call the production `/webhook/...` URL for active workflows.".to_string(),
            );
        } else if path.starts_with("/webhook/") {
            err.suggestion = Some(
                "Production webhook 404s usually mean the path is wrong, the workflow is inactive, or n8n has not registered the webhook yet. Check `n8nc workflow show <file>` for the expected URL and re-activate the workflow if needed.".to_string(),
            );
        }
    }
    err
}

fn resolve_trigger_path(base_url: &str, target: &str) -> Option<String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        reqwest::Url::parse(target)
            .ok()
            .map(|url| url.path().to_string())
    } else {
        reqwest::Url::parse(base_url)
            .ok()
            .and_then(|base| base.join(target.trim_start_matches('/')).ok())
            .map(|url| url.path().to_string())
    }
}
