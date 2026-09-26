//! One module per subcommand. Each exposes `run` and `STATUS`.

pub mod capabilities;
pub mod changes;
pub mod check;
pub mod context;
pub mod init;
pub mod install;
pub mod record;
pub mod report;
pub mod scan;
pub mod stub;
pub mod verify;

/// Whether a command is ready to run in this build.
///
/// A command that cannot run because a required tool is missing at runtime is
/// reported as `unavailable` by `capabilities`, not here. `Status` reflects
/// only the build-time implementation state.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The command is fully implemented and can be invoked.
    Available,
    /// The command is declared but not yet implemented.
    Planned,
}
