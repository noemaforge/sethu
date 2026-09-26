//! Implementation of `sethu record`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;

use crate::cli::{RecordArgs, UsageError};
use crate::commands::Status;
use crate::state::attempt::AttemptRecord;
use crate::state::layout;
use crate::state::ledger;

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
    let repo = working_repo()?;
    let root = layout::state_root(&repo);
    let migration = sole_migration(&root)?;
    run_on(&repo, &migration, args)
}

/// Record into one migration that dispatch already resolved.
///
/// Validation, evidence rules, history handling, and atomic writes
/// behave exactly as the plain entry point. Only the lookup differs.
/// Dispatch calls this after it turns the attempt selector into one
/// migration directory.
pub fn run_on(repo: &Path, migration: &Path, args: &RecordArgs) -> anyhow::Result<ExitCode> {
    let root = layout::state_root(repo);
    let manifest: AttemptRecord = crate::state::read_state_file(&layout::manifest_path(migration))?;
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
        checked.push(ledger::check_reference(repo, reference)?);
    }
    let summary = ledger::summarize(&checked);
    ledger::check_evidence(outcome, &summary, args.note.as_deref())?;

    let mut stored = ledger::load_ledger(migration)?;
    stored.record(
        &args.id,
        ledger::Entry::new(outcome, args.evidence.clone(), args.note.clone()),
    );
    crate::state::write_state_file(&layout::ledger_path(migration), &stored)?;

    println!("recorded {} {outcome}", args.id);
    Ok(ExitCode::SUCCESS)
}

/// Resolve the working repository from the current directory.
///
/// The path is canonicalised, so later lookups compare one spelling.
/// Failures name the directory that could not be read or resolved.
pub fn working_repo() -> anyhow::Result<PathBuf> {
    let repo = std::env::current_dir().with_context(|| "read working directory for `record`")?;
    repo.canonicalize()
        .with_context(|| format!("resolve working directory {}", repo.display()))
}

/// Resolve the migration that a command operates on.
///
/// A selector resolves by full id or unique prefix through the shared
/// state lookup. The stored manifest confirms the full id, so a
/// tampered directory fails instead of recording into the wrong
/// attempt. An unknown value fails as a usage error naming the value.
/// An ambiguous prefix fails as a usage error naming every candidate.
/// Without a selector the single stored migration wins, and zero or
/// several fail exactly as before.
pub fn resolve_migration(root: &Path, selector: Option<&str>) -> anyhow::Result<PathBuf> {
    let Some(wanted) = selector else {
        return sole_migration(root);
    };
    if wanted.is_empty() {
        return Err(UsageError::new("attempt selection needs a non-empty id or prefix").into());
    }
    let dir = layout::migrations_dir(root);
    let names = list_migration_names(&dir)?;
    if names.iter().any(|name| name == wanted) {
        let migration = dir.join(wanted);
        layout::check_manifest_identity(&migration, wanted)?;
        return Ok(migration);
    }
    let hits: Vec<&str> = names
        .iter()
        .filter(|name| name.starts_with(wanted))
        .map(String::as_str)
        .collect();
    match hits.as_slice() {
        [] => Err(UsageError::new(format!(
            "unknown attempt `{wanted}`, no migration under {} starts with it",
            dir.display()
        ))
        .into()),
        [single] => {
            let migration = dir.join(single);
            layout::check_manifest_identity(&migration, single)?;
            Ok(migration)
        }
        _ => {
            let candidates = hits
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            Err(UsageError::new(format!(
                "attempt prefix `{wanted}` is ambiguous under {}, it matches {candidates}",
                dir.display()
            ))
            .into())
        }
    }
}

/// Find the single migration under a state root.
///
/// Zero migrations means nothing was initialised here. Several means
/// the choice is ambiguous, and this command refuses to guess. Both
/// cases fail with the directory named.
fn sole_migration(root: &Path) -> anyhow::Result<PathBuf> {
    let dir = layout::migrations_dir(root);
    let names = list_migration_names(&dir)?;
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

/// List stored migration names in sorted order.
///
/// A missing directory reads as empty. Pending temp files stay
/// excluded, so interrupted writers never appear in lookups.
fn list_migration_names(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    match std::fs::read_dir(dir) {
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
    Ok(names)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a state root with named migrations holding valid manifests.
    fn root_with_attempts(names: &[&str]) -> tempfile::TempDir {
        let scratch = tempfile::tempdir().unwrap();
        let root = layout::state_root(scratch.path());
        for name in names {
            let migration = layout::migration_dir(&root, name);
            std::fs::create_dir_all(&migration).unwrap();
            let manifest =
                crate::state::MigrationManifest::new(name, &"a".repeat(64), &"b".repeat(64));
            crate::state::write_state_file(&layout::manifest_path(&migration), &manifest).unwrap();
        }
        scratch
    }

    #[test]
    fn selector_resolves_full_ids_and_unique_prefixes() {
        let scratch = root_with_attempts(&["alpha-one", "alpha-two", "beta-one"]);
        let root = layout::state_root(scratch.path());

        let exact = resolve_migration(&root, Some("alpha-one")).unwrap();
        assert_eq!(exact.file_name().unwrap(), "alpha-one");

        let prefixed = resolve_migration(&root, Some("beta")).unwrap();
        assert_eq!(prefixed.file_name().unwrap(), "beta-one");
    }

    #[test]
    fn unknown_selector_names_the_value() {
        let scratch = root_with_attempts(&["alpha-one"]);
        let root = layout::state_root(scratch.path());

        let err = resolve_migration(&root, Some("deadbeef")).unwrap_err();
        assert!(err.downcast_ref::<UsageError>().is_some());
        let text = format!("{err:#}");
        assert!(text.contains("unknown attempt"));
        assert!(text.contains("deadbeef"));
    }

    #[test]
    fn ambiguous_prefix_names_every_candidate() {
        let scratch = root_with_attempts(&["alpha-one", "alpha-two", "beta-one"]);
        let root = layout::state_root(scratch.path());

        let err = resolve_migration(&root, Some("alpha")).unwrap_err();
        assert!(err.downcast_ref::<UsageError>().is_some());
        let text = format!("{err:#}");
        assert!(text.contains("ambiguous"));
        assert!(text.contains("alpha-one"));
        assert!(text.contains("alpha-two"));
        assert!(!text.contains("beta-one"));
    }

    #[test]
    fn empty_selector_is_a_usage_error() {
        let scratch = root_with_attempts(&["alpha-one"]);
        let root = layout::state_root(scratch.path());

        let err = resolve_migration(&root, Some("")).unwrap_err();
        assert!(err.downcast_ref::<UsageError>().is_some());
        assert!(format!("{err:#}").contains("non-empty"));
    }

    #[test]
    fn absent_selector_keeps_the_single_migration_fallback() {
        let scratch = root_with_attempts(&["alpha-one", "alpha-two"]);
        let root = layout::state_root(scratch.path());

        let err = resolve_migration(&root, None).unwrap_err();
        assert!(err.downcast_ref::<UsageError>().is_none());
        assert!(format!("{err:#}").contains("needs exactly one"));

        let single = root_with_attempts(&["alpha-one"]);
        let single_root = layout::state_root(single.path());
        let migration = resolve_migration(&single_root, None).unwrap();
        assert_eq!(migration.file_name().unwrap(), "alpha-one");
    }
}
