mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use assert_cmd::Command;
use common::{base_command, parse_json, write_repo, write_tracked_workflow};
use serde_json::{Value, json};
use tempfile::tempdir;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

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

fn snapshot_settings() -> insta::Settings {
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".version", "[VERSION]");
    settings.add_redaction(".data.**.pulled_at", "[TIMESTAMP]");
    settings.add_redaction(".data.**.pushed_at", "[TIMESTAMP]");
    settings.add_redaction(".data.**.remote_updated_at", "[TIMESTAMP]");
    settings.add_redaction(".data.**.workflow_path", "[PATH]");
    settings.add_redaction(".data.**.meta_path", "[PATH]");
    settings.add_redaction(".data.**.local_relpath", "[PATH]");
    settings.add_redaction(".data.**.cache_path", "[PATH]");
    settings.add_redaction(".data.**.file", "[PATH]");
    settings.add_redaction(".data.**.sidecar", "[PATH]");
    settings.add_redaction(".data.**.base_url", "[BASE_URL]");
    settings
}

#[tokio::test]
async fn snapshot_ls_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .and(header("X-N8N-API-KEY", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": "wf-1",
                    "name": "Test Workflow",
                    "active": true,
                    "createdAt": "2026-01-01T00:00:00Z",
                    "updatedAt": "2026-01-02T00:00:00Z",
                    "tags": []
                }
            ],
            "nextCursor": null
        })))
        .mount(&server)
        .await;

    let output = base_command(dir.path()).arg("ls").output().expect("run ls");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("ls_success", json);
    });
}

#[tokio::test]
async fn snapshot_ls_auth_error() {
    let dir = tempdir().unwrap();
    // Write repo but don't set token env var
    common::write_repo_with_alias(dir.path(), "http://localhost:9999", "notoken");

    let output = Command::cargo_bin("n8nc")
        .expect("n8nc binary")
        .arg("--json")
        .arg("--repo-root")
        .arg(dir.path())
        .arg("ls")
        .output()
        .expect("run ls");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("ls_auth_error", json);
    });
}

#[tokio::test]
async fn snapshot_get_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("X-N8N-API-KEY", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "wf-1",
            "name": "Test Workflow",
            "active": true,
            "settings": {},
            "nodes": [
                {
                    "id": "node-1",
                    "name": "Start",
                    "type": "n8n-nodes-base.manualTrigger",
                    "typeVersion": 1,
                    "position": [0, 0],
                    "parameters": {}
                }
            ],
            "connections": {}
        })))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("get")
        .arg("wf-1")
        .output()
        .expect("run get");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("get_success", json);
    });
}

#[tokio::test]
async fn snapshot_get_not_found() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    // Also mock the list endpoint for name resolution fallback
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [],
            "nextCursor": null
        })))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("get")
        .arg("wf-missing")
        .output()
        .expect("run get");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("get_not_found", json);
    });
}

#[tokio::test]
async fn snapshot_pull_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("X-N8N-API-KEY", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-1",
                "name": "Test Workflow",
                "active": false,
                "settings": {},
                "nodes": [
                    {
                        "id": "node-1",
                        "name": "Start",
                        "type": "n8n-nodes-base.manualTrigger",
                        "typeVersion": 1,
                        "position": [0, 0],
                        "parameters": {}
                    }
                ],
                "connections": {}
            }
        })))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("pull")
        .arg("wf-1")
        .output()
        .expect("run pull");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("pull_success", json);
    });
}

#[tokio::test]
async fn snapshot_pull_not_found() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [],
            "nextCursor": null
        })))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("pull")
        .arg("wf-missing")
        .output()
        .expect("run pull");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("pull_not_found", json);
    });
}

#[tokio::test]
async fn snapshot_push_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    let wf_fixture = json!({
        "data": {
            "id": "wf-1",
            "name": "Example",
            "active": false,
            "nodes": [],
            "connections": {},
            "settings": {}
        }
    });

    let wf_updated = json!({
        "data": {
            "id": "wf-1",
            "name": "Example Renamed",
            "active": false,
            "nodes": [],
            "connections": {},
            "settings": {}
        }
    });

    let get_calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(JsonSequenceResponder {
            calls: get_calls,
            responses: vec![
                wf_fixture.clone(), // pull
                wf_fixture.clone(), // push: check remote hash
                wf_updated.clone(), // push: re-fetch after update
            ],
        })
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wf_updated))
        .expect(1)
        .mount(&server)
        .await;

    // Pull the workflow first
    let pull_output = base_command(dir.path())
        .arg("pull")
        .arg("wf-1")
        .output()
        .expect("run pull");
    assert!(pull_output.status.success());

    // Modify the local workflow name
    let wf_path = dir
        .path()
        .join("workflows")
        .join("example--wf-1.workflow.json");
    let mut wf: Value =
        serde_json::from_str(&std::fs::read_to_string(&wf_path).expect("read workflow"))
            .expect("parse workflow");
    wf["name"] = json!("Example Renamed");
    std::fs::write(
        &wf_path,
        serde_json::to_string_pretty(&wf).expect("serialize"),
    )
    .expect("write modified workflow");

    // Push
    let push_output = base_command(dir.path())
        .arg("push")
        .arg("workflows/example--wf-1.workflow.json")
        .output()
        .expect("run push");

    assert!(push_output.status.success());
    let json = parse_json(&push_output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("push_success", json);
    });
}

#[tokio::test]
async fn snapshot_push_conflict() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    let wf_original = json!({
        "data": {
            "id": "wf-1",
            "name": "Example",
            "active": false,
            "nodes": [],
            "connections": {},
            "settings": {}
        }
    });

    // Remote has been changed (different name) since we pulled
    let wf_remote_changed = json!({
        "data": {
            "id": "wf-1",
            "name": "Example Changed Remotely",
            "active": false,
            "nodes": [],
            "connections": {},
            "settings": {}
        }
    });

    let get_calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(JsonSequenceResponder {
            calls: get_calls,
            responses: vec![
                wf_original.clone(),       // pull
                wf_remote_changed.clone(), // push: check remote hash -> mismatch
            ],
        })
        .mount(&server)
        .await;

    // Pull the workflow
    let pull_output = base_command(dir.path())
        .arg("pull")
        .arg("wf-1")
        .output()
        .expect("run pull");
    assert!(pull_output.status.success());

    // Modify locally
    let wf_path = dir
        .path()
        .join("workflows")
        .join("example--wf-1.workflow.json");
    let mut wf: Value =
        serde_json::from_str(&std::fs::read_to_string(&wf_path).expect("read workflow"))
            .expect("parse workflow");
    wf["name"] = json!("Example Local Edit");
    std::fs::write(
        &wf_path,
        serde_json::to_string_pretty(&wf).expect("serialize"),
    )
    .expect("write modified workflow");

    // Push should fail with conflict (exit code 12)
    let push_output = base_command(dir.path())
        .arg("push")
        .arg("workflows/example--wf-1.workflow.json")
        .output()
        .expect("run push");

    assert_eq!(push_output.status.code(), Some(12));
    let json = parse_json(&push_output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("push_conflict", json);
    });
}

#[tokio::test]
async fn snapshot_status_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    // Pull a tracked workflow
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-1",
                "name": "Tracked Workflow",
                "active": false,
                "nodes": [],
                "connections": {},
                "settings": {}
            }
        })))
        .mount(&server)
        .await;

    let pull_output = base_command(dir.path())
        .arg("pull")
        .arg("wf-1")
        .output()
        .expect("pull for status test");
    assert!(pull_output.status.success());

    // Write an untracked workflow file
    std::fs::write(
        dir.path()
            .join("workflows")
            .join("untracked--wf-draft.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-draft",
            "name": "Untracked Draft",
            "active": false,
            "nodes": [],
            "connections": {}
        }))
        .expect("serialize untracked"),
    )
    .expect("write untracked workflow");

    let output = base_command(dir.path())
        .arg("status")
        .output()
        .expect("run status");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".data.**.remote_hash", "[HASH]");
    settings.add_redaction(".data.**.local_hash", "[HASH]");
    settings.bind(|| {
        insta::assert_json_snapshot!("status_success", json);
    });
}

#[tokio::test]
async fn snapshot_status_no_repo() {
    let dir = tempdir().unwrap();
    // No n8n.toml — should fail with config error (exit 3)
    let output = Command::cargo_bin("n8nc")
        .expect("n8nc binary")
        .arg("--json")
        .arg("--repo-root")
        .arg(dir.path())
        .arg("status")
        .output()
        .expect("run status");

    assert_eq!(output.status.code(), Some(3));
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("status_no_repo", json);
    });
}

#[tokio::test]
async fn snapshot_diff_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    // Pull a workflow to set up tracked state with cache
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "wf-1",
                "name": "Diff Example",
                "active": false,
                "nodes": [],
                "connections": {},
                "settings": {}
            }
        })))
        .mount(&server)
        .await;

    let pull_output = base_command(dir.path())
        .arg("pull")
        .arg("wf-1")
        .output()
        .expect("pull for diff test");
    assert!(pull_output.status.success());
    let pull_envelope = parse_json(&pull_output.stdout);
    let workflow_path = pull_envelope["data"]["workflow_path"]
        .as_str()
        .expect("workflow path");

    // Modify the local workflow
    let mut wf: Value =
        serde_json::from_str(&std::fs::read_to_string(workflow_path).expect("read workflow"))
            .expect("parse workflow");
    wf["name"] = json!("Diff Example Modified");
    std::fs::write(
        workflow_path,
        serde_json::to_string_pretty(&wf).expect("serialize"),
    )
    .expect("write modified workflow");

    let output = base_command(dir.path())
        .arg("diff")
        .arg(workflow_path)
        .output()
        .expect("run diff");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".data.**.remote_hash", "[HASH]");
    settings.add_redaction(".data.**.local_hash", "[HASH]");
    settings.bind(|| {
        insta::assert_json_snapshot!("diff_success", json);
    });
}

#[tokio::test]
async fn snapshot_diff_not_found() {
    let dir = tempdir().unwrap();
    write_repo(dir.path(), "http://localhost:9999");

    let output = base_command(dir.path())
        .arg("diff")
        .arg("nonexistent.workflow.json")
        .output()
        .expect("run diff");

    // diff returns exit 11 (not found) for missing files
    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[MESSAGE]");
    settings.bind(|| {
        insta::assert_json_snapshot!("diff_not_found", json);
    });
}

// ── activate ──────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_activate_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());
    write_tracked_workflow(dir.path(), "mock", "wf-1", "Activate Me");

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
                        "name": "Activate Me",
                        "active": false,
                        "settings": {},
                        "nodes": [],
                        "connections": {}
                    }
                }),
                json!({
                    "data": {
                        "id": "wf-1",
                        "name": "Activate Me",
                        "active": false,
                        "settings": {},
                        "nodes": [],
                        "connections": {}
                    }
                }),
                json!({
                    "data": {
                        "id": "wf-1",
                        "name": "Activate Me",
                        "active": true,
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
        .and(path("/api/v1/workflows/wf-1/activate"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("activate")
        .arg("wf-1")
        .output()
        .expect("run activate");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("activate_success", json);
    });
}

#[tokio::test]
async fn snapshot_activate_not_found() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [],
            "nextCursor": null
        })))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("activate")
        .arg("wf-missing")
        .output()
        .expect("run activate");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("activate_not_found", json);
    });
}

// ── deactivate ────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_deactivate_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());
    write_tracked_workflow(dir.path(), "mock", "wf-1", "Deactivate Me");

    // Overwrite the tracked workflow with active: true
    std::fs::write(
        dir.path()
            .join("workflows")
            .join("deactivate-me--wf-1.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Deactivate Me",
            "active": true,
            "settings": {},
            "nodes": [],
            "connections": {}
        }))
        .expect("serialize"),
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

    let output = base_command(dir.path())
        .arg("deactivate")
        .arg("wf-1")
        .output()
        .expect("run deactivate");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("deactivate_success", json);
    });
}

#[tokio::test]
async fn snapshot_deactivate_not_found() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [],
            "nextCursor": null
        })))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("deactivate")
        .arg("wf-missing")
        .output()
        .expect("run deactivate");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("deactivate_not_found", json);
    });
}

// ── archive / unarchive ───────────────────────────────────────────────

#[tokio::test]
async fn snapshot_archive_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("X-N8N-API-KEY", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "wf-1",
            "name": "Archive Target",
            "active": true,
            "isArchived": false,
            "settings": {},
            "nodes": [],
            "connections": {}
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/workflows/wf-1/archive"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .env("N8NC_SESSION_COOKIE_MOCK", "test-session-cookie")
        .env("N8NC_BROWSER_ID_MOCK", "test-browser-id")
        .arg("archive")
        .arg("wf-1")
        .output()
        .expect("run archive");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("archive_success", json);
    });
}

#[tokio::test]
async fn snapshot_archive_no_session() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("X-N8N-API-KEY", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "wf-1",
            "name": "Archive Target",
            "active": true,
            "isArchived": false,
            "settings": {},
            "nodes": [],
            "connections": {}
        })))
        .mount(&server)
        .await;

    // No session cookie env vars — should fail with auth error
    let output = base_command(dir.path())
        .arg("archive")
        .arg("wf-1")
        .output()
        .expect("run archive");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("archive_no_session", json);
    });
}

#[tokio::test]
async fn snapshot_unarchive_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-1"))
        .and(header("X-N8N-API-KEY", "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "wf-1",
            "name": "Unarchive Target",
            "active": false,
            "isArchived": true,
            "settings": {},
            "nodes": [],
            "connections": {}
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/workflows/wf-1/unarchive"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .env("N8NC_SESSION_COOKIE_MOCK", "test-session-cookie")
        .env("N8NC_BROWSER_ID_MOCK", "test-browser-id")
        .arg("unarchive")
        .arg("wf-1")
        .output()
        .expect("run unarchive");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("unarchive_success", json);
    });
}

#[tokio::test]
async fn snapshot_unarchive_not_found() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/wf-missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/workflows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [],
            "nextCursor": null
        })))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .env("N8NC_SESSION_COOKIE_MOCK", "test-session-cookie")
        .env("N8NC_BROWSER_ID_MOCK", "test-browser-id")
        .arg("unarchive")
        .arg("wf-missing")
        .output()
        .expect("run unarchive");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("unarchive_not_found", json);
    });
}

// ── trigger ───────────────────────────────────────────────────────────

struct EchoJsonResponder;

impl Respond for EchoJsonResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = serde_json::from_slice::<Value>(&request.body).unwrap_or_else(|_| json!({}));
        ResponseTemplate::new(200).set_body_json(body)
    }
}

#[tokio::test]
async fn snapshot_trigger_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("POST"))
        .and(path("/webhook/orders/new"))
        .and(header("content-type", "application/json"))
        .respond_with(EchoJsonResponder)
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("trigger")
        .arg("--instance")
        .arg("mock")
        .arg("/webhook/orders/new")
        .arg("--data")
        .arg(r#"{"order_id":"123"}"#)
        .output()
        .expect("run trigger");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".data.headers.date", "[DATE]");
    settings.bind(|| {
        insta::assert_json_snapshot!("trigger_success", json);
    });
}

#[tokio::test]
async fn snapshot_trigger_not_found() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("POST"))
        .and(path("/webhook/missing"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"message": "Webhook not registered"})),
        )
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("trigger")
        .arg("--instance")
        .arg("mock")
        .arg("/webhook/missing")
        .output()
        .expect("run trigger");

    assert_eq!(output.status.code(), Some(6));
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.add_redaction(".error.suggestion", "[SUGGESTION]");
    settings.add_redaction(".data.headers.date", "[DATE]");
    settings.bind(|| {
        insta::assert_json_snapshot!("trigger_not_found", json);
    });
}

// ── runs ls ───────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_runs_ls_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

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

    let output = base_command(dir.path())
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
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("runs_ls_success", json);
    });
}

#[tokio::test]
async fn snapshot_runs_ls_auth_error() {
    let dir = tempdir().unwrap();
    common::write_repo_with_alias(dir.path(), "http://localhost:9999", "notoken");

    let output = Command::cargo_bin("n8nc")
        .expect("n8nc binary")
        .arg("--json")
        .arg("--repo-root")
        .arg(dir.path())
        .arg("runs")
        .arg("ls")
        .arg("--instance")
        .arg("notoken")
        .arg("--workflow")
        .arg("wf-1")
        .output()
        .expect("run runs ls");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("runs_ls_auth_error", json);
    });
}

// ── runs get ──────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_runs_get_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

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
                        "Node A": [{
                            "executionStatus": "success",
                            "executionTime": 42,
                            "data": {
                                "main": [[{"json": {"ok": true}}]]
                            }
                        }]
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

    let output = base_command(dir.path())
        .arg("runs")
        .arg("get")
        .arg("--instance")
        .arg("mock")
        .arg("42")
        .arg("--details")
        .output()
        .expect("run runs get");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("runs_get_success", json);
    });
}

#[tokio::test]
async fn snapshot_runs_get_not_found() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/executions/missing"))
        .and(header("x-n8n-api-key", "test-token"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let output = base_command(dir.path())
        .arg("runs")
        .arg("get")
        .arg("--instance")
        .arg("mock")
        .arg("missing")
        .output()
        .expect("run runs get");

    assert_eq!(output.status.code(), Some(11));
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("runs_get_not_found", json);
    });
}

// ── workflow new ──────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_workflow_new_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    let output = base_command(dir.path())
        .arg("workflow")
        .arg("new")
        .arg("Order Alert")
        .arg("--id")
        .arg("wf-draft")
        .output()
        .expect("run workflow new");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);
    snapshot_settings().bind(|| {
        insta::assert_json_snapshot!("workflow_new_success", json);
    });
}

#[tokio::test]
async fn snapshot_workflow_new_duplicate() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    // Create the draft first
    let draft_path = dir
        .path()
        .join("workflows")
        .join("order-alert--wf-draft.workflow.json");
    std::fs::write(
        &draft_path,
        serde_json::to_string_pretty(&json!({
            "id": "wf-draft",
            "name": "Order Alert",
            "active": false,
            "nodes": [],
            "connections": {}
        }))
        .expect("serialize"),
    )
    .expect("write draft");

    let output = base_command(dir.path())
        .arg("workflow")
        .arg("new")
        .arg("Order Alert")
        .arg("--id")
        .arg("wf-draft")
        .output()
        .expect("run workflow new duplicate");

    // Should fail because file already exists
    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("workflow_new_duplicate", json);
    });
}

// ── workflow show ─────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_workflow_show_success() {
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    write_repo(dir.path(), &server.uri());

    std::fs::write(
        dir.path().join("workflows").join("webhook.workflow.json"),
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
        .expect("serialize"),
    )
    .expect("write workflow");

    let output = base_command(dir.path())
        .arg("workflow")
        .arg("show")
        .arg("--instance")
        .arg("mock")
        .arg("workflows/webhook.workflow.json")
        .output()
        .expect("run workflow show");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".data.**.production_url", "[WEBHOOK_URL]");
    settings.add_redaction(".data.**.test_url", "[WEBHOOK_URL]");
    settings.bind(|| {
        insta::assert_json_snapshot!("workflow_show_success", json);
    });
}

#[tokio::test]
async fn snapshot_workflow_show_not_found() {
    let dir = tempdir().unwrap();
    write_repo(dir.path(), "http://localhost:9999");

    let output = base_command(dir.path())
        .arg("workflow")
        .arg("show")
        .arg("workflows/nonexistent.workflow.json")
        .output()
        .expect("run workflow show");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("workflow_show_not_found", json);
    });
}

// ── fmt ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_fmt_success() {
    let dir = tempdir().unwrap();
    write_repo(dir.path(), "https://example.test");

    std::fs::write(
        dir.path().join("workflows").join("example.workflow.json"),
        r#"{"connections":{},"active":false,"nodes":[],"name":"Example","id":"wf-1"}"#,
    )
    .expect("write workflow");

    let output = base_command(dir.path())
        .arg("fmt")
        .output()
        .expect("run fmt");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".data.changed[]", "[PATH]");
    settings.bind(|| {
        insta::assert_json_snapshot!("fmt_success", json);
    });
}

#[tokio::test]
async fn snapshot_fmt_check_failure() {
    let dir = tempdir().unwrap();
    write_repo(dir.path(), "https://example.test");

    std::fs::write(
        dir.path()
            .join("workflows")
            .join("unformatted.workflow.json"),
        r#"{"connections":{},"nodes":[],"name":"Unformatted","id":"wf-2","active":false}"#,
    )
    .expect("write workflow");

    let output = base_command(dir.path())
        .arg("fmt")
        .arg("--check")
        .output()
        .expect("run fmt --check");

    assert!(!output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".error.message", "[ERROR_MSG]");
    settings.add_redaction(".data.unformatted[]", "[PATH]");
    settings.bind(|| {
        insta::assert_json_snapshot!("fmt_check_failure", json);
    });
}

// ── validate ──────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_validate_success() {
    let dir = tempdir().unwrap();
    write_repo(dir.path(), "https://example.test");

    std::fs::write(
        dir.path().join("workflows").join("clean.workflow.json"),
        serde_json::to_string_pretty(&json!({
            "id": "wf-1",
            "name": "Clean Workflow",
            "active": false,
            "nodes": [],
            "connections": {}
        }))
        .expect("serialize"),
    )
    .expect("write workflow");

    let output = base_command(dir.path())
        .arg("validate")
        .output()
        .expect("run validate");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".data.diagnostics[].file", "[PATH]");
    settings.bind(|| {
        insta::assert_json_snapshot!("validate_success", json);
    });
}

#[tokio::test]
async fn snapshot_validate_warnings() {
    let dir = tempdir().unwrap();
    write_repo(dir.path(), "https://example.test");

    std::fs::write(
        dir.path().join("workflows").join("sensitive.workflow.json"),
        serde_json::to_string_pretty(&json!({
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
        }))
        .expect("serialize"),
    )
    .expect("write workflow");

    let output = base_command(dir.path())
        .arg("validate")
        .output()
        .expect("run validate");

    assert!(output.status.success());
    let json = parse_json(&output.stdout);

    let mut settings = snapshot_settings();
    settings.add_redaction(".data.diagnostics[].file", "[PATH]");
    settings.add_redaction(".data.diagnostics[].message", "[DIAG_MSG]");
    settings.bind(|| {
        insta::assert_json_snapshot!("validate_warnings", json);
    });
}
