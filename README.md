# n8n-cli

`n8n-cli` is a Rust CLI for working with n8n workflows from the terminal.

The binary is `n8nc`.

## What It Is

`n8nc` is two things:

- a same-instance Git sync tool for workflows you want to track locally
- a local authoring CLI for draft workflows and structured node edits
- a development CLI for common remote interactions like listing workflows, fetching one, activating it, and calling webhook trigger URLs

This is intentionally narrower than a full deployment platform.

## Current Scope

Implemented commands:

- `init`
- `doctor`
- `auth add`
- `auth test`
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
- `workflow show`
- `node ls`
- `node add`
- `node set`
- `node rename`
- `node rm`
- `conn add`
- `conn rm`
- `expr set`
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
- a generic “run workflow by ID” command through the public API

For triggering during development, use `trigger` against a webhook or test webhook URL. Webhook `404`s now include the resolved path, response body, and a suggestion that distinguishes production `/webhook/...` URLs from `/webhook-test/...` URLs.

## Quickstart

Initialize a repo:

```bash
n8nc init --instance prod --url https://your-instance.app.n8n.cloud
```

Store an API token:

```bash
n8nc auth add prod --token <api_key>
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

Publish that draft to n8n and start tracking it:

```bash
n8nc workflow create workflows/order-alert--wf-draft.workflow.json --instance prod
```

Publish and activate a webhook workflow and get the resolved webhook URL back:

```bash
n8nc workflow create workflows/order-alert--wf-draft.workflow.json --instance prod --activate
```

Inspect a local workflow summary, graph edges, and webhook URLs:

```bash
n8nc workflow show workflows/order-alert--abc123.workflow.json
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

## Repo Model

`n8nc init` creates:

```text
.
├── n8n.toml
├── workflows/
└── .n8n/
    └── cache/
```

Tracked workflow files use:

```text
workflows/<workflow-slug>--<workflow-id>.workflow.json
workflows/<workflow-slug>--<workflow-id>.meta.json
```

`workflow new` creates a local `.workflow.json` draft without a sidecar. `workflow create` turns a sidecar-free local file into a tracked workflow by creating it remotely, writing the tracked file plus sidecar, and removing the original in-repo draft when the tracked path changes.

Webhook nodes get safer defaults in the local authoring flow:

- `node add --type n8n-nodes-base.webhook` defaults to `typeVersion = 2`
- webhook nodes automatically get a `webhookId`
- setting `path` normalizes leading and trailing slashes and keeps `webhookId` in sync while it still uses the auto-derived value
- `workflow create`, `workflow show`, and `activate` surface resolved webhook URLs

The sidecar stores:

- the instance alias
- the workflow ID
- the canonicalization version
- the hash algorithm
- the remote hash recorded at pull time

The cache stores the last pulled canonical workflow snapshot for local `diff`.

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

`pull`, successful `push`, and `validate` all scan tracked workflow files for likely sensitive literals such as inline tokens, private keys, and URLs with embedded credentials.

## Design Notes

- Same-instance first: tracked files are bound to the instance they were pulled from.
- Agent-safe: every command supports `--json`.
- Deterministic: workflows are canonicalized before storage and hashing.
- Local authoring first: `workflow new`, `workflow show`, `node ls`, `node add`, `node set`, `node rename`, `node rm`, `expr set`, `credential set`, `conn add`, and `conn rm` edit local workflow files directly.
- Draft-to-tracked flow: `workflow create` publishes a local draft through the official workflow-create API and converts it into a tracked file plus sidecar.
- Better webhook ergonomics: webhook nodes are normalized for remote creation, publish and activate return the resolved URLs, and webhook trigger failures explain likely `404` causes.
- Explicit refresh: remote drift is only reported when you ask for it with `--refresh`.
- Sensitive-data aware: `validate` emits warnings, not hard failures, for likely secret literals in tracked workflow files.
- Fast setup check: `doctor` validates repo layout, token availability, live API reachability, and scans tracked workflow files for likely sensitive literals.
- Dev-loop friendly: `runs ls`, `runs get --details`, and `runs watch` cover recent execution inspection without leaving the terminal.
- Time-window aware: `runs ls` and `runs watch` support `--since <RFC3339>` and `--last <window>` with `s`, `m`, `h`, and `d` units.
- Honest triggering: `trigger` is an HTTP call helper for webhook URLs, not a guessed “execute workflow” API wrapper.

`node set` and `expr set` default unknown paths to `parameters.*`, so `url` means `parameters.url` and `options.timeout` means `parameters.options.timeout`.

`runs watch --json` emits one compact JSON envelope per poll. The first event is `snapshot`; later events are `update` when new executions appear or `heartbeat` when the latest window is unchanged.

## Spec

Implementation notes and roadmap live in [docs/cli-spec.md](/Users/ruben/Projects/cli-tools/n8n-cli/docs/cli-spec.md).
