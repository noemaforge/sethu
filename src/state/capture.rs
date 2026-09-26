//! Captures: immutable diff output bound to one pair.
//!
//! A capture binds a pair to the exact generator and invocation that
//! produced its change list. The capture id hashes that binding, so a new
//! generator version or a new invocation lands in a new directory. Stored
//! captures never change. Reuse checks the stored record and its change
//! list before returning the existing directory.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::vimanam::DiffDocument;

use super::layout;
use super::{SCHEMA_VERSION, check_schema_version, read_state_file, write_state_file};

/// Capture record stored as `capture.json` inside a capture directory.
///
/// Shared field names let older readers still parse the id they need.
/// Extra fields stay invisible to them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRecord {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Full id of the capture. Never a prefix alone.
    pub capture_id: String,
    /// Name of the generator, always `vimanam`.
    pub generator_name: String,
    /// Version of the generator that produced the change list.
    pub generator_version: String,
    /// Exact argument list passed to the generator.
    pub invocation: Vec<String>,
    /// Schema version reported inside the generator output.
    pub vimanam_schema_version: u32,
    /// Full hash of the old spec.
    pub old_spec_hash: String,
    /// Full hash of the new spec.
    pub new_spec_hash: String,
}

impl CaptureRecord {
    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "capture")
    }
}

/// Inputs that identify and fill one capture.
///
/// Callers compute the id with `capture_id_for` first, so creating and
/// reporting share one value.
pub struct NewCapture<'a> {
    /// Full capture id from `capture_id_for`.
    pub capture_id: &'a str,
    /// Name of the generator, always `vimanam`.
    pub generator_name: &'a str,
    /// Version of the generator that produced the change list.
    pub generator_version: &'a str,
    /// Exact argument list passed to the generator.
    pub invocation: &'a [String],
    /// Schema version reported inside the generator output.
    pub vimanam_schema_version: u32,
    /// Full hash of the old spec.
    pub old_spec_hash: &'a str,
    /// Full hash of the new spec.
    pub new_spec_hash: &'a str,
    /// Parsed diff that the bytes encode.
    pub changes: &'a DiffDocument,
    /// Canonical bytes stored as `changes.json`.
    pub changes_bytes: &'a [u8],
}

/// Derive a capture id from its generator binding.
///
/// The id hashes the generator name and version, the full invocation,
/// the generator schema version, and both spec hashes. Length prefixes
/// frame each part so no two bindings share one fingerprint.
pub fn capture_id_for(
    generator_name: &str,
    generator_version: &str,
    invocation: &[String],
    vimanam_schema_version: u32,
    old_hash: &str,
    new_hash: &str,
) -> String {
    let mut framed = String::new();
    for part in [
        generator_name,
        generator_version,
        &vimanam_schema_version.to_string(),
        old_hash,
        new_hash,
    ] {
        push_framed(&mut framed, part);
    }
    push_framed(&mut framed, &invocation.len().to_string());
    for arg in invocation {
        push_framed(&mut framed, arg);
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

/// Store a capture or reuse the one already stored.
///
/// Reuse needs the pair, generator version, and invocation to match, as
/// proven by the id, plus a stored change list that parses and carries
/// the same change ids in the same order. Anything else fails loudly.
/// This function never rewrites a stored record. The change list lands
/// before the record, so a stored record always implies a complete list.
pub fn find_or_create_capture(
    pair_dir: &Path,
    fresh: &NewCapture<'_>,
) -> anyhow::Result<(PathBuf, bool)> {
    let dir = layout::capture_dir(pair_dir, fresh.capture_id);
    let record_path = layout::capture_file(&dir);
    let changes_path = layout::changes_file(&dir);
    if record_path.is_file() {
        layout::check_capture_identity(&dir, fresh.capture_id)?;
        let stored: CaptureRecord = read_state_file(&record_path)?;
        stored.validate()?;
        if stored.generator_name != fresh.generator_name
            || stored.generator_version != fresh.generator_version
            || stored.invocation != fresh.invocation
            || stored.vimanam_schema_version != fresh.vimanam_schema_version
            || stored.old_spec_hash != fresh.old_spec_hash
            || stored.new_spec_hash != fresh.new_spec_hash
        {
            anyhow::bail!(
                "capture directory {} stores a different generator binding in {}",
                dir.display(),
                record_path.display()
            );
        }
        let kept = std::fs::read(&changes_path)
            .with_context(|| format!("read stored change list {}", changes_path.display()))?;
        check_reused_changes(&kept, fresh.changes, fresh.generator_version)?;
        return Ok((dir, false));
    }
    // No record claims this id yet, so writing stays safe even when an
    // interrupted first attempt left a change list behind.
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create capture directory {}", dir.display()))?;
    super::atomic::write_atomic(&changes_path, fresh.changes_bytes)?;
    let record = CaptureRecord {
        schema_version: SCHEMA_VERSION,
        capture_id: fresh.capture_id.to_string(),
        generator_name: fresh.generator_name.to_string(),
        generator_version: fresh.generator_version.to_string(),
        invocation: fresh.invocation.to_vec(),
        vimanam_schema_version: fresh.vimanam_schema_version,
        old_spec_hash: fresh.old_spec_hash.to_string(),
        new_spec_hash: fresh.new_spec_hash.to_string(),
    };
    write_state_file(&record_path, &record)?;
    Ok((dir, true))
}

/// Check a stored change list against a fresh diff.
///
/// The stored bytes must parse with the same generator version and the
/// same change ids in the same order. Records stay exactly as reported.
fn check_reused_changes(
    kept: &[u8],
    fresh: &DiffDocument,
    generator_version: &str,
) -> anyhow::Result<()> {
    let stored = crate::vimanam::parse_diff_output(kept)?;
    if stored.generator.version != generator_version {
        anyhow::bail!(
            "stored change list reports generator version {}, not {generator_version}",
            stored.generator.version
        );
    }
    let kept_ids: Vec<&str> = stored.changes.iter().map(|item| item.id.as_str()).collect();
    let fresh_ids: Vec<&str> = fresh.changes.iter().map(|item| item.id.as_str()).collect();
    if kept_ids != fresh_ids {
        anyhow::bail!(
            "stored change list holds {} changes, the fresh diff holds {}",
            kept_ids.len(),
            fresh_ids.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vimanam::{DiffSummary, Generator, SpecSide};

    fn empty_document(version: &str) -> DiffDocument {
        DiffDocument {
            schema_version: 1,
            generator: Generator {
                name: "vimanam".to_string(),
                version: version.to_string(),
            },
            old: SpecSide {
                title: "Demo".to_string(),
                version: "1".to_string(),
                file_sha256: "a".repeat(64),
            },
            new: SpecSide {
                title: "Demo".to_string(),
                version: "2".to_string(),
                file_sha256: "b".repeat(64),
            },
            summary: DiffSummary {
                endpoints_added: 0,
                endpoints_removed: 0,
                endpoints_changed: 0,
                breaking: 0,
                non_breaking: 0,
                review: 0,
            },
            changes: Vec::new(),
        }
    }

    fn invocation() -> Vec<String> {
        vec![
            "vimanam".to_string(),
            "diff".to_string(),
            "old.json".to_string(),
            "new.json".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ]
    }

    #[test]
    fn capture_id_is_stable_and_sensitive() {
        let args = invocation();
        let first = capture_id_for(
            "vimanam",
            "1.3.0",
            &args,
            1,
            &"a".repeat(64),
            &"b".repeat(64),
        );
        let same = capture_id_for(
            "vimanam",
            "1.3.0",
            &args,
            1,
            &"a".repeat(64),
            &"b".repeat(64),
        );
        assert_eq!(first, same);
        assert_eq!(first.len(), 64);
        let newer = capture_id_for(
            "vimanam",
            "1.4.0",
            &args,
            1,
            &"a".repeat(64),
            &"b".repeat(64),
        );
        assert_ne!(first, newer);
        let mut moved = args.clone();
        moved[2] = "elsewhere.json".to_string();
        let renamed = capture_id_for(
            "vimanam",
            "1.3.0",
            &moved,
            1,
            &"a".repeat(64),
            &"b".repeat(64),
        );
        assert_ne!(first, renamed);
    }

    #[test]
    fn create_then_reuse_keeps_stored_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = layout::state_root(dir.path());
        let old_hash = "a".repeat(64);
        let new_hash = "b".repeat(64);
        let pair_dir = layout::pair_dir(&root, &old_hash, &new_hash).unwrap();
        std::fs::create_dir_all(&pair_dir).unwrap();

        let document = empty_document("1.3.0");
        let mut bytes = serde_json::to_vec_pretty(&document).unwrap();
        bytes.push(b'\n');
        let args = invocation();
        let id = capture_id_for("vimanam", "1.3.0", &args, 1, &old_hash, &new_hash);
        let fresh = NewCapture {
            capture_id: &id,
            generator_name: "vimanam",
            generator_version: "1.3.0",
            invocation: &args,
            vimanam_schema_version: 1,
            old_spec_hash: &old_hash,
            new_spec_hash: &new_hash,
            changes: &document,
            changes_bytes: &bytes,
        };
        let (first, created) = find_or_create_capture(&pair_dir, &fresh).unwrap();
        assert!(created);
        let (second, created) = find_or_create_capture(&pair_dir, &fresh).unwrap();
        assert!(!created);
        assert_eq!(first, second);
        assert_eq!(std::fs::read(layout::changes_file(&first)).unwrap(), bytes);

        let stored: CaptureRecord = read_state_file(&layout::capture_file(&first)).unwrap();
        stored.validate().unwrap();
        assert_eq!(stored.capture_id, id);
    }
}
