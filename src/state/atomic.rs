//! Atomic writes for state files.
//!
//! Each write lands fully or not at all. Readers never observe a half written file.
//! The writer creates a temp file beside the target, syncs the temp file, then
//! renames it over the target. A crash before the rename keeps the old file.
//! Stale temp files stay harmless. Lookups ignore them by name.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;

/// Marker at the start of every pending temp file name.
///
/// Readers and lookups skip names with this marker. A crash can leave such
/// files behind. The next write picks a fresh name and ignores stale files.
pub const PENDING_PREFIX: &str = ".sethu-tmp-";

/// Marker at the end of every pending temp file name.
///
/// The marker keeps temp files distinct from state files during scans.
pub const PENDING_SUFFIX: &str = ".tmp";

/// Counter that keeps temp names unique inside one process.
///
/// Process ids repeat across runs, so a counter plus a timestamp separates names.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Report whether a path names a pending temp file.
///
/// Lookups call this check to skip leftovers from interrupted writers.
pub fn is_pending_temp(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(PENDING_PREFIX) && name.ends_with(PENDING_SUFFIX))
}

/// Write bytes to a path atomically.
///
/// This function creates the parent directory, writes a temp file beside the
/// target, syncs the temp file, then renames it over the target. It syncs the
/// parent directory after the rename. Readers observe either the old content
/// or the new content, never a mix.
pub fn write_atomic(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = match path.parent() {
        Some(dir) => dir,
        None => anyhow::bail!("state path has no parent {}", path.display()),
    };
    let dir = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create state directory {}", dir.display()))?;
    let temp = temp_path(dir);
    let outcome = write_and_commit(&temp, path, dir, contents);
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    outcome
}

/// Pick a fresh temp path inside a directory.
///
/// The name carries the process id, a per process counter, and the time.
/// Later calls never reuse an earlier name.
fn temp_path(dir: &Path) -> PathBuf {
    let process = std::process::id();
    let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(span) => span.as_nanos(),
        Err(_) => 0,
    };
    dir.join(format!(
        "{PENDING_PREFIX}{process}-{count}-{nanos}{PENDING_SUFFIX}"
    ))
}

/// Fill a temp file, sync it, then rename it over the target.
///
/// The rename is atomic on one filesystem. The temp file lives beside the
/// target, so both paths share one filesystem. The directory sync makes the
/// rename durable before this function returns.
fn write_and_commit(temp: &Path, target: &Path, dir: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .with_context(|| format!("create temp file {}", temp.display()))?;
    file.write_all(contents)
        .with_context(|| format!("write temp file {}", temp.display()))?;
    file.flush()
        .with_context(|| format!("flush temp file {}", temp.display()))?;
    file.sync_all()
        .with_context(|| format!("sync temp file {}", temp.display()))?;
    drop(file);
    std::fs::rename(temp, target)
        .with_context(|| format!("publish state file {}", target.display()))?;
    sync_dir(dir)
}

/// Sync a directory so a fresh rename survives a crash.
///
/// The sync flushes the directory entry for the renamed file.
fn sync_dir(dir: &Path) -> anyhow::Result<()> {
    let handle = OpenOptions::new()
        .read(true)
        .open(dir)
        .with_context(|| format!("open state directory {}", dir.display()))?;
    handle
        .sync_all()
        .with_context(|| format!("sync state directory {}", dir.display()))?;
    Ok(())
}
