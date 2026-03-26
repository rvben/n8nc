use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::tempdir;
use wiremock::{
    Match, Mock, MockServer, Request, Respond, ResponseTemplate,
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

#[derive(Debug)]
struct SequenceResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for SequenceResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let body = if call == 0 {
            json!({
                "data": [
                    {
                        "id": "100",
                        "workflowId": "wf-1",
                        "status": "success",
                        "mode": "trigger",
                        "startedAt": "2026-03-26T12:00:00.000Z",
                        "stoppedAt": "2026-03-26T12:00:00.100Z"
                    }
                ]
            })
        } else {
            json!({
                "data": [
                    {
                        "id": "101",
                        "workflowId": "wf-1",
                        "status": "success",
                        "mode": "trigger",
                        "startedAt": "2026-03-26T12:00:01.000Z",
                        "stoppedAt": "2026-03-26T12:00:01.150Z"
                    },
                    {
                        "id": "100",
                        "workflowId": "wf-1",
                        "status": "success",
                        "mode": "trigger",
                        "startedAt": "2026-03-26T12:00:00.000Z",
                        "stoppedAt": "2026-03-26T12:00:00.100Z"
                    }
                ]
            })
        };
        ResponseTemplate::new(200).set_body_json(body)
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
async fn runs_ls_json_since_filters_across_pages() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/executions"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "3"))
        .and(MissingQueryParam("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": "102",
                    "status": "success",
                    "mode": "trigger",
                    "startedAt": "2026-03-26T12:02:00.000Z"
                },
                {
                    "id": "101",
                    "status": "success",
                    "mode": "trigger",
                    "startedAt": "2026-03-26T12:01:00.000Z"
                }
            ],
            "nextCursor": "next-1"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/executions"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "3"))
        .and(query_param("cursor", "next-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": "100",
                    "status": "success",
                    "mode": "trigger",
                    "waitTill": "2026-03-26T12:00:30.000Z"
                },
                {
                    "id": "099",
                    "status": "success",
                    "mode": "trigger",
                    "startedAt": "2026-03-26T11:59:00.000Z"
                }
            ]
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("runs")
        .arg("ls")
        .arg("--instance")
        .arg("mock")
        .arg("--limit")
        .arg("3")
        .arg("--since")
        .arg("2026-03-26T12:00:00Z")
        .output()
        .expect("run runs ls since");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["count"], 3);
    assert_eq!(envelope["data"]["executions"][0]["id"], "102");
    assert_eq!(envelope["data"]["executions"][1]["id"], "101");
    assert_eq!(envelope["data"]["executions"][2]["id"], "100");
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

#[tokio::test]
async fn runs_watch_json_emits_snapshot_for_single_iteration() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/executions"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": "101",
                    "workflowId": "wf-1",
                    "status": "success",
                    "mode": "trigger",
                    "startedAt": "2026-03-26T12:00:00.000Z",
                    "stoppedAt": "2026-03-26T12:00:00.250Z"
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
        .arg("watch")
        .arg("--instance")
        .arg("mock")
        .arg("--limit")
        .arg("2")
        .arg("--iterations")
        .arg("1")
        .output()
        .expect("run runs watch");

    assert!(output.status.success());
    let events = parse_json_lines(&output.stdout);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["ok"], true);
    assert_eq!(events[0]["command"], "runs");
    assert_eq!(events[0]["data"]["event"], "snapshot");
    assert_eq!(events[0]["data"]["new_count"], 1);
    assert_eq!(
        events[0]["data"]["executions"][0]["workflow_name"],
        "Alpha Workflow"
    );
}

#[tokio::test]
async fn runs_watch_json_since_filters_snapshot() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/executions"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": "101",
                    "status": "success",
                    "mode": "trigger",
                    "startedAt": "2026-03-26T12:00:00.000Z"
                },
                {
                    "id": "100",
                    "status": "success",
                    "mode": "trigger",
                    "startedAt": "2026-03-26T11:59:59.000Z"
                }
            ]
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("runs")
        .arg("watch")
        .arg("--instance")
        .arg("mock")
        .arg("--limit")
        .arg("2")
        .arg("--since")
        .arg("2026-03-26T12:00:00Z")
        .arg("--iterations")
        .arg("1")
        .output()
        .expect("run runs watch since");

    assert!(output.status.success());
    let events = parse_json_lines(&output.stdout);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["data"]["event"], "snapshot");
    assert_eq!(events[0]["data"]["count"], 1);
    assert_eq!(events[0]["data"]["new_count"], 1);
    assert_eq!(events[0]["data"]["executions"][0]["id"], "101");
}

#[tokio::test]
async fn runs_watch_json_emits_update_for_new_execution() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/executions"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "2"))
        .respond_with(SequenceResponder {
            calls: Arc::new(AtomicUsize::new(0)),
        })
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
        .arg("watch")
        .arg("--instance")
        .arg("mock")
        .arg("--limit")
        .arg("2")
        .arg("--interval")
        .arg("1")
        .arg("--iterations")
        .arg("2")
        .output()
        .expect("run runs watch update");

    assert!(output.status.success());
    let events = parse_json_lines(&output.stdout);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["data"]["event"], "snapshot");
    assert_eq!(events[0]["data"]["new_count"], 1);
    assert_eq!(events[1]["data"]["event"], "update");
    assert_eq!(events[1]["data"]["new_count"], 1);
    assert_eq!(events[1]["data"]["new_executions"][0]["id"], "101");
}

#[tokio::test]
async fn status_refresh_json_degrades_when_remote_lookup_fails() {
    let setup_server = MockServer::start().await;
    let error_server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &setup_server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": workflow_fixture("wf-1", "Alpha Workflow", false)
        })))
        .mount(&setup_server)
        .await;

    let pull_output = base_command(repo.path())
        .arg("pull")
        .arg("--instance")
        .arg("mock")
        .arg("wf-1")
        .output()
        .expect("pull workflow for refresh status test");
    assert!(pull_output.status.success());

    fs::write(
        repo.path()
            .join("workflows")
            .join("untracked--wf-local.workflow.json"),
        serde_json::to_string_pretty(&workflow_fixture("wf-local", "Local Only", false))
            .expect("serialize untracked workflow"),
    )
    .expect("write untracked workflow");

    write_repo(repo.path(), &error_server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"message": "backend unavailable"})),
        )
        .mount(&error_server)
        .await;

    let output = base_command(repo.path())
        .arg("status")
        .arg("--refresh")
        .output()
        .expect("run status refresh");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "status");
    assert_eq!(envelope["data"]["summary"]["clean"], 1);
    assert_eq!(envelope["data"]["summary"]["untracked"], 1);
    assert_eq!(envelope["data"]["sync_summary"]["unavailable"], 1);

    let workflows = envelope["data"]["workflows"]
        .as_array()
        .expect("workflow list");
    let tracked = workflows
        .iter()
        .find(|row| row["workflow_id"] == "wf-1")
        .expect("tracked workflow row");
    assert_eq!(tracked["state"], "clean");
    assert!(tracked.get("sync_state").is_none());
    assert!(
        tracked["remote_detail"]
            .as_str()
            .unwrap_or_default()
            .contains("backend unavailable")
    );

    let untracked = workflows
        .iter()
        .find(|row| row["workflow_id"] == "wf-local")
        .expect("untracked workflow row");
    assert_eq!(untracked["state"], "untracked");
    assert!(untracked.get("remote_detail").is_none());
}

#[tokio::test]
async fn diff_refresh_json_preserves_local_diff_when_remote_lookup_fails() {
    let setup_server = MockServer::start().await;
    let error_server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &setup_server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": workflow_fixture("wf-1", "Alpha Workflow", false)
        })))
        .mount(&setup_server)
        .await;

    let pull_output = base_command(repo.path())
        .arg("pull")
        .arg("--instance")
        .arg("mock")
        .arg("wf-1")
        .output()
        .expect("pull workflow for refresh diff test");
    assert!(pull_output.status.success());
    let pull_envelope = parse_json(&pull_output.stdout);
    let workflow_path = pull_envelope["data"]["workflow_path"]
        .as_str()
        .expect("workflow path");

    fs::write(
        workflow_path,
        serde_json::to_string_pretty(&workflow_fixture("wf-1", "Alpha Workflow", true))
            .expect("serialize modified workflow"),
    )
    .expect("write modified workflow");

    write_repo(repo.path(), &error_server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"message": "backend unavailable"})),
        )
        .mount(&error_server)
        .await;

    let output = base_command(repo.path())
        .arg("diff")
        .arg("--refresh")
        .arg(workflow_path)
        .output()
        .expect("run diff refresh");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "diff");
    assert_eq!(envelope["data"]["status"]["state"], "modified");
    assert_eq!(envelope["data"]["remote_comparison_available"], false);
    assert_eq!(envelope["data"]["base_snapshot_available"], true);
    assert!(
        envelope["data"]["patch"]
            .as_str()
            .unwrap_or_default()
            .contains("--- base")
    );
    assert!(
        envelope["data"]["status"]["remote_detail"]
            .as_str()
            .unwrap_or_default()
            .contains("backend unavailable")
    );
    assert!(envelope["data"].get("remote_patch").is_none());
}

#[tokio::test]
async fn runs_ls_json_rejects_invalid_last_window() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    let output = base_command(repo.path())
        .arg("runs")
        .arg("ls")
        .arg("--instance")
        .arg("mock")
        .arg("--last")
        .arg("10x")
        .output()
        .expect("run runs ls invalid last");

    assert_eq!(output.status.code(), Some(2));
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "runs");
    assert_eq!(envelope["error"]["code"], "usage.invalid");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("`--last` must use one of these units")
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

fn workflow_fixture(id: &str, name: &str, active: bool) -> Value {
    json!({
        "id": id,
        "name": name,
        "active": active,
        "nodes": [],
        "connections": {}
    })
}

fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("valid json output")
}

fn parse_json_lines(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .expect("utf8 output")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid json line"))
        .collect()
}
