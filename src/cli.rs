use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

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
    pub no_color: bool,
    #[arg(long, global = true)]
    pub quiet: bool,
    #[arg(long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
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
    /// Show local workflow sync state
    Status(StatusArgs),
    /// Show local changes for one tracked workflow
    Diff(DiffArgs),
    /// Activate a workflow
    Activate(IdArgs),
    /// Deactivate a workflow
    Deactivate(IdArgs),
    /// Call a webhook or trigger URL
    Trigger(TriggerArgs),
    /// Format workflow and sidecar files
    Fmt(FmtArgs),
    /// Validate local workflow files
    Validate(ValidateArgs),
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
    /// Show configured aliases and token availability
    List,
    /// Remove a stored token
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
    pub identifier: String,
}

#[derive(Debug, Args)]
pub struct PushArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,
    pub file: PathBuf,
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
