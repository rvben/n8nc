use std::{fs, path::Path};

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::tempdir;
use wiremock::{
    Match, Mock, MockServer, Request, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

#[derive(Debug)]
struct MissingQueryParam(&'static str);

impl Match for MissingQueryParam {
    fn matches(&self, request: &Request) -> bool {
        !request
            .url
            .query_pairs()
            .any(|(key, _)| key.as_ref() == self.0)
    }
}

#[tokio::test]
async fn ls_json_honors_limit_across_pages() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "3"))
        .and(MissingQueryParam("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "wf-1", "name": "Alpha"},
                {"id": "wf-2", "name": "Beta"}
            ],
            "nextCursor": "next-1"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "3"))
        .and(query_param("cursor", "next-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "wf-3", "name": "Gamma"},
                {"id": "wf-4", "name": "Delta"}
            ]
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("ls")
        .arg("--instance")
        .arg("mock")
        .arg("--limit")
        .arg("3")
        .output()
        .expect("run ls");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "ls");
    assert_eq!(envelope["data"]["count"], 3);
    assert_eq!(
        envelope["data"]["workflows"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(envelope["data"]["workflows"][2]["id"], "wf-3");
}

#[tokio::test]
async fn runs_ls_json_resolves_workflow_name() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/executions"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "2"))
        .and(query_param("workflowId", "wf-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": "101",
                    "workflowId": "wf-1",
                    "status": "success",
                    "mode": "trigger",
                    "startedAt": "2026-03-26T12:00:00.000Z",
                    "stoppedAt": "2026-03-26T12:00:00.250Z"
                },
                {
                    "id": "100",
                    "workflowId": "wf-1",
                    "status": "error",
                    "mode": "manual",
                    "startedAt": "2026-03-26T11:59:00.000Z",
                    "stoppedAt": "2026-03-26T11:59:01.000Z"
                }
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-1",
                "name": "Alpha Workflow"
            }
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("runs")
        .arg("ls")
        .arg("--instance")
        .arg("mock")
        .arg("--workflow")
        .arg("wf-1")
        .arg("--limit")
        .arg("2")
        .output()
        .expect("run runs ls");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "runs");
    assert_eq!(envelope["data"]["count"], 2);
    assert_eq!(
        envelope["data"]["executions"][0]["workflow_name"],
        "Alpha Workflow"
    );
    assert_eq!(envelope["data"]["executions"][0]["duration_ms"], 250);
}

#[tokio::test]
async fn runs_get_json_details_returns_execution_payload() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/executions/42"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("includeData", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "42",
            "workflowId": "wf-1",
            "status": "success",
            "data": {
                "resultData": {
                    "runData": {
                        "Node A": []
                    }
                }
            },
            "workflowData": {
                "id": "wf-1",
                "name": "Alpha Workflow"
            }
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("runs")
        .arg("get")
        .arg("--instance")
        .arg("mock")
        .arg("42")
        .arg("--details")
        .output()
        .expect("run runs get");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["execution"]["id"], "42");
    assert_eq!(
        envelope["data"]["execution"]["workflowData"]["name"],
        "Alpha Workflow"
    );
    assert!(
        envelope["data"]["execution"]["data"]["resultData"]["runData"].is_object(),
        "expected detailed execution payload"
    );
}

#[tokio::test]
async fn runs_get_json_not_found_emits_error_envelope() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/executions/missing"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("runs")
        .arg("get")
        .arg("--instance")
        .arg("mock")
        .arg("missing")
        .output()
        .expect("run missing runs get");

    assert_eq!(output.status.code(), Some(11));
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "runs");
    assert_eq!(envelope["error"]["code"], "resource.not_found");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Execution `missing` was not found.")
    );
}

fn base_command(repo_root: &Path) -> Command {
    let mut command = Command::cargo_bin("n8nc").expect("n8nc binary");
    command
        .arg("--json")
        .arg("--repo-root")
        .arg(repo_root)
        .env("N8NC_TOKEN_MOCK", "test-token");
    command
}

fn write_repo(root: &Path, base_url: &str) {
    fs::create_dir_all(root.join("workflows")).expect("workflow dir");
    let config = format!(
        r#"schema_version = 1
default_instance = "mock"
workflow_dir = "workflows"

[instances.mock]
base_url = "{base_url}"
api_version = "v1"
"#
    );
    fs::write(root.join("n8n.toml"), config).expect("write n8n.toml");
}

fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("valid json output")
}
