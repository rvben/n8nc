use std::{fs, path::Path};

use assert_cmd::Command;
use serde_json::{Value, json};

pub fn base_command(repo_root: &Path) -> Command {
    let mut command = Command::cargo_bin("n8nc").expect("n8nc binary");
    command
        .arg("--json")
        .arg("--repo-root")
        .arg(repo_root)
        .env("N8NC_TOKEN_MOCK", "test-token");
    command
}

pub fn write_repo(root: &Path, base_url: &str) {
    write_repo_with_alias(root, base_url, "mock");
}

pub fn write_repo_with_alias(root: &Path, base_url: &str, alias: &str) {
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

pub fn workflow_fixture(id: &str, name: &str, active: bool) -> Value {
    json!({
        "id": id,
        "name": name,
        "active": active,
        "nodes": [],
        "connections": {}
    })
}

pub fn write_tracked_workflow(
    root: &Path,
    alias: &str,
    id: &str,
    name: &str,
) -> std::path::PathBuf {
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

    let slug = name.to_lowercase().replace(' ', "-");
    let meta_path = root
        .join("workflows")
        .join(format!("{slug}--{id}.meta.json"));
    let local_relpath = format!("workflows/{slug}--{id}.workflow.json");
    fs::write(
        &meta_path,
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "canonical_version": 1,
            "hash_algorithm": "sha256",
            "instance": alias,
            "workflow_id": id,
            "local_relpath": local_relpath,
            "pulled_at": "2026-03-26T00:00:00Z",
            "remote_updated_at": null,
            "remote_hash": "sha256:test"
        }))
        .expect("serialize meta"),
    )
    .expect("write meta");

    // Write cache snapshot (required for diff tests and existing behavioral tests)
    fs::write(
        root.join(".n8n")
            .join("cache")
            .join(format!("{alias}--{id}.workflow.json")),
        serde_json::to_string_pretty(&workflow_fixture(id, name, false)).expect("serialize cache"),
    )
    .expect("write cache");

    workflow_path
}

pub fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("valid json output")
}
