//! Implementation of `sethu stub`.

use std::process::ExitCode;

use crate::cli::StubArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu stub`.
pub fn run(_args: &StubArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("stub is not available yet")
}
