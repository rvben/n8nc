#![allow(clippy::result_large_err)]
mod api;
mod app;
mod auth;
mod canonical;
mod cli;
mod cmd;
mod config;
mod edit;
mod error;
mod execute;
mod lint;
mod repo;
mod schema;
mod tree;
mod validate;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json || !std::io::stdout().is_terminal();
    match app::run(cli, json).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => err.emit_and_exit(json),
    }
}
