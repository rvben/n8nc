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

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&ch) = chars.peek() {
                chars.next();
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // Let clap print help and version text itself; only intercept real parse errors.
            if err.kind() == clap::error::ErrorKind::DisplayHelp
                || err.kind() == clap::error::ErrorKind::DisplayVersion
            {
                err.print().ok();
                return ExitCode::SUCCESS;
            }
            let raw = strip_ansi(&err.to_string());
            let message = raw
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("Invalid arguments")
                .trim()
                .to_string();
            eprintln!(
                "{}",
                serde_json::json!({"error": {"kind": "usage", "message": message}})
            );
            return ExitCode::from(2);
        }
    };
    let json = cli.json_output();
    match app::run(cli, json).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => err.emit_and_exit(json),
    }
}
