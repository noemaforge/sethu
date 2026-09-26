//! Migration attempts: one capture bound to one consumer state.
//!
//! An attempt references one capture plus the consumer repository path,
//! baseline commit, declared scope, and binary version. The attempt id
//! hashes all of those, so any change lands in a new directory. Resume
//! needs every field to match exactly. Reinitialising never merges
//! attempts and never touches a stored ledger.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::layout;
use super::{SCHEMA_VERSION, check_schema_version, read_state_file, write_state_file};

/// Attempt manifest stored as `manifest.json` in a migration directory.
///
/// Shared field names let older readers still parse the attempt id and
/// spec hashes they need. Extra fields stay invisible to them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Full id of the attempt. Never a prefix alone.
    pub attempt_id: String,
    /// Full hash of the old spec. Never a prefix alone.
    pub old_spec_hash: String,
    /// Full hash of the new spec. Never a prefix alone.
    pub new_spec_hash: String,
    /// Full id of the capture this attempt is bound to.
    pub capture_id: String,
    /// Canonical absolute path of the consumer repository.
    pub repo_path: String,
    /// Commit read as the consumer baseline.
    pub baseline_commit: String,
    /// Declared scope paths in sorted order.
    pub scope: Vec<String>,
    /// Version of the binary that created the attempt.
    pub sethu_version: String,
}

impl AttemptRecord {
    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "manifest")
    }
}

/// Inputs that identify and fill one attempt.
///
/// Callers compute the id with `attempt_id_for` first, so creating and
/// reporting share one value.
pub struct NewAttempt<'a> {
    /// Full attempt id from `attempt_id_for`.
    pub attempt_id: &'a str,
    /// Full hash of the old spec.
    pub old_spec_hash: &'a str,
    /// Full hash of the new spec.
    pub new_spec_hash: &'a str,
    /// Full id of the capture this attempt binds.
    pub capture_id: &'a str,
    /// Canonical absolute path of the consumer repository.
    pub repo_path: &'a str,
    /// Commit read as the consumer baseline.
    pub baseline_commit: &'a str,
    /// Declared scope paths in sorted order.
    pub scope: &'a [String],
    /// Version of the binary that creates the attempt.
    pub sethu_version: &'a str,
}

/// Derive an attempt id from its full binding.
///
/// The id hashes both spec hashes, the capture id, the repository path,
/// the baseline commit, the scope list, and the binary version. Length
/// prefixes frame each part so no two bindings share one fingerprint.
#[allow(clippy::too_many_arguments)]
pub fn attempt_id_for(
    old_hash: &str,
    new_hash: &str,
    capture_id: &str,
    repo_path: &str,
    baseline: &str,
    scope: &[String],
    sethu_version: &str,
) -> String {
    let mut framed = String::new();
    for part in [
        old_hash,
        new_hash,
        capture_id,
        repo_path,
        baseline,
        sethu_version,
    ] {
        push_framed(&mut framed, part);
    }
    push_framed(&mut framed, &scope.len().to_string());
    for item in scope {
        push_framed(&mut framed, item);
    }
    layout::sha256_hex(framed.as_bytes())
}

/// Append one length prefixed part to a fingerprint.
fn push_framed(framed: &mut String, part: &str) {
    framed.push_str(&part.len().to_string());
    framed.push(':');
    framed.push_str(part);
    framed.push('\n');
}

/// Report the binary version that creates attempts.
///
/// The value comes from the package manifest at compile time.
pub fn current_sethu_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Canonicalise the consumer repository path.
///
/// Absolute paths keep reruns with different spellings on one attempt id.
/// The error names the path that failed to resolve.
pub fn canonical_repo_path(repo: &Path) -> anyhow::Result<PathBuf> {
    repo.canonicalize()
        .with_context(|| format!("resolve consumer repository {}", repo.display()))
}

/// Read the consumer baseline commit.
///
/// The command runs `git rev-parse HEAD` inside the repository and keeps
/// its output. Outside a repository it fails with the path named. Every
/// spawn sets its working directory and captures both streams.
pub fn read_baseline_commit(repo: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(repo)
        .output()
        .with_context(|| format!("run `git rev-parse HEAD` in {}", repo.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        anyhow::bail!(
            "cannot read consumer baseline commit in {}: {}",
            repo.display(),
            if detail.is_empty() {
                "not a git repository"
            } else {
                detail
            }
        );
    }
    let text = String::from_utf8(output.stdout)
        .with_context(|| format!("read baseline commit in {}", repo.display()))?;
    let commit = text.trim().to_string();
    if commit.is_empty() {
        anyhow::bail!("empty baseline commit in {}", repo.display());
    }
    Ok(commit)
}

/// Normalise declared scope paths.
///
/// Output sorts and dedupes the given spellings, so reordered flags
/// resume the same attempt. An empty scope names the whole repository.
pub fn normalize_scope(scope: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = scope
        .iter()
        .map(|item| item.display().to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Store an attempt or resume the one already stored.
///
/// Resume compares the full binding field by field. Any stored mismatch
/// fails loudly instead of overwriting. New attempts never touch other
/// attempts or their ledgers.
pub fn find_or_create_attempt(
    root: &Path,
    fresh: &NewAttempt<'_>,
) -> anyhow::Result<(PathBuf, bool)> {
    let dir = layout::migration_dir(root, fresh.attempt_id);
    let file = layout::manifest_path(&dir);
    if file.is_file() {
        layout::check_manifest_identity(&dir, fresh.attempt_id)?;
        let stored: AttemptRecord = read_state_file(&file)?;
        stored.validate()?;
        if stored.old_spec_hash != fresh.old_spec_hash
            || stored.new_spec_hash != fresh.new_spec_hash
            || stored.capture_id != fresh.capture_id
            || stored.repo_path != fresh.repo_path
            || stored.baseline_commit != fresh.baseline_commit
            || stored.scope != fresh.scope
            || stored.sethu_version != fresh.sethu_version
        {
            anyhow::bail!(
                "migration directory {} stores a different binding in {}",
                dir.display(),
                file.display()
            );
        }
        return Ok((dir, false));
    }
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create migration directory {}", dir.display()))?;
    let record = AttemptRecord {
        schema_version: SCHEMA_VERSION,
        attempt_id: fresh.attempt_id.to_string(),
        old_spec_hash: fresh.old_spec_hash.to_string(),
        new_spec_hash: fresh.new_spec_hash.to_string(),
        capture_id: fresh.capture_id.to_string(),
        repo_path: fresh.repo_path.to_string(),
        baseline_commit: fresh.baseline_commit.to_string(),
        scope: fresh.scope.to_vec(),
        sethu_version: fresh.sethu_version.to_string(),
    };
    write_state_file(&file, &record)?;
    Ok((dir, true))
}

/// List stored attempts for one spec pair.
///
/// Output sorts by attempt id. Manifests that fail to parse stay skipped
/// with a stderr note, so one broken directory never hides the rest.
pub fn find_attempts_for_pair(
    root: &Path,
    old_hash: &str,
    new_hash: &str,
) -> anyhow::Result<Vec<(PathBuf, AttemptRecord)>> {
    let dir = layout::migrations_dir(root);
    let mut found = Vec::new();
    for name in list_child_dirs(&dir)? {
        let migration = dir.join(&name);
        let file = layout::manifest_path(&migration);
        let stored: AttemptRecord = match read_state_file(&file) {
            Ok(value) => value,
            Err(err) => {
                eprintln!(
                    "warning: skip unreadable attempt manifest {}: {err:#}",
                    file.display()
                );
                continue;
            }
        };
        if stored.validate().is_err() {
            eprintln!(
                "warning: skip attempt manifest with unknown version {}",
                file.display()
            );
            continue;
        }
        if stored.old_spec_hash == old_hash && stored.new_spec_hash == new_hash {
            found.push((migration, stored));
        }
    }
    found.sort_by(|left, right| left.1.attempt_id.cmp(&right.1.attempt_id));
    Ok(found)
}

/// List child directory names in sorted order.
///
/// A missing directory reads as empty. Pending temp files stay excluded,
/// so interrupted writers never appear in lookups.
fn list_child_dirs(dir: &Path) -> anyhow::Result<Vec<String>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_normalises_order_and_dupes() {
        let scope = vec![
            PathBuf::from("tests"),
            PathBuf::from("src"),
            PathBuf::from("src"),
        ];
        assert_eq!(normalize_scope(&scope), vec!["src", "tests"]);
        assert!(normalize_scope(&[]).is_empty());
    }

    #[test]
    fn attempt_id_is_stable_and_sensitive() {
        let scope = vec!["src".to_string()];
        let first = attempt_id_for(
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
            "/repo",
            "abc123",
            &scope,
            "0.1.0",
        );
        let same = attempt_id_for(
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
            "/repo",
            "abc123",
            &scope,
            "0.1.0",
        );
        assert_eq!(first, same);
        assert_eq!(first.len(), 64);
        let moved = attempt_id_for(
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
            "/repo",
            "def456",
            &scope,
            "0.1.0",
        );
        assert_ne!(first, moved);
        let rescoped = attempt_id_for(
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
            "/repo",
            "abc123",
            &[],
            "0.1.0",
        );
        assert_ne!(first, rescoped);
    }

    #[test]
    fn baseline_fails_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_baseline_commit(dir.path()).unwrap_err();
        assert!(err.to_string().contains("baseline"));
    }

    #[test]
    fn create_then_resume_compares_full_binding() {
        let dir = tempfile::tempdir().unwrap();
        let root = layout::state_root(dir.path());
        let scope = vec!["src".to_string()];
        let id = attempt_id_for(
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
            "/repo",
            "abc123",
            &scope,
            "0.1.0",
        );
        let fresh = NewAttempt {
            attempt_id: &id,
            old_spec_hash: &"a".repeat(64),
            new_spec_hash: &"b".repeat(64),
            capture_id: &"c".repeat(64),
            repo_path: "/repo",
            baseline_commit: "abc123",
            scope: &scope,
            sethu_version: "0.1.0",
        };
        let (first, created) = find_or_create_attempt(&root, &fresh).unwrap();
        assert!(created);
        let (second, created) = find_or_create_attempt(&root, &fresh).unwrap();
        assert!(!created);
        assert_eq!(first, second);

        let stored: AttemptRecord = read_state_file(&layout::manifest_path(&first)).unwrap();
        stored.validate().unwrap();
        assert_eq!(stored.attempt_id, id);

        let listed = find_attempts_for_pair(&root, &"a".repeat(64), &"b".repeat(64)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.attempt_id, id);
        let other = find_attempts_for_pair(&root, &"a".repeat(64), &"d".repeat(64)).unwrap();
        assert!(other.is_empty());
    }
}
