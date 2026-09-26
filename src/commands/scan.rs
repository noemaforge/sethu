//! Implementation of `sethu scan`.

use std::process::ExitCode;

use crate::cli::ScanArgs;
use crate::commands::Status;
use crate::scan::{ScanRequest, render_report, run_scan};

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Entry point for `sethu scan`.
///
/// The command walks the repository tag history in version order, diffs
/// adjacent spec revisions through the contract diff tool, and prints a
/// deterministic ranking with one rationale per pair. Missing specs,
/// moved specs, and parse failures print by tag. The ranking is a
/// heuristic and never claims a consumer is affected. Diagnostics go to
/// stderr. Nothing here checks anything out.
pub fn run(args: &ScanArgs) -> anyhow::Result<ExitCode> {
    let report = run_scan(&ScanRequest {
        repo: args.repo.clone(),
        spec: args.spec.clone(),
        pattern: args.tags.clone(),
    })?;
    for line in render_report(&report) {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}
