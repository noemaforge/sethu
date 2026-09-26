//! Implementation of `sethu init`.

use std::process::ExitCode;

use crate::cli::InitArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu init`.
pub fn run(_args: &InitArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("init is not available yet")
}
