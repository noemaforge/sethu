//! Shared library for the sethu binary and its integration tests.
//!
//! The binary entry point parses the command line and calls [`dispatch`].
//! Integration tests import the modules declared here directly.

/// Accounting and readiness for one migration attempt.
pub mod check;
/// Command line shapes for every subcommand.
pub mod cli;
/// One module per subcommand.
pub mod commands;
/// Focused contract context for one change.
pub mod context;
/// Embedded pack and repository installation.
pub mod install;
/// Shared origin for captured changes.
pub mod provenance;
/// Reviewable migration reports from validated state.
pub mod report;
/// Tag history walk that ranks candidate upgrade pairs.
pub mod scan;
/// Versioned state files for the state tree.
pub mod state;
/// Fixture server pieces for contract-sensitive behaviour.
pub mod stub;
/// Attributable verification runs for one migration attempt.
pub mod verify;
/// Subprocess adapter for the contract diff tool.
pub mod vimanam;

use std::process::ExitCode;

use cli::{Cli, Command};

/// Run the parsed command line and return its exit code.
///
/// Every arm calls the same command entry point. The `record` and `check`
/// arms first resolve the global attempt selector into one migration
/// through the shared lookup. Other arms keep ignoring the selector until
/// their own handling changes. A bad selector fails before the command
/// runs. Without a selector each command keeps its single migration
/// fallback with its current errors.
pub fn dispatch(cli: &Cli) -> anyhow::Result<ExitCode> {
    match &cli.command {
        Command::Install(args) => commands::install::run(args),
        Command::Init(args) => commands::init::run(args),
        Command::Changes(args) => commands::changes::run(args),
        Command::Context(args) => commands::context::run(args),
        Command::Record(args) => record_selected(cli, args),
        Command::Check(args) => check_selected(cli, args),
        Command::Stub(args) => commands::stub::run(args),
        Command::Verify(args) => commands::verify::run(args),
        Command::Report(args) => report_selected(cli, args),
        Command::Scan(args) => commands::scan::run(args),
        Command::Capabilities(args) => commands::capabilities::run(args),
    }
}

/// Resolve the attempt selector, then record into that migration.
///
/// Without a selector this keeps the single migration fallback with its
/// current errors. A selector resolves by id or unique prefix before
/// anything runs or writes.
fn record_selected(cli: &Cli, args: &cli::RecordArgs) -> anyhow::Result<ExitCode> {
    let Some(wanted) = cli.attempt.as_deref() else {
        return commands::record::run(args);
    };
    let repo = commands::record::working_repo()?;
    let root = crate::state::layout::state_root(&repo);
    let migration = commands::record::resolve_migration(&root, Some(wanted))?;
    commands::record::run_on(&repo, &migration, args)
}

/// Resolve the attempt selector, then check that migration.
///
/// Without a selector this keeps the single migration fallback with its
/// current errors. A selector resolves by id or unique prefix through the
/// same lookup that recording uses, so unknown and ambiguous values fail
/// the same way before anything runs.
fn check_selected(cli: &Cli, args: &cli::CheckArgs) -> anyhow::Result<ExitCode> {
    let Some(wanted) = cli.attempt.as_deref() else {
        return commands::check::run(args);
    };
    let repo = commands::check::working_repo()?;
    let root = crate::state::layout::state_root(&repo);
    let migration = commands::record::resolve_migration(&root, Some(wanted))?;
    commands::check::run_on(&repo, &migration, args)
}

/// Resolve the attempt selector, then report on that migration.
///
/// Without a selector this keeps the single migration fallback with its
/// current errors. A selector resolves by id or unique prefix through the
/// same lookup that recording uses, so unknown and ambiguous values fail
/// the same way before anything runs.
fn report_selected(cli: &Cli, args: &cli::ReportArgs) -> anyhow::Result<ExitCode> {
    let Some(wanted) = cli.attempt.as_deref() else {
        return commands::report::run(args);
    };
    let repo = commands::report::working_repo()?;
    let root = crate::state::layout::state_root(&repo);
    let migration = commands::record::resolve_migration(&root, Some(wanted))?;
    commands::report::run_on(&repo, &migration, args)
}
