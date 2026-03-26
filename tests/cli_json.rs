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
struct WorkflowWebhookPayloadMatcher {
    path: &'static str,
    webhook_id: &'static str,
    type_version: f64,
}

impl Match for WorkflowWebhookPayloadMatcher {
    fn matches(&self, request: &Request) -> bool {
        let Ok(payload) = serde_json::from_slice::<Value>(&request.body) else {
            return false;
        };
        let Some(node) = payload
            .get("nodes")
            .and_then(Value::as_array)
            .and_then(|nodes| nodes.first())
        else {
            return false;
        };
        node.get("type").and_then(Value::as_str) == Some("n8n-nodes-base.webhook")
            && node.get("typeVersion").and_then(Value::as_f64) == Some(self.type_version)
            && node.get("webhookId").and_then(Value::as_str) == Some(self.webhook_id)
            && node
                .get("parameters")
                .and_then(Value::as_object)
                .and_then(|parameters| parameters.get("path"))
                .and_then(Value::as_str)
                == Some(self.path)
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
async fn doctor_json_reports_success() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "wf-1", "name": "Alpha Workflow"}
            ]
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("doctor")
        .arg("--instance")
        .arg("mock")
        .output()
        .expect("run doctor");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "doctor");
    assert_eq!(envelope["data"]["selected_instance"], "mock");
    assert_eq!(envelope["data"]["summary"]["fail"], 0);

    let checks = envelope["data"]["checks"].as_array().expect("check list");
    assert!(
        checks
            .iter()
            .any(|check| check["name"] == "token" && check["status"] == "ok")
    );
    assert!(
        checks
            .iter()
            .any(|check| check["name"] == "api" && check["status"] == "ok")
    );
}

#[tokio::test]
async fn validate_json_reports_sensitive_warnings_without_failing() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path()
            .join("workflows")
            .join("sensitive.workflow.json"),
        serde_json::to_string_pretty(&workflow_with_sensitive_literal())
            .expect("serialize workflow"),
    )
    .expect("write workflow");

    let output = base_command(repo.path())
        .arg("validate")
        .output()
        .expect("run validate");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "validate");
    assert_eq!(envelope["data"]["error_count"], 0);
    assert_eq!(envelope["data"]["warning_count"], 1);
    assert_eq!(envelope["data"]["diagnostics"][0]["severity"], "warning");
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

#[tokio::test]
async fn doctor_json_reports_failure_with_attached_data() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    let alias = "doctor-missing-token-alias-7f5a1c";
    write_repo_with_alias(repo.path(), &server.uri(), alias);

    let output = Command::cargo_bin("n8nc")
        .expect("n8nc binary")
        .arg("--json")
        .arg("--repo-root")
        .arg(repo.path())
        .arg("doctor")
        .arg("--instance")
        .arg(alias)
        .env_remove("N8NC_TOKEN_DOCTOR_MISSING_TOKEN_ALIAS_7F5A1C")
        .output()
        .expect("run doctor failure");

    assert_eq!(output.status.code(), Some(13));
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "doctor");
    assert_eq!(envelope["error"]["code"], "doctor.failed");
    assert_eq!(envelope["data"]["selected_instance"], alias);
    assert_eq!(envelope["data"]["summary"]["fail"], 1);
    assert_eq!(envelope["data"]["summary"]["skip"], 1);

    let checks = envelope["data"]["checks"].as_array().expect("check list");
    let token_check = checks
        .iter()
        .find(|check| check["name"] == "token")
        .expect("token check");
    assert_eq!(token_check["status"], "fail");
    assert!(
        token_check["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("No token configured")
    );
    let api_check = checks
        .iter()
        .find(|check| check["name"] == "api")
        .expect("api check");
    assert_eq!(api_check["status"], "skip");
}

#[tokio::test]
async fn doctor_json_fails_for_sensitive_workflow_literals() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path()
            .join("workflows")
            .join("sensitive.workflow.json"),
        serde_json::to_string_pretty(&workflow_with_sensitive_literal())
            .expect("serialize workflow"),
    )
    .expect("write workflow");

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "wf-1", "name": "Alpha Workflow"}
            ]
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("doctor")
        .arg("--instance")
        .arg("mock")
        .output()
        .expect("run doctor sensitive");

    assert_eq!(output.status.code(), Some(13));
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "doctor.failed");
    let checks = envelope["data"]["checks"].as_array().expect("check list");
    let sensitive_check = checks
        .iter()
        .find(|check| check["name"] == "sensitive_data")
        .expect("sensitive data check");
    assert_eq!(sensitive_check["status"], "fail");
    assert!(
        sensitive_check["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("potential sensitive-data warning")
    );
}

#[tokio::test]
async fn workflow_new_json_creates_local_draft_file() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    let output = base_command(repo.path())
        .arg("workflow")
        .arg("new")
        .arg("Order Alert")
        .arg("--id")
        .arg("wf-draft")
        .output()
        .expect("run workflow new");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "workflow");
    assert_eq!(envelope["data"]["changed"], true);
    let workflow_path = envelope["data"]["workflow_path"]
        .as_str()
        .expect("workflow path");
    let workflow = read_json_file(Path::new(workflow_path));
    assert_eq!(workflow["id"], "wf-draft");
    assert_eq!(workflow["name"], "Order Alert");
    assert_eq!(workflow["nodes"], json!([]));
}

#[tokio::test]
async fn workflow_create_json_promotes_draft_to_tracked_workflow() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    let draft_path = repo
        .path()
        .join("workflows")
        .join("order-alert--wf-draft.workflow.json");
    fs::write(
        &draft_path,
        serde_json::to_string_pretty(&workflow_fixture("wf-draft", "Order Alert", false))
            .expect("serialize draft"),
    )
    .expect("write draft");

    Mock::given(method("POST"))
        .and(path("/api/v1/workflows"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-remote",
                "name": "Order Alert",
                "nodes": [],
                "connections": {},
                "settings": {},
                "active": false
            }
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("workflow")
        .arg("create")
        .arg("--instance")
        .arg("mock")
        .arg("workflows/order-alert--wf-draft.workflow.json")
        .output()
        .expect("run workflow create");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "workflow");
    assert_eq!(envelope["data"]["workflow_id"], "wf-remote");
    assert_eq!(envelope["data"]["source_removed"], true);

    let workflow_path = envelope["data"]["workflow_path"]
        .as_str()
        .expect("workflow path");
    let meta_path = envelope["data"]["meta_path"].as_str().expect("meta path");
    assert!(!draft_path.exists());
    assert!(Path::new(workflow_path).exists());
    assert!(Path::new(meta_path).exists());

    let meta = read_json_file(Path::new(meta_path));
    assert_eq!(meta["workflow_id"], "wf-remote");
    assert_eq!(meta["instance"], "mock");
}

#[tokio::test]
async fn workflow_create_activate_json_fetches_active_remote_workflow() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path()
            .join("workflows")
            .join("activate-me--wf-draft.workflow.json"),
        serde_json::to_string_pretty(&workflow_fixture("wf-draft", "Activate Me", false))
            .expect("serialize draft"),
    )
    .expect("write draft");

    Mock::given(method("POST"))
        .and(path("/api/v1/workflows"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-created",
                "name": "Activate Me",
                "nodes": [],
                "connections": {},
                "settings": {}
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/wf-created/activate"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-created"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-created",
                "name": "Activate Me",
                "active": true,
                "nodes": [],
                "connections": {},
                "settings": {}
            }
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("workflow")
        .arg("create")
        .arg("--instance")
        .arg("mock")
        .arg("--activate")
        .arg("workflows/activate-me--wf-draft.workflow.json")
        .output()
        .expect("run workflow create activate");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["workflow_id"], "wf-created");
    assert_eq!(envelope["data"]["active"], true);
}

#[tokio::test]
async fn workflow_create_json_emits_webhook_urls_and_normalizes_payload() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    let draft_path = repo
        .path()
        .join("workflows")
        .join("incoming-webhook--wf-draft.workflow.json");
    fs::write(
        &draft_path,
        serde_json::to_string_pretty(&json!({
            "id": "wf-draft",
            "name": "Incoming Webhook",
            "nodes": [
                {
                    "id": "node-1",
                    "name": "Webhook",
                    "type": "n8n-nodes-base.webhook",
                    "typeVersion": 1,
                    "position": [0, 0],
                    "parameters": {
                        "path": "/orders/new/",
                        "httpMethod": "POST"
                    }
                }
            ],
            "connections": {},
            "settings": {}
        }))
        .expect("serialize webhook draft"),
    )
    .expect("write draft");

    Mock::given(method("POST"))
        .and(path("/api/v1/workflows"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(WorkflowWebhookPayloadMatcher {
            path: "orders/new",
            webhook_id: "orders/new",
            type_version: 2.0,
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-webhook",
                "name": "Incoming Webhook",
                "active": false,
                "nodes": [
                    {
                        "id": "node-1",
                        "name": "Webhook",
                        "type": "n8n-nodes-base.webhook",
                        "typeVersion": 2,
                        "position": [0, 0],
                        "webhookId": "orders/new",
                        "parameters": {
                            "path": "orders/new",
                            "httpMethod": "POST"
                        }
                    }
                ],
                "connections": {},
                "settings": {}
            }
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("workflow")
        .arg("create")
        .arg("--instance")
        .arg("mock")
        .arg("workflows/incoming-webhook--wf-draft.workflow.json")
        .output()
        .expect("run workflow create webhook");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["workflow_id"], "wf-webhook");
    assert_eq!(
        envelope["data"]["webhooks"][0]["production_url"],
        format!("{}/webhook/orders/new", server.uri())
    );
    assert_eq!(
        envelope["data"]["webhooks"][0]["test_url"],
        format!("{}/webhook-test/orders/new", server.uri())
    );
}

#[tokio::test]
async fn workflow_show_json_reports_webhook_urls() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path().join("workflows").join("webhook.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Incoming Webhook",
            "active": true,
            "nodes": [
                {
                    "id": "node-1",
                    "name": "Webhook",
                    "type": "n8n-nodes-base.webhook",
                    "typeVersion": 2,
                    "position": [0, 0],
                    "webhookId": "orders/new",
                    "parameters": {
                        "path": "orders/new",
                        "httpMethod": "POST"
                    }
                }
            ],
            "connections": {},
            "settings": {}
        }))
        .expect("serialize workflow"),
    )
    .expect("write workflow");

    let output = base_command(repo.path())
        .arg("workflow")
        .arg("show")
        .arg("--instance")
        .arg("mock")
        .arg("workflows/webhook.workflow.json")
        .output()
        .expect("run workflow show");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["node_count"], 1);
    assert_eq!(
        envelope["data"]["webhooks"][0]["production_url"],
        format!("{}/webhook/orders/new", server.uri())
    );
}

#[tokio::test]
async fn node_add_and_set_json_update_local_workflow() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path().join("workflows").join("example.workflow.json"),
        serde_json::to_string_pretty(&workflow_fixture("wf-1", "Example", false))
            .expect("serialize workflow"),
    )
    .expect("write workflow");

    let add_output = base_command(repo.path())
        .arg("node")
        .arg("add")
        .arg("workflows/example.workflow.json")
        .arg("--name")
        .arg("HTTP Request")
        .arg("--type")
        .arg("n8n-nodes-base.httpRequest")
        .arg("--type-version")
        .arg("4.2")
        .arg("--x")
        .arg("120")
        .arg("--y")
        .arg("240")
        .output()
        .expect("run node add");
    assert!(add_output.status.success());

    let set_output = base_command(repo.path())
        .arg("node")
        .arg("set")
        .arg("workflows/example.workflow.json")
        .arg("HTTP Request")
        .arg("url")
        .arg("https://example.com")
        .output()
        .expect("run node set");
    assert!(set_output.status.success());
    let envelope = parse_json(&set_output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["changed"], true);

    let workflow = read_json_file(&repo.path().join("workflows").join("example.workflow.json"));
    let node = workflow["nodes"]
        .as_array()
        .and_then(|nodes| nodes.first())
        .expect("node");
    assert_eq!(node["name"], "HTTP Request");
    assert_eq!(node["position"], json!([120, 240]));
    assert_eq!(node["parameters"]["url"], "https://example.com");
}

#[tokio::test]
async fn expr_set_json_wraps_expression_and_credential_set_updates_reference() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path().join("workflows").join("example.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Example",
            "nodes": [
                {
                    "id": "node-1",
                    "name": "HTTP Request",
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [0, 0],
                    "parameters": {}
                }
            ],
            "connections": {}
        }))
        .expect("serialize workflow"),
    )
    .expect("write workflow");

    let expr_output = base_command(repo.path())
        .arg("expr")
        .arg("set")
        .arg("workflows/example.workflow.json")
        .arg("HTTP Request")
        .arg("authentication")
        .arg("$json.auth.mode")
        .output()
        .expect("run expr set");
    assert!(expr_output.status.success());

    let credential_output = base_command(repo.path())
        .arg("credential")
        .arg("set")
        .arg("workflows/example.workflow.json")
        .arg("HTTP Request")
        .arg("--type")
        .arg("httpBasicAuth")
        .arg("--id")
        .arg("cred-123")
        .arg("--name")
        .arg("Primary Basic Auth")
        .output()
        .expect("run credential set");
    assert!(credential_output.status.success());

    let workflow = read_json_file(&repo.path().join("workflows").join("example.workflow.json"));
    let node = workflow["nodes"]
        .as_array()
        .and_then(|nodes| nodes.first())
        .expect("node");
    assert_eq!(node["parameters"]["authentication"], "={{$json.auth.mode}}");
    assert_eq!(node["credentials"]["httpBasicAuth"]["id"], "cred-123");
    assert_eq!(
        node["credentials"]["httpBasicAuth"]["name"],
        "Primary Basic Auth"
    );
}

#[tokio::test]
async fn conn_add_json_creates_connection_branch() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path().join("workflows").join("example.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Example",
            "nodes": [
                {
                    "id": "node-1",
                    "name": "Start",
                    "type": "n8n-nodes-base.manualTrigger",
                    "typeVersion": 1,
                    "position": [0, 0],
                    "parameters": {}
                },
                {
                    "id": "node-2",
                    "name": "HTTP Request",
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [200, 0],
                    "parameters": {}
                }
            ],
            "connections": {}
        }))
        .expect("serialize workflow"),
    )
    .expect("write workflow");

    let output = base_command(repo.path())
        .arg("conn")
        .arg("add")
        .arg("workflows/example.workflow.json")
        .arg("--from")
        .arg("Start")
        .arg("--to")
        .arg("HTTP Request")
        .arg("--kind")
        .arg("main")
        .output()
        .expect("run conn add");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "conn");

    let workflow = read_json_file(&repo.path().join("workflows").join("example.workflow.json"));
    assert_eq!(
        workflow["connections"]["Start"]["main"][0][0],
        json!({
            "node": "HTTP Request",
            "type": "main",
            "index": 0
        })
    );
}

#[tokio::test]
async fn node_rename_json_rewrites_connections() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path().join("workflows").join("example.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Example",
            "nodes": [
                {
                    "id": "node-1",
                    "name": "Start",
                    "type": "n8n-nodes-base.manualTrigger",
                    "typeVersion": 1,
                    "position": [0, 0],
                    "parameters": {}
                },
                {
                    "id": "node-2",
                    "name": "HTTP Request",
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [200, 0],
                    "parameters": {}
                }
            ],
            "connections": {
                "Start": {
                    "main": [[{"node": "HTTP Request", "type": "main", "index": 0}]]
                }
            }
        }))
        .expect("serialize workflow"),
    )
    .expect("write workflow");

    let output = base_command(repo.path())
        .arg("node")
        .arg("rename")
        .arg("workflows/example.workflow.json")
        .arg("HTTP Request")
        .arg("Fetch Orders")
        .output()
        .expect("run node rename");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    let workflow = read_json_file(&repo.path().join("workflows").join("example.workflow.json"));
    assert_eq!(workflow["nodes"][1]["name"], "Fetch Orders");
    assert_eq!(
        workflow["connections"]["Start"]["main"][0][0]["node"],
        "Fetch Orders"
    );
}

#[tokio::test]
async fn node_rm_json_removes_connections() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path().join("workflows").join("example.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Example",
            "nodes": [
                {
                    "id": "node-1",
                    "name": "Start",
                    "type": "n8n-nodes-base.manualTrigger",
                    "typeVersion": 1,
                    "position": [0, 0],
                    "parameters": {}
                },
                {
                    "id": "node-2",
                    "name": "HTTP Request",
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [200, 0],
                    "parameters": {}
                }
            ],
            "connections": {
                "Start": {
                    "main": [[{"node": "HTTP Request", "type": "main", "index": 0}]]
                }
            }
        }))
        .expect("serialize workflow"),
    )
    .expect("write workflow");

    let output = base_command(repo.path())
        .arg("node")
        .arg("rm")
        .arg("workflows/example.workflow.json")
        .arg("HTTP Request")
        .output()
        .expect("run node rm");

    assert!(output.status.success());
    let workflow = read_json_file(&repo.path().join("workflows").join("example.workflow.json"));
    assert_eq!(workflow["nodes"].as_array().map(Vec::len), Some(1));
    assert_eq!(workflow["connections"]["Start"]["main"][0], json!([]));
}

#[tokio::test]
async fn conn_rm_json_removes_single_edge() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path().join("workflows").join("example.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Example",
            "nodes": [
                {
                    "id": "node-1",
                    "name": "Start",
                    "type": "n8n-nodes-base.manualTrigger",
                    "typeVersion": 1,
                    "position": [0, 0],
                    "parameters": {}
                },
                {
                    "id": "node-2",
                    "name": "HTTP Request",
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [200, 0],
                    "parameters": {}
                },
                {
                    "id": "node-3",
                    "name": "Slack",
                    "type": "n8n-nodes-base.slack",
                    "typeVersion": 2,
                    "position": [400, 0],
                    "parameters": {}
                }
            ],
            "connections": {
                "Start": {
                    "main": [[
                        {"node": "HTTP Request", "type": "main", "index": 0},
                        {"node": "Slack", "type": "main", "index": 0}
                    ]]
                }
            }
        }))
        .expect("serialize workflow"),
    )
    .expect("write workflow");

    let output = base_command(repo.path())
        .arg("conn")
        .arg("rm")
        .arg("workflows/example.workflow.json")
        .arg("--from")
        .arg("Start")
        .arg("--to")
        .arg("HTTP Request")
        .arg("--kind")
        .arg("main")
        .arg("--output-index")
        .arg("0")
        .arg("--input-index")
        .arg("0")
        .output()
        .expect("run conn rm");

    assert!(output.status.success());
    let workflow = read_json_file(&repo.path().join("workflows").join("example.workflow.json"));
    let branch = workflow["connections"]["Start"]["main"][0]
        .as_array()
        .expect("connection branch");
    assert_eq!(branch.len(), 1);
    assert_eq!(branch[0]["node"], "Slack");
}

#[tokio::test]
async fn trigger_json_404_includes_webhook_guidance() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("POST"))
        .and(path("/webhook/orders/new"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"message": "Webhook not registered"})),
        )
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("trigger")
        .arg("--instance")
        .arg("mock")
        .arg("/webhook/orders/new")
        .output()
        .expect("run trigger");

    assert_eq!(output.status.code(), Some(6));
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "trigger");
    assert_eq!(envelope["error"]["code"], "trigger.http_404");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("/webhook/orders/new")
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Webhook not registered")
    );
    assert!(
        envelope["error"]["suggestion"]
            .as_str()
            .unwrap_or_default()
            .contains("workflow show")
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
    write_repo_with_alias(root, base_url, "mock");
}

fn write_repo_with_alias(root: &Path, base_url: &str, alias: &str) {
    fs::create_dir_all(root.join("workflows")).expect("workflow dir");
    fs::create_dir_all(root.join(".n8n").join("cache")).expect("cache dir");
    let config = format!(
        r#"schema_version = 1
default_instance = "{alias}"
workflow_dir = "workflows"

[instances.{alias}]
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

fn workflow_with_sensitive_literal() -> Value {
    json!({
        "id": "wf-sensitive",
        "name": "Sensitive Workflow",
        "nodes": [
            {
                "name": "HTTP Request",
                "parameters": {
                    "password": "super-secret-value"
                }
            }
        ],
        "connections": {}
    })
}

fn read_json_file(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read json file"))
        .expect("parse json file")
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
