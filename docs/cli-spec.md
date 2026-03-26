# n8n-cli Spec

This document describes the current `0.1.x` shape of `n8nc`.

It is intentionally narrower than the original brainstorm. The tool is now specified as a same-instance workflow sync and development CLI, not as a multi-environment deployment system.

## 1. Product Boundary

`n8nc` is for:

- listing workflows from a configured n8n instance
- fetching one workflow into a canonical local artifact
- validating and formatting local workflow files
- pushing a tracked workflow back safely
- activating and deactivating workflows
- calling webhook trigger URLs during development

`n8nc` is not yet for:

- promoting a workflow across multiple environments
- remapping credential IDs or project bindings between instances
- creating a new workflow from a local file
- generic execution control through undocumented or unverified endpoints

## 2. Command Surface

Top-level commands:

```text
n8nc
├── init
├── auth add
├── auth test
├── auth list
├── auth remove
├── ls
├── get
├── pull
├── push
├── activate
├── deactivate
├── trigger
├── fmt
└── validate
```

## 3. Repository Model

`init` creates:

```text
.
├── n8n.toml
├── workflows/
└── .n8n/
    └── cache/
```

`n8n.toml` stores:

- `schema_version`
- `default_instance`
- `workflow_dir`
- instance aliases and base URLs

Example:

```toml
schema_version = 1
default_instance = "prod"
workflow_dir = "workflows"

[instances.prod]
base_url = "https://your-instance.app.n8n.cloud"
api_version = "v1"
```

Tracked files:

```text
workflows/<slug>--<workflow_id>.workflow.json
workflows/<slug>--<workflow_id>.meta.json
```

The path is intentionally environment-neutral, but the sidecar binds the file to the instance it came from. That keeps the Git history of a single instance clean while making the current scope explicit.

## 4. Credentials

Credentials are resolved in this order:

1. `N8NC_TOKEN_<ALIAS>`
2. OS keychain entry stored by `auth add`

Example:

- `N8NC_TOKEN_PROD`
- `N8NC_TOKEN_STAGING`

`auth add` is non-interactive in v0.1. You must provide `--token` or `--stdin`.

## 5. API Assumptions

The implementation targets the public n8n API with:

- header: `X-N8N-API-KEY`
- default base path: `/api/v1`

The CLI currently assumes these workflow endpoints exist and are reachable through the public API:

- `GET /workflows`
- `GET /workflows/{id}`
- `PUT /workflows/{id}`
- `POST /workflows/{id}/activate`
- `POST /workflows/{id}/deactivate`

`trigger` does not use the public API. It makes a direct HTTP request to a full URL or a path resolved against the configured instance base URL.

## 6. Canonical Workflow Artifact

Tracked workflow files are canonical JSON.

Current canonicalization rules:

- top-level payload must be a JSON object
- top-level volatile fields are removed:
  - `createdAt`
  - `updatedAt`
  - `versionId`
- top-level keys are emitted in this order when present:
  - `id`
  - `name`
  - `active`
  - `tags`
  - `settings`
  - `nodes`
  - `connections`
- nested object keys are sorted
- array order is preserved
- output is pretty JSON with 2-space indentation and trailing newline

This canonicalization is versioned.

Current values:

- `canonical_version = 1`
- `hash_algorithm = "sha256"`

## 7. Metadata Sidecar

Each pulled workflow has a committed sidecar:

```json
{
  "schema_version": 1,
  "canonical_version": 1,
  "hash_algorithm": "sha256",
  "instance": "prod",
  "workflow_id": "abc123",
  "local_relpath": "workflows/order-alert--abc123.workflow.json",
  "pulled_at": "2026-03-26T10:31:54Z",
  "remote_updated_at": "2026-03-26T10:30:10Z",
  "remote_hash": "sha256:..."
}
```

The important field is `remote_hash`. It is the lease token used by `push`.

## 8. Push Safety Model

`push` is update-only in `0.1.x`.

Algorithm:

1. Read the local workflow file.
2. Canonicalize it and hash it.
3. Read the sidecar.
4. Fetch the current remote workflow by ID.
5. Canonicalize the remote payload and hash it.
6. Compare remote hash to `meta.remote_hash`.

Outcomes:

- if `remote_hash != meta.remote_hash`, refuse the push with exit code `12`
- if `local_hash == meta.remote_hash`, report no-op
- otherwise, update the workflow with `PUT /workflows/{id}`

After a successful push, the CLI re-writes the workflow and sidecar from the server response so local state stays canonical.

## 9. Validation

`validate` currently checks:

- file parses as JSON
- workflow payload is an object
- `id` exists
- `nodes` is an array
- `connections` is an object
- node names are unique
- connection targets point to existing node names
- if a sidecar exists, `workflow_id` matches the workflow file

The current diagnostics model is intentionally simple:

- `severity`
- `code`
- `message`
- `file`
- optional JSON path
- optional suggestion

## 10. Triggering

The user concern that started this implementation was valid: developers need more than `pull` and `push`.

The current answer is:

- use `ls` and `get` for fast inspection
- use `activate` and `deactivate` for workflow state changes
- use `trigger` for webhook-based development flows

`trigger` supports:

- full URLs
- instance-relative paths
- custom method
- repeated `--header key:value`
- repeated `--query key=value`
- request body from `--data`, `--data-file`, or `--stdin`

This avoids pretending there is a stable public “run workflow by ID” endpoint when that has not been verified in the implementation.

## 11. JSON Contract

Every command supports `--json`.

Success envelope:

```json
{
  "ok": true,
  "command": "ls",
  "version": "0.1.0",
  "contract_version": 1,
  "data": {}
}
```

Error envelope:

```json
{
  "ok": false,
  "command": "push",
  "version": "0.1.0",
  "contract_version": 1,
  "error": {
    "code": "conflict.remote_changed",
    "message": "Remote workflow changed since the last pull."
  }
}
```

Validation failures may also include a `data` object with diagnostics.

## 12. Exit Codes

- `0`: success
- `2`: usage error
- `3`: config error
- `4`: auth error
- `5`: network error
- `6`: API error
- `10`: validation error
- `11`: not found
- `12`: conflict refusal

## 13. Known Limits

- The tool is currently strongest when a repo mirrors one n8n instance.
- `tags` are preserved structurally, not normalized semantically.
- `ls` assumes a paginated workflow list response with `data` and optional `nextCursor`.
- The implementation has no local `status` or `diff` command yet.

## 14. Next Likely Steps

The next improvements that fit the current design are:

1. `status`
2. `diff`
3. create workflow from local file
4. richer execution inspection if public endpoints are verified
5. only after that: a real environment-promotion model with explicit mappings and lock files
