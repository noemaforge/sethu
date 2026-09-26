//! Versioned state files for the state tree.
//!
//! Every file carries a schema version beside its payload. Readers refuse
//! unknown versions instead of guessing. Writers encode deterministically and
//! store bytes through atomic writes, so files land whole or not at all.

pub mod atomic;
pub mod attempt;
pub mod capture;
pub mod layout;
pub mod ledger;
pub mod pair;

use std::path::Path;

use anyhow::Context;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Schema version written into every state file.
///
/// Readers accept exactly this version. Writers stamp exactly this version.
pub const SCHEMA_VERSION: u32 = 1;

/// Check a stored schema version against the supported version.
///
/// Readers call this check after parsing and before trusting any payload.
pub fn check_schema_version(found: u32, kind: &str) -> anyhow::Result<()> {
    if found != SCHEMA_VERSION {
        anyhow::bail!("unsupported {kind} schema version {found}");
    }
    Ok(())
}

/// Encode a state value and store it atomically.
///
/// Encoding uses pretty JSON with a trailing newline. Storage reuses the
/// atomic writer, so readers see the old file or the new file.
pub fn write_state_file<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .with_context(|| format!("encode state file {}", path.display()))?;
    bytes.push(b'\n');
    atomic::write_atomic(path, &bytes)
}

/// Load and parse a state file.
///
/// The error names the file for both read failures and parse failures.
/// Callers validate the schema version on the parsed value.
pub fn read_state_file<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read state file {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse state file {}", path.display()))
}

/// Installation record stored at the state root.
///
/// Writers create it once per repository. Readers use it to confirm that the
/// tree belongs to this tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installation {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Version of the binary that created the tree. Kept for diagnostics.
    pub sethu_version: String,
}

impl Installation {
    /// Create an installation record for a binary version.
    pub fn new(version: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            sethu_version: version.to_string(),
        }
    }

    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "installation")
    }
}

/// Pair record stored inside a pair directory.
///
/// The record keeps both full spec hashes. Lookups compare these values.
/// Directory names carry only the short prefixes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairFile {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Full hash of the old spec. Never a prefix alone.
    pub old_spec_hash: String,
    /// Full hash of the new spec. Never a prefix alone.
    pub new_spec_hash: String,
}

impl PairFile {
    /// Create a pair record from two full spec hashes.
    pub fn new(old_full: &str, new_full: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            old_spec_hash: old_full.to_string(),
            new_spec_hash: new_full.to_string(),
        }
    }

    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "pair")
    }
}

/// Capture record stored inside a capture directory.
///
/// The record keeps the full capture id. Lookups compare this value.
/// The directory name repeats the same id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureFile {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Full id of the capture. Never a prefix alone.
    pub capture_id: String,
}

impl CaptureFile {
    /// Create a capture record from a full capture id.
    pub fn new(capture_id: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            capture_id: capture_id.to_string(),
        }
    }

    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "capture")
    }
}

/// One stored change entry.
///
/// Values arrive from the diff output and stay unchanged here. Sethu never
/// recomputes an id or a severity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRef {
    /// Stable id of the change. Copied from the diff output.
    pub id: String,
    /// Severity word of the change. Copied from the diff output.
    pub severity: String,
}

impl ChangeRef {
    /// Create a change entry from an id and a severity word.
    pub fn new(id: &str, severity: &str) -> Self {
        Self {
            id: id.to_string(),
            severity: severity.to_string(),
        }
    }
}

/// Change list stored inside a capture directory.
///
/// The list preserves the order in which the diff reported the changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesFile {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Stored changes in report order.
    pub changes: Vec<ChangeRef>,
}

impl ChangesFile {
    /// Create a change list from stored entries.
    pub fn new(changes: Vec<ChangeRef>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            changes,
        }
    }

    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "changes")
    }
}

/// Origins map stored inside a capture directory.
///
/// The map links each change id to its origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginsFile {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Origin per change id in insertion order. The map keeps write order stable.
    pub origins: IndexMap<String, String>,
}

impl OriginsFile {
    /// Create an origins map from change ids to origins.
    pub fn new(origins: IndexMap<String, String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            origins,
        }
    }

    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "origins")
    }
}

/// Attempt manifest stored inside a migration directory.
///
/// The manifest keeps the full attempt id and both full spec hashes. Lookups
/// compare these values. The directory name repeats the attempt id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationManifest {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Full id of the attempt. Never a prefix alone.
    pub attempt_id: String,
    /// Full hash of the old spec. Never a prefix alone.
    pub old_spec_hash: String,
    /// Full hash of the new spec. Never a prefix alone.
    pub new_spec_hash: String,
}

impl MigrationManifest {
    /// Create a manifest from an attempt id and two full spec hashes.
    pub fn new(attempt_id: &str, old_full: &str, new_full: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            attempt_id: attempt_id.to_string(),
            old_spec_hash: old_full.to_string(),
            new_spec_hash: new_full.to_string(),
        }
    }

    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "manifest")
    }
}

/// One ledger entry keyed by change id.
///
/// The outcome stays a plain word here. Later commands interpret it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Recorded outcome word for the change.
    pub outcome: String,
    /// Optional note stored beside the outcome.
    pub note: Option<String>,
}

impl LedgerEntry {
    /// Create a ledger entry from an outcome word and an optional note.
    pub fn new(outcome: &str, note: Option<&str>) -> Self {
        Self {
            outcome: outcome.to_string(),
            note: note.map(str::to_string),
        }
    }
}

/// Outcome ledger stored inside a migration directory.
///
/// Entries stay keyed by change id in insertion order. The map keeps write
/// order stable across reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerFile {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Ledger entries per change id in insertion order.
    pub entries: IndexMap<String, LedgerEntry>,
}

impl LedgerFile {
    /// Create a ledger from change ids to entries.
    pub fn new(entries: IndexMap<String, LedgerEntry>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries,
        }
    }

    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "ledger")
    }
}
