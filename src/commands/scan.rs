//! Implementation of `sethu scan`.

use std::process::ExitCode;

use crate::cli::ScanArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu scan`.
pub fn run(_args: &ScanArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("scan is not available yet")
}
