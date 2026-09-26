//! Implementation of `sethu check`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;

use crate::check::{Evaluation, Inputs, evaluate, exit_code};
use crate::cli::CheckArgs;
use crate::commands::Status;
use crate::provenance::OriginsDocument;
use crate::state::attempt::AttemptRecord;
use crate::state::capture::CaptureRecord;
use crate::state::layout;
use crate::state::pair::PairRecord;

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Entry point for `sethu check`.
///
/// The command resolves the single migration under the working directory,
/// recomputes the required set from the attempt capture, and revalidates
/// every ledger disposition from disk. It prints an accounting and
/// readiness report and exits 0 when the ledger is complete, 4 when it is
/// incomplete or invalid, and 5 when it is complete but not ready under
/// the readiness flag. Damaged state outside the ledger fails as a tool
/// error. The command writes nothing.
pub fn run(args: &CheckArgs) -> anyhow::Result<ExitCode> {
    let repo = working_repo()?;
    let root = layout::state_root(&repo);
    let migration = sole_migration(&root)?;
    run_on(&repo, &migration, args)
}

/// Check one migration that dispatch already resolved.
///
/// Accounting, readiness handling, and reporting behave exactly as the
/// plain entry point. Only the lookup differs. Dispatch calls this after
/// it turns the attempt selector into one migration directory.
pub fn run_on(repo: &Path, migration: &Path, args: &CheckArgs) -> anyhow::Result<ExitCode> {
    let root = layout::state_root(repo);
    let manifest: AttemptRecord = crate::state::read_state_file(&layout::manifest_path(migration))?;
    manifest.validate()?;

    let loaded = load_attempt_state(&root, migration, &manifest)?;
    let origins = load_origins(&loaded.capture_dir)?;
    let report = evaluate(&Inputs {
        manifest: &manifest,
        capture: &loaded.capture_record,
        origins: origins.as_ref(),
        document: &loaded.document,
        ledger_bytes: loaded.ledger_bytes.as_deref(),
        repo,
    });

    if args.json {
        let text =
            serde_json::to_string_pretty(&report).with_context(|| "render check report as JSON")?;
        println!("{text}");
    } else {
        for line in human_report(&report) {
            println!("{line}");
        }
    }
    Ok(exit_code(
        report.accounted,
        report.ready,
        args.require_ready,
    ))
}

/// Resolve the working repository from the current directory.
///
/// The path is canonicalised, so later lookups compare one spelling.
/// Failures name the directory that could not be read or resolved.
pub fn working_repo() -> anyhow::Result<PathBuf> {
    let repo = std::env::current_dir().with_context(|| "read working directory for `check`")?;
    repo.canonicalize()
        .with_context(|| format!("resolve working directory {}", repo.display()))
}

/// Loaded capture state for one attempt.
///
/// Identity mismatches against the manifest become checker problems, not
/// tool errors, so this shape keeps the stored records beside the parsed
/// change list for the core to compare.
struct LoadedAttempt {
    /// Directory holding the capture files for this attempt.
    capture_dir: PathBuf,
    /// Stored capture record naming the generator binding.
    capture_record: CaptureRecord,
    /// Parsed change list from the attempt capture.
    document: crate::vimanam::DiffDocument,
    /// Raw ledger bytes, or none when no ledger file exists yet.
    ledger_bytes: Option<Vec<u8>>,
}

/// Load the capture state bound to one manifest.
///
/// Pair and capture records are read through the state helpers and checked
/// field by field by the core. Missing or unreadable files fail as tool
/// errors with the file named. A missing ledger reads as empty.
fn load_attempt_state(
    root: &Path,
    migration: &Path,
    manifest: &AttemptRecord,
) -> anyhow::Result<LoadedAttempt> {
    let pair = layout::pair_dir(root, &manifest.old_spec_hash, &manifest.new_spec_hash)?;
    let pair_record: PairRecord = crate::state::read_state_file(&layout::pair_file(&pair))?;
    pair_record.validate()?;
    if pair_record.old_spec_hash != manifest.old_spec_hash
        || pair_record.new_spec_hash != manifest.new_spec_hash
    {
        anyhow::bail!(
            "pair directory {} stores a different spec pair in {}",
            pair.display(),
            layout::pair_file(&pair).display()
        );
    }
    let capture_dir = layout::capture_dir(&pair, &manifest.capture_id);
    let capture_record: CaptureRecord =
        crate::state::read_state_file(&layout::capture_file(&capture_dir))?;
    capture_record.validate()?;
    let changes_path = layout::changes_file(&capture_dir);
    let raw = std::fs::read(&changes_path)
        .with_context(|| format!("read capture change list {}", changes_path.display()))?;
    let document = crate::vimanam::parse_diff_output(&raw)?;
    let ledger_path = layout::ledger_path(migration);
    let ledger_bytes = match std::fs::read(&ledger_path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("read ledger {}", ledger_path.display()))
            );
        }
    };
    Ok(LoadedAttempt {
        capture_dir,
        capture_record,
        document,
        ledger_bytes,
    })
}

/// Load and validate the stored origins for one capture.
///
/// A missing file reads as none, so older captures without stored origins
/// still check. An unreadable file fails as a tool error with the file
/// named.
fn load_origins(capture_dir: &Path) -> anyhow::Result<Option<OriginsDocument>> {
    let path = layout::origins_file(capture_dir);
    match crate::state::read_state_file::<OriginsDocument>(&path) {
        Ok(origins) => {
            origins.validate()?;
            Ok(Some(origins))
        }
        Err(err) => {
            let missing = err
                .chain()
                .any(|cause| matches!(cause.downcast_ref::<std::io::Error>(), Some(io) if io.kind() == std::io::ErrorKind::NotFound));
            if missing {
                return Ok(None);
            }
            Err(err)
        }
    }
}

/// Render an evaluation as deterministic human lines.
///
/// Counts come first, then one line per problem, then one line per
/// readiness blocker. Output order never depends on hash iteration.
fn human_report(report: &Evaluation) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("attempt {}", report.attempt_id));
    lines.push(format!("required {}", report.required.len()));
    let valid = report
        .dispositions
        .values()
        .filter(|view| view.valid)
        .count();
    if report.accounted {
        lines.push(format!(
            "accounted: yes ({}/{})",
            valid,
            report.required.len()
        ));
    } else {
        lines.push(format!(
            "accounted: no ({}/{} valid, {} problems)",
            valid,
            report.required.len(),
            report.problems.len()
        ));
    }
    if report.ready {
        lines.push("ready: yes".to_string());
    } else {
        lines.push(format!("ready: no ({} blockers)", report.not_ready.len()));
    }
    let scope = if report.scope.is_empty() {
        "-".to_string()
    } else {
        report.scope.join(",")
    };
    lines.push(format!("scope: {scope}"));
    for problem in &report.problems {
        lines.push(format!(
            "problem {} {}: {}",
            problem.code, problem.id, problem.message
        ));
    }
    for id in &report.not_ready {
        let outcome = report
            .dispositions
            .get(id)
            .map(|view| view.outcome.as_str())
            .unwrap_or("unknown");
        lines.push(format!("blocked {id} {outcome}: still waits on open work"));
    }
    lines
}

/// Find the single migration under a state root.
///
/// Zero migrations means nothing was initialised here. Several means
/// the choice is ambiguous, and this command refuses to guess. Both
/// cases fail with the directory named.
fn sole_migration(root: &Path) -> anyhow::Result<PathBuf> {
    let dir = layout::migrations_dir(root);
    let mut names = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry =
                    entry.with_context(|| format!("read entry in directory {}", dir.display()))?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if crate::state::atomic::is_pending_temp(&path) {
                    continue;
                }
                if let Some(name) = path.file_name().and_then(|part| part.to_str()) {
                    names.push(name.to_string());
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("list directory {}", dir.display()))
            );
        }
    }
    names.sort();
    match names.len() {
        0 => anyhow::bail!(
            "found no migration attempt under {}, run `init` first",
            dir.display()
        ),
        1 => {
            let migration = dir.join(&names[0]);
            layout::check_manifest_identity(&migration, &names[0])?;
            Ok(migration)
        }
        _ => anyhow::bail!(
            "found {} migration attempts under {}, `check` needs exactly one",
            names.len(),
            dir.display()
        ),
    }
}
