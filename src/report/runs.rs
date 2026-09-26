//! Verification run artefacts found under one migration.
//!
//! The verification flow stores one `run.json` per invocation beside its
//! manifest, with per-check stage records and stub request traces below
//! it. When the manifest lives inside the migration tree, those files sit
//! under the migration too. Discovery walks the migration directory for
//! run records and reads whatever stage detail survived. Unreadable or
//! misshapen files are skipped, never fatal, so a damaged artefact tree
//! still yields a useful listing of what remains.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Deepest directory level searched for run records.
///
/// Run records sit at most three levels down (`runs/<id>/run.json`, or
/// deeper when the manifest lives in a nested harness directory). The
/// bound keeps the walk cheap on large consumer checkouts.
const MAX_WALK_DEPTH: usize = 8;

/// File name of a verification run record.
const RUN_FILE_NAME: &str = "run.json";

/// File name of a per-stage verification record.
const STAGE_FILE_NAME: &str = "stage.json";

/// File name of a stub request trace beside a stage record.
const TRACE_FILE_NAME: &str = "requests.jsonl";

/// One stored check summary inside a run record.
///
/// Every field tolerates absence, so older or hand-edited records still
/// parse. Missing names fall back to the directory name at use time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct StoredCheck {
    /// Check name from the manifest.
    #[serde(default)]
    name: String,
    /// Check role from the manifest.
    #[serde(default)]
    role: String,
    /// Contract change records the check covered when the run started.
    #[serde(default)]
    change_ids: Vec<String>,
    /// Whether the three stages met the check matrix.
    #[serde(default)]
    verified: bool,
    /// Per-stage verdict words in stage order.
    #[serde(default)]
    stages: Vec<String>,
}

/// One run record as stored beside a verification manifest.
///
/// Only the fields the listing needs are kept. Unknown fields stay
/// unread. Every field tolerates absence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct StoredRun {
    /// Unique run identifier and directory name.
    #[serde(default)]
    run_id: String,
    /// Live harness content hash for this run.
    #[serde(default)]
    harness_hash: String,
    /// Frozen hash beside the manifest, when one existed.
    #[serde(default)]
    frozen_hash: Option<String>,
    /// Whether the harness moved since the freeze.
    #[serde(default)]
    harness_changed: bool,
    /// Binary version that ran the verification.
    #[serde(default)]
    sethu_version: String,
    /// Nextest version, or a note that the fallback applied.
    #[serde(default)]
    nextest_version: String,
    /// Per-check summaries in manifest order.
    #[serde(default)]
    checks: Vec<StoredCheck>,
}

/// One stored stage record beside its test streams and stub trace.
///
/// Only the fields the listing needs are kept. Unknown fields stay
/// unread. Every field tolerates absence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct StoredStage {
    /// Check name from the manifest.
    #[serde(default)]
    check: String,
    /// Stage name, such as `original-new`.
    #[serde(default)]
    stage: String,
    /// Contract change records the check covered when the run started.
    #[serde(default)]
    change_ids: Vec<String>,
    /// Verdict word for this stage.
    #[serde(default)]
    verdict: String,
    /// Verdict detail for non-green stages.
    #[serde(default)]
    detail: String,
    /// Which report parser produced the case list.
    #[serde(default)]
    parser: String,
}

/// One verification stage with its verdict and trace size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageInfo {
    /// Check name from the run record.
    pub check: String,
    /// Stage name, such as `original-new`.
    pub stage: String,
    /// Verdict word for this stage.
    pub verdict: String,
    /// Verdict detail for non-green stages.
    pub detail: String,
    /// Which report parser produced the case list.
    pub parser: String,
    /// Non-blank trace lines beside the stage record.
    pub trace_entries: usize,
}

/// One verification check across its stored stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCheck {
    /// Check name from the run record.
    pub name: String,
    /// Check role from the run record.
    pub role: String,
    /// Contract change records the check covered when the run started.
    pub change_ids: Vec<String>,
    /// Whether the three stages met the check matrix.
    pub verified: bool,
    /// Per-stage verdicts in stored order.
    pub stages: Vec<StageInfo>,
}

/// One verification run found under a migration directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRun {
    /// Unique run identifier and directory name.
    pub run_id: String,
    /// Migration-relative directory holding the run record.
    pub relative_dir: String,
    /// Live harness content hash for this run.
    pub harness_hash: String,
    /// Whether the harness moved since the freeze.
    pub harness_changed: bool,
    /// Binary version that ran the verification.
    pub sethu_version: String,
    /// Nextest version, or a note that the fallback applied.
    pub nextest_version: String,
    /// Per-check summaries in stored order.
    pub checks: Vec<DiscoveredCheck>,
}

/// Find every verification run record under a migration directory.
///
/// The walk covers the whole migration tree up to a fixed depth, so runs
/// stored beside a nested harness manifest are found as well as runs in
/// the top-level runs directory. Results sort by run id. Damaged files
/// are skipped quietly and counted out, so one broken record never hides
/// the runs that remain.
pub fn discover_runs(migration: &Path) -> Vec<DiscoveredRun> {
    let mut files = Vec::new();
    collect_run_files(migration, 0, &mut files);
    let mut runs = Vec::new();
    for file in files {
        if let Some(run) = read_run(migration, &file) {
            runs.push(run);
        }
    }
    runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    runs
}

/// Collect every run record path under a directory up to the depth bound.
///
/// Pending temp files stay excluded, so interrupted writers never appear
/// in the listing. Sorting happens in the caller, not here.
fn collect_run_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if crate::state::atomic::is_pending_temp(&path) {
            continue;
        }
        if path.is_file() {
            if path.file_name().and_then(|name| name.to_str()) == Some(RUN_FILE_NAME) {
                out.push(path);
            }
            continue;
        }
        if path.is_dir() {
            collect_run_files(&path, depth + 1, out);
        }
    }
}

/// Read one run record with its stage detail.
///
/// A broken record yields none, so the caller skips it. Stage detail is
/// best effort: a missing stage file leaves the stored verdict words in
/// place with an empty trace count.
fn read_run(migration: &Path, file: &Path) -> Option<DiscoveredRun> {
    let bytes = std::fs::read(file).ok()?;
    let stored: StoredRun = serde_json::from_slice(&bytes).ok()?;
    let run_dir = file.parent()?;
    let relative = run_dir
        .strip_prefix(migration)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let run_id = if stored.run_id.is_empty() {
        run_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        stored.run_id.clone()
    };
    let mut checks = Vec::with_capacity(stored.checks.len());
    for item in &stored.checks {
        checks.push(read_check(run_dir, item));
    }
    Some(DiscoveredRun {
        run_id,
        relative_dir: relative,
        harness_hash: stored.harness_hash.clone(),
        harness_changed: stored.harness_changed,
        sethu_version: stored.sethu_version.clone(),
        nextest_version: stored.nextest_version.clone(),
        checks,
    })
}

/// Read one check summary with its stored stage records.
///
/// Stage directories carry the check name. A stage record that parses
/// contributes its verdict and trace size. One that does not parse
/// leaves the stored verdict word with an empty trace count.
fn read_check(run_dir: &Path, item: &StoredCheck) -> DiscoveredCheck {
    let check_dir = run_dir.join(&item.name);
    let mut stages = Vec::new();
    if check_dir.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&check_dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .filter_map(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        for name in &names {
            stages.push(read_stage(&check_dir.join(name), &item.name, name));
        }
    }
    DiscoveredCheck {
        name: item.name.clone(),
        role: item.role.clone(),
        change_ids: item.change_ids.clone(),
        verified: item.verified,
        stages,
    }
}

/// Read one stage record with its trace size.
///
/// A missing or broken stage file keeps the verdict unknown rather than
/// failing the whole run listing.
fn read_stage(stage_dir: &Path, check: &str, name: &str) -> StageInfo {
    let stored: Option<StoredStage> = std::fs::read(stage_dir.join(STAGE_FILE_NAME))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let trace_entries = count_trace_lines(&stage_dir.join(TRACE_FILE_NAME));
    StageInfo {
        check: check.to_string(),
        stage: name.to_string(),
        verdict: stored
            .as_ref()
            .map(|stage| stage.verdict.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        detail: stored
            .as_ref()
            .map(|stage| stage.detail.clone())
            .unwrap_or_default(),
        parser: stored
            .as_ref()
            .map(|stage| stage.parser.clone())
            .unwrap_or_default(),
        trace_entries,
    }
}

/// Count the non-blank lines of a stub request trace.
///
/// The count never parses the entries, so a truncated trace still
/// reports how many lines survived.
fn count_trace_lines(path: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    text.lines().filter(|line| !line.trim().is_empty()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a minimal run record with one check and one stage.
    fn write_run(base: &Path, relative: &str, run_id: &str) -> PathBuf {
        let dir = base.join(relative);
        std::fs::create_dir_all(dir.join("picker").join("original-new")).unwrap();
        let record = serde_json::json!({
            "run_id": run_id,
            "harness_hash": "abc",
            "harness_changed": false,
            "sethu_version": "0.1.0",
            "nextest_version": "nextest",
            "checks": [
                {"name": "picker", "role": "regression",
                 "change_ids": ["vc1_demo"], "verified": true,
                 "stages": ["pass"]}
            ]
        });
        std::fs::write(
            dir.join("run.json"),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("picker").join("original-new").join("stage.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "check": "picker", "stage": "original-new",
                "verdict": "pass", "detail": "", "parser": "nextest-junit"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("picker")
                .join("original-new")
                .join("requests.jsonl"),
            "{\"scenario_id\":\"demo\"}\n\n{\"scenario_id\":\"demo\"}\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn finds_nested_runs_with_stage_detail() {
        let base = tempfile::tempdir().unwrap();
        write_run(base.path(), "runs/first", "first");
        write_run(base.path(), "harness/runs/second", "second");
        std::fs::write(base.path().join("runs/broken.json"), "not json").unwrap();
        let runs = discover_runs(base.path());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, "first");
        assert_eq!(runs[1].relative_dir, "harness/runs/second");
        assert_eq!(runs[0].checks.len(), 1);
        assert_eq!(runs[0].checks[0].stages.len(), 1);
        assert_eq!(runs[0].checks[0].stages[0].verdict, "pass");
        assert_eq!(runs[0].checks[0].stages[0].trace_entries, 2);
    }

    #[test]
    fn broken_run_records_are_skipped() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("runs/bad")).unwrap();
        std::fs::write(base.path().join("runs/bad/run.json"), "not json").unwrap();
        assert!(discover_runs(base.path()).is_empty());
    }

    #[test]
    fn missing_stage_files_keep_unknown_verdicts() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs/solo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("run.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "run_id": "solo", "checks": [{"name": "picker"}]
            }))
            .unwrap(),
        )
        .unwrap();
        let runs = discover_runs(base.path());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].checks[0].stages.len(), 0);
    }
}
