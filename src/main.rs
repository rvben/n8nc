mod api;
mod app;
mod auth;
mod canonical;
mod cli;
mod config;
mod edit;
mod error;
mod repo;
mod validate;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    match app::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => err.emit_and_exit(json),
    }
}
