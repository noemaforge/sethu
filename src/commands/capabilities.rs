//! Implementation of `sethu capabilities`.
//!
//! The command reports the build version, the pack version, the tools found
//! in this environment, and the runtime state of every command. With
//! `--json` it prints one versioned document. Without the flag it prints
//! the same facts as a short human table. Nothing here writes any state.

use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::Context;
use indexmap::IndexMap;
use serde::Serialize;

use crate::cli::CapabilitiesArgs;
use crate::commands::Status;
use crate::commands::{changes, check, context, init, install, record, report, scan, stub, verify};
use crate::vimanam::{self, MINIMUM_VERSION, Version};

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Envelope version of the capabilities document.
///
/// Readers accept the fields they know and ignore the rest. Adding a field
/// keeps this version. Renaming or removing one bumps it.
const SCHEMA_VERSION: u32 = 1;

/// Entry point for `sethu capabilities`.
pub fn run(args: &CapabilitiesArgs) -> anyhow::Result<ExitCode> {
    let document = collect()?;
    if args.json {
        let text = serde_json::to_string_pretty(&document)
            .with_context(|| "render capabilities as JSON")?;
        println!("{text}");
    } else {
        for line in table(&document) {
            println!("{line}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Full capabilities document printed with `--json`.
#[derive(Serialize)]
struct Capabilities {
    /// Envelope version of this document.
    schema_version: u32,
    /// Version of the running binary.
    sethu: String,
    /// Version of the embedded pack. It ships inside the binary.
    pack: String,
    /// State of the contract diff tool.
    vimanam: VimanamStatus,
    /// State of the git binary.
    git: ToolStatus,
    /// State of the nextest runner.
    nextest: ToolStatus,
    /// Runtime state of every command in dispatch order.
    commands: IndexMap<String, CommandEntry>,
}

/// Version state of the contract diff tool.
#[derive(Serialize)]
struct VimanamStatus {
    /// Whether a binary answered on PATH.
    found: bool,
    /// Reported version, when the probe could read one.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// Whether the found version meets the minimum.
    supported: bool,
}

/// Version state of one helper tool.
#[derive(Serialize)]
struct ToolStatus {
    /// Whether the tool answered on PATH.
    found: bool,
    /// Reported version, when the probe could read one.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

/// Runtime state of one command.
#[derive(Serialize)]
struct CommandEntry {
    /// Reported state of the command.
    status: CommandState,
    /// Why the command cannot run. Always present when unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// Reported states for the commands map.
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum CommandState {
    /// The command is implemented and ready in this environment.
    Available,
    /// The command is declared but not yet implemented.
    Planned,
    /// The command cannot run here. The entry always names why.
    Unavailable,
}

/// Gather every reported fact in one pass.
fn collect() -> anyhow::Result<Capabilities> {
    let workdir =
        std::env::current_dir().with_context(|| "read working directory for tool probes")?;
    let version = env!("CARGO_PKG_VERSION").to_string();
    let vimanam = probe_vimanam(&workdir);
    let reason = vimanam_reason(&vimanam);
    let mut commands = IndexMap::new();
    for (name, status) in command_statuses() {
        let entry = match &reason {
            Some(text) if needs_vimanam(name) => CommandEntry {
                status: CommandState::Unavailable,
                reason: Some(text.clone()),
            },
            _ => from_status(status),
        };
        commands.insert(name.to_string(), entry);
    }
    Ok(Capabilities {
        schema_version: SCHEMA_VERSION,
        sethu: version.clone(),
        pack: version,
        vimanam,
        git: probe_git(&workdir),
        nextest: probe_nextest(&workdir),
        commands,
    })
}

/// Build-time state of every command in dispatch order.
fn command_statuses() -> [(&'static str, Status); 10] {
    [
        ("install", install::STATUS),
        ("init", init::STATUS),
        ("changes", changes::STATUS),
        ("context", context::STATUS),
        ("record", record::STATUS),
        ("check", check::STATUS),
        ("stub", stub::STATUS),
        ("verify", verify::STATUS),
        ("report", report::STATUS),
        ("scan", scan::STATUS),
    ]
}

/// Names of the commands that invoke the contract diff tool at runtime.
///
/// These commands cannot run without a supported binary on PATH, so a
/// missing or outdated binary marks them unavailable with a reason. The
/// remaining commands keep their build-time state either way.
fn needs_vimanam(name: &str) -> bool {
    matches!(name, "init" | "changes" | "context" | "scan")
}

/// Translate build-time state into a reported entry.
fn from_status(status: Status) -> CommandEntry {
    match status {
        Status::Available => CommandEntry {
            status: CommandState::Available,
            reason: None,
        },
        Status::Planned => CommandEntry {
            status: CommandState::Planned,
            reason: None,
        },
    }
}

/// Probe the contract diff tool without failing the command.
///
/// A usable binary reports its version as supported. Anything else records
/// what was found and leaves the command entries to explain the impact.
fn probe_vimanam(workdir: &Path) -> VimanamStatus {
    match vimanam::probe_vimanam(workdir) {
        Ok(version) => VimanamStatus {
            found: true,
            version: Some(version.to_string()),
            supported: true,
        },
        Err(error) => {
            let message = format!("{error:#}");
            if is_missing(&error) {
                VimanamStatus {
                    found: false,
                    version: None,
                    supported: false,
                }
            } else {
                VimanamStatus {
                    found: true,
                    version: old_version(&message),
                    supported: false,
                }
            }
        }
    }
}

/// Check whether a probe failed because the binary is absent from PATH.
fn is_missing(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

/// Recover the found version from a rejection message.
///
/// The version probe names the version it found when it rejects an old
/// binary, so the report keeps that version instead of dropping it.
fn old_version(message: &str) -> Option<String> {
    let rest = message.strip_prefix("unsupported vimanam version ")?;
    let token = rest.split([',', ' ']).next()?;
    let version = Version::parse(token).ok()?;
    Some(version.to_string())
}

/// Explain why the diff tool blocks its dependent commands, if it does.
fn vimanam_reason(status: &VimanamStatus) -> Option<String> {
    if status.supported {
        return None;
    }
    if !status.found {
        return Some(format!(
            "vimanam not found on PATH, need {MINIMUM_VERSION} or newer"
        ));
    }
    match &status.version {
        Some(version) => Some(format!(
            "vimanam {version} is older than the minimum {MINIMUM_VERSION}"
        )),
        None => Some(format!(
            "vimanam is unusable, need {MINIMUM_VERSION} or newer"
        )),
    }
}

/// Probe the git binary on PATH.
fn probe_git(workdir: &Path) -> ToolStatus {
    probe_tool(workdir, "git", &["--version"], parse_git_version)
}

/// Probe the nextest runner through cargo.
///
/// The runner installs as a cargo subcommand, so the probe asks cargo for
/// the subcommand version. A missing subcommand reports not found.
fn probe_nextest(workdir: &Path) -> ToolStatus {
    probe_tool(
        workdir,
        "cargo",
        &["nextest", "--version"],
        parse_nextest_version,
    )
}

/// Run one `--version` probe and parse its version.
///
/// Every spawn sets its working directory and captures both streams. A
/// missing binary reports not found. Any other failure reports a present
/// tool with an unknown version, so one broken tool never fails the report.
fn probe_tool(
    workdir: &Path,
    program: &str,
    args: &[&str],
    parse: fn(&str) -> Option<String>,
) -> ToolStatus {
    let output = match Command::new(program)
        .args(args)
        .current_dir(workdir)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ToolStatus {
                found: false,
                version: None,
            };
        }
        Err(_) => {
            return ToolStatus {
                found: true,
                version: None,
            };
        }
    };
    if !output.status.success() {
        return ToolStatus {
            found: true,
            version: None,
        };
    }
    let text = String::from_utf8_lossy(&output.stdout);
    ToolStatus {
        found: true,
        version: parse(&text),
    }
}

/// Read the version from `git --version` output.
///
/// The output looks like `git version 2.51.0`. Anything else reports a
/// present tool with an unknown version.
fn parse_git_version(output: &str) -> Option<String> {
    let mut words = output.split_whitespace();
    if words.next() == Some("git") && words.next() == Some("version") {
        let version = words.next()?;
        if words.next().is_none() && !version.is_empty() {
            return Some(version.to_string());
        }
    }
    None
}

/// Read the version from `cargo nextest --version` output.
///
/// The first line looks like `cargo-nextest 0.9.146 (8af696ddc
/// 2026-09-21)`, followed by detail lines. Anything else reports a
/// present tool with an unknown version.
fn parse_nextest_version(output: &str) -> Option<String> {
    let first = output.lines().next()?;
    let mut words = first.split_whitespace();
    if words.next() == Some("cargo-nextest") {
        let version = words.next()?;
        let head = version.chars().next()?;
        if head.is_ascii_digit() {
            return Some(version.to_string());
        }
    }
    None
}

/// Render the same facts as short human lines.
fn table(document: &Capabilities) -> Vec<String> {
    let mut lines = vec![
        format!("sethu {} (pack {})", document.sethu, document.pack),
        format!("vimanam {}", describe_vimanam(&document.vimanam)),
        format!("git {}", describe_tool(&document.git)),
        format!("cargo-nextest {}", describe_tool(&document.nextest)),
        String::new(),
    ];
    for (name, entry) in &document.commands {
        let state = match entry.status {
            CommandState::Available => "available",
            CommandState::Planned => "planned",
            CommandState::Unavailable => "unavailable",
        };
        match &entry.reason {
            Some(reason) => lines.push(format!("{name}: {state} ({reason})")),
            None => lines.push(format!("{name}: {state}")),
        }
    }
    lines
}

/// Describe the diff tool for the human table.
fn describe_vimanam(status: &VimanamStatus) -> String {
    match (status.found, status.version.as_deref(), status.supported) {
        (true, Some(version), true) => format!("{version} (supported)"),
        (true, Some(version), false) => {
            format!("{version} (unsupported, need {MINIMUM_VERSION} or newer)")
        }
        (true, None, _) => "found (version unknown)".to_string(),
        (false, _, _) => format!("missing (need {MINIMUM_VERSION} or newer)"),
    }
}

/// Describe one helper tool for the human table.
fn describe_tool(status: &ToolStatus) -> String {
    match (status.found, status.version.as_deref()) {
        (true, Some(version)) => format!("{version} (found)"),
        (true, None) => "found (version unknown)".to_string(),
        (false, _) => "missing".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_state_maps_to_matching_report_state() {
        let available = from_status(Status::Available);
        assert!(matches!(available.status, CommandState::Available));
        assert_eq!(available.reason, None);

        let planned = from_status(Status::Planned);
        assert!(matches!(planned.status, CommandState::Planned));
        assert_eq!(planned.reason, None);
    }

    #[test]
    fn diff_dependents_cover_diff_commands() {
        for name in ["init", "changes", "context", "scan"] {
            assert!(needs_vimanam(name), "{name} should need the diff tool");
        }
        for name in [
            "install",
            "record",
            "check",
            "stub",
            "verify",
            "report",
            "capabilities",
        ] {
            assert!(!needs_vimanam(name), "{name} should not need the diff tool");
        }
    }

    #[test]
    fn rejection_message_yields_found_version() {
        assert_eq!(
            old_version("unsupported vimanam version 1.2.0, need 1.3.0 or newer"),
            Some("1.2.0".to_string())
        );
        assert_eq!(old_version("unrelated failure"), None);
    }

    #[test]
    fn missing_binary_explains_minimum_version() {
        let status = VimanamStatus {
            found: false,
            version: None,
            supported: false,
        };
        let reason = vimanam_reason(&status).unwrap();
        assert!(reason.contains(&MINIMUM_VERSION.to_string()));
    }

    #[test]
    fn supported_binary_needs_no_reason() {
        let status = VimanamStatus {
            found: true,
            version: Some("1.3.0".to_string()),
            supported: true,
        };
        assert_eq!(vimanam_reason(&status), None);
    }

    #[test]
    fn git_parser_reads_expected_shape_only() {
        assert_eq!(
            parse_git_version("git version 2.51.0\n"),
            Some("2.51.0".to_string())
        );
        assert_eq!(parse_git_version("git version\n"), None);
        assert_eq!(parse_git_version("unexpected"), None);
    }

    #[test]
    fn nextest_parser_reads_expected_shape_only() {
        assert_eq!(
            parse_nextest_version(
                "cargo-nextest 0.9.146 (8af696ddc 2026-09-21)\nrelease: 0.9.146\n"
            ),
            Some("0.9.146".to_string())
        );
        assert_eq!(parse_nextest_version("error: no such command\n"), None);
    }
}
