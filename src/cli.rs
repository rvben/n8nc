use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

#[derive(Debug, Parser)]
#[command(
    name = "n8nc",
    version,
    about = "Human- and agent-friendly CLI for n8n workflows"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true)]
    pub repo_root: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
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
    /// Create a new local workflow file
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
    /// Show local workflow sync state
    Status(StatusArgs),
    /// Show local changes for one tracked workflow
    Diff(DiffArgs),
    /// Activate a workflow
    Activate(IdArgs),
    /// Deactivate a workflow
    Deactivate(IdArgs),
    /// Call a webhook URL directly
    Trigger(TriggerArgs),
    /// Format workflow and sidecar files
    Fmt(FmtArgs),
    /// Validate local workflow files
    Validate(ValidateArgs),
    /// Generate shell completions
    Completions(CompletionsArgs),
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    pub shell: Shell,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub instance: String,
    #[arg(long)]
    pub url: String,
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
    List,
    /// Remove a stored token
    Remove(AuthAliasArgs),
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
    #[arg(long, default_value_t = 100)]
    pub limit: u16,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub identifier: String,
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
    #[arg(long, default_value_t = 20)]
    pub limit: u16,
}

#[derive(Debug, Args)]
pub struct RunsGetArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub execution_id: String,
    /// Include detailed execution data and workflow metadata
    #[arg(long)]
    pub details: bool,
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
}

#[derive(Debug, Args)]
pub struct PushArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub file: PathBuf,
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
    /// Rename a node and rewrite connection references
    Rename(NodeRenameArgs),
    /// Remove a node and all of its connections
    Rm(NodeRemoveArgs),
}

#[derive(Debug, Args)]
pub struct NodeListArgs {
    pub file: PathBuf,
}

#[derive(Debug, Args)]
pub struct NodeAddArgs {
    pub file: PathBuf,
    #[arg(long)]
    pub name: String,
    #[arg(long = "type")]
    pub node_type: String,
    #[arg(long)]
    pub type_version: Option<f64>,
    #[arg(long, default_value_t = 0)]
    pub x: i64,
    #[arg(long, default_value_t = 0)]
    pub y: i64,
    #[arg(long)]
    pub disabled: bool,
}

#[derive(Debug, Args)]
pub struct NodeSetArgs {
    pub file: PathBuf,
    pub node: String,
    pub path: String,
    #[arg(required_unless_present = "null")]
    pub value: Option<String>,
    #[command(flatten)]
    pub mode: ValueModeArgs,
}

#[derive(Debug, Args)]
pub struct NodeRenameArgs {
    pub file: PathBuf,
    pub current_name: String,
    pub new_name: String,
}

#[derive(Debug, Args)]
pub struct NodeRemoveArgs {
    pub file: PathBuf,
    pub node: String,
}

#[derive(Debug, Args)]
pub struct ValueModeArgs {
    #[arg(long = "json-value", conflicts_with_all = ["number", "bool_value", "null"])]
    pub json_value: bool,
    #[arg(long, conflicts_with_all = ["json_value", "bool_value", "null"])]
    pub number: bool,
    #[arg(long = "bool", conflicts_with_all = ["json_value", "number", "null"])]
    pub bool_value: bool,
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
    #[arg(value_name = "PATH")]
    pub file: PathBuf,
    /// Compare the local workflow against the current remote workflow
    #[arg(long)]
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
