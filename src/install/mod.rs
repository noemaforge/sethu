//! Pack embedding, ownership tracking, and repository installation.
//!
//! The binary embeds the pack directory at compile time. Installing copies
//! owned files into a consumer repository, merges owned modes into the
//! project mode file, and records every owned file hash. A later run reads
//! the record to tell user edits apart from untouched files. User edits are
//! never overwritten. The new bytes land beside the edited file instead.
//!
//! Every write lands atomically. Readers see the old file or the new file,
//! never a mix. All writes stay below the consumer repository.

/// Merge of owned modes into the project mode file.
pub mod merge;

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use include_dir::{Dir, include_dir};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::state::{atomic, layout};

/// The pack directory as embedded in the binary.
///
/// The tree mirrors the project layout below `.bob`, plus a version file
/// and the mode file at the locations the pack layout defines.
static PACK: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/pack");

/// Name of the embedded file holding the pack version.
const VERSION_FILE: &str = "version.txt";

/// Name of the embedded file holding the owned modes.
const MODES_SOURCE: &str = "custom_modes.yaml";

/// Installed path of the merged project mode file.
const MODES_TARGET: &str = ".bob/custom_modes.yaml";

/// Line added to the repository exclude file.
const EXCLUDE_LINE: &str = ".sethu/";

/// Schema version of the installation record.
///
/// Readers refuse any other version instead of guessing.
pub const INSTALLATION_SCHEMA_VERSION: u32 = 1;

/// Nextest profile block owned by the installer.
///
/// The block configures JUnit output for the named profile, so later
/// verification runs can parse per test results.
const NEXTEST_PROFILE_BLOCK: &str = "[profile.sethu.junit]\npath = \"junit.xml\"\nstore-success-output = true\nstore-failure-output = true\n";

/// One embedded pack file with its installed location.
///
/// Targets use forward slashes and stay below the consumer `.bob` tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedFile {
    /// Installed path relative to the repository root.
    pub target: String,
    /// Exact bytes to write.
    pub bytes: Vec<u8>,
}

/// List every owned file in sorted target order.
///
/// The version marker and the merged mode file stay excluded. They follow
/// their own paths and never count as plain owned files.
pub fn owned_files() -> anyhow::Result<Vec<OwnedFile>> {
    let mut found: Vec<(String, Vec<u8>)> = Vec::new();
    collect_files(&PACK, &mut found)?;
    let mut owned = Vec::with_capacity(found.len());
    for (rel, bytes) in found {
        if rel == VERSION_FILE || rel == MODES_SOURCE {
            continue;
        }
        owned.push(OwnedFile {
            target: check_target(&rel)?,
            bytes,
        });
    }
    owned.sort_by(|left, right| left.target.cmp(&right.target));
    Ok(owned)
}

/// Read the pack version from the embedded version file.
///
/// The value is the file text trimmed of surrounding whitespace. An empty
/// value fails instead of reporting a blank version.
pub fn pack_version() -> anyhow::Result<String> {
    let file = PACK
        .get_file(VERSION_FILE)
        .with_context(|| format!("embedded pack is missing its version file {VERSION_FILE}"))?;
    let version = String::from_utf8_lossy(file.contents()).trim().to_string();
    if version.is_empty() {
        anyhow::bail!("embedded pack version file {VERSION_FILE} is empty");
    }
    Ok(version)
}

/// Read the embedded mode file text.
///
/// A fresh install writes the merged form of this text. Later merges draw
/// owned modes from it.
pub fn pack_modes_text() -> anyhow::Result<String> {
    let file = PACK
        .get_file(MODES_SOURCE)
        .with_context(|| format!("embedded pack is missing its mode file {MODES_SOURCE}"))?;
    String::from_utf8(file.contents().to_vec())
        .with_context(|| format!("read embedded mode file {MODES_SOURCE} as text"))
}

/// Record of one installation inside the consumer repository.
///
/// The file lives at `.sethu/installation.json`. It stores the installer
/// version, the pack version, the owned mode slugs, and one hash per owned
/// file the installer wrote. A missing hash entry means the installer never
/// wrote that path, so live bytes there always count as user content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationRecord {
    /// Schema version of this file. Writers stamp the shared version.
    pub schema_version: u32,
    /// Version of the binary that wrote this record.
    pub sethu: String,
    /// Pack version that produced the installed bytes.
    pub pack: String,
    /// Last written owned hash per installed path, in install order.
    pub files: IndexMap<String, String>,
    /// Mode slugs the installer owns in the merged file, sorted.
    pub owned_modes: Vec<String>,
}

impl InstallationRecord {
    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != INSTALLATION_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported installation schema version {}",
                self.schema_version
            );
        }
        Ok(())
    }
}

/// Read the installation record of a repository.
///
/// A missing file reads as no record. A present file must parse and carry
/// a supported schema version.
pub fn read_installation(repo: &Path) -> anyhow::Result<Option<InstallationRecord>> {
    let path = installation_path(repo);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!("read installation record {}", path.display())));
        }
    };
    let record: InstallationRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse installation record {}", path.display()))?;
    record.validate()?;
    Ok(Some(record))
}

/// What one install run did to one owned file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    /// The target was absent and now holds the pack bytes.
    Created,
    /// The target already held the pack bytes.
    Unchanged,
    /// The target held untouched owned bytes and took the new bytes.
    Updated,
    /// The target held user edits, so the new bytes went beside it.
    KeptWithCopy,
}

/// One owned file with the outcome of an install run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledFile {
    /// Installed path relative to the repository root.
    pub path: String,
    /// What the run did to the file.
    pub outcome: FileOutcome,
}

/// Full result of installing the pack into one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Canonical path of the consumer repository.
    pub repo: String,
    /// Version of the binary that ran the install.
    pub sethu: String,
    /// Pack version read from the embedded version file.
    pub pack: String,
    /// One entry per owned file, in sorted target order.
    pub files: Vec<InstalledFile>,
    /// Owned mode slugs appended to the project file, in pack order.
    pub modes_added: Vec<String>,
    /// Owned mode slugs replaced with pack content, in pack order.
    pub modes_updated: Vec<String>,
    /// Colliding slugs left untouched, in pack order.
    pub mode_collisions: Vec<String>,
    /// Whether the run added the exclude entry.
    pub exclude_updated: bool,
    /// Whether the run wrote the nextest profile.
    pub nextest_updated: bool,
    /// Whether a diff binary answered on PATH.
    pub vimanam_found: bool,
    /// Reported diff version, when the probe could read one.
    pub vimanam_version: Option<String>,
    /// Whether the found version meets the minimum.
    pub vimanam_supported: bool,
    /// First line of `git --version`, when git answered.
    pub git_version: Option<String>,
}

impl InstallReport {
    /// Render the report as human lines in stable order.
    ///
    /// Diagnostics stay out. Every line states one fact about the run.
    pub fn summary(&self) -> Vec<String> {
        let mut lines = vec![format!("pack {} installed into {}", self.pack, self.repo)];
        for file in &self.files {
            match file.outcome {
                FileOutcome::Created => lines.push(format!("created {}", file.path)),
                FileOutcome::Unchanged => lines.push(format!("unchanged {}", file.path)),
                FileOutcome::Updated => lines.push(format!("updated {}", file.path)),
                FileOutcome::KeptWithCopy => lines.push(format!(
                    "kept {} (local edits kept, new version at {}.sethu-new)",
                    file.path, file.path
                )),
            }
        }
        for slug in &self.modes_added {
            lines.push(format!("mode added {slug}"));
        }
        for slug in &self.modes_updated {
            lines.push(format!("mode updated {slug}"));
        }
        for slug in &self.mode_collisions {
            lines.push(format!(
                "mode {slug} collides with a project mode, left in place"
            ));
        }
        if self.exclude_updated {
            lines.push("added .sethu/ to .git/info/exclude".to_string());
        } else {
            lines.push("exclude entry for .sethu/ already present".to_string());
        }
        if self.nextest_updated {
            lines.push("wrote nextest profile sethu into .config/nextest.toml".to_string());
        } else {
            lines.push("nextest profile sethu already present".to_string());
        }
        lines.push(self.vimanam_line());
        match &self.git_version {
            Some(version) => lines.push(format!("git {version}")),
            None => lines.push("git is missing".to_string()),
        }
        lines
    }

    /// Describe the diff tool state for the human summary.
    fn vimanam_line(&self) -> String {
        match (
            self.vimanam_found,
            self.vimanam_version.as_deref(),
            self.vimanam_supported,
        ) {
            (true, Some(version), true) => {
                format!("vimanam {version} (migration available)")
            }
            (true, Some(version), false) => {
                format!("vimanam {version} is too old (migration is blocked)")
            }
            (true, None, _) => {
                "vimanam answered without a version (migration is blocked)".to_string()
            }
            (false, _, _) => {
                "vimanam is missing (migration is blocked, installation is complete)".to_string()
            }
        }
    }
}

/// Install the embedded pack into a consumer repository.
///
/// The target must be a git repository. Owned files land below `.bob`,
/// owned modes merge into the project mode file, the exclude entry keeps
/// migration state out of the working tree, and the nextest profile lands
/// merged into the existing runner config. The run records every owned
/// hash and reports tools and capabilities state. Nothing writes outside
/// the repository.
pub fn install_into(repo: &Path) -> anyhow::Result<InstallReport> {
    let repo = canonical_repo(repo)?;
    ensure_git_repo(&repo)?;
    let pack = pack_version()?;
    let files = owned_files()?;
    let pack_text = pack_modes_text()?;
    let previous = read_installation(&repo)?;
    let previous_hashes = previous.as_ref().map(|record| &record.files);
    let previous_modes = previous
        .as_ref()
        .map(|record| record.owned_modes.clone())
        .unwrap_or_default();

    let mut installed = Vec::with_capacity(files.len());
    let mut hashes = IndexMap::with_capacity(files.len());
    for file in &files {
        let outcome = sync_owned_file(&repo, file, previous_hashes)?;
        match outcome {
            FileOutcome::Created | FileOutcome::Unchanged | FileOutcome::Updated => {
                hashes.insert(file.target.clone(), layout::sha256_hex(&file.bytes));
            }
            FileOutcome::KeptWithCopy => {
                if let Some(hash) = previous_hashes.and_then(|map| map.get(&file.target)) {
                    hashes.insert(file.target.clone(), hash.clone());
                }
            }
        }
        installed.push(InstalledFile {
            path: file.target.clone(),
            outcome,
        });
    }

    let modes_path = repo.join(MODES_TARGET);
    let existing_modes = read_optional_text(&modes_path)?;
    let merged = merge::merge_modes(existing_modes.as_deref(), &pack_text, &previous_modes)?;
    if merged.changed {
        atomic::write_atomic(&modes_path, merged.content.as_bytes())
            .with_context(|| format!("write merged project mode file {}", modes_path.display()))?;
    }

    let exclude_updated = ensure_exclude(&repo)?;
    let nextest_updated = ensure_nextest_profile(&repo)?;

    let mut owned_modes: Vec<String> = previous_modes;
    owned_modes.extend(merged.added.iter().cloned());
    owned_modes.extend(merged.updated.iter().cloned());
    owned_modes.sort();
    owned_modes.dedup();
    let record = InstallationRecord {
        schema_version: INSTALLATION_SCHEMA_VERSION,
        sethu: env!("CARGO_PKG_VERSION").to_string(),
        pack: pack.clone(),
        files: hashes,
        owned_modes,
    };
    crate::state::write_state_file(&installation_path(&repo), &record)?;

    let (vimanam_found, vimanam_version, vimanam_supported) = probe_vimanam_state(&repo);
    Ok(InstallReport {
        repo: repo.display().to_string(),
        sethu: env!("CARGO_PKG_VERSION").to_string(),
        pack,
        files: installed,
        modes_added: merged.added,
        modes_updated: merged.updated,
        mode_collisions: merged.collisions,
        exclude_updated,
        nextest_updated,
        vimanam_found,
        vimanam_version,
        vimanam_supported,
        git_version: probe_git_version(&repo),
    })
}

/// Add `.sethu/` to the repository exclude file.
///
/// A missing file or missing parent directories come into being. A present
/// entry stays exactly where it is. The function never duplicates the line
/// and reports whether it wrote anything.
pub fn ensure_exclude(repo: &Path) -> anyhow::Result<bool> {
    let path = repo.join(".git").join("info").join("exclude");
    let text = read_optional_text(&path)?.unwrap_or_default();
    if text.lines().any(|line| line.trim() == EXCLUDE_LINE) {
        return Ok(false);
    }
    let mut next = text;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(EXCLUDE_LINE);
    next.push('\n');
    atomic::write_atomic(&path, next.as_bytes())
        .with_context(|| format!("write repository exclude file {}", path.display()))?;
    Ok(true)
}

/// Ensure the nextest `sethu` profile in the runner config.
///
/// A missing config comes into being with the owned block. A present
/// config keeps every byte outside the owned profile section. An existing
/// owned section with identical settings stays untouched. The function
/// reports whether it wrote anything.
pub fn ensure_nextest_profile(repo: &Path) -> anyhow::Result<bool> {
    let path = repo.join(".config").join("nextest.toml");
    let existing = read_optional_text(&path)?;
    let next = match existing {
        None => NEXTEST_PROFILE_BLOCK.to_string(),
        Some(text) => match splice_profile(&text) {
            ProfileSplice::Present => return Ok(false),
            ProfileSplice::Changed(next) => next,
        },
    };
    atomic::write_atomic(&path, next.as_bytes())
        .with_context(|| format!("write runner config {}", path.display()))?;
    Ok(true)
}

/// Resolve the installation record path for a repository.
fn installation_path(repo: &Path) -> PathBuf {
    repo.join(".sethu").join("installation.json")
}

/// Collect every embedded file below a directory.
///
/// Paths use forward slashes so targets stay stable across platforms.
fn collect_files(dir: &Dir, out: &mut Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
    for file in dir.files() {
        let rel = file.path().to_string_lossy().replace('\\', "/");
        out.push((rel, file.contents().to_vec()));
    }
    for sub in dir.dirs() {
        collect_files(sub, out)?;
    }
    Ok(())
}

/// Map one embedded relative path to its installed target.
///
/// Version and mode files never reach this function. Anything trying to
/// escape the pack fails instead of writing outside the repository.
fn check_target(rel: &str) -> anyhow::Result<String> {
    let path = Path::new(rel);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        anyhow::bail!("embedded path escapes the pack {rel}");
    }
    Ok(format!(".bob/{rel}"))
}

/// Canonicalize the consumer repository path.
///
/// The canonical path anchors every later write. A missing path or a
/// non directory fails with the path named.
fn canonical_repo(repo: &Path) -> anyhow::Result<PathBuf> {
    let resolved = repo
        .canonicalize()
        .with_context(|| format!("resolve consumer repository {}", repo.display()))?;
    if !resolved.is_dir() {
        anyhow::bail!(
            "consumer repository {} is not a directory",
            resolved.display()
        );
    }
    Ok(resolved)
}

/// Refuse a target that is not a git repository.
///
/// The check runs `git rev-parse` inside the target. Anything else fails
/// with the path named. Every spawn sets its working directory and
/// captures both streams.
fn ensure_git_repo(repo: &Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--git-dir")
        .current_dir(repo)
        .output()
        .with_context(|| format!("run `git rev-parse --git-dir` in {}", repo.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "cannot install into {}: not a git repository",
            repo.display()
        );
    }
    Ok(())
}

/// Sync one owned file with the repository copy.
///
/// A missing target takes the pack bytes. A matching target stays. A
/// target holding untouched owned bytes takes the new bytes. Anything else
/// counts as a user edit. The edit stays and the new bytes land beside it
/// with a `.sethu-new` suffix.
fn sync_owned_file(
    repo: &Path,
    file: &OwnedFile,
    previous: Option<&IndexMap<String, String>>,
) -> anyhow::Result<FileOutcome> {
    let target = repo.join(&file.target);
    if target.is_dir() {
        anyhow::bail!("owned path {} is a directory", target.display());
    }
    let current = match std::fs::read(&target) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(
                anyhow::Error::new(error).context(format!("read owned path {}", target.display()))
            );
        }
    };
    let current = match current {
        None => {
            atomic::write_atomic(&target, &file.bytes)
                .with_context(|| format!("write owned file {}", target.display()))?;
            return Ok(FileOutcome::Created);
        }
        Some(bytes) => bytes,
    };
    if current == file.bytes {
        return Ok(FileOutcome::Unchanged);
    }
    let owned_before = previous
        .and_then(|map| map.get(&file.target))
        .is_some_and(|hash| *hash == layout::sha256_hex(&current));
    if owned_before {
        atomic::write_atomic(&target, &file.bytes)
            .with_context(|| format!("write owned file {}", target.display()))?;
        return Ok(FileOutcome::Updated);
    }
    let beside = beside_path(&target)?;
    write_if_different(&beside, &file.bytes)?;
    Ok(FileOutcome::KeptWithCopy)
}

/// Resolve the `.sethu-new` path beside an owned file.
fn beside_path(target: &Path) -> anyhow::Result<PathBuf> {
    let name = target
        .file_name()
        .with_context(|| format!("owned path {} has no file name", target.display()))?;
    let mut beside = name.to_os_string();
    beside.push(".sethu-new");
    Ok(target.with_file_name(beside))
}

/// Write bytes unless the path already holds them.
///
/// The helper keeps reinstalls from touching modification times when the
/// content already matches.
fn write_if_different(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let current = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!("read companion path {}", path.display())));
        }
    };
    if current.is_some_and(|found| found == bytes) {
        return Ok(());
    }
    atomic::write_atomic(path, bytes)
        .with_context(|| format!("write companion file {}", path.display()))
}

/// Read a text file, treating absence as no content.
///
/// A present directory or an unreadable file fails with the path named.
fn read_optional_text(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .with_context(|| format!("read file {} as text", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(anyhow::Error::new(error).context(format!("read file {}", path.display())))
        }
    }
}

/// Outcome of splicing the owned profile into runner config text.
enum ProfileSplice {
    /// The canonical block is already present with identical settings.
    Present,
    /// The config with the owned section ensured.
    Changed(String),
}

/// Splice the owned profile section into runner config text.
///
/// Existing sections outside the owned profile keep their exact text.
/// An owned section with identical settings reports present, so callers
/// skip the write. Anything else yields the merged text.
fn splice_profile(text: &str) -> ProfileSplice {
    let headers = section_headers(text);
    let position = headers
        .iter()
        .position(|header| is_owned_profile(&header.name));
    let Some(index) = position else {
        return ProfileSplice::Changed(append_profile(text));
    };
    let start = headers[index].start;
    let end = headers
        .iter()
        .skip(index + 1)
        .find(|header| !is_owned_profile(&header.name))
        .map(|header| header.start)
        .unwrap_or(text.len());
    if text[start..end].trim_end() == NEXTEST_PROFILE_BLOCK.trim_end() {
        return ProfileSplice::Present;
    }
    let head = text[..start].trim_end_matches('\n');
    let tail = text[end..].trim_start_matches('\n');
    let mut next = String::new();
    if !head.is_empty() {
        next.push_str(head);
        next.push_str("\n\n");
    }
    next.push_str(NEXTEST_PROFILE_BLOCK);
    if !tail.is_empty() {
        next.push('\n');
        next.push_str(tail);
        if !tail.ends_with('\n') {
            next.push('\n');
        }
    }
    ProfileSplice::Changed(next)
}

/// Append the owned profile block to runner config text.
fn append_profile(text: &str) -> String {
    let mut next = text.to_string();
    if !next.is_empty() {
        if !next.ends_with('\n') {
            next.push('\n');
        }
        next.push('\n');
    }
    next.push_str(NEXTEST_PROFILE_BLOCK);
    next
}

/// One section header with its byte offset and name.
struct SectionHeader {
    /// Byte offset of the header line start.
    start: usize,
    /// Section name without brackets.
    name: String,
}

/// List every section header in config text with byte offsets.
fn section_headers(text: &str) -> Vec<SectionHeader> {
    let mut headers = Vec::new();
    let mut offset = 0;
    for chunk in text.split_inclusive('\n') {
        if let Some(name) = section_name(chunk) {
            headers.push(SectionHeader {
                start: offset,
                name,
            });
        }
        offset += chunk.len();
    }
    headers
}

/// Read the section name of one line, if the line opens a section.
///
/// A trailing comment after the closing bracket is allowed.
fn section_name(line: &str) -> Option<String> {
    let inner = line.trim().strip_prefix('[')?;
    let end = inner.find(']')?;
    let rest = inner[end + 1..].trim();
    if rest.is_empty() || rest.starts_with('#') {
        Some(inner[..end].trim().to_string())
    } else {
        None
    }
}

/// Report whether a section name belongs to the owned profile.
///
/// The owned profile is `profile.sethu` with any of its subsections. A
/// longer name with a different stem never matches.
fn is_owned_profile(name: &str) -> bool {
    name == "profile.sethu" || name.starts_with("profile.sethu.")
}

/// Probe the diff binary for the install report.
///
/// A missing binary reports not found. An answering binary reports its
/// parsed version when readable, plus whether it meets the minimum. Every
/// spawn sets its working directory and captures both streams.
fn probe_vimanam_state(repo: &Path) -> (bool, Option<String>, bool) {
    let output = match Command::new("vimanam")
        .arg("--version")
        .current_dir(repo)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (false, None, false);
        }
        Err(_) => return (true, None, false),
    };
    if !output.status.success() {
        return (true, None, false);
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    match crate::vimanam::Version::parse(&text) {
        Ok(version) => {
            let supported = version.is_supported();
            (true, Some(version.to_string()), supported)
        }
        Err(_) => (true, None, false),
    }
}

/// Read the first line of `git --version` for the install report.
///
/// A missing or broken binary reports nothing. Every spawn sets its
/// working directory and captures both streams.
fn probe_git_version(repo: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("--version")
        .current_dir(repo)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    Some(line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_version_is_not_blank() {
        assert!(!pack_version().unwrap().is_empty());
    }

    #[test]
    fn owned_files_map_below_bob_in_order() {
        let files = owned_files().unwrap();
        assert!(!files.is_empty());
        let mut targets: Vec<&str> = files.iter().map(|file| file.target.as_str()).collect();
        assert!(targets.iter().all(|target| target.starts_with(".bob/")));
        let sorted = {
            let mut ordered = targets.clone();
            ordered.sort();
            ordered
        };
        assert_eq!(targets, sorted);
        targets.dedup();
        assert_eq!(targets.len(), files.len());
    }

    #[test]
    fn embedded_paths_cannot_escape() {
        assert!(check_target("../outside.md").is_err());
        assert!(check_target("/absolute.md").is_err());
        assert_eq!(
            check_target("commands/api-upgrade.md").unwrap(),
            ".bob/commands/api-upgrade.md"
        );
    }

    #[test]
    fn exclude_appends_once_and_keeps_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git").join("info")).unwrap();
        let path = dir.path().join(".git").join("info").join("exclude");
        std::fs::write(&path, "*.log\n").unwrap();
        assert!(ensure_exclude(dir.path()).unwrap());
        assert!(!ensure_exclude(dir.path()).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "*.log\n.sethu/\n");
    }

    #[test]
    fn exclude_creates_missing_parents() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_exclude(dir.path()).unwrap());
        let text =
            std::fs::read_to_string(dir.path().join(".git").join("info").join("exclude")).unwrap();
        assert_eq!(text, ".sethu/\n");
    }

    #[test]
    fn exclude_handles_a_file_without_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git").join("info")).unwrap();
        let path = dir.path().join(".git").join("info").join("exclude");
        std::fs::write(&path, "*.log").unwrap();
        assert!(ensure_exclude(dir.path()).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "*.log\n.sethu/\n");
    }

    #[test]
    fn nextest_writes_block_then_reports_present() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_nextest_profile(dir.path()).unwrap());
        assert!(!ensure_nextest_profile(dir.path()).unwrap());
        let text =
            std::fs::read_to_string(dir.path().join(".config").join("nextest.toml")).unwrap();
        assert_eq!(text, NEXTEST_PROFILE_BLOCK);
    }

    #[test]
    fn nextest_appends_beside_existing_profiles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".config")).unwrap();
        let path = dir.path().join(".config").join("nextest.toml");
        std::fs::write(&path, "[profile.default]\nretries = 2\n").unwrap();
        assert!(ensure_nextest_profile(dir.path()).unwrap());
        assert!(!ensure_nextest_profile(dir.path()).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("[profile.default]\nretries = 2\n"));
        assert!(text.contains(NEXTEST_PROFILE_BLOCK.trim_end()));
    }

    #[test]
    fn nextest_normalizes_a_drifted_owned_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".config")).unwrap();
        let path = dir.path().join(".config").join("nextest.toml");
        std::fs::write(
            &path,
            "[profile.default]\nretries = 2\n\n[profile.sethu.junit]\npath = \"old.xml\"\n",
        )
        .unwrap();
        assert!(ensure_nextest_profile(dir.path()).unwrap());
        assert!(!ensure_nextest_profile(dir.path()).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("path = \"junit.xml\""));
        assert!(!text.contains("old.xml"));
        assert!(text.contains("[profile.default]"));
    }

    #[test]
    fn nextest_ignores_similar_profile_names() {
        let text = "[profile.sethu-old.junit]\npath = \"old.xml\"\n";
        let ProfileSplice::Changed(next) = splice_profile(text) else {
            panic!("a similar profile name must not count as owned");
        };
        assert!(next.contains("[profile.sethu-old.junit]"));
        assert!(next.contains(NEXTEST_PROFILE_BLOCK.trim_end()));
    }
}
