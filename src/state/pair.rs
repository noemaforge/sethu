//! Contract pairs: hashed specs, copied inputs, and provenance.
//!
//! A pair binds two raw spec files by their full SHA-256 hashes. The pair
//! directory name carries only short hash prefixes. Every lookup compares
//! the full hashes stored in `pair.json`. Provenance records what the
//! caller knew without extra flags. Git remote and commit come from the
//! spec directory when a repository holds it. Missing values stay marked
//! as unknown. Nothing here touches the network.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::layout;
use super::{SCHEMA_VERSION, check_schema_version, write_state_file};

/// Marker stored when a provenance value has no source.
///
/// Git remote and commit fall back to this marker outside a repository.
/// Readers treat it as missing information, never as a real value.
pub const UNKNOWN: &str = "unknown";

/// File name of the copied old spec inside a pair `inputs` directory.
///
/// Fixed names keep lookups deterministic when both specs share a name.
pub const INPUT_OLD_FILE: &str = "old.json";

/// File name of the copied new spec inside a pair `inputs` directory.
pub const INPUT_NEW_FILE: &str = "new.json";

/// Provenance of one input spec file.
///
/// The hash and size pin the exact bytes. Git fields name the repository
/// that held the file when one did. They hold `UNKNOWN` otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecProvenance {
    /// Path of the spec as passed on the command line.
    pub source: String,
    /// Full SHA-256 of the raw spec file bytes.
    pub file_sha256: String,
    /// Length of the raw spec file in bytes.
    pub file_bytes: u64,
    /// Origin remote URL, or `UNKNOWN` outside a repository.
    pub git_remote: String,
    /// Commit holding the file, or `UNKNOWN` outside a repository.
    pub git_commit: String,
}

/// Pair record stored as `pair.json` inside a pair directory.
///
/// Shared field names let older readers still parse the hashes they
/// need. Extra fields stay invisible to them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRecord {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Full hash of the old spec. Never a prefix alone.
    pub old_spec_hash: String,
    /// Full hash of the new spec. Never a prefix alone.
    pub new_spec_hash: String,
    /// Provenance of the old spec file.
    pub old_provenance: SpecProvenance,
    /// Provenance of the new spec file.
    pub new_provenance: SpecProvenance,
    /// Version of the binary that first stored the pair.
    pub sethu_version: String,
}

impl PairRecord {
    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "pair")
    }
}

/// Resolve the copied old spec inside a pair directory.
pub fn inputs_old_path(pair: &Path) -> PathBuf {
    layout::pair_inputs_dir(pair).join(INPUT_OLD_FILE)
}

/// Resolve the copied new spec inside a pair directory.
pub fn inputs_new_path(pair: &Path) -> PathBuf {
    layout::pair_inputs_dir(pair).join(INPUT_NEW_FILE)
}

/// Read one spec file as raw bytes.
///
/// The error names the file for both missing and unreadable inputs.
pub fn read_spec_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("read spec file {}", path.display()))
}

/// Refuse specs with references that point outside their own file.
///
/// Every `$ref` must start with `#`. Anything else needs snapshotted
/// content that this command does not build, so it fails with the file
/// and the offending reference named. Parsing accepts JSON and YAML.
pub fn reject_external_refs(bytes: &[u8], path: &Path) -> anyhow::Result<()> {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(json_err) => match serde_norway::from_slice(bytes) {
            Ok(value) => value,
            Err(_) => {
                return Err(anyhow::anyhow!(
                    "cannot parse spec file {}: {json_err}",
                    path.display()
                ));
            }
        },
    };
    let mut refs = Vec::new();
    collect_refs(&value, &mut refs);
    let external: Vec<&String> = refs.iter().filter(|item| !item.starts_with('#')).collect();
    if let Some(first) = external.first() {
        anyhow::bail!(
            "spec file {} uses external reference {first:?}, only local refs starting with `#` are supported ({} more)",
            path.display(),
            external.len() - 1
        );
    }
    Ok(())
}

/// Collect every `$ref` string in a parsed spec.
fn collect_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref"
                    && let Some(text) = child.as_str()
                {
                    out.push(text.to_string());
                }
                collect_refs(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        _ => {}
    }
}

/// Collect provenance for one spec file.
///
/// Hash and size come from the bytes already read. Git remote and commit
/// come from the file directory when a repository holds it. Failures
/// leave `UNKNOWN` markers instead of failing the command.
pub fn collect_provenance(arg: &Path, bytes: &[u8], hash: &str) -> SpecProvenance {
    let (remote, commit) = match absolute_parent(arg) {
        Some(dir) => (
            run_git(&dir, &["remote", "get-url", "origin"]),
            run_git(&dir, &["rev-parse", "HEAD"]),
        ),
        None => (None, None),
    };
    SpecProvenance {
        source: arg.display().to_string(),
        file_sha256: hash.to_string(),
        file_bytes: bytes.len() as u64,
        git_remote: remote.unwrap_or_else(|| UNKNOWN.to_string()),
        git_commit: commit.unwrap_or_else(|| UNKNOWN.to_string()),
    }
}

/// Resolve the absolute parent directory of a spec argument.
///
/// Relative arguments resolve against the current directory. A missing
/// parent reads as no anchor, which later becomes unknown provenance.
fn absolute_parent(arg: &Path) -> Option<PathBuf> {
    let absolute = if arg.is_absolute() {
        arg.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(arg)
    };
    absolute.parent().map(Path::to_path_buf)
}

/// Run one read only git query inside a directory.
///
/// The spawn sets its working directory and captures both streams. Any
/// failure reads as absent, so callers can mark provenance unknown.
fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Create a pair directory or reuse the one already stored.
///
/// The directory name joins the twelve character hash prefixes. Reuse
/// compares full hashes and never rewrites `pair.json`. Copied inputs
/// refresh from the given bytes when missing or changed. The return
/// value carries the directory and whether this call created it.
pub fn ensure_pair(
    root: &Path,
    old_arg: &Path,
    new_arg: &Path,
    old_bytes: &[u8],
    new_bytes: &[u8],
    old_hash: &str,
    new_hash: &str,
) -> anyhow::Result<(PathBuf, bool)> {
    let dir = layout::pair_dir(root, old_hash, new_hash)?;
    if layout::pair_file(&dir).is_file() {
        layout::check_pair_identity(&dir, old_hash, new_hash)?;
        refresh_inputs(&dir, old_bytes, new_bytes)?;
        return Ok((dir, false));
    }
    std::fs::create_dir_all(layout::pair_inputs_dir(&dir))
        .with_context(|| format!("create pair directory {}", dir.display()))?;
    let record = PairRecord {
        schema_version: SCHEMA_VERSION,
        old_spec_hash: old_hash.to_string(),
        new_spec_hash: new_hash.to_string(),
        old_provenance: collect_provenance(old_arg, old_bytes, old_hash),
        new_provenance: collect_provenance(new_arg, new_bytes, new_hash),
        sethu_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    write_state_file(&layout::pair_file(&dir), &record)?;
    refresh_inputs(&dir, old_bytes, new_bytes)?;
    Ok((dir, true))
}

/// Copy both specs into a pair `inputs` directory.
///
/// Files land through atomic writes. Matching copies stay untouched, so
/// a resume never rewrites identical bytes.
fn refresh_inputs(dir: &Path, old_bytes: &[u8], new_bytes: &[u8]) -> anyhow::Result<()> {
    let targets = [
        (inputs_old_path(dir), old_bytes),
        (inputs_new_path(dir), new_bytes),
    ];
    for (path, bytes) in targets {
        let stale = match std::fs::read(&path) {
            Ok(current) => current != bytes,
            Err(_) => true,
        };
        if stale {
            super::atomic::write_atomic(&path, bytes)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn accepts_local_refs_only() {
        let bytes = br##"{
            "openapi": "3.0.0",
            "components": {
                "schemas": {
                    "A": { "$ref": "#/components/schemas/B" },
                    "B": { "type": "object" }
                }
            }
        }"##;
        assert!(reject_external_refs(bytes, Path::new("spec.json")).is_ok());
    }

    #[test]
    fn refuses_remote_reference() {
        let bytes = br#"{
            "openapi": "3.0.0",
            "components": {
                "schemas": {
                    "A": { "$ref": "https://example.com/common.json#/B" }
                }
            }
        }"#;
        let err = reject_external_refs(bytes, Path::new("new.json")).unwrap_err();
        assert!(err.to_string().contains("external reference"));
        assert!(err.to_string().contains("new.json"));
    }

    #[test]
    fn refuses_relative_file_reference() {
        let bytes =
            br#"{"paths": {"/x": {"get": {"responses": {"200": {"$ref": "other.json"}}}}}}"#;
        let err = reject_external_refs(bytes, Path::new("old.json")).unwrap_err();
        assert!(err.to_string().contains("other.json"));
    }

    #[test]
    fn provenance_marks_unknown_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_file(&dir, "spec.json", b"{}");
        let provenance = collect_provenance(&path, b"{}", &"a".repeat(64));
        assert_eq!(provenance.git_remote, UNKNOWN);
        assert_eq!(provenance.git_commit, UNKNOWN);
        assert_eq!(provenance.file_bytes, 2);
    }

    #[test]
    fn ensure_pair_creates_then_reuses_without_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let root = super::super::layout::state_root(dir.path());
        let old = b"{\"openapi\": \"3.0.0\"}";
        let new = b"{\"openapi\": \"3.0.1\"}";
        let old_hash = layout::sha256_hex(old);
        let new_hash = layout::sha256_hex(new);
        let old_arg = Path::new("old.json");
        let new_arg = Path::new("new.json");

        let (first, created) =
            ensure_pair(&root, old_arg, new_arg, old, new, &old_hash, &new_hash).unwrap();
        assert!(created);
        let before = std::fs::read(layout::pair_file(&first)).unwrap();

        let (second, created) =
            ensure_pair(&root, old_arg, new_arg, old, new, &old_hash, &new_hash).unwrap();
        assert!(!created);
        assert_eq!(first, second);
        let after = std::fs::read(layout::pair_file(&second)).unwrap();
        assert_eq!(before, after);
        assert_eq!(std::fs::read(inputs_old_path(&first)).unwrap(), old);
        assert_eq!(std::fs::read(inputs_new_path(&first)).unwrap(), new);

        let stored: PairRecord = super::super::read_state_file(&layout::pair_file(&first)).unwrap();
        stored.validate().unwrap();
        assert_eq!(stored.old_spec_hash, old_hash);
        assert_eq!(stored.new_spec_hash, new_hash);
    }
}
