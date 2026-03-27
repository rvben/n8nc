# n8n-cli

`n8n-cli` is a Rust CLI for working with n8n workflows from the terminal.

The binary is `n8nc`.

## What It Is

`n8nc` is two things:

- a same-instance Git sync tool for workflows you want to track locally
- a local authoring CLI for draft workflows and structured node edits
- a development CLI for common remote interactions like listing workflows, fetching one, activating it, calling webhook trigger URLs, and delegating non-webhook execution through a configured backend

This is intentionally narrower than a full deployment platform.

## Current Scope

Implemented commands:

- `init`
- `doctor`
- `auth add`
- `auth test`
- `auth session add`
- `auth session test`
- `auth session remove`
- `auth list`
- `auth remove`
- `ls`
- `get`
- `runs ls`
- `runs get`
- `runs watch`
- `pull`
- `push`
- `workflow new`
- `workflow create`
- `workflow execute`
- `workflow show`
- `workflow rm`
- `node ls`
- `node add`
- `node set`
- `node rename`
- `node rm`
- `conn add`
- `conn rm`
- `expr set`
- `credential ls`
- `credential schema`
- `credential set`
- `status`
- `diff`
- `activate`
- `deactivate`
- `trigger`
- `fmt`
- `validate`

Not implemented yet:

- environment promotion across `dev/staging/prod`
- built-in execution of non-webhook workflows through a stable public n8n API

For triggering during development, use `trigger` against a webhook or test webhook URL. For schedule-triggered or other non-webhook workflows, use `workflow execute` with a configured external backend. Webhook `404`s now include the resolved path, response body, and a suggestion that distinguishes production `/webhook/...` URLs from `/webhook-test/...` URLs.

## Quickstart

Initialize a repo:

```bash
n8nc init --instance prod --url https://your-instance.app.n8n.cloud
```

Store an API token:

```bash
n8nc auth add prod --token <api_key>
```

Optional: store browser-session auth for the internal REST credential fallback:

```bash
n8nc auth session add prod --cookie 'n8n-auth=...' --browser-id '<browser-id>'
n8nc auth session test prod
```

Check repo, auth, and API health:

```bash
n8nc doctor --instance prod
```

List workflows:

```bash
n8nc ls --instance prod
```

Pull one into `workflows/`:

```bash
n8nc pull <workflow-id-or-exact-name> --instance prod
```

Inspect recent executions:

```bash
n8nc runs ls --instance prod --limit 10
```

Filter execution listings to a time window:

```bash
n8nc runs ls --instance prod --workflow "Homelab: Container Restart Alert" --last 30m
```

Inspect one execution with node-level details:

```bash
n8nc runs get <execution-id> --instance prod --details
```

In `--json` mode, `runs get --details` now returns the raw execution payload plus stable `run_data` and `node_executions` fields so agents do not need to parse `data.resultData.runData` manually.

Watch recent executions as they appear:

```bash
n8nc runs watch --instance prod --workflow "Homelab: Container Restart Alert"
```

Watch only executions started after a fixed point in time:

```bash
n8nc runs watch --instance prod --since 2026-03-26T12:00:00Z
```

Use finite polling for scripts or tests:

```bash
n8nc runs watch --instance prod --limit 5 --interval 1 --iterations 2 --json
```

Validate tracked workflows:

```bash
n8nc validate
```

`validate` fails on structural errors and emits non-fatal warnings for likely sensitive literals in tracked workflow files.

Create a local workflow draft:

```bash
n8nc workflow new "Order Alert"
```

New drafts default to execution-saving settings that make fresh workflows visible in `runs ls` on instances where n8n does not save successful executions by default.

Publish that draft to n8n and start tracking it:

```bash
n8nc workflow create workflows/order-alert--wf-draft.workflow.json --instance prod
```

Publish and activate a webhook workflow and get the resolved webhook URL back:

```bash
n8nc workflow create workflows/order-alert--wf-draft.workflow.json --instance prod --activate
```

Configure a non-webhook execution backend in `n8n.toml`:

```toml
[instances.prod.execute]
backend = "command"
program = "uvx"
args = ["your-mcp-runner", "execute_workflow", "{workflow_id}", "{instance_alias}"]
stdin_json = true
```

Execute a schedule-triggered or other non-webhook workflow through that backend:

```bash
n8nc workflow execute "Nightly Digest" --instance prod --input '{"dryRun":true}'
```

Inspect a local workflow summary, graph edges, and webhook URLs:

```bash
n8nc workflow show workflows/order-alert--abc123.workflow.json
```

Remove a remote workflow and clean up matching local tracked artifacts:

```bash
n8nc workflow rm abc123
```

List nodes in a local workflow file:

```bash
n8nc node ls workflows/order-alert--wf-draft.workflow.json
```

Add a node to a local workflow file:

```bash
n8nc node add workflows/order-alert--wf-draft.workflow.json --name "HTTP Request" --type n8n-nodes-base.httpRequest --type-version 4.2 --x 300 --y 160
```

Set a node parameter path:

```bash
n8nc node set workflows/order-alert--wf-draft.workflow.json "HTTP Request" url https://example.com
```

Rename or remove a node safely:

```bash
n8nc node rename workflows/order-alert--wf-draft.workflow.json "HTTP Request" "Fetch Orders"
n8nc node rm workflows/order-alert--wf-draft.workflow.json "Fetch Orders"
```

Set an expression on a node path:

```bash
n8nc expr set workflows/order-alert--wf-draft.workflow.json "HTTP Request" authentication '{{$json.authMode}}'
```

Set a credential reference on a node:

```bash
n8nc credential set workflows/order-alert--wf-draft.workflow.json "HTTP Request" --type httpBasicAuth --id cred-123 --name "Primary Basic Auth"
```

Discover credentials on the remote instance, letting `n8nc` choose the best available inventory source:

```bash
n8nc credential ls --instance prod
```

Force the safest partial mode when you only care about workflow usage:

```bash
n8nc credential ls --instance prod --source workflow-refs
```

If your instance does not expose `GET /api/v1/credentials`, you can opt into the internal browser-session fallback:

```bash
n8nc auth session add prod --cookie 'n8n-auth=...' --browser-id '<browser-id>'
n8nc credential ls --instance prod --source rest-session
```

You can still use environment variables instead. They override keychain-stored session auth:

```bash
export N8NC_SESSION_COOKIE_PROD='n8n-auth=...'
export N8NC_BROWSER_ID_PROD='...'
```

Inspect the official schema for a credential type:

```bash
n8nc credential schema --instance prod httpBasicAuth
```

`credential set` still intentionally uses an existing credential ID, but the CLI now helps in two ways:

- `credential ls` uses `auto` mode by default:
  - public API inventory if available
  - internal REST inventory only if browser-session auth is configured, either through `auth session add` or both `N8NC_SESSION_COOKIE_<ALIAS>` and `N8NC_BROWSER_ID_<ALIAS>`
  - workflow-reference fallback otherwise
- `credential schema` shows the official schema for a credential type

`credential ls --workflow <id-or-name>` is intentionally workflow-reference scoped, so it only works with `--source auto` or `--source workflow-refs`.

Connect two nodes:

```bash
n8nc conn add workflows/order-alert--wf-draft.workflow.json --from "Start" --to "HTTP Request"
```

Remove one edge from a branch:

```bash
n8nc conn rm workflows/order-alert--wf-draft.workflow.json --from "Start" --to "HTTP Request" --output-index 0 --input-index 0
```

See which tracked files changed locally:

```bash
n8nc status
```

Refresh tracked workflows against the current remote state:

```bash
n8nc status --refresh
```

Inspect one tracked workflow against its cached base snapshot:

```bash
n8nc diff workflows/order-alert--abc123.workflow.json
```

Compare one tracked workflow against the current remote workflow too:

```bash
n8nc diff workflows/order-alert--abc123.workflow.json --refresh
```

Push a tracked workflow back:

```bash
n8nc push workflows/order-alert--abc123.workflow.json
```

Trigger a webhook during development:

```bash
n8nc trigger /webhook-test/order-alert --instance prod --method POST --data '{"hello":"world"}'
```

Execute a non-webhook workflow through the configured backend:

```bash
n8nc workflow execute "Nightly Digest" --instance prod --input '{"dryRun":true}'
```

## Repo Model

`n8nc init` creates:

```text
.
├── n8n.toml
├── workflows/
└── .n8n/
    └── cache/
```

Optional instance config can also define a non-webhook execution backend:

```toml
[instances.prod.execute]
backend = "command"
program = "uvx"
args = ["your-mcp-runner", "execute_workflow", "{workflow_id}", "{instance_alias}"]
stdin_json = true
cwd = "."
```

Tracked workflow files use:

```text
workflows/<workflow-slug>--<workflow-id>.workflow.json
workflows/<workflow-slug>--<workflow-id>.meta.json
```

`workflow new` creates a local `.workflow.json` draft without a sidecar. `workflow create` turns a sidecar-free local file into a tracked workflow by creating it remotely, re-fetching the created workflow from n8n as the canonical source of truth, writing the tracked file plus sidecar, and removing the original in-repo draft when the tracked path changes.

`workflow show` now falls back to the repo default instance when a local draft has no sidecar yet, so draft webhook files still render full production and test URLs.

`workflow rm` fills the missing cleanup step:

- `n8nc workflow rm <workflow-id-or-name>` deletes the remote workflow and removes matching tracked local artifacts when they exist
- `n8nc workflow rm <file>` removes a local draft file directly
- `--local-only` removes local artifacts without touching remote
- `--keep-local` deletes remotely but keeps local files and metadata

Drafts and create payloads now also fill in execution-saving workflow settings when they are missing:

- `executionOrder = "v1"`
- `saveDataSuccessExecution = "all"`
- `saveDataErrorExecution = "all"`
- `saveManualExecutions = true`
- `saveExecutionProgress = true`

Webhook nodes get safer defaults in the local authoring flow:

- `node add --type n8n-nodes-base.webhook` defaults to `typeVersion = 2`
- webhook nodes automatically get a `webhookId`
- setting `path` normalizes leading and trailing slashes and keeps `webhookId` in sync while it still uses the auto-derived value
- `workflow create`, `workflow show`, and `activate` surface resolved webhook URLs
- `activate` and `deactivate` now wait until n8n reports the requested state and refresh matching tracked local artifacts when they exist

The sidecar stores:

- the instance alias
- the workflow ID
- the canonicalization version
- the hash algorithm
- the remote hash recorded at pull time

The cache stores the last pulled canonical workflow snapshot for local `diff`.

`workflow execute` expands placeholders in the configured `args`:

- `{workflow_id}`
- `{workflow_name}`
- `{instance_alias}`
- `{base_url}`

It also exports execution context to the backend process:

- `N8NC_EXECUTE_INSTANCE_ALIAS`
- `N8NC_EXECUTE_BASE_URL`
- `N8NC_EXECUTE_WORKFLOW_ID`
- `N8NC_EXECUTE_WORKFLOW_NAME`
- `N8NC_EXECUTE_WORKFLOW_ACTIVE`
- `N8NC_EXECUTE_INPUT_JSON`

If `stdin_json = true`, stdin receives a JSON request envelope with the workflow identity, instance details, and optional input payload. This is the intended integration point for MCP-style adapters and other local runners.

`status` is local by default in `0.1.x`: it reports whether tracked files are `clean`, `modified`, `untracked`, `invalid`, or `orphaned_meta`.

`status --refresh` adds live remote sync states for tracked workflows:

- `clean`
- `modified`
- `drifted`
- `conflict`
- `missing_remote`

If remote refresh fails for a tracked workflow, `status --refresh` still returns the local row, leaves the sync state unavailable, and records the remote error in `remote_detail`. `untracked`, `invalid`, and `orphaned_meta` entries remain visible but do not count toward `sync_summary`.

`diff` is local by default. `diff --refresh` adds a second comparison against the current remote workflow and shows a remote/local patch when the workflow is still remotely available. If the remote lookup fails, the command still returns the local diff and marks the remote comparison as unavailable.

`push` uses the sidecar metadata as a lease check and refuses to overwrite a workflow that changed remotely since the last `pull`.

`push` only sends the API-supported mutable workflow fields: `name`, `nodes`, `connections`, and `settings`. If local edits also changed unsupported top-level fields such as `active`, the command fails explicitly instead of silently dropping those changes.

`pull`, successful `push`, and `validate` all scan tracked workflow files for likely sensitive literals such as inline tokens, private keys, and URLs with embedded credentials.

When `runs ls --workflow ...` returns no rows for an active workflow whose settings do not explicitly save successful production executions, the CLI includes a note explaining that successful runs may be omitted from execution history.

## Design Notes

- Same-instance first: tracked files are bound to the instance they were pulled from.
- Agent-safe: every command supports `--json`.
- Deterministic: workflows are canonicalized before storage and hashing.
- Local authoring first: `workflow new`, `workflow show`, `workflow rm`, `node ls`, `node add`, `node set`, `node rename`, `node rm`, `expr set`, `credential set`, `conn add`, and `conn rm` edit local workflow files directly.
- Better credential discovery: `credential ls` now probes inventory capabilities at runtime, prefers the public API when available, supports an explicit internal REST fallback via `auth session add` or the matching `N8NC_SESSION_COOKIE_<ALIAS>` / `N8NC_BROWSER_ID_<ALIAS>` env vars, and still falls back safely to workflow references when full inventory is unavailable. `credential schema`, `workflow show`, and `node ls` surface the rest of the credential context.
- Better session auth UX: `auth session add/test/remove` stores the browser session cookie and browser ID alongside token auth, and `auth list` now reports both token and session readiness.
- Honest execution split: `trigger` is only for webhook HTTP calls; `workflow execute` is the separate path for non-webhook workflows and requires an explicit external backend.
- Adapter-friendly execution: `workflow execute` can pass workflow context through placeholders, environment variables, and optional stdin JSON so MCP-style runners can integrate without pretending there is a public run-by-ID API.
- Draft-to-tracked flow: `workflow create` publishes a local draft through the official workflow-create API and converts it into a tracked file plus sidecar.
- Server-truth tracking: `workflow create` and successful `push` both re-fetch the remote workflow before storing it locally, so lease hashes are based on the same shape that later reads use.
- Full cleanup path: `workflow rm` removes the remote workflow and cleans tracked repo artifacts instead of forcing raw API calls.
- Better webhook ergonomics: webhook nodes are normalized for remote creation, publish and activate return the resolved URLs, and webhook trigger failures explain likely `404` causes.
- Truthful lifecycle commands: `activate` and `deactivate` only report success after the remote workflow state converges and tracked local files are refreshed.
- Better execution ergonomics: new drafts default to saved successful executions, and `runs ls` explains one common “why is history empty?” workflow-settings pitfall.
- Explicit refresh: remote drift is only reported when you ask for it with `--refresh`.
- Sensitive-data aware: `validate` emits warnings, not hard failures, for likely secret literals in tracked workflow files.
- Fast setup check: `doctor` validates repo layout, token availability, live API reachability, credential-inventory coverage, execute-backend readiness, and scans tracked workflow files for likely sensitive literals.
- Dev-loop friendly: `runs ls`, `runs get --details`, and `runs watch` cover recent execution inspection without leaving the terminal.
- Time-window aware: `runs ls` and `runs watch` support `--since <RFC3339>` and `--last <window>` with `s`, `m`, `h`, and `d` units.
- Honest triggering: `trigger` is an HTTP call helper for webhook URLs, not a guessed “execute workflow” API wrapper.
- Explicit non-webhook execution: `workflow execute` delegates to a configured local backend instead of claiming the n8n public API can run schedule-triggered workflows directly.
- Smarter trigger bodies: when `--data` or `--data-file` contains JSON and you did not set `Content-Type` yourself, `trigger` sends `application/json`.

`node set` and `expr set` default unknown paths to `parameters.*`, so `url` means `parameters.url` and `options.timeout` means `parameters.options.timeout`.

`runs watch --json` emits one compact JSON envelope per poll. The first event is `snapshot`; later events are `update` when new executions appear or `heartbeat` when the latest window is unchanged.

## Spec

Implementation notes and roadmap live in [docs/cli-spec.md](/Users/ruben/Projects/cli-tools/n8n-cli/docs/cli-spec.md).
