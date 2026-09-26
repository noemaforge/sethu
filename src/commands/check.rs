//! Implementation of `sethu check`.

use std::process::ExitCode;

use crate::cli::CheckArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu check`.
pub fn run(_args: &CheckArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("check is not available yet")
}
