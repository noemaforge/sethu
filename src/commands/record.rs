//! Implementation of `sethu record`.

use std::process::ExitCode;

use crate::cli::RecordArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu record`.
pub fn run(_args: &RecordArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("record is not available yet")
}
