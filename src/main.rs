mod app;
mod cli;
mod commands;
mod config;
mod gateway_server;
mod logs;
mod model;
mod overlay;
mod provision;
mod sandbox;
mod tailscale;
mod templates;
mod util;

use clap::{CommandFactory, Parser};

use app::App;
use cli::{Cli, Command};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Command::Completions { shell } = cli.command {
        clap_complete::generate(shell, &mut Cli::command(), "lilbox", &mut std::io::stdout());
        std::process::exit(0);
    }
    let command = cli.command;
    let code = match App::new() {
        Ok(app) => match commands::dispatch(&app, command).await {
            Ok(code) => code,
            Err(error) => {
                eprintln!("error: {error:#}");
                1
            }
        },
        Err(error) => {
            eprintln!("error: {error:#}");
            1
        }
    };
    std::process::exit(code);
}
