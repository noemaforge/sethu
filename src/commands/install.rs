//! Implementation of `sethu install`.
//!
//! The command installs the embedded pack into a consumer repository and
//! reports what landed. The target must be a git repository. A missing
//! diff binary blocks later migration work but never blocks the install.
//! The capabilities table follows the install summary, so the report shows
//! the same command states the workflow itself will read.

use std::process::ExitCode;

use crate::cli::{CapabilitiesArgs, InstallArgs};
use crate::commands::Status;
use crate::install;

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Entry point for `sethu install`.
///
/// The installer writes owned files, merges owned modes, and records the
/// result. The summary prints first, then the capabilities table prints
/// through the capabilities module. Installing never starts a migration.
pub fn run(args: &InstallArgs) -> anyhow::Result<ExitCode> {
    let report = install::install_into(&args.repo)?;
    for line in report.summary() {
        println!("{line}");
    }
    println!();
    crate::commands::capabilities::run(&CapabilitiesArgs { json: false })?;
    Ok(ExitCode::SUCCESS)
}
