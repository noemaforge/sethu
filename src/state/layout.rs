//! Paths and lookups for the state tree.
//!
//! All state lives under a `.sethu` directory at the repository root. Pair
//! directories carry short hash prefixes in their names. Every lookup reads the
//! full hash stored inside the files and compares it. Prefix collisions fail
//! loudly instead of merging silently.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// Name of the state directory at the repository root.
///
/// Every helper builds on this directory. Nothing writes outside it.
pub const STATE_DIR_NAME: &str = ".sethu";

/// File name of the installation record at the state root.
///
/// The file proves that the tree belongs to this tool.
pub const INSTALLATION_FILE: &str = "installation.json";

/// Directory name holding one entry per spec pair.
///
/// Each entry pairs an old spec hash with a new spec hash.
pub const PAIRS_DIR_NAME: &str = "pairs";

/// File name of the pair record inside a pair directory.
///
/// The record stores both full spec hashes for later comparison.
pub const PAIR_FILE_NAME: &str = "pair.json";

/// Directory name holding the input specs of a pair.
///
/// Later commands copy both specs here for provenance.
pub const INPUTS_DIR_NAME: &str = "inputs";

/// Directory name holding one entry per capture of a pair.
///
/// Each entry records one diff run against the pair.
pub const CAPTURES_DIR_NAME: &str = "captures";

/// File name of the capture record inside a capture directory.
///
/// The record stores the full capture id for later comparison.
pub const CAPTURE_FILE_NAME: &str = "capture.json";

/// File name of the stored change list inside a capture directory.
///
/// Later commands read this list to enumerate open changes.
pub const CHANGES_FILE_NAME: &str = "changes.json";

/// File name of the stored origins inside a capture directory.
///
/// The file maps each change id to its origin.
pub const ORIGINS_FILE_NAME: &str = "origins.json";

/// Directory name holding one entry per migration attempt.
///
/// Each entry tracks one upgrade attempt from start to report.
pub const MIGRATIONS_DIR_NAME: &str = "migrations";

/// File name of the attempt manifest inside a migration directory.
///
/// The manifest stores the full attempt id for later comparison.
pub const MANIFEST_FILE_NAME: &str = "manifest.json";

/// File name of the outcome ledger inside a migration directory.
///
/// Only the record flow writes this file. The check flow reads it back.
pub const LEDGER_FILE_NAME: &str = "ledger.json";

/// Directory name holding tracer output inside a migration directory.
///
/// Tracers stay read only, so this tree only holds their reports.
pub const TRACES_DIR_NAME: &str = "traces";

/// Directory name holding prepared change context inside a migration.
///
/// The context flow writes one file per change group here.
pub const CONTEXT_DIR_NAME: &str = "context";

/// Directory name holding harness snapshots inside a migration.
///
/// Verification freezes the harness hash here before repair starts.
pub const HARNESS_DIR_NAME: &str = "harness";

/// Directory name holding one entry per verification run.
///
/// Each run keeps its artefacts apart from other runs.
pub const RUNS_DIR_NAME: &str = "runs";

/// Directory name holding generated reports inside a migration.
///
/// The report flow writes its output here.
pub const REPORTS_DIR_NAME: &str = "reports";

/// Number of hash characters used in directory names.
///
/// Twelve hex characters carry 48 bits. Lookups still compare full hashes.
pub const HASH_PREFIX_LEN: usize = 12;

/// Resolve the state root for a repository.
///
/// The root is the repository path joined with the state directory name.
pub fn state_root(repo: &Path) -> PathBuf {
    repo.join(STATE_DIR_NAME)
}

/// Resolve the installation record path for a state root.
pub fn installation_path(root: &Path) -> PathBuf {
    root.join(INSTALLATION_FILE)
}

/// Resolve the pairs directory for a state root.
pub fn pairs_dir(root: &Path) -> PathBuf {
    root.join(PAIRS_DIR_NAME)
}

/// Resolve the migrations directory for a state root.
pub fn migrations_dir(root: &Path) -> PathBuf {
    root.join(MIGRATIONS_DIR_NAME)
}

/// Shorten a full hash to its directory prefix.
///
/// The prefix holds the first twelve characters in lowercase. The function
/// refuses short values and non hexadecimal values.
pub fn hash_prefix(full_hash: &str) -> anyhow::Result<String> {
    let prefix = match full_hash.get(..HASH_PREFIX_LEN) {
        Some(part) => part,
        None => anyhow::bail!("hash is shorter than expected {full_hash}"),
    };
    if !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("hash prefix is not hexadecimal {prefix}");
    }
    Ok(prefix.to_ascii_lowercase())
}

/// Hash bytes with SHA-256 and render the full lowercase hex digest.
///
/// Writers store the full digest inside state files. Directory names use the
/// twelve character prefix. Lookups compare the full stored digest.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(data);
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// Build a pair directory name from two full spec hashes.
///
/// The name joins the old prefix and the new prefix with a dash.
pub fn pair_dir_name(old_full: &str, new_full: &str) -> anyhow::Result<String> {
    Ok(format!(
        "{}-{}",
        hash_prefix(old_full)?,
        hash_prefix(new_full)?
    ))
}

/// Resolve a pair directory for a state root and two full spec hashes.
///
/// The path is deterministic. Callers verify the stored full hashes after.
pub fn pair_dir(root: &Path, old_full: &str, new_full: &str) -> anyhow::Result<PathBuf> {
    Ok(pairs_dir(root).join(pair_dir_name(old_full, new_full)?))
}

/// Resolve the pair record path inside a pair directory.
pub fn pair_file(pair: &Path) -> PathBuf {
    pair.join(PAIR_FILE_NAME)
}

/// Resolve the input specs directory inside a pair directory.
pub fn pair_inputs_dir(pair: &Path) -> PathBuf {
    pair.join(INPUTS_DIR_NAME)
}

/// Resolve the captures directory inside a pair directory.
pub fn pair_captures_dir(pair: &Path) -> PathBuf {
    pair.join(CAPTURES_DIR_NAME)
}

/// Resolve a capture directory inside a pair.
///
/// The directory name is the full capture id.
pub fn capture_dir(pair: &Path, capture_id: &str) -> PathBuf {
    pair_captures_dir(pair).join(capture_id)
}

/// Resolve the capture record path inside a capture directory.
pub fn capture_file(capture: &Path) -> PathBuf {
    capture.join(CAPTURE_FILE_NAME)
}

/// Resolve the change list path inside a capture directory.
pub fn changes_file(capture: &Path) -> PathBuf {
    capture.join(CHANGES_FILE_NAME)
}

/// Resolve the origins path inside a capture directory.
pub fn origins_file(capture: &Path) -> PathBuf {
    capture.join(ORIGINS_FILE_NAME)
}

/// Resolve a migration directory for a state root and a full attempt id.
///
/// The directory name is the full attempt id. Lookups accept a unique prefix.
pub fn migration_dir(root: &Path, attempt_id: &str) -> PathBuf {
    migrations_dir(root).join(attempt_id)
}

/// Resolve the manifest path inside a migration directory.
pub fn manifest_path(migration: &Path) -> PathBuf {
    migration.join(MANIFEST_FILE_NAME)
}

/// Resolve the ledger path inside a migration directory.
pub fn ledger_path(migration: &Path) -> PathBuf {
    migration.join(LEDGER_FILE_NAME)
}

/// Resolve the tracer output directory inside a migration directory.
pub fn traces_dir(migration: &Path) -> PathBuf {
    migration.join(TRACES_DIR_NAME)
}

/// Resolve the prepared context directory inside a migration directory.
pub fn context_dir(migration: &Path) -> PathBuf {
    migration.join(CONTEXT_DIR_NAME)
}

/// Resolve the harness snapshot directory inside a migration directory.
pub fn harness_dir(migration: &Path) -> PathBuf {
    migration.join(HARNESS_DIR_NAME)
}

/// Resolve the runs directory inside a migration directory.
pub fn runs_dir(migration: &Path) -> PathBuf {
    migration.join(RUNS_DIR_NAME)
}

/// Resolve one run directory inside a migration directory.
///
/// The name is the full run id.
pub fn run_dir(migration: &Path, run_id: &str) -> PathBuf {
    runs_dir(migration).join(run_id)
}

/// Resolve the reports directory inside a migration directory.
pub fn reports_dir(migration: &Path) -> PathBuf {
    migration.join(REPORTS_DIR_NAME)
}

/// Find a pair directory by its two full spec hashes.
///
/// The function returns no match when the directory is absent. It reads the
/// stored pair record and compares both full hashes. A stored mismatch fails
/// instead of returning the wrong pair.
pub fn find_pair(root: &Path, old_full: &str, new_full: &str) -> anyhow::Result<Option<PathBuf>> {
    let dir = pair_dir(root, old_full, new_full)?;
    if !dir.is_dir() {
        return Ok(None);
    }
    check_pair_identity(&dir, old_full, new_full)?;
    Ok(Some(dir))
}

/// Verify that a pair directory stores the expected full spec hashes.
///
/// Writers call this check before reusing a directory. A mismatch means that
/// two different spec pairs share one directory prefix. The function fails
/// rather than merging them.
pub fn check_pair_identity(pair: &Path, old_full: &str, new_full: &str) -> anyhow::Result<()> {
    let file = pair_file(pair);
    let stored: super::PairFile = super::read_state_file(&file)?;
    if stored.old_spec_hash != old_full || stored.new_spec_hash != new_full {
        anyhow::bail!(
            "pair directory {} stores a different spec pair in {}",
            pair.display(),
            file.display()
        );
    }
    Ok(())
}

/// Find a capture directory by id or by unique prefix.
///
/// The function returns no match when nothing shares the prefix. It reads the
/// stored capture record and compares the full id. An ambiguous prefix fails.
pub fn find_capture(pair: &Path, wanted: &str) -> anyhow::Result<Option<PathBuf>> {
    let dir = pair_captures_dir(pair);
    let names = dir_names(&dir)?;
    let hit = match unique_match(&names, wanted, "capture", &dir)? {
        Some(name) => name,
        None => return Ok(None),
    };
    let capture = dir.join(&hit);
    check_capture_identity(&capture, &hit)?;
    Ok(Some(capture))
}

/// Verify that a capture directory stores the expected full capture id.
///
/// A mismatch means corruption or a lookup error. The function fails rather
/// than returning the wrong capture.
pub fn check_capture_identity(capture: &Path, capture_id: &str) -> anyhow::Result<()> {
    let file = capture_file(capture);
    let stored: super::CaptureFile = super::read_state_file(&file)?;
    if stored.capture_id != capture_id {
        anyhow::bail!(
            "capture directory {} stores a different capture in {}",
            capture.display(),
            file.display()
        );
    }
    Ok(())
}

/// Find a migration directory by id or by unique prefix.
///
/// The function returns no match when nothing shares the prefix. It reads the
/// stored manifest and compares the full attempt id. An ambiguous prefix fails.
pub fn find_migration(root: &Path, wanted: &str) -> anyhow::Result<Option<PathBuf>> {
    let dir = migrations_dir(root);
    let names = dir_names(&dir)?;
    let hit = match unique_match(&names, wanted, "attempt", &dir)? {
        Some(name) => name,
        None => return Ok(None),
    };
    let migration = dir.join(&hit);
    check_manifest_identity(&migration, &hit)?;
    Ok(Some(migration))
}

/// Verify that a migration directory stores the expected full attempt id.
///
/// A mismatch means corruption or a lookup error. The function fails rather
/// than returning the wrong attempt.
pub fn check_manifest_identity(migration: &Path, attempt_id: &str) -> anyhow::Result<()> {
    let file = manifest_path(migration);
    let stored: super::MigrationManifest = super::read_state_file(&file)?;
    if stored.attempt_id != attempt_id {
        anyhow::bail!(
            "migration directory {} stores a different attempt in {}",
            migration.display(),
            file.display()
        );
    }
    Ok(())
}

/// List child directory names in sorted order.
///
/// A missing directory reads as empty. Pending temp files stay excluded, so
/// interrupted writers never appear in lookups.
fn dir_names(dir: &Path) -> anyhow::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("list directory {}", dir.display()))
            );
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry in directory {}", dir.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if super::atomic::is_pending_temp(&path) {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|part| part.to_str()) {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Resolve one wanted id against known directory names.
///
/// An exact name wins at once. A prefix must match exactly one name. Zero
/// matches read as absent. Several matches fail as ambiguous.
fn unique_match(
    names: &[String],
    wanted: &str,
    kind: &str,
    dir: &Path,
) -> anyhow::Result<Option<String>> {
    if names.iter().any(|name| name.as_str() == wanted) {
        return Ok(Some(wanted.to_string()));
    }
    let mut hits = names
        .iter()
        .filter(|name| name.as_str().starts_with(wanted));
    let first = match hits.next() {
        Some(name) => name.clone(),
        None => return Ok(None),
    };
    if hits.next().is_some() {
        anyhow::bail!("{kind} prefix {wanted} is ambiguous in {}", dir.display());
    }
    Ok(Some(first))
}
