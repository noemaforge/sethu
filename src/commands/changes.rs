//! Implementation of `sethu changes`.

use std::process::ExitCode;

use crate::cli::ChangesArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu changes`.
pub fn run(_args: &ChangesArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("changes is not available yet")
}
