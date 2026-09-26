//! Implementation of `sethu install`.

use std::process::ExitCode;

use crate::cli::InstallArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu install`.
pub fn run(_args: &InstallArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("install is not available yet")
}
