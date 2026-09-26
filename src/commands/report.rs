//! Implementation of `sethu report`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;

use crate::cli::ReportArgs;
use crate::commands::Status;
use crate::state::layout;

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Entry point for `sethu report`.
///
/// The command resolves the single migration under the working
/// directory, assembles the listing from validated state, and writes
/// the selected formats. An incomplete migration still gets a useful
/// listing of every required change, so the process exits 0 whenever
/// the files land. Damaged state outside the ledger fails as a tool
/// error. The command writes only its listing files.
pub fn run(args: &ReportArgs) -> anyhow::Result<ExitCode> {
    let repo = working_repo()?;
    let root = layout::state_root(&repo);
    let migration = sole_migration(&root)?;
    run_on(&repo, &migration, args)
}

/// Report on one migration that dispatch already resolved.
///
/// Assembly, rendering, and file layout behave exactly as the plain
/// entry point. Only the lookup differs. Dispatch calls this after it
/// turns the attempt selector into one migration directory.
pub fn run_on(repo: &Path, migration: &Path, args: &ReportArgs) -> anyhow::Result<ExitCode> {
    let report = crate::report::assemble(repo, migration)?;
    let written =
        crate::report::write_reports(&report, migration, &args.format, args.out.as_deref())?;
    for file in &written {
        println!("wrote {} ({})", file.path.display(), file.format);
    }
    if report.accounted {
        println!("accounted: yes");
    } else {
        println!("accounted: no ({} problems)", report.problems.len());
    }
    if report.ready {
        println!("ready: yes");
    } else {
        println!("ready: no ({} blockers)", report.not_ready.len());
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolve the working repository from the current directory.
///
/// The path is canonicalised, so later lookups compare one spelling.
/// Failures name the directory that could not be read or resolved.
pub fn working_repo() -> anyhow::Result<PathBuf> {
    let repo = std::env::current_dir().with_context(|| "read working directory for `report`")?;
    repo.canonicalize()
        .with_context(|| format!("resolve working directory {}", repo.display()))
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
            "found {} migration attempts under {}, `report` needs exactly one",
            names.len(),
            dir.display()
        ),
    }
}
