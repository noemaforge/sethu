//! Entry point for the sethu binary.
//!
//! The binary parses the command line and calls the single library
//! dispatch. This wrapper forwards the exit code that dispatch returns.
//! Usage errors exit with code 2 and every other failure exits with
//! code 1.

use std::process::ExitCode;

use clap::Parser;
use sethu::cli::{Cli, UsageError};

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();

    let cli = Cli::parse();
    match sethu::dispatch(&cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            if err.is::<UsageError>() {
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
    }
}
