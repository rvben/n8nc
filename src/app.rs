use clap::CommandFactory;

use crate::{
    cli::{Cli, Command},
    cmd::common::Context,
    error::AppError,
};

pub async fn run(cli: Cli) -> Result<(), AppError> {
    let context = Context {
        json: cli.json,
        repo_root: cli.repo_root,
    };

    match cli.command {
        Command::Init(args) => crate::cmd::init::cmd_init(&context, args).await,
        Command::Doctor(args) => crate::cmd::doctor::cmd_doctor(&context, args).await,
        Command::Auth(args) => crate::cmd::auth::cmd_auth(&context, args).await,
        Command::Ls(args) => crate::cmd::ls::cmd_ls(&context, args).await,
        Command::Get(args) => crate::cmd::workflow::cmd_get(&context, args).await,
        Command::Runs(args) => crate::cmd::runs::cmd_runs(&context, args).await,
        Command::Pull(args) => crate::cmd::pull::cmd_pull(&context, args).await,
        Command::Push(args) => crate::cmd::push::cmd_push(&context, args).await,
        Command::Workflow(args) => crate::cmd::workflow::cmd_workflow(&context, args).await,
        Command::Node(args) => crate::cmd::edit::cmd_node(&context, args).await,
        Command::Conn(args) => crate::cmd::edit::cmd_conn(&context, args).await,
        Command::Expr(args) => crate::cmd::edit::cmd_expr(&context, args).await,
        Command::Credential(args) => crate::cmd::credential::cmd_credential(&context, args).await,
        Command::Status(args) => crate::cmd::status::cmd_status(&context, args).await,
        Command::Diff(args) => crate::cmd::status::cmd_diff(&context, args).await,
        Command::Activate(args) => crate::cmd::activate::cmd_activation(&context, args, true).await,
        Command::Deactivate(args) => {
            crate::cmd::activate::cmd_activation(&context, args, false).await
        }
        Command::Archive(args) => crate::cmd::activate::cmd_archive(&context, args, true).await,
        Command::Unarchive(args) => crate::cmd::activate::cmd_archive(&context, args, false).await,
        Command::Trigger(args) => crate::cmd::trigger::cmd_trigger(&context, args).await,
        Command::Fmt(args) => crate::cmd::fmt::cmd_fmt(&context, args).await,
        Command::Validate(args) => crate::cmd::validate_cmd::cmd_validate(&context, args).await,
        Command::Lint(args) => crate::cmd::lint::cmd_lint(&context, args).await,
        Command::Search(args) => crate::cmd::search::cmd_search(&context, args).await,
        Command::Completions(args) => {
            clap_complete::generate(
                args.shell,
                &mut Cli::command(),
                "n8nc",
                &mut std::io::stdout(),
            );
            Ok(())
        }
    }
}
