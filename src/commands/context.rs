//! Implementation of `sethu context`.

use std::process::ExitCode;

use crate::cli::ContextArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu context`.
pub fn run(_args: &ContextArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("context is not available yet")
}
