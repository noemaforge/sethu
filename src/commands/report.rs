//! Implementation of `sethu report`.

use std::process::ExitCode;

use crate::cli::ReportArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu report`.
pub fn run(_args: &ReportArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("report is not available yet")
}
