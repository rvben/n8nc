use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
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
struct WorkflowSettingsMatcher;

impl Match for WorkflowSettingsMatcher {
    fn matches(&self, request: &Request) -> bool {
        let Ok(payload) = serde_json::from_slice::<Value>(&request.body) else {
            return false;
        };
        let settings = payload.get("settings").and_then(Value::as_object);
        settings
            .and_then(|settings| settings.get("executionOrder"))
            .and_then(Value::as_str)
            == Some("v1")
            && settings
                .and_then(|settings| settings.get("saveDataSuccessExecution"))
                .and_then(Value::as_str)
                == Some("all")
            && settings
                .and_then(|settings| settings.get("saveDataErrorExecution"))
                .and_then(Value::as_str)
                == Some("all")
            && settings
                .and_then(|settings| settings.get("saveManualExecutions"))
                .and_then(Value::as_bool)
                == Some(true)
            && settings
                .and_then(|settings| settings.get("saveExecutionProgress"))
                .and_then(Value::as_bool)
                == Some(true)
    }
}

#[derive(Debug)]
struct WorkflowUpdatePayloadMatcher {
    expected_name: &'static str,
    expected_path: &'static str,
}

impl Match for WorkflowUpdatePayloadMatcher {
    fn matches(&self, request: &Request) -> bool {
        let Ok(payload) = serde_json::from_slice::<Value>(&request.body) else {
            return false;
        };
        let Some(object) = payload.as_object() else {
            return false;
        };
        let keys = object.keys().cloned().collect::<BTreeSet<_>>();
        if keys
            != BTreeSet::from([
                "connections".to_string(),
                "name".to_string(),
                "nodes".to_string(),
                "settings".to_string(),
            ])
        {
            return false;
        }
        if payload.get("name").and_then(Value::as_str) != Some(self.expected_name) {
            return false;
        }
        payload
            .get("nodes")
            .and_then(Value::as_array)
            .and_then(|nodes| nodes.first())
            .and_then(|node| node.get("parameters"))
            .and_then(Value::as_object)
            .and_then(|parameters| parameters.get("path"))
            .and_then(Value::as_str)
            == Some(self.expected_path)
    }
}

#[derive(Debug)]
struct EchoJsonResponder;

impl Respond for EchoJsonResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = serde_json::from_slice::<Value>(&request.body).unwrap_or_else(|_| json!({}));
        ResponseTemplate::new(200).set_body_json(body)
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

#[derive(Debug)]
struct JsonSequenceResponder {
    calls: Arc<AtomicUsize>,
    responses: Vec<Value>,
}

impl Respond for JsonSequenceResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let index = call.min(self.responses.len().saturating_sub(1));
        ResponseTemplate::new(200).set_body_json(self.responses[index].clone())
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
    assert_eq!(workflow["settings"]["executionOrder"], "v1");
    assert_eq!(workflow["settings"]["saveDataSuccessExecution"], "all");
    assert_eq!(workflow["settings"]["saveDataErrorExecution"], "all");
    assert_eq!(workflow["settings"]["saveManualExecutions"], true);
    assert_eq!(workflow["settings"]["saveExecutionProgress"], true);
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
        .and(WorkflowSettingsMatcher)
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
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-remote"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-remote",
                "name": "Order Alert",
                "nodes": [],
                "connections": {},
                "settings": {},
                "active": false,
                "tags": []
            }
        })))
        .expect(1)
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

    let workflow = read_json_file(Path::new(workflow_path));
    assert_eq!(workflow["tags"], json!([]));
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
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-webhook"))
        .and(header("x-n8n-api-key", "test-token"))
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
        .expect(1)
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
async fn workflow_show_json_reports_credentials() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path()
            .join("workflows")
            .join("credential.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Credential Example",
            "active": false,
            "nodes": [
                {
                    "id": "node-1",
                    "name": "HTTP Request",
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [0, 0],
                    "parameters": {},
                    "credentials": {
                        "httpBasicAuth": {
                            "id": "cred-123",
                            "name": "Primary Basic Auth"
                        }
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
        .arg("workflows/credential.workflow.json")
        .output()
        .expect("run workflow show credentials");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["credential_count"], 1);
    assert_eq!(
        envelope["data"]["nodes"][0]["credentials"][0]["credential_type"],
        "httpBasicAuth"
    );
    assert_eq!(
        envelope["data"]["credentials"][0]["credential_id"],
        "cred-123"
    );
}

#[tokio::test]
async fn workflow_show_uses_default_instance_for_local_draft_urls() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    fs::write(
        repo.path().join("workflows").join("draft.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-draft",
            "name": "Draft Webhook",
            "active": false,
            "settings": {},
            "nodes": [
                {
                    "id": "node-1",
                    "name": "Webhook",
                    "type": "n8n-nodes-base.webhook",
                    "typeVersion": 2,
                    "position": [0, 0],
                    "webhookId": "draft-url",
                    "parameters": {
                        "path": "draft-url",
                        "httpMethod": "POST"
                    }
                }
            ],
            "connections": {}
        }))
        .expect("serialize workflow"),
    )
    .expect("write workflow");

    let output = base_command(repo.path())
        .arg("workflow")
        .arg("show")
        .arg("workflows/draft.workflow.json")
        .output()
        .expect("run workflow show default instance");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["instance"], "mock");
    assert_eq!(
        envelope["data"]["webhooks"][0]["production_url"],
        format!("{}/webhook/draft-url", server.uri())
    );
}

#[tokio::test]
async fn push_json_sanitizes_update_payload_and_refetches_remote() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    let calls = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(JsonSequenceResponder {
            calls: calls.clone(),
            responses: vec![
                json!({
                    "data": {
                        "id": "wf-1",
                        "name": "Incoming Webhook",
                        "active": false,
                        "tags": [],
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
                }),
                json!({
                    "data": {
                        "id": "wf-1",
                        "name": "Incoming Webhook",
                        "active": false,
                        "tags": [],
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
                }),
                json!({
                    "data": {
                        "id": "wf-1",
                        "name": "Incoming Webhook",
                        "active": false,
                        "tags": [],
                        "nodes": [
                            {
                                "id": "node-1",
                                "name": "Webhook",
                                "type": "n8n-nodes-base.webhook",
                                "typeVersion": 2,
                                "position": [0, 0],
                                "webhookId": "orders/new-2",
                                "parameters": {
                                    "path": "orders/new-2",
                                    "httpMethod": "POST"
                                }
                            }
                        ],
                        "connections": {},
                        "settings": {}
                    }
                }),
            ],
        })
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(WorkflowUpdatePayloadMatcher {
            expected_name: "Incoming Webhook",
            expected_path: "orders/new-2",
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-1",
                "name": "Incoming Webhook",
                "active": false,
                "tags": [],
                "nodes": [
                    {
                        "id": "node-1",
                        "name": "Webhook",
                        "type": "n8n-nodes-base.webhook",
                        "typeVersion": 2,
                        "position": [0, 0],
                        "webhookId": "orders/new-2",
                        "parameters": {
                            "path": "orders/new-2",
                            "httpMethod": "POST"
                        }
                    }
                ],
                "connections": {},
                "settings": {}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let pull_output = base_command(repo.path())
        .arg("pull")
        .arg("wf-1")
        .output()
        .expect("run pull");
    assert!(pull_output.status.success());
    let pull_envelope = parse_json(&pull_output.stdout);
    let workflow_path = pull_envelope["data"]["workflow_path"]
        .as_str()
        .expect("workflow path");

    let edit_output = base_command(repo.path())
        .arg("node")
        .arg("set")
        .arg("workflows/incoming-webhook--wf-1.workflow.json")
        .arg("Webhook")
        .arg("parameters.path")
        .arg("orders/new-2")
        .output()
        .expect("run node set before push");
    assert!(edit_output.status.success());

    let push_output = base_command(repo.path())
        .arg("push")
        .arg("workflows/incoming-webhook--wf-1.workflow.json")
        .output()
        .expect("run push");
    assert!(push_output.status.success());
    let envelope = parse_json(&push_output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["changed"], true);

    let workflow = read_json_file(Path::new(workflow_path));
    assert_eq!(workflow["nodes"][0]["parameters"]["path"], "orders/new-2");
    assert_eq!(workflow["tags"], json!([]));
}

#[tokio::test]
async fn push_json_rejects_unsupported_top_level_changes() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-1",
                "name": "Example",
                "active": false,
                "nodes": [],
                "connections": {},
                "settings": {}
            }
        })))
        .mount(&server)
        .await;

    let pull_output = base_command(repo.path())
        .arg("pull")
        .arg("wf-1")
        .output()
        .expect("run pull");
    assert!(pull_output.status.success());

    let workflow_path = repo
        .path()
        .join("workflows")
        .join("example--wf-1.workflow.json");
    let mut workflow = read_json_file(&workflow_path);
    workflow["active"] = json!(true);
    fs::write(
        &workflow_path,
        serde_json::to_string_pretty(&workflow).expect("serialize modified workflow"),
    )
    .expect("write modified workflow");

    let push_output = base_command(repo.path())
        .arg("push")
        .arg("workflows/example--wf-1.workflow.json")
        .output()
        .expect("run push unsupported field");

    assert_eq!(push_output.status.code(), Some(10));
    let envelope = parse_json(&push_output.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "push");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unsupported field(s): active")
    );
}

#[tokio::test]
async fn deactivate_json_waits_for_remote_state_and_updates_tracked_file() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    let tracked_path = write_tracked_workflow(repo.path(), "mock", "wf-1", "Deactivate Me");
    fs::write(
        &tracked_path,
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Deactivate Me",
            "active": true,
            "settings": {},
            "nodes": [],
            "connections": {}
        }))
        .expect("serialize tracked workflow"),
    )
    .expect("write tracked workflow");

    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(JsonSequenceResponder {
            calls: calls.clone(),
            responses: vec![
                json!({
                    "data": {
                        "id": "wf-1",
                        "name": "Deactivate Me",
                        "active": true,
                        "settings": {},
                        "nodes": [],
                        "connections": {}
                    }
                }),
                json!({
                    "data": {
                        "id": "wf-1",
                        "name": "Deactivate Me",
                        "active": true,
                        "settings": {},
                        "nodes": [],
                        "connections": {}
                    }
                }),
                json!({
                    "data": {
                        "id": "wf-1",
                        "name": "Deactivate Me",
                        "active": false,
                        "settings": {},
                        "nodes": [],
                        "connections": {}
                    }
                }),
            ],
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/wf-1/deactivate"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("deactivate")
        .arg("wf-1")
        .output()
        .expect("run deactivate");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["active"], false);

    let tracked = read_json_file(&tracked_path);
    assert_eq!(tracked["active"], false);
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
    let credential_envelope = parse_json(&credential_output.stdout);
    assert!(
        credential_envelope["data"]["credential_discovery"]
            .as_str()
            .unwrap_or_default()
            .contains("credential ls")
    );

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
async fn credential_ls_json_discovers_referenced_credentials() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "250"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "wf-1", "name": "Orders"},
                {"id": "wf-2", "name": "Alerts"}
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
                "name": "Orders",
                "nodes": [
                    {
                        "name": "Fetch Orders",
                        "credentials": {
                            "httpBasicAuth": {
                                "id": "cred-123",
                                "name": "Primary Basic Auth"
                            }
                        }
                    },
                    {
                        "name": "Post Alert",
                        "credentials": {
                            "slackApi": {
                                "id": "cred-999",
                                "name": "Slack Primary"
                            }
                        }
                    }
                ],
                "connections": {}
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-2"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-2",
                "name": "Alerts",
                "nodes": [
                    {
                        "name": "Send Alert",
                        "credentials": {
                            "httpBasicAuth": {
                                "id": "cred-123",
                                "name": "Primary Basic Auth"
                            }
                        }
                    }
                ],
                "connections": {}
            }
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("credential")
        .arg("ls")
        .arg("--instance")
        .arg("mock")
        .output()
        .expect("run credential ls");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["count"], 2);
    assert_eq!(envelope["data"]["coverage"], "workflow_references_only");
    assert!(
        envelope["data"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("Unused credentials")
    );
    assert_eq!(
        envelope["data"]["credentials"][0]["credential_type"],
        "httpBasicAuth"
    );
    assert_eq!(envelope["data"]["credentials"][0]["usage_count"], 2);
    assert_eq!(envelope["data"]["credentials"][0]["workflow_count"], 2);
}

#[tokio::test]
async fn credential_schema_json_returns_schema_payload() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/credentials/schema/httpBasicAuth"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "object",
            "properties": {
                "user": {"type": "string"},
                "password": {"type": "string"}
            }
        })))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("credential")
        .arg("schema")
        .arg("--instance")
        .arg("mock")
        .arg("httpBasicAuth")
        .output()
        .expect("run credential schema");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["credential_type"], "httpBasicAuth");
    assert_eq!(
        envelope["data"]["schema"]["properties"]["user"]["type"],
        "string"
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

#[tokio::test]
async fn trigger_json_defaults_content_type_for_json_data() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("POST"))
        .and(path("/webhook/echo"))
        .and(header("content-type", "application/json"))
        .respond_with(EchoJsonResponder)
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("trigger")
        .arg("--instance")
        .arg("mock")
        .arg("/webhook/echo")
        .arg("--data")
        .arg("{\"hello\":\"world\"}")
        .output()
        .expect("run trigger echo");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["status"], 200);
    assert_eq!(envelope["data"]["body"]["hello"], "world");
}

#[tokio::test]
async fn runs_ls_json_reports_note_when_successful_executions_are_not_saved() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-1",
                "name": "No Saved Runs",
                "active": true,
                "settings": {},
                "nodes": [],
                "connections": {}
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/executions"))
        .and(header("x-n8n-api-key", "test-token"))
        .and(query_param("limit", "5"))
        .and(query_param("workflowId", "wf-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": []
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
        .arg("5")
        .output()
        .expect("run runs ls note");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["count"], 0);
    assert!(
        envelope["data"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("saveDataSuccessExecution = unset")
    );
}

#[tokio::test]
async fn workflow_rm_by_id_deletes_remote_and_local_artifacts() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    let tracked_path = write_tracked_workflow(repo.path(), "mock", "wf-1", "Delete Me");

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": workflow_fixture("wf-1", "Delete Me", false)
        })))
        .mount(&server)
        .await;

    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let output = base_command(repo.path())
        .arg("workflow")
        .arg("rm")
        .arg("wf-1")
        .output()
        .expect("run workflow rm by id");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["remote_removed"], true);
    assert_eq!(envelope["data"]["local_removed"], true);
    assert!(!tracked_path.exists());
    assert!(
        !tracked_path
            .with_file_name("delete-me--wf-1.meta.json")
            .exists()
    );
    assert!(
        !repo
            .path()
            .join(".n8n")
            .join("cache")
            .join("mock--wf-1.workflow.json")
            .exists()
    );
}

#[tokio::test]
async fn workflow_rm_local_draft_removes_file_without_remote_delete() {
    let server = MockServer::start().await;
    let repo = tempdir().expect("tempdir");
    write_repo(repo.path(), &server.uri());
    let draft_path = repo
        .path()
        .join("workflows")
        .join("draft-only.workflow.json");
    fs::write(
        &draft_path,
        serde_json::to_string_pretty(&workflow_fixture("wf-draft", "Draft Only", false))
            .expect("serialize draft"),
    )
    .expect("write draft");

    let output = base_command(repo.path())
        .arg("workflow")
        .arg("rm")
        .arg("workflows/draft-only.workflow.json")
        .output()
        .expect("run workflow rm local draft");

    assert!(output.status.success());
    let envelope = parse_json(&output.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["remote_removed"], false);
    assert_eq!(envelope["data"]["local_removed"], true);
    assert!(!draft_path.exists());
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

fn write_tracked_workflow(root: &Path, alias: &str, id: &str, name: &str) -> PathBuf {
    let workflow_path = root.join("workflows").join(format!(
        "{}--{}.workflow.json",
        name.to_lowercase().replace(' ', "-"),
        id
    ));
    fs::write(
        &workflow_path,
        serde_json::to_string_pretty(&json!({
            "id": id,
            "name": name,
            "active": false,
            "settings": {},
            "nodes": [],
            "connections": {}
        }))
        .expect("serialize workflow"),
    )
    .expect("write tracked workflow");

    fs::write(
        workflow_path.with_file_name(format!("{}--{}.meta.json", name.to_lowercase().replace(' ', "-"), id)),
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "canonical_version": 1,
            "hash_algorithm": "sha256",
            "instance": alias,
            "workflow_id": id,
            "local_relpath": format!("workflows/{}--{}.workflow.json", name.to_lowercase().replace(' ', "-"), id),
            "pulled_at": "2026-03-26T00:00:00Z",
            "remote_updated_at": null,
            "remote_hash": "sha256:test"
        }))
        .expect("serialize meta"),
    )
    .expect("write meta");

    fs::write(
        root.join(".n8n")
            .join("cache")
            .join(format!("{alias}--{id}.workflow.json")),
        serde_json::to_string_pretty(&workflow_fixture(id, name, false)).expect("serialize cache"),
    )
    .expect("write cache");

    workflow_path
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
