//! Entry point for the sethu binary.
//!
//! Parses the CLI, dispatches to the appropriate command module,
//! and maps results to documented exit codes.

use std::process::ExitCode;

use clap::Parser;

mod cli;
mod commands;

use cli::{Cli, Command};

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();

    let cli = Cli::parse();

    let result = dispatch(&cli);

    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(cli: &Cli) -> anyhow::Result<ExitCode> {
    match &cli.command {
        Command::Install(args) => commands::install::run(args),
        Command::Init(args) => commands::init::run(args),
        Command::Changes(args) => commands::changes::run(args),
        Command::Context(args) => commands::context::run(args),
        Command::Record(args) => commands::record::run(args),
        Command::Check(args) => commands::check::run(args),
        Command::Stub(args) => commands::stub::run(args),
        Command::Verify(args) => commands::verify::run(args),
        Command::Report(args) => commands::report::run(args),
        Command::Scan(args) => commands::scan::run(args),
        Command::Capabilities(args) => commands::capabilities::run(args),
    }
}
