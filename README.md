# n8n-cli

`n8n-cli` is a Rust CLI for working with n8n workflows from the terminal.

The binary is `n8nc`.

## What It Is

`n8nc` is two things:

- a same-instance Git sync tool for workflows you want to track locally
- a development CLI for common remote interactions like listing workflows, fetching one, activating it, and calling webhook trigger URLs

This is intentionally narrower than a full deployment platform.

## Current Scope

Implemented commands:

- `init`
- `auth add`
- `auth test`
- `auth list`
- `auth remove`
- `ls`
- `get`
- `runs ls`
- `runs get`
- `pull`
- `push`
- `status`
- `diff`
- `activate`
- `deactivate`
- `trigger`
- `fmt`
- `validate`

Not implemented yet:

- environment promotion across `dev/staging/prod`
- workflow creation from local files
- a generic “run workflow by ID” command through the public API

For triggering during development, use `trigger` against a webhook or test webhook URL.

## Quickstart

Initialize a repo:

```bash
n8nc init --instance prod --url https://your-instance.app.n8n.cloud
```

Store an API token:

```bash
n8nc auth add prod --token <api_key>
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

Inspect one execution with node-level details:

```bash
n8nc runs get <execution-id> --instance prod --details
```

Validate tracked workflows:

```bash
n8nc validate
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

Entries that cannot be compared remotely still show their local state and count toward the refresh summary as `unavailable`.

`diff` is local by default. `diff --refresh` adds a second comparison against the current remote workflow and shows a remote/local patch when the workflow is still remotely available.

`push` uses the sidecar metadata as a lease check and refuses to overwrite a workflow that changed remotely since the last `pull`.

## Design Notes

- Same-instance first: tracked files are bound to the instance they were pulled from.
- Agent-safe: every command supports `--json`.
- Deterministic: workflows are canonicalized before storage and hashing.
- Explicit refresh: remote drift is only reported when you ask for it with `--refresh`.
- Dev-loop friendly: `runs ls` and `runs get --details` cover recent execution inspection without leaving the terminal.
- Honest triggering: `trigger` is an HTTP call helper for webhook URLs, not a guessed “execute workflow” API wrapper.

## Spec

Implementation notes and roadmap live in [docs/cli-spec.md](/Users/ruben/Projects/cli-tools/n8n-cli/docs/cli-spec.md).
