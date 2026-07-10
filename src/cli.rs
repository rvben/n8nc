use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

/// Output format selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Detect automatically: JSON when stdout is not a TTY, text otherwise
    Auto,
    /// Always emit human-readable text
    Text,
    /// Always emit JSON
    Json,
}

#[derive(Debug, Parser)]
#[command(
    name = "n8nc",
    version,
    about = "Human- and agent-friendly CLI for n8n workflows",
    after_help = "Run `n8nc schema` to get a machine-readable description of all commands."
)]
pub struct Cli {
    /// Output format: auto (default), json, or text. Use `schema` for full introspection.
    #[arg(long, short = 'o', global = true, value_enum, default_value = "auto")]
    pub output: OutputFormat,

    /// Output as JSON (hidden alias for --output json)
    #[arg(long, global = true, hide = true, conflicts_with = "output")]
    pub json: bool,

    /// Suppress non-data output (summary lines, confirmations)
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Override the repository root directory
    #[arg(long, global = true)]
    pub repo_root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Resolve the effective JSON mode: --output json or --json or auto-detect from TTY.
    pub fn json_output(&self) -> bool {
        use std::io::IsTerminal;
        if self.json {
            return true;
        }
        match self.output {
            OutputFormat::Json => true,
            OutputFormat::Text => false,
            OutputFormat::Auto => !std::io::stdout().is_terminal(),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize a repository for workflow sync
    Init(InitArgs),
    /// Check repo, auth, and API connectivity
    Doctor(DoctorArgs),
    /// Manage credentials for configured instances
    Auth(AuthArgs),
    /// List workflows from a remote instance
    #[command(alias = "list")]
    Ls(ListArgs),
    /// Get a workflow and print canonical JSON
    Get(GetArgs),
    /// Inspect recent workflow executions
    Runs(RunsArgs),
    /// Pull a workflow into the local repository
    Pull(PullArgs),
    /// Push a tracked workflow back to n8n
    Push(PushArgs),
    /// Manage workflows: new, create, execute, show, rm
    Workflow(WorkflowArgs),
    /// Add or edit nodes in a local workflow file
    Node(NodeArgs),
    /// Add a connection between nodes in a local workflow file
    #[command(alias = "connection")]
    Conn(ConnArgs),
    /// Set an expression value on a node path
    Expr(ExprArgs),
    /// Set a credential reference on a node
    #[command(alias = "cred")]
    Credential(CredentialArgs),
    /// Move an inline node secret into an n8n credential
    Secret(SecretArgs),
    /// Show local workflow sync state
    Status(StatusArgs),
    /// Show local changes for one tracked workflow
    Diff(DiffArgs),
    /// Activate a workflow
    Activate(IdArgs),
    /// Deactivate a workflow
    Deactivate(IdArgs),
    /// Archive a workflow (requires session auth)
    Archive(IdArgs),
    /// Unarchive a workflow (requires session auth)
    Unarchive(IdArgs),
    /// Call a webhook URL directly
    Trigger(TriggerArgs),
    /// Format workflow and sidecar files
    Fmt(FmtArgs),
    /// Validate local workflow files
    Validate(ValidateArgs),
    /// Lint workflow files against configurable rules
    Lint(LintArgs),
    /// Search local workflow files for patterns
    Search(SearchArgs),
    /// Generate shell completions
    Completions(CompletionsArgs),
    /// Dump CLI schema as JSON for agent introspection
    Schema,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    pub shell: Shell,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Instance alias (prompted interactively when omitted)
    #[arg(long)]
    pub instance: Option<String>,
    /// n8n base URL (prompted interactively when omitted)
    #[arg(long)]
    pub url: Option<String>,
    /// API key to store (prompted interactively when omitted)
    #[arg(long)]
    pub token: Option<String>,
    #[arg(long, default_value = "workflows")]
    pub workflow_dir: PathBuf,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    /// Skip live API checks and only inspect local config and auth state
    #[arg(long)]
    pub skip_network: bool,
}

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Store an API token for an alias
    Add(AuthAddArgs),
    /// Verify that an alias is configured and reachable
    Test(AuthAliasArgs),
    /// Store, verify, or remove browser-session auth for internal REST fallbacks
    Session(AuthSessionArgs),
    /// Show configured aliases and token availability
    List(AuthListArgs),
    /// Remove a stored token
    Remove(AuthAliasArgs),
}

#[derive(Debug, Args)]
pub struct AuthListArgs {
    /// Maximum number of instances to return
    #[arg(long, default_value_t = 100)]
    pub limit: u16,
    /// Number of instances to skip (for pagination)
    #[arg(long, default_value_t = 0)]
    pub offset: u32,
    /// Comma-separated list of fields to include in output
    #[arg(long, value_name = "FIELDS")]
    pub fields: Option<String>,
}

#[derive(Debug, Args)]
pub struct AuthSessionArgs {
    #[command(subcommand)]
    pub command: AuthSessionCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthSessionCommand {
    /// Store a browser session cookie and browser ID for an alias
    Add(AuthSessionAddArgs),
    /// Verify that the internal REST session fallback is configured and reachable
    Test(AuthAliasArgs),
    /// Remove stored browser-session auth
    Remove(AuthAliasArgs),
}

#[derive(Debug, Args)]
pub struct AuthAliasArgs {
    pub alias: String,
}

#[derive(Debug, Args)]
pub struct AuthAddArgs {
    pub alias: String,
    #[arg(long, conflicts_with = "stdin")]
    pub token: Option<String>,
    #[arg(long, conflicts_with = "token")]
    pub stdin: bool,
}

#[derive(Debug, Args)]
pub struct AuthSessionAddArgs {
    pub alias: String,
    #[arg(long, value_name = "COOKIE", conflicts_with = "cookie_stdin")]
    pub cookie: Option<String>,
    #[arg(long, conflicts_with = "cookie")]
    pub cookie_stdin: bool,
    #[arg(long = "browser-id", value_name = "BROWSER_ID")]
    pub browser_id: String,
}

#[derive(Debug, Args, Clone)]
pub struct RemoteArgs {
    #[arg(long)]
    pub instance: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    #[arg(long)]
    pub active: bool,
    #[arg(long, conflicts_with = "active")]
    pub inactive: bool,
    #[arg(long)]
    pub name: Option<String>,
    /// Maximum number of workflows to return
    #[arg(long, default_value_t = 100)]
    pub limit: u16,
    /// Number of workflows to skip (for pagination)
    #[arg(long, default_value_t = 0)]
    pub offset: u32,
    /// Comma-separated list of fields to include in output
    #[arg(long, value_name = "FIELDS")]
    pub fields: Option<String>,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub identifier: String,
    /// Print one node's definition, selected by its display name
    #[arg(long, value_name = "NAME", conflicts_with_all = ["nodes", "connections"])]
    pub node: Option<String>,
    /// Print a summary row per node: name, type, typeVersion, disabled
    #[arg(long, conflicts_with = "connections")]
    pub nodes: bool,
    /// Print the workflow's connections, showing what each branch feeds
    #[arg(long)]
    pub connections: bool,
}

#[derive(Debug, Args)]
pub struct RunsArgs {
    #[command(subcommand)]
    pub command: RunsCommand,
}

#[derive(Debug, Args, Clone)]
pub struct RunsTimeArgs {
    /// Only include executions at or after this RFC3339 timestamp
    #[arg(long, value_name = "RFC3339", conflicts_with = "last")]
    pub since: Option<String>,
    /// Only include executions from the last window, for example `15m`, `2h`, or `1d`
    #[arg(long, value_name = "WINDOW", conflicts_with = "since")]
    pub last: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum RunsCommand {
    /// List recent executions
    #[command(alias = "list")]
    Ls(RunsListArgs),
    /// Get one execution by ID
    Get(RunsGetArgs),
    /// Watch recent executions for changes
    Watch(RunsWatchArgs),
    /// Show execution statistics
    Stats(RunsStatsArgs),
}

#[derive(Debug, Args)]
pub struct RunsListArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    #[command(flatten)]
    pub time: RunsTimeArgs,
    /// Filter by workflow ID or exact workflow name
    #[arg(long, value_name = "ID_OR_NAME")]
    pub workflow: Option<String>,
    /// Filter by execution status, for example `success`, `error`, or `waiting`
    #[arg(long)]
    pub status: Option<String>,
    /// Maximum number of executions to return
    #[arg(long, default_value_t = 20)]
    pub limit: u16,
    /// Number of executions to skip (for pagination)
    #[arg(long, default_value_t = 0)]
    pub offset: u32,
    /// Comma-separated list of fields to include in output
    #[arg(long, value_name = "FIELDS")]
    pub fields: Option<String>,
    /// Add the failing node and error message to each row. Costs one extra API
    /// request per returned execution, so pair it with `--status error`.
    #[arg(long)]
    pub explain: bool,
}

#[derive(Debug, Args)]
pub struct RunsGetArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub execution_id: String,
    /// Include the full run payload and workflow metadata. This can reach
    /// megabytes on a large workflow; prefer `--summary` or `--node`.
    #[arg(long, conflicts_with_all = ["summary", "node"])]
    pub details: bool,
    /// Per-node status, duration, and output counts, without the run payload
    #[arg(long, conflicts_with = "node")]
    pub summary: bool,
    /// Output items produced by a single node, selected by its display name
    #[arg(long, value_name = "NAME")]
    pub node: Option<String>,
}

#[derive(Debug, Args)]
pub struct RunsWatchArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    #[command(flatten)]
    pub time: RunsTimeArgs,
    /// Filter by workflow ID or exact workflow name
    #[arg(long, value_name = "ID_OR_NAME")]
    pub workflow: Option<String>,
    /// Filter by execution status, for example `success`, `error`, or `waiting`
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: u16,
    /// Poll interval in seconds
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
    pub interval: u64,
    /// Number of polls to perform before exiting
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub iterations: Option<u32>,
}

#[derive(Debug, Args)]
pub struct RunsStatsArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    #[command(flatten)]
    pub time: RunsTimeArgs,
    /// Workflow ID, name, or file path
    pub workflow: Option<String>,
}

#[derive(Debug, Args)]
pub struct PullArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    /// Workflow ID or exact name (required unless --all is set)
    pub identifier: Option<String>,
    /// Pull all workflows from the remote instance
    #[arg(long)]
    pub all: bool,
    /// Only pull active workflows (requires --all)
    #[arg(long, requires = "all")]
    pub active: bool,
    /// Only pull inactive workflows (requires --all)
    #[arg(long, requires = "all", conflicts_with = "active")]
    pub inactive: bool,
    /// Remove local tracked workflows that no longer exist on the remote (requires --all)
    #[arg(long, requires = "all")]
    pub prune: bool,
}

#[derive(Debug, Args)]
pub struct PushArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    /// Workflow file, id, or slug to push (required unless --all is set)
    #[arg(value_name = "FILE_OR_ID")]
    pub file: Option<PathBuf>,
    /// Push all modified tracked workflows
    #[arg(long)]
    pub all: bool,
    /// After pushing, re-fetch the workflow and report any sections the server
    /// changed from what was sent. Verifies one workflow; not usable with --all.
    #[arg(long)]
    pub verify: bool,
}

#[derive(Debug, Args)]
pub struct WorkflowArgs {
    #[command(subcommand)]
    pub command: WorkflowCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    /// Create a new local workflow draft
    New(WorkflowNewArgs),
    /// Create a remote workflow from a local file and start tracking it
    Create(WorkflowCreateArgs),
    /// Execute a workflow through a configured external backend
    #[command(alias = "run")]
    Execute(WorkflowExecuteArgs),
    /// Show a local workflow summary, graph, and webhook URLs
    Show(WorkflowShowArgs),
    /// Remove a workflow remotely and clean up local artifacts
    Rm(WorkflowRemoveArgs),
}

#[derive(Debug, Args)]
pub struct WorkflowNewArgs {
    /// Local workflow name
    pub name: String,
    /// Output path for the workflow file
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Explicit workflow ID to embed in the local draft
    #[arg(long)]
    pub id: Option<String>,
    /// Create the workflow as active instead of inactive
    #[arg(long)]
    pub active: bool,
}

#[derive(Debug, Args)]
pub struct WorkflowCreateArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub file: PathBuf,
    /// Activate the workflow immediately after creation
    #[arg(long)]
    pub activate: bool,
}

#[derive(Debug, Args)]
pub struct WorkflowExecuteArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    /// Workflow ID or exact workflow name
    pub identifier: String,
    /// Inline JSON or plain-text input passed through to the execution backend
    #[arg(long, conflicts_with_all = ["input_file", "stdin"])]
    pub input: Option<String>,
    /// Read JSON or plain-text input from a file
    #[arg(long, value_name = "PATH", conflicts_with_all = ["input", "stdin"])]
    pub input_file: Option<PathBuf>,
    /// Read JSON or plain-text input from stdin
    #[arg(long, conflicts_with_all = ["input", "input_file"])]
    pub stdin: bool,
}

#[derive(Debug, Args)]
pub struct WorkflowShowArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub file: PathBuf,
    /// Show execution flow as a tree
    #[arg(long)]
    pub tree: bool,
    /// Disable colored output (also respects NO_COLOR env var)
    #[arg(long)]
    pub no_color: bool,
}

#[derive(Debug, Args)]
pub struct WorkflowRemoveArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    /// Workflow file path, workflow ID, or exact workflow name
    pub target: String,
    /// Remove only local artifacts and skip any remote delete
    #[arg(long, conflicts_with = "keep_local")]
    pub local_only: bool,
    /// Delete remotely but keep local workflow files and metadata
    #[arg(long, conflicts_with = "local_only")]
    pub keep_local: bool,
    /// Confirm destructive operation without interactive prompt
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct NodeArgs {
    #[command(subcommand)]
    pub command: NodeCommand,
}

#[derive(Debug, Subcommand)]
pub enum NodeCommand {
    /// List nodes in a local workflow file
    Ls(NodeListArgs),
    /// Add a node to a local workflow file
    Add(NodeAddArgs),
    /// Set a node field or parameter path
    Set(NodeSetArgs),
    /// Set a node field directly on a remote workflow, without tracking it
    #[command(name = "set-remote")]
    SetRemote(NodeSetRemoteArgs),
    /// Rename a node and rewrite connection references
    Rename(NodeRenameArgs),
    /// Remove a node and all of its connections
    Rm(NodeRemoveArgs),
}

#[derive(Debug, Args)]
pub struct NodeListArgs {
    /// Path to the local workflow file to inspect (`*.workflow.json`).
    pub file: PathBuf,
}

#[derive(Debug, Args)]
pub struct NodeAddArgs {
    /// Path to the local workflow file to edit (`*.workflow.json`).
    pub file: PathBuf,
    /// Display name for the new node (must be unique within the workflow).
    #[arg(long)]
    pub name: String,
    /// Node type identifier (e.g. `n8n-nodes-base.httpRequest`).
    #[arg(long = "type")]
    pub node_type: String,
    /// Node type version; defaults to the type's known default when omitted.
    #[arg(long)]
    pub type_version: Option<f64>,
    /// Canvas x position.
    #[arg(long, default_value_t = 0)]
    pub x: i64,
    /// Canvas y position.
    #[arg(long, default_value_t = 0)]
    pub y: i64,
    /// Add the node in a disabled state.
    #[arg(long)]
    pub disabled: bool,
}

#[derive(Debug, Args)]
pub struct NodeSetArgs {
    /// Path to the local workflow file to edit (`*.workflow.json`).
    pub file: PathBuf,
    /// Target node, matched by its display name first, then by its `id`.
    pub node: String,
    /// Field to set: a `parameters` dotted/bracket path (e.g. `options.timeout`,
    /// `headerParameters.parameters[0].value`) or a node-level property such as
    /// `retryOnFail`, `maxTries`, or `waitBetweenTries`.
    pub path: String,
    /// Value to assign; treated as a string unless a typing flag is given.
    #[arg(required_unless_present_any = ["null", "value_file"])]
    pub value: Option<String>,
    /// Read the value from a file instead of argv. Use this for multiline
    /// bodies such as a `code` node's `jsCode`, which argv quoting mangles.
    #[arg(long, value_name = "PATH", conflicts_with = "value")]
    pub value_file: Option<PathBuf>,
    #[command(flatten)]
    pub mode: ValueModeArgs,
}

#[derive(Debug, Args)]
pub struct NodeSetRemoteArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    /// Remote workflow ID or exact workflow name.
    pub workflow: String,
    /// Target node, matched by its display name first, then by its `id`.
    pub node: String,
    /// Field to set, using the same path syntax as `node set`.
    pub path: String,
    /// Value to assign; treated as a string unless a typing flag is given.
    #[arg(required_unless_present_any = ["null", "value_file"])]
    pub value: Option<String>,
    /// Read the value from a file instead of argv, for multiline bodies.
    #[arg(long, value_name = "PATH", conflicts_with = "value")]
    pub value_file: Option<PathBuf>,
    /// Report the change without writing it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub mode: ValueModeArgs,
}

#[derive(Debug, Args)]
pub struct NodeRenameArgs {
    /// Path to the local workflow file to edit (`*.workflow.json`).
    pub file: PathBuf,
    /// Existing node to rename, matched by display name first, then by `id`.
    pub current_name: String,
    /// New display name; outbound and inbound connections are rewritten to match.
    pub new_name: String,
}

#[derive(Debug, Args)]
pub struct NodeRemoveArgs {
    /// Path to the local workflow file to edit (`*.workflow.json`).
    pub file: PathBuf,
    /// Node to remove, matched by display name first, then by `id`.
    pub node: String,
}

#[derive(Debug, Args)]
pub struct ValueModeArgs {
    /// Parse VALUE as raw JSON (object, array, or any JSON literal).
    #[arg(long = "json-value", conflicts_with_all = ["number", "bool_value", "null"])]
    pub json_value: bool,
    /// Parse VALUE as a JSON number; integral inputs stay integers (`3`, not `3.0`).
    #[arg(long, conflicts_with_all = ["json_value", "bool_value", "null"])]
    pub number: bool,
    /// Parse VALUE as a boolean (accepts true/false, 1/0, yes/no).
    #[arg(long = "bool", conflicts_with_all = ["json_value", "number", "null"])]
    pub bool_value: bool,
    /// Set the field to JSON null; no VALUE argument is required.
    #[arg(long, conflicts_with_all = ["json_value", "number", "bool_value"])]
    pub null: bool,
}

#[derive(Debug, Args)]
pub struct ConnArgs {
    #[command(subcommand)]
    pub command: ConnCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConnCommand {
    /// Add a connection between two nodes
    Add(ConnAddArgs),
    /// Remove a connection between two nodes
    Rm(ConnRemoveArgs),
}

#[derive(Debug, Args)]
pub struct ConnAddArgs {
    pub file: PathBuf,
    #[arg(long)]
    pub from: String,
    #[arg(long)]
    pub to: String,
    #[arg(long, default_value = "main")]
    pub kind: String,
    #[arg(long)]
    pub target_kind: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub output_index: usize,
    #[arg(long, default_value_t = 0)]
    pub input_index: usize,
}

#[derive(Debug, Args)]
pub struct ConnRemoveArgs {
    pub file: PathBuf,
    #[arg(long)]
    pub from: String,
    #[arg(long)]
    pub to: String,
    #[arg(long, default_value = "main")]
    pub kind: String,
    #[arg(long)]
    pub target_kind: Option<String>,
    #[arg(long)]
    pub output_index: Option<usize>,
    #[arg(long)]
    pub input_index: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ExprArgs {
    #[command(subcommand)]
    pub command: ExprCommand,
}

#[derive(Debug, Subcommand)]
pub enum ExprCommand {
    /// Set an expression string on a node field or parameter path
    Set(ExprSetArgs),
}

#[derive(Debug, Args)]
pub struct ExprSetArgs {
    pub file: PathBuf,
    pub node: String,
    pub path: String,
    pub expression: String,
}

#[derive(Debug, Args)]
pub struct CredentialArgs {
    #[command(subcommand)]
    pub command: CredentialCommand,
}

#[derive(Debug, Subcommand)]
pub enum CredentialCommand {
    /// List credentials from the best available remote inventory source
    Ls(CredentialListArgs),
    /// Show the official credential schema for a credential type
    Schema(CredentialSchemaArgs),
    /// Set a credential reference on a node using an existing n8n credential ID
    Set(CredentialSetArgs),
}

#[derive(Debug, Args)]
pub struct SecretArgs {
    #[command(subcommand)]
    pub command: SecretCommand,
}

#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    /// Move an inline request-header secret into a new n8n credential and
    /// rewrite the node to use it (run `push` afterwards to apply on the remote)
    Extract(SecretExtractArgs),
}

#[derive(Debug, Args)]
pub struct SecretExtractArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    /// Workflow file, id, or slug containing the node
    #[arg(value_name = "FILE_OR_ID")]
    pub file: PathBuf,
    /// Node holding the inline header, matched by display name first, then id
    pub node: String,
    /// Request header to extract (default: Authorization)
    #[arg(long)]
    pub header: Option<String>,
    /// Credential type to create (default: httpHeaderAuth)
    #[arg(long = "type")]
    pub credential_type: Option<String>,
    /// Display name for the new credential (default: "<node> <header>")
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CredentialSource {
    Auto,
    Public,
    RestSession,
    WorkflowRefs,
}

#[derive(Debug, Args)]
pub struct CredentialListArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    /// Limit discovery to one workflow ID or exact workflow name
    #[arg(long)]
    pub workflow: Option<String>,
    #[arg(long = "type")]
    pub credential_type: Option<String>,
    /// Select how credential inventory is discovered
    #[arg(long, value_enum, default_value_t = CredentialSource::Auto)]
    pub source: CredentialSource,
}

#[derive(Debug, Args)]
pub struct CredentialSchemaArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    #[arg(value_name = "CREDENTIAL_TYPE")]
    pub credential_type: String,
}

#[derive(Debug, Args)]
pub struct CredentialSetArgs {
    pub file: PathBuf,
    pub node: String,
    #[arg(long = "type")]
    pub credential_type: String,
    /// Existing credential ID from n8n; use `n8nc credential ls` to discover referenced IDs
    #[arg(long = "id")]
    pub credential_id: String,
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Refresh tracked workflows against the current remote instance state
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Workflow file, id, or slug to diff
    #[arg(value_name = "FILE_OR_ID")]
    pub file: PathBuf,
    /// Compare the local workflow against the live remote workflow (fetches it
    /// from n8n). Also available as `--remote`.
    #[arg(long, visible_alias = "remote")]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub identifier: String,
}

#[derive(Debug, Args)]
pub struct TriggerArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub target: String,
    #[arg(long, default_value = "POST")]
    pub method: String,
    #[arg(long = "header")]
    pub headers: Vec<String>,
    #[arg(long = "query")]
    pub query: Vec<String>,
    #[arg(long, conflicts_with_all = ["data_file", "stdin"])]
    pub data: Option<String>,
    #[arg(long, value_name = "PATH", conflicts_with_all = ["data", "stdin"])]
    pub data_file: Option<PathBuf>,
    #[arg(long, conflicts_with_all = ["data", "data_file"])]
    pub stdin: bool,
}

#[derive(Debug, Args)]
pub struct FmtArgs {
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct LintArgs {
    /// Workflow files to lint (defaults to all tracked workflows)
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Run only the specified rule
    #[arg(long, value_name = "RULE")]
    pub rule: Option<String>,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Text pattern to search for in workflow JSON
    pub query: Option<String>,
    /// Filter by node type (substring match)
    #[arg(long, value_name = "TYPE")]
    pub node_type: Option<String>,
    /// Filter by credential name or type
    #[arg(long, value_name = "NAME")]
    pub credential: Option<String>,
    /// Filter by expression content inside ={{...}}
    #[arg(long, value_name = "PATTERN")]
    pub expression: Option<String>,
    /// Use case-sensitive matching
    #[arg(long)]
    pub case_sensitive: bool,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;

    #[test]
    fn diff_accepts_remote_as_alias_for_refresh() {
        let cli = Cli::try_parse_from(["n8nc", "diff", "wf", "--remote"])
            .expect("`--remote` should be accepted as an alias for `--refresh`");
        match cli.command {
            Command::Diff(args) => assert!(args.refresh, "`--remote` should set refresh"),
            _ => panic!("expected the diff command"),
        }
    }

    #[test]
    fn push_accepts_verify_flag() {
        let cli = Cli::try_parse_from(["n8nc", "push", "wf", "--verify"])
            .expect("`--verify` should be accepted by push");
        match cli.command {
            Command::Push(args) => assert!(args.verify, "`--verify` should set verify"),
            _ => panic!("expected the push command"),
        }
    }
}
