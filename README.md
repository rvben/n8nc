# n8nc

CLI for n8n workflow automation. Sync workflows with Git, author them locally, and manage remote instances from the terminal.

## Quick start

```bash
# Install (pick one)
cargo install n8nc              # From source
pip install n8nc                # Via pip
uvx n8nc --help                 # Run without installing

# Set up a repo
n8nc init --instance prod --url https://your-instance.app.n8n.cloud
n8nc auth add prod --token <api_key>

# Pull and track workflows
n8nc pull --all --instance prod
n8nc status
```

## Installation

### From crates.io

```bash
cargo install n8nc
```

### From PyPI

```bash
pip install n8nc
# or run without installing:
uvx n8nc ls --instance prod
```

### From GitHub releases

Pre-built binaries for Linux (x64, arm64), macOS (x64, arm64), and Windows (x64) on the [releases page](https://github.com/rvben/n8nc/releases).

## What it does

`n8nc` is three things:

- **Git sync** for n8n workflows you want to track locally
- **Local authoring** for draft workflows and structured node edits
- **Development CLI** for listing, pulling, pushing, triggering, and inspecting executions

Every command supports `--json` for agent and script integration.

## Commands

```text
n8nc init                          Set up a workflow repo
n8nc doctor                        Check repo, auth, and API health

n8nc auth add|test|list|remove     Manage API tokens
n8nc auth session add|test|remove  Manage browser-session auth

n8nc ls                            List remote workflows
n8nc get <id-or-name>              Fetch and print a workflow
n8nc pull <id-or-name>             Pull a workflow into the repo
n8nc pull --all [--active]         Pull all workflows
n8nc push <file>                   Push a tracked workflow back
n8nc push --all                    Push all modified tracked workflows
n8nc activate <id-or-name>         Activate a workflow
n8nc deactivate <id-or-name>       Deactivate a workflow

n8nc runs ls                       List recent executions
n8nc runs get <id> [--details]     Inspect one execution
n8nc runs watch                    Watch executions live

n8nc workflow new <name>           Create a local draft
n8nc workflow create <file>        Publish a draft to n8n
n8nc workflow execute <id-or-name> Execute via configured backend
n8nc workflow show <file>          Inspect a local workflow
n8nc workflow rm <target>          Remove a workflow

n8nc node ls|add|set|rename|rm     Edit nodes in local workflows
n8nc conn add|rm                   Edit connections
n8nc expr set                      Set expressions on nodes
n8nc credential ls|schema|set      Discover and assign credentials

n8nc status [--refresh]            Show local sync state
n8nc diff <file> [--refresh]       Show local changes
n8nc trigger <url>                 Call a webhook URL
n8nc fmt [--check]                 Format workflow files
n8nc validate                      Validate workflow files
n8nc completions <shell>           Generate shell completions
```

## Repo model

`n8nc init` creates:

```text
.
├── n8n.toml           # Instance config
├── workflows/         # Tracked and draft workflow files
└── .n8n/cache/        # Base snapshots for local diff
```

Tracked workflows are stored as canonical JSON with a metadata sidecar:

```text
workflows/<slug>--<id>.workflow.json
workflows/<slug>--<id>.meta.json
```

The sidecar binds each file to the instance it came from and records a hash used for safe push (lease check).

## Auth

Tokens are resolved in order:

1. `N8NC_TOKEN_<ALIAS>` env var
2. OS keychain entry from `n8nc auth add`

Optional browser-session auth (for credential inventory fallback):

1. `N8NC_SESSION_COOKIE_<ALIAS>` + `N8NC_BROWSER_ID_<ALIAS>` env vars
2. OS keychain entries from `n8nc auth session add`

## Workflow execution

`trigger` calls webhook URLs directly. For non-webhook workflows, configure an external backend in `n8n.toml`:

```toml
[instances.prod.execute]
backend = "command"
program = "uvx"
args = ["your-runner", "execute_workflow", "{workflow_id}", "{instance_alias}"]
stdin_json = true
```

Then run:

```bash
n8nc workflow execute "Nightly Digest" --instance prod --input '{"dryRun":true}'
```

## Design

- **Same-instance first** -- tracked files are bound to the instance they were pulled from
- **Agent-safe** -- every command supports `--json` with structured envelopes
- **Deterministic** -- workflows are canonicalized before storage and hashing
- **Safe push** -- refuses to overwrite remote changes since the last pull
- **Sensitive-data aware** -- scans for inline secrets on pull, push, and validate
- **Honest execution** -- `trigger` is for webhooks; `workflow execute` delegates to a configured backend

## Spec

Implementation notes and roadmap live in [docs/cli-spec.md](docs/cli-spec.md).

## License

MIT
