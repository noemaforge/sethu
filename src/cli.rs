//! Command-line interface definitions for sethu.
//!
//! All subcommands and their arguments are declared here using clap derive.
//! Each subcommand dispatches to its own module in `commands/`.

use clap::{Parser, Subcommand, ValueEnum};
use std::process::ExitCode;

use crate::commands;

/// Trace OpenAPI changes into your application, repair them, and prove it.
#[derive(Debug, Parser)]
#[command(name = "sethu", version, about)]
pub struct Cli {
    /// Select a migration attempt by ID or unique prefix.
    #[arg(long, global = true, value_name = "ID")]
    pub attempt: Option<String>,

    #[command(subcommand)]
    /// The subcommand to run.
    pub command: Command,
}

/// All subcommands offered by sethu.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install the Bob pack into a consumer repository.
    Install(InstallArgs),

    /// Initialise a migration from a pair of OpenAPI specs.
    Init(InitArgs),

    /// List the changes that need investigating.
    Changes(ChangesArgs),

    /// Show context for one change ID.
    Context(ContextArgs),

    /// Record the outcome and evidence for one change ID.
    Record(RecordArgs),

    /// Report accounting completeness and upgrade readiness.
    Check(CheckArgs),

    /// Start a local fixture server for one version of the spec.
    Stub(StubArgs),

    /// Run verification checks against the stub and record the results.
    Verify(VerifyArgs),

    /// Generate a migration report.
    Report(ReportArgs),

    /// Walk a spec repository's tag history and find candidate upgrade pairs.
    Scan(ScanArgs),

    /// Report what sethu can do in this environment.
    Capabilities(CapabilitiesArgs),
}

/// Usage error found after argument parsing.
///
/// Clap exits on malformed flags before dispatch runs. Values that need
/// state lookup, like an attempt selector, fail here instead. The binary
/// prints the message and exits with code 2. The message always names
/// the rejected value.
#[derive(Debug)]
pub struct UsageError {
    message: String,
}

impl UsageError {
    /// Build a usage error from one message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UsageError {}

impl Cli {
    /// Run the parsed command line.
    ///
    /// Every arm calls the same command entry point as the library
    /// runner. The `record` arm first resolves the global attempt
    /// selector into one migration. Other arms keep ignoring the
    /// selector until their own handling changes. A bad selector fails
    /// before the command runs.
    pub fn dispatch(&self) -> anyhow::Result<ExitCode> {
        match &self.command {
            Command::Install(args) => commands::install::run(args),
            Command::Init(args) => commands::init::run(args),
            Command::Changes(args) => commands::changes::run(args),
            Command::Context(args) => commands::context::run(args),
            Command::Record(args) => self.dispatch_record(args),
            Command::Check(args) => commands::check::run(args),
            Command::Stub(args) => commands::stub::run(args),
            Command::Verify(args) => commands::verify::run(args),
            Command::Report(args) => commands::report::run(args),
            Command::Scan(args) => commands::scan::run(args),
            Command::Capabilities(args) => commands::capabilities::run(args),
        }
    }

    /// Resolve the attempt selector, then record into that migration.
    ///
    /// Without a selector this keeps the single migration fallback with
    /// its current errors. A selector resolves by id or unique prefix
    /// before anything runs or writes.
    fn dispatch_record(&self, args: &RecordArgs) -> anyhow::Result<ExitCode> {
        let Some(wanted) = self.attempt.as_deref() else {
            return commands::record::run(args);
        };
        let repo = commands::record::working_repo()?;
        let root = crate::state::layout::state_root(&repo);
        let migration = commands::record::resolve_migration(&root, Some(wanted))?;
        commands::record::run_on(&repo, &migration, args)
    }
}

/// Arguments for `sethu install`.
#[derive(Debug, clap::Args)]
pub struct InstallArgs {
    /// Path to the consumer repository.
    pub repo: std::path::PathBuf,
}

/// Arguments for `sethu init`.
#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Path to the old OpenAPI spec.
    pub old: std::path::PathBuf,

    /// Path to the new OpenAPI spec.
    pub new: std::path::PathBuf,

    /// Path to the consumer repository (defaults to the current directory).
    #[arg(long, default_value = ".", value_name = "PATH")]
    pub repo: std::path::PathBuf,

    /// Restrict the investigation to this path (repeatable).
    #[arg(long, value_name = "PATH")]
    pub scope: Vec<std::path::PathBuf>,

    /// List existing attempts for this spec pair instead of initialising.
    #[arg(long)]
    pub list: bool,
}

/// Arguments for `sethu changes`.
#[derive(Debug, clap::Args)]
pub struct ChangesArgs {
    /// Include non-breaking changes in the output (shown separately).
    #[arg(long)]
    pub all: bool,

    /// Split output into groups of this size.
    #[arg(long, value_name = "N")]
    pub group: Option<usize>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `sethu context`.
#[derive(Debug, clap::Args)]
pub struct ContextArgs {
    /// The change ID to retrieve context for.
    pub id: String,

    /// Detail level to request from vimanam.
    #[arg(long, default_value = "endpoint", value_name = "LEVEL")]
    pub level: ContextLevel,

    /// Write context for a whole group of IDs instead of printing one.
    #[arg(long, value_name = "GROUP")]
    pub prepare: Option<String>,
}

/// Detail levels for `sethu context`.
#[derive(Debug, Clone, ValueEnum)]
pub enum ContextLevel {
    /// High-level summary of all endpoints.
    Overview,
    /// Standard endpoint detail.
    Endpoint,
    /// Full schema detail.
    Schema,
}

/// Arguments for `sethu record`.
#[derive(Debug, clap::Args)]
pub struct RecordArgs {
    /// The change ID to record an outcome for.
    pub id: String,

    /// The outcome to record.
    #[arg(long, required = true, value_name = "OUTCOME")]
    pub outcome: Outcome,

    /// An evidence reference (repeatable).
    #[arg(long, value_name = "REF")]
    pub evidence: Vec<String>,

    /// A free-text note to attach to this record.
    #[arg(long, value_name = "TEXT")]
    pub note: Option<String>,
}

/// Recognised outcomes for `sethu record`.
#[derive(Debug, Clone, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum Outcome {
    /// The change was fixed and the fix was verified by a passing run.
    FixedAndVerified,
    /// The application is not affected within the inspected scope.
    UnaffectedInApplication,
    /// No usage was found within the inspected scope.
    NoUsageFound,
    /// A product decision is needed before this can be resolved.
    DecisionRequired,
    /// The outcome is still unknown or a fix is still failing.
    Unresolved,
}

/// Arguments for `sethu check`.
#[derive(Debug, clap::Args)]
pub struct CheckArgs {
    /// Fail unless every required change is also ready (not just accounted for).
    #[arg(long)]
    pub require_ready: bool,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `sethu stub`.
#[derive(Debug, clap::Args)]
pub struct StubArgs {
    /// Which spec version to serve fixtures for.
    #[arg(long, required = true, value_name = "VERSION")]
    pub version: SpecVersion,

    /// TCP port to bind to (0 means any free port).
    #[arg(long, default_value = "0", value_name = "PORT")]
    pub port: u16,

    /// Directory containing fixture scenarios.
    #[arg(long, value_name = "DIR")]
    pub scenarios: Option<std::path::PathBuf>,
}

/// Which spec version the stub serves.
#[derive(Debug, Clone, ValueEnum)]
pub enum SpecVersion {
    /// The old (pre-upgrade) spec.
    Old,
    /// The new (post-upgrade) spec.
    New,
}

/// Arguments for `sethu verify`.
#[derive(Debug, clap::Args)]
pub struct VerifyArgs {
    /// Record the harness hash before repair starts.
    #[arg(long)]
    pub freeze: bool,

    /// Path to the verification manifest.
    #[arg(long, value_name = "PATH")]
    pub manifest: Option<std::path::PathBuf>,

    /// Run only these named checks (repeatable).
    #[arg(long, value_name = "NAME")]
    pub check: Vec<String>,
}

/// Arguments for `sethu report`.
#[derive(Debug, clap::Args)]
pub struct ReportArgs {
    /// Output format(s) to generate.
    #[arg(long, default_value = "both", value_name = "FORMAT")]
    pub format: ReportFormat,

    /// Directory to write the report into.
    #[arg(long, value_name = "DIR")]
    pub out: Option<std::path::PathBuf>,
}

/// Output format for `sethu report`.
#[derive(Debug, Clone, ValueEnum)]
pub enum ReportFormat {
    /// Markdown only.
    Md,
    /// HTML only.
    Html,
    /// Both Markdown and HTML.
    Both,
}

/// Arguments for `sethu scan`.
#[derive(Debug, clap::Args)]
pub struct ScanArgs {
    /// Path to the git repository to scan.
    pub repo: std::path::PathBuf,

    /// Path to the spec file within the repository.
    #[arg(long, required = true, value_name = "PATH")]
    pub spec: String,

    /// Tag pattern to walk (default matches all tags).
    #[arg(long, default_value = "*", value_name = "PATTERN")]
    pub tags: String,
}

/// Arguments for `sethu capabilities`.
#[derive(Debug, clap::Args)]
pub struct CapabilitiesArgs {
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}
