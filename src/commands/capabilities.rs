//! Implementation of `sethu capabilities`.

use std::process::ExitCode;

use crate::cli::CapabilitiesArgs;
use crate::commands::Status;

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Planned;

/// Entry point for `sethu capabilities`.
pub fn run(_args: &CapabilitiesArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("capabilities is not available yet")
}
