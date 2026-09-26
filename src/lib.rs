//! Shared library for the sethu binary and its integration tests.
//!
//! The binary entry point calls [`run`]. Integration tests import the
//! modules declared here directly.

/// Command line shapes for every subcommand.
pub mod cli;
/// One module per subcommand.
pub mod commands;
/// Shared origin for captured changes.
pub mod provenance;
/// Versioned state files for the state tree.
pub mod state;
/// Fixture server pieces for contract-sensitive behaviour.
pub mod stub;
/// Subprocess adapter for the contract diff tool.
pub mod vimanam;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

/// Parse the command line and run the chosen subcommand.
///
/// Logging goes to stderr. A command failure prints its message to stderr
/// and yields exit code 1. Usage errors keep the codes that the argument
/// parser assigns.
pub fn run() -> ExitCode {
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
