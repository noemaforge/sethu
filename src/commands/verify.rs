//! Implementation of `sethu verify`.

use std::process::ExitCode;

use crate::cli::VerifyArgs;
use crate::commands::Status;

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Entry point for `sethu verify`.
///
/// With `--freeze` the command records the harness content hash named
/// by the manifest. Without `--check` it stops there, since freezing
/// happens before the repair exists. With `--check` (or without
/// `--freeze`) it runs the three verification stages per selected
/// check. Without `--check` every manifest check runs. Each
/// named check runs once per stage with its own fresh stub instance,
/// and every artefact lands under `runs/<run-id>/` beside the
/// manifest. The process exits 0 when every selected check verifies,
/// 4 when at least one does not, and 1 on tool or usage errors, which
/// print to stderr.
pub fn run(args: &VerifyArgs) -> anyhow::Result<ExitCode> {
    crate::verify::runner::run(args, args.manifest.as_deref())
}
