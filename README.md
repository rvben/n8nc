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
- `pull`
- `push`
- `activate`
- `deactivate`
- `trigger`
- `fmt`
- `validate`

Not implemented yet:

- environment promotion across `dev/staging/prod`
- `status` and `diff`
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

Validate tracked workflows:

```bash
n8nc validate
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

`push` uses that metadata as a lease check and refuses to overwrite a workflow that changed remotely since the last `pull`.

## Design Notes

- Same-instance first: tracked files are bound to the instance they were pulled from.
- Agent-safe: every command supports `--json`.
- Deterministic: workflows are canonicalized before storage and hashing.
- Honest triggering: `trigger` is an HTTP call helper for webhook URLs, not a guessed “execute workflow” API wrapper.

## Spec

Implementation notes and roadmap live in [docs/cli-spec.md](/Users/ruben/Projects/cli-tools/n8n-cli/docs/cli-spec.md).
