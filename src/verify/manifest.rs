//! Verification manifest, freeze records, and harness hashing.
//!
//! The manifest is plain JSON with its own schema version. Loading
//! resolves relative paths against the manifest directory and refuses
//! unknown versions, empty names, and role mistakes. Freezing records a
//! content hash over the sorted harness files so a later run can tell
//! whether the harness moved since the freeze.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Schema version stamped into manifests and freeze files.
pub const SCHEMA_VERSION: u32 = 1;

/// Suffix appended to the manifest path for the freeze file.
pub const FREEZE_SUFFIX: &str = ".frozen";

/// Directory name holding every verification run beside the manifest.
pub const RUNS_DIR_NAME: &str = "runs";

/// Role of one named check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Covers change IDs and follows green, red, green.
    Regression,
    /// Protects behaviour that must not change in any stage.
    Guard,
}

/// Expected diagnostic for the red stage of a regression check.
///
/// A plain string must appear verbatim in the failure output. A regex
/// object must match somewhere in the same output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExpectedDiagnostic {
    /// Verbatim substring of the failure output.
    Substring(String),
    /// Regular expression matched against the failure output.
    Pattern {
        /// Regular expression source.
        regex: String,
    },
}

/// One expected stub exchange for a named check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedExchange {
    /// Scenario ID the trace must show.
    pub scenario: String,
    /// Request method the trace must show.
    pub method: String,
    /// Request path template the trace must show.
    pub path: String,
}

/// One named check inside a verification manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSpec {
    /// Stable check name used by `--check` and by run directories.
    pub name: String,
    /// Whether the check regresses or guards behaviour.
    pub role: Role,
    /// Contract change records this check covers.
    #[serde(default)]
    pub change_ids: Vec<String>,
    /// Test identifier run once per stage.
    pub test: String,
    /// Failure signature required on the red stage.
    #[serde(default)]
    pub expected_diagnostic: Option<ExpectedDiagnostic>,
    /// Stub exchanges every stage of this check must show.
    #[serde(default)]
    pub expected_exchange: Vec<ExpectedExchange>,
}

/// A verification manifest as read from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version of this file.
    pub schema_version: u32,
    /// Consumer repository holding the baseline and patched commits.
    pub repo: PathBuf,
    /// Application commit the upgrade starts from.
    pub baseline_commit: String,
    /// Application commit carrying the repair.
    pub patched_commit: String,
    /// Harness source directory with tests and scenarios.
    pub harness: PathBuf,
    /// Scenario directory answered under the old contract.
    pub scenarios_old: PathBuf,
    /// Scenario directory answered under the new contract.
    pub scenarios_new: PathBuf,
    /// Pinned hash of the exact old spec text.
    pub spec_old_sha256: String,
    /// Pinned hash of the exact new spec text.
    pub spec_new_sha256: String,
    /// Named checks run by the verification.
    #[serde(default)]
    pub checks: Vec<CheckSpec>,
}

/// One hashed file inside a harness snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashedFile {
    /// Path relative to the harness root, with forward slashes.
    pub path: String,
    /// Hex hash of the exact file bytes.
    pub sha256: String,
}

/// Freeze record stored beside the manifest.
///
/// The file proves which harness content the repair started from. A
/// later run compares the live harness hash against this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreezeFile {
    /// Schema version of this file.
    pub schema_version: u32,
    /// Content hash of the harness at freeze time.
    pub harness_hash: String,
    /// Per-file hashes at freeze time, in sorted path order.
    pub files: Vec<HashedFile>,
    /// Seconds since the Unix epoch when the freeze was written.
    pub frozen_at_epoch_secs: u64,
}

/// Load and validate a manifest file.
///
/// Relative paths resolve against the manifest parent directory.
/// Anything structural fails here with the manifest path named.
pub fn load_manifest(path: &Path) -> anyhow::Result<(Manifest, PathBuf)> {
    let bytes = std::fs::read(path).with_context(|| format!("read manifest {}", path.display()))?;
    let mut manifest: Manifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse manifest {}", path.display()))?;
    if manifest.schema_version != SCHEMA_VERSION {
        anyhow::bail!(
            "manifest {} has schema version {}, need {SCHEMA_VERSION}",
            path.display(),
            manifest.schema_version
        );
    }
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    manifest.repo = resolve_against(&dir, &manifest.repo);
    manifest.harness = resolve_against(&dir, &manifest.harness);
    manifest.scenarios_old = resolve_against(&dir, &manifest.scenarios_old);
    manifest.scenarios_new = resolve_against(&dir, &manifest.scenarios_new);
    validate_manifest(path, &manifest)?;
    Ok((manifest, dir))
}

/// Resolve one manifest path against the manifest directory.
///
/// Absolute paths stay untouched. Relative paths join the directory.
fn resolve_against(dir: &Path, value: &Path) -> PathBuf {
    if value.is_absolute() {
        value.to_path_buf()
    } else {
        dir.join(value)
    }
}

/// Check every manifest field that has a static rule.
///
/// The manifest path names the file in each error. Dynamic checks
/// (commit existence, scenario validity) run later per stage.
fn validate_manifest(path: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    if manifest.baseline_commit.trim().is_empty() {
        anyhow::bail!("manifest {} names an empty baseline commit", path.display());
    }
    if manifest.patched_commit.trim().is_empty() {
        anyhow::bail!("manifest {} names an empty patched commit", path.display());
    }
    if manifest.spec_old_sha256.trim().is_empty() {
        anyhow::bail!("manifest {} names an empty old spec hash", path.display());
    }
    if manifest.spec_new_sha256.trim().is_empty() {
        anyhow::bail!("manifest {} names an empty new spec hash", path.display());
    }
    if manifest.checks.is_empty() {
        anyhow::bail!("manifest {} names no checks", path.display());
    }
    let mut names = Vec::with_capacity(manifest.checks.len());
    for check in &manifest.checks {
        if check.name.trim().is_empty() {
            anyhow::bail!(
                "manifest {} names a check with an empty name",
                path.display()
            );
        }
        if names.contains(&check.name) {
            anyhow::bail!(
                "manifest {} repeats check name {:?}",
                path.display(),
                check.name
            );
        }
        names.push(check.name.clone());
        if check.test.trim().is_empty() {
            anyhow::bail!(
                "manifest {} gives check {:?} an empty test identifier",
                path.display(),
                check.name
            );
        }
        match check.role {
            Role::Regression => {
                if check.expected_diagnostic.is_none() {
                    anyhow::bail!(
                        "manifest {} gives regression check {:?} no expected diagnostic",
                        path.display(),
                        check.name
                    );
                }
            }
            Role::Guard => {
                if check.expected_diagnostic.is_some() {
                    anyhow::bail!(
                        "manifest {} gives guard check {:?} an expected diagnostic, which only a regression check declares",
                        path.display(),
                        check.name
                    );
                }
            }
        }
        if check.expected_exchange.is_empty() {
            anyhow::bail!(
                "manifest {} gives check {:?} no expected exchange",
                path.display(),
                check.name
            );
        }
        for exchange in &check.expected_exchange {
            if exchange.scenario.trim().is_empty()
                || exchange.method.trim().is_empty()
                || exchange.path.trim().is_empty()
            {
                anyhow::bail!(
                    "manifest {} gives check {:?} an exchange with an empty field",
                    path.display(),
                    check.name
                );
            }
        }
    }
    Ok(())
}

/// Pick the checks named by `--check`, or every check without a filter.
///
/// An unknown name fails with the manifest path and the known names.
pub fn select_checks<'a>(
    manifest: &'a Manifest,
    manifest_path: &Path,
    wanted: &[String],
) -> anyhow::Result<Vec<&'a CheckSpec>> {
    if wanted.is_empty() {
        return Ok(manifest.checks.iter().collect());
    }
    let mut selected = Vec::with_capacity(wanted.len());
    for name in wanted {
        match manifest.checks.iter().find(|check| &check.name == name) {
            Some(check) => selected.push(check),
            None => {
                let known = manifest
                    .checks
                    .iter()
                    .map(|check| check.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!(
                    "manifest {} has no check {name:?} (known: {known})",
                    manifest_path.display()
                );
            }
        }
    }
    Ok(selected)
}

/// Hash every file under the harness directory.
///
/// Files sort by forward-slash relative path. Each entry feeds its
/// path, a zero byte, and its exact bytes into one SHA-256 digest.
/// The returned list carries per-file hashes in the same order.
pub fn hash_harness(dir: &Path) -> anyhow::Result<(String, Vec<HashedFile>)> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    files.sort_by(|left: &HashedFile, right: &HashedFile| left.path.cmp(&right.path));
    let mut digest = sha2::Sha256::new();
    use sha2::Digest;
    for file in &files {
        digest.update(file.path.as_bytes());
        digest.update([0u8]);
        let bytes = std::fs::read(dir.join(file.path.replace('/', &stand_sep())))
            .with_context(|| format!("read harness file {}", file.path))?;
        digest.update(crate::state::layout::sha256_hex(&bytes).as_bytes());
    }
    let mut hash = String::with_capacity(64);
    for byte in digest.finalize() {
        hash.push_str(&format!("{byte:02x}"));
    }
    Ok((hash, files))
}

/// Separator used inside stored harness paths.
///
/// Stored paths always use forward slashes so the hash stays stable
/// across platforms. Disk access translates them back here.
fn stand_sep() -> String {
    std::path::MAIN_SEPARATOR.to_string()
}

/// Collect every regular file under a harness directory.
///
/// The walk skips nothing, so stray files change the hash instead of
/// hiding. Each entry stores its forward-slash relative path and its
/// hex hash.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<HashedFile>) -> anyhow::Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("read harness directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("list {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("relativise harness path {}", path.display()))?;
        let mut text = relative.display().to_string();
        if std::path::MAIN_SEPARATOR != '/' {
            text = text.replace(std::path::MAIN_SEPARATOR, "/");
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read harness file {}", path.display()))?;
        out.push(HashedFile {
            path: text,
            sha256: crate::state::layout::sha256_hex(&bytes),
        });
    }
    Ok(())
}

/// Path of the freeze file stored beside a manifest.
pub fn freeze_path(manifest_path: &Path) -> PathBuf {
    let mut text = manifest_path.as_os_str().to_owned();
    text.push(FREEZE_SUFFIX);
    PathBuf::from(text)
}

/// Write a freeze record for the manifest harness.
///
/// The write is atomic, so readers see the old freeze or the new one.
/// The returned value carries the fresh hash and file list.
pub fn freeze_harness(manifest_path: &Path, manifest: &Manifest) -> anyhow::Result<FreezeFile> {
    if !manifest.harness.is_dir() {
        anyhow::bail!(
            "manifest {} names a harness that is not a directory: {}",
            manifest_path.display(),
            manifest.harness.display()
        );
    }
    let (hash, files) = hash_harness(&manifest.harness)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0);
    let frozen = FreezeFile {
        schema_version: SCHEMA_VERSION,
        harness_hash: hash,
        files,
        frozen_at_epoch_secs: now,
    };
    let mut bytes = serde_json::to_vec_pretty(&frozen)
        .with_context(|| format!("encode freeze for {}", manifest_path.display()))?;
    bytes.push(b'\n');
    crate::state::atomic::write_atomic(&freeze_path(manifest_path), &bytes)?;
    Ok(frozen)
}

/// Read the freeze record beside a manifest, if one exists.
///
/// A missing file reads as none. A corrupt file fails loudly instead
/// of running against an unknown harness.
pub fn read_freeze(manifest_path: &Path) -> anyhow::Result<Option<FreezeFile>> {
    let path = freeze_path(manifest_path);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(
                anyhow::Error::new(error).context(format!("read freeze file {}", path.display()))
            );
        }
    };
    let frozen: FreezeFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse freeze {}", path.display()))?;
    if frozen.schema_version != SCHEMA_VERSION {
        anyhow::bail!(
            "freeze {} has schema version {}, need {SCHEMA_VERSION}",
            path.display(),
            frozen.schema_version
        );
    }
    Ok(Some(frozen))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a manifest JSON value into a temp directory.
    fn write_manifest(dir: &Path, value: &serde_json::Value) -> PathBuf {
        let path = dir.join("verify-manifest.json");
        std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        path
    }

    /// One minimal valid manifest value.
    fn valid_value() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "repo": ".",
            "baseline_commit": "abc",
            "patched_commit": "def",
            "harness": "harness",
            "scenarios_old": "harness/scenarios/old",
            "scenarios_new": "harness/scenarios/new",
            "spec_old_sha256": "0".repeat(64),
            "spec_new_sha256": "1".repeat(64),
            "checks": [
                {
                    "name": "random-picker",
                    "role": "regression",
                    "change_ids": ["vc1_x"],
                    "test": "verify_random_picker",
                    "expected_diagnostic": "random picker ids",
                    "expected_exchange": [
                        {"scenario": "random-search", "method": "POST", "path": "/search/random"}
                    ]
                },
                {
                    "name": "smart-guard",
                    "role": "guard",
                    "change_ids": [],
                    "test": "verify_smart_guard",
                    "expected_exchange": [
                        {"scenario": "smart-search", "method": "POST", "path": "/search/smart"}
                    ]
                }
            ]
        })
    }

    #[test]
    fn valid_manifest_loads_with_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_manifest(dir.path(), &valid_value());
        let (manifest, _) = load_manifest(&path).unwrap();
        assert_eq!(manifest.checks.len(), 2);
        assert!(manifest.repo.is_absolute());
        assert!(manifest.harness.is_absolute());
    }

    #[test]
    fn regression_without_diagnostic_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut value = valid_value();
        value["checks"][0]
            .as_object_mut()
            .unwrap()
            .remove("expected_diagnostic");
        let path = write_manifest(dir.path(), &value);
        assert!(load_manifest(&path).is_err());
    }

    #[test]
    fn guard_with_diagnostic_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut value = valid_value();
        value["checks"][1]["expected_diagnostic"] = serde_json::json!("ids");
        let path = write_manifest(dir.path(), &value);
        assert!(load_manifest(&path).is_err());
    }

    #[test]
    fn unknown_check_selection_names_known_checks() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_manifest(dir.path(), &valid_value());
        let (manifest, _) = load_manifest(&path).unwrap();
        let error = select_checks(&manifest, &path, &["ghost".to_string()]).unwrap_err();
        assert!(format!("{error:#}").contains("random-picker"));
    }

    #[test]
    fn harness_hash_moves_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let harness = dir.path().join("harness");
        std::fs::create_dir_all(harness.join("tests")).unwrap();
        std::fs::write(harness.join("tests/a.rs"), "one").unwrap();
        let (first, files) = hash_harness(&harness).unwrap();
        assert_eq!(files.len(), 1);
        std::fs::write(harness.join("tests/a.rs"), "two").unwrap();
        let (second, _) = hash_harness(&harness).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn freeze_round_trip_keeps_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_manifest(dir.path(), &valid_value());
        let (mut manifest, _) = load_manifest(&path).unwrap();
        let harness = dir.path().join("harness");
        std::fs::create_dir_all(&harness).unwrap();
        manifest.harness = harness;
        let frozen = freeze_harness(&path, &manifest).unwrap();
        let back = read_freeze(&path).unwrap().unwrap();
        assert_eq!(back.harness_hash, frozen.harness_hash);
    }
}
