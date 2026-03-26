# n8n-cli Spec

This document describes the current `0.1.x` shape of `n8nc`.

It is intentionally narrower than the original brainstorm. The tool is now specified as a same-instance workflow sync and development CLI, not as a multi-environment deployment system.

## 1. Product Boundary

`n8nc` is for:

- listing workflows from a configured n8n instance
- listing recent executions and fetching one execution by ID
- fetching one workflow into a canonical local artifact
- creating local workflow drafts and editing local workflow JSON structurally
- creating a remote workflow from a local file and converting it into a tracked artifact
- validating and formatting local workflow files
- pushing a tracked workflow back safely
- activating and deactivating workflows
- calling webhook trigger URLs during development

`n8nc` is not yet for:

- promoting a workflow across multiple environments
- remapping credential IDs or project bindings between instances
- generic execution control through undocumented or unverified endpoints

## 2. Command Surface

Top-level commands:

```text
n8nc
├── init
├── doctor
├── auth add
├── auth test
├── auth list
├── auth remove
├── ls
├── get
├── runs ls
├── runs get
├── runs watch
├── pull
├── push
├── workflow new
├── workflow create
├── workflow show
├── node ls
├── node add
├── node set
├── node rename
├── node rm
├── conn add
├── conn rm
├── expr set
├── credential set
├── status
├── diff
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

`workflow new` creates a local `.workflow.json` draft without a sidecar. `workflow create` takes a sidecar-free local file, creates it remotely, writes the tracked workflow file plus sidecar, and removes the original in-repo draft when the tracked target path changes.

New drafts and create payloads fill in these workflow settings when they are missing:

- `executionOrder = "v1"`
- `saveDataSuccessExecution = "all"`
- `saveDataErrorExecution = "all"`
- `saveManualExecutions = true`
- `saveExecutionProgress = true`

The cache stores one canonical base snapshot per tracked workflow:

```text
.n8n/cache/<instance>--<workflow_id>.workflow.json
```

That snapshot is refreshed on `pull` and successful `push`.

## 4. Credentials

Credentials are resolved in this order:

1. `N8NC_TOKEN_<ALIAS>`
2. OS keychain entry stored by `auth add`

Example:

- `N8NC_TOKEN_PROD`
- `N8NC_TOKEN_STAGING`

`auth add` is non-interactive in v0.1. You must provide `--token` or `--stdin`.

### Doctor

`doctor` is a setup and reachability check for humans and agents.

Supported options:

- `--instance <alias>`
- `--skip-network`

Checks currently include:

- repo `config_file`
- repo `workflow_dir`
- repo `cache_dir`
- repo `sensitive_data`
- repo `instances`
- repo `default_instance`
- per-instance `config`
- per-instance `token`
- per-instance `api`

Failure behavior:

- returns exit code `13` when any check fails
- in JSON mode, returns an error envelope with the full doctor report attached under `data`
- in human mode, prints the report before returning the failure summary

`repo.sensitive_data` scans tracked `.workflow.json` files and fails when it finds likely inline secrets. It is skipped when the workflow directory is missing.

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

The execution commands currently assume these endpoints exist and are reachable through the public API:

- `GET /executions`
- `GET /executions/{id}`

`runs get --details` uses `includeData=true` when fetching a single execution.

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

## 8. Status Model

`status` is local by default in `0.1.x`.

Base local states:

- `clean`: workflow file and sidecar are valid, and the local canonical hash matches the recorded `remote_hash`
- `modified`: workflow file and sidecar are valid, and the local canonical hash differs from the recorded `remote_hash`
- `untracked`: workflow file exists without a sidecar
- `invalid`: workflow file or sidecar cannot be used safely
- `orphaned_meta`: sidecar exists without a matching workflow file

`invalid` currently covers cases like:

- workflow JSON parse failure
- sidecar parse failure
- metadata `workflow_id` mismatch
- unsupported `canonical_version`
- unsupported `hash_algorithm`
- validation errors such as missing node targets

`status --refresh` adds live remote sync classification for entries that are already `clean` or `modified`.

Remote sync states:

- `clean`: local file still matches the remote lease recorded in the sidecar
- `modified`: local file changed, but the remote still matches the recorded lease
- `drifted`: local file is unchanged, but the remote no longer matches the recorded lease
- `conflict`: both local file and remote changed since the last pull or successful push
- `missing_remote`: the tracked workflow no longer exists remotely

If remote refresh fails for a tracked workflow, the CLI still returns the local row, leaves `sync_state` unset, and records the reason in `remote_detail`. `untracked`, `invalid`, and `orphaned_meta` entries remain visible with their local state but do not contribute to `sync_summary`.

## 9. Push Safety Model

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

## 10. Diff Model

`diff` is local by default in `0.1.x`.

It compares:

- the current canonical local workflow file
- the cached base snapshot from `.n8n/cache`

If a cache snapshot is unavailable, `diff` falls back to hash and state reporting only and tells the user to re-pull the workflow to seed local diff data.

The human output includes:

- status summary
- file path
- workflow ID
- local, recorded, and base hashes when available
- changed top-level sections
- unified patch when a base snapshot exists and content changed

The JSON output includes:

- the local status object
- `base_hash`
- `base_snapshot_available`
- `changed_sections`
- optional `patch`

`diff --refresh` keeps the base snapshot comparison and also fetches the current remote workflow by ID.

Additional human output in refresh mode:

- remote sync state
- remote hash
- remote update timestamp when present
- changed top-level sections between the current remote workflow and the local file
- unified `remote` vs `local` patch when both sides are available and differ

If remote refresh fails, the command still returns the local base snapshot diff, leaves the remote comparison unavailable, and records the reason in `status.remote_detail`.

Additional JSON fields in refresh mode:

- `status.sync_state`
- `status.remote_hash`
- `status.remote_updated_at`
- `status.remote_detail`
- `remote_comparison_available`
- `remote_changed_sections`
- optional `remote_patch`

## 11. Local Authoring

The local authoring surface in `0.1.x` is intentionally narrow and file-based.

Current commands:

- `workflow new <name> [--path <path>] [--id <id>] [--active]`
- `workflow create <file> --instance <alias> [--activate]`
- `workflow show <file> [--instance <alias>]`
- `node ls <file>`
- `node add <file> --name <name> --type <node_type> [--type-version <number>] [--x <int>] [--y <int>] [--disabled]`
- `node set <file> <node> <path> [value] [--json-value|--number|--bool|--null]`
- `node rename <file> <current_name> <new_name>`
- `node rm <file> <node>`
- `expr set <file> <node> <path> <expression>`
- `credential set <file> <node> --type <credential_type> --id <credential_id> [--name <credential_name>]`
- `conn add <file> --from <node> --to <node> [--kind <type>] [--target-kind <type>] [--output-index <n>] [--input-index <n>]`
- `conn rm <file> --from <node> --to <node> [--kind <type>] [--target-kind <type>] [--output-index <n>] [--input-index <n>]`

Behavior:

- all edit commands operate on local workflow files only
- edit commands rewrite the file in canonical JSON form after each successful mutation
- tracked sidecars are left untouched, so tracked files become locally `modified` until they are pushed
- edit commands also run the sensitive-data scanner after write and include `warning_count` in JSON output
- `workflow show` summarizes local nodes, edges, and webhook URLs, using the explicit `--instance` or the tracked sidecar instance when available
- `workflow create` requires a repo because it writes the new tracked file and sidecar into the configured workflow directory
- `workflow create` refuses files that already have a sidecar and expects you to use `push` for tracked workflows
- `workflow create` removes local `id` and `active` before the create request, ensures execution-saving `settings` defaults exist, normalizes webhook nodes for remote creation, and stores the server response as the new source of truth

Webhook-specific behavior:

- `node add --type n8n-nodes-base.webhook` defaults `typeVersion` to `2`
- webhook nodes get an auto-derived `webhookId`
- setting `path` normalizes leading and trailing slashes
- when `webhookId` still uses the auto-derived value, changing `path` updates `webhookId` too
- `workflow create`, `workflow show`, and `activate` return resolved production and test webhook URLs when a base URL is available

Path rules for `node set` and `expr set`:

- `url` means `parameters.url`
- `options.timeout` means `parameters.options.timeout`
- explicit top-level node fields such as `position`, `disabled`, `typeVersion`, `notes`, `alwaysOutputData`, and retry-related fields are supported directly
- `id`, `name`, `type`, and `credentials` are intentionally blocked from `node set`

Expression rules:

- if the input already looks like `={{ ... }}`, it is preserved
- if the input looks like `{{ ... }}`, the CLI prefixes `=`
- otherwise the CLI wraps the value as `={{...}}`

Connection rules:

- source and target node names must already exist
- duplicate connection edges are deduplicated
- the default source output type is `main`
- the default target input type is the same as `--kind`
- `node rename` rewrites the node name, outbound connection key, and inbound edge targets
- `node rm` removes the node, its outbound key, and inbound edges pointing at it
- `conn rm` removes matching edges without disturbing other edges in the same branch

## 12. Validation

`validate` currently checks:

- file parses as JSON
- workflow payload is an object
- `id` exists
- `nodes` is an array
- `connections` is an object
- node names are unique
- connection targets point to existing node names
- if a sidecar exists, `workflow_id` matches the workflow file

`validate` also emits non-fatal warnings for likely sensitive literals in tracked workflow files, including:

- inline private key material
- URLs with embedded basic-auth credentials
- token-like literal prefixes such as `sk-`, `ghp_`, `github_pat_`, `xoxb-`, `xoxp-`, and `Bearer ...`
- literal values stored under field names like `password`, `token`, `secret`, `clientSecret`, or `apiKey`

The scanner intentionally ignores obvious placeholders and common n8n dynamic references such as `={{ ... }}` and `$env.*`.

Warnings do not fail `validate`, but they are returned in human output, JSON output, and the post-write summaries from `pull` and successful `push`.

## 13. Execution Inspection

`runs ls` returns recent executions from the remote instance.

Current supported options:

- `--limit`
- `--workflow <id-or-exact-name>`
- `--status <value>`
- `--since <RFC3339>`
- `--last <window>`

Time filtering is client-side in `0.1.x`. The CLI pages through recent executions until it collects the requested number of matching rows or exhausts the remote result set.

Because execution listings are treated as recent-first, the CLI stops paging once a page has crossed below the active `--since` cutoff.

`--since` includes executions at or after the given timestamp.

`--last` computes a rolling window from the current time and accepts these suffixes:

- `s` seconds
- `m` minutes
- `h` hours
- `d` days

List rows currently include:

- execution ID
- workflow ID
- workflow name when it can be resolved
- status
- mode
- started and stopped timestamps
- computed duration in milliseconds when both timestamps exist

If `runs ls --workflow ...` returns zero rows for an active workflow whose settings do not explicitly save successful production executions, the CLI includes a note explaining that successful runs may not appear in history.

`runs get <execution-id>` returns the execution summary.

`runs get <execution-id> --details` fetches the detailed execution payload and, in human output, summarizes:

- workflow name and ID
- status and mode
- start and stop timestamps
- computed duration
- node-level execution status
- node execution time
- output item counts per node based on `data.resultData.runData`

`runs watch` polls the execution list repeatedly and is intended for active debugging sessions.

Current supported options:

- `--workflow <id-or-exact-name>`
- `--status <value>`
- `--since <RFC3339>`
- `--last <window>`
- `--limit`
- `--interval <seconds>`
- `--iterations <count>`

Human output behavior:

- first poll prints the current execution window
- prints the active workflow, status, and time-window filters when present
- later polls print only newly seen executions
- no output is emitted for unchanged polls after the initial snapshot

JSON output behavior:

- emits one compact JSON envelope per poll
- first poll uses `event = "snapshot"`
- later polls use `event = "update"` when new executions appear
- later polls use `event = "heartbeat"` when no new execution IDs appear

Each JSON watch event currently includes:

- `poll`
- `interval_seconds`
- `count`
- `new_count`
- `executions`
- `new_executions`

The current diagnostics model is intentionally simple:

- `severity`
- `code`
- `message`
- `file`
- optional JSON path
- optional suggestion

## 14. Triggering

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

If the request body looks like JSON and no `Content-Type` header was provided explicitly, `trigger` sends `Content-Type: application/json`.

Webhook-specific error handling:

- non-2xx responses include the resolved request path and a summarized response body in the error message
- `404` responses for `/webhook-test/...` explain that test listeners must be active in the n8n editor
- `404` responses for `/webhook/...` explain that the path may be wrong, the workflow may be inactive, or n8n may not have registered the webhook yet

This avoids pretending there is a stable public “run workflow by ID” endpoint when that has not been verified in the implementation.

## 15. JSON Contract

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

Validation failures and `doctor` failures may also include a `data` object with diagnostics or the full doctor report.

`validate` success and failure payloads include both `error_count` and `warning_count`. `pull` and successful `push` also include `warning_count`, plus `diagnostics` when warnings are present.

Local edit command success payloads include:

- `workflow_path`
- `changed`
- `warning_count`
- command-specific fields such as `workflow_id`, `node`, `path`, `from`, `to`, or `credential_type`

`workflow create` success payloads also include:

- `instance`
- `source_path`
- `source_removed`
- `meta_path`
- optional `active`
- optional `webhooks`

## 16. Exit Codes

- `0`: success
- `2`: usage error
- `3`: config error
- `4`: auth error
- `5`: network error
- `6`: API error
- `10`: validation error
- `11`: not found
- `12`: conflict refusal
- `13`: doctor failures

## 17. Known Limits

- The tool is currently strongest when a repo mirrors one n8n instance.
- `workflow create` depends on the public workflow-create endpoint and still assumes the returned payload can be stored with the same canonicalization rules as pulled workflows.
- `tags` are preserved structurally, not normalized semantically.
- `ls` assumes a paginated workflow list response with `data` and optional `nextCursor`.
- remote drift and API health remain opt-in via `status --refresh`, `diff --refresh`, and `doctor`.
- `doctor` uses a cheap workflow-list probe and does not verify every endpoint.
- `diff` is best after a fresh `pull`, because older repos may not have cached base snapshots yet.
- sensitive-data scanning is heuristic. It is tuned to catch likely mistakes, not to prove a workflow is secret-free.
- workflow deletion is still outside the CLI even though the public delete endpoint was used in manual verification.

## 18. Next Likely Steps

The next improvements that fit the current design are:

1. shell completions and packaging
2. workflow deletion through the public delete endpoint
3. more contract snapshot coverage for agent-facing JSON
4. richer workflow inspection or graph rendering in human output
5. only after that: a real environment-promotion model with explicit mappings and lock files
