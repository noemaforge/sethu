//! Implementation of `sethu verify`.

use std::process::ExitCode;

use crate::cli::VerifyArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu verify`.
pub fn run(_args: &VerifyArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("verify is not available yet")
}
