//! Implementation of `sethu record`.

/// Outcome ledger for one migration attempt.
///
/// The state module does not declare the ledger file yet, so this
/// command includes it by path. The include gives way to a plain
/// declaration once the parent module names the file.
#[path = "../state/ledger.rs"]
pub mod ledger;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;

use crate::cli::RecordArgs;
use crate::commands::Status;
use crate::state::attempt::AttemptRecord;
use crate::state::layout;

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Entry point for `sethu record`.
///
/// The command resolves the single migration under the working
/// directory, checks the id against the attempt capture, validates the
/// outcome and its evidence, then stores one current disposition per id
/// with replaced dispositions kept as history in the same file. Writes
/// are atomic. Refusals change nothing and explain what is missing.
pub fn run(args: &RecordArgs) -> anyhow::Result<ExitCode> {
    let repo = std::env::current_dir().with_context(|| "read working directory for `record`")?;
    let repo = repo
        .canonicalize()
        .with_context(|| format!("resolve working directory {}", repo.display()))?;
    let root = layout::state_root(&repo);

    let migration = sole_migration(&root)?;
    let manifest: AttemptRecord =
        crate::state::read_state_file(&layout::manifest_path(&migration))?;
    manifest.validate()?;
    let known = capture_change_ids(&root, &manifest)?;
    if !known.contains(args.id.as_str()) {
        anyhow::bail!(
            "unknown change id `{}` for this attempt, it names no change in the attempt capture",
            args.id
        );
    }

    let outcome = ledger::Outcome::from_cli(&args.outcome);
    let mut checked = Vec::with_capacity(args.evidence.len());
    for reference in &args.evidence {
        checked.push(ledger::check_reference(&repo, reference)?);
    }
    let summary = ledger::summarize(&checked);
    ledger::check_evidence(outcome, &summary, args.note.as_deref())?;

    let mut stored = ledger::load_ledger(&migration)?;
    stored.record(
        &args.id,
        ledger::Entry::new(outcome, args.evidence.clone(), args.note.clone()),
    );
    crate::state::write_state_file(&layout::ledger_path(&migration), &stored)?;

    println!("recorded {} {outcome}", args.id);
    Ok(ExitCode::SUCCESS)
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
            "found {} migration attempts under {}, `record` needs exactly one",
            names.len(),
            dir.display()
        ),
    }
}

/// Read the change ids of the capture bound to one attempt.
///
/// The manifest carries full hashes and the full capture id. Every
/// lookup compares those stored values, so a tampered tree fails here
/// instead of validating against the wrong change list.
fn capture_change_ids(root: &Path, manifest: &AttemptRecord) -> anyhow::Result<HashSet<String>> {
    let pair = layout::pair_dir(root, &manifest.old_spec_hash, &manifest.new_spec_hash)?;
    layout::check_pair_identity(&pair, &manifest.old_spec_hash, &manifest.new_spec_hash)?;
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    layout::check_capture_identity(&capture, &manifest.capture_id)?;
    let changes_path = layout::changes_file(&capture);
    let raw = std::fs::read(&changes_path)
        .with_context(|| format!("read capture change list {}", changes_path.display()))?;
    let document = crate::vimanam::parse_diff_output(&raw)?;
    Ok(document
        .changes
        .iter()
        .map(|item| item.id.clone())
        .collect())
}
