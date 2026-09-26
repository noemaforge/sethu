//! Reviewable migration reports from validated state.
//!
//! The listing reads the attempt manifest, the capture, the origins, the
//! ledger, and the stored verification runs through the shared helpers,
//! revalidates the ledger through the check evaluator, and renders one
//! Markdown file plus one static HTML file. Missing or stale evidence is
//! marked in both outputs, never omitted. An incomplete migration still
//! gets a useful listing of every required change.

/// Reviewable migration model assembled from validated state.
pub mod model;
/// Markdown and static HTML rendering for the migration model.
pub mod render;
/// Verification run artefacts found under one migration.
pub mod runs;

use std::path::{Path, PathBuf};

pub use model::{Report, assemble};
pub use render::{html_escape, md_escape, render_html, render_markdown};

use crate::cli::ReportFormat;

/// File name of the Markdown listing inside the output directory.
pub const MARKDOWN_FILE_NAME: &str = "report.md";

/// File name of the HTML listing inside the output directory.
pub const HTML_FILE_NAME: &str = "report.html";

/// One written listing file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenReport {
    /// Absolute path of the written file.
    pub path: PathBuf,
    /// Listing format written to the path.
    pub format: String,
}

/// Render the model and write the selected listings.
///
/// The default directory is the migration `reports` tree. An explicit
/// output directory wins instead. Every write goes through the shared
/// atomic writer, so readers see the old file or the new file. The
/// return lists each written file in a fixed order.
pub fn write_reports(
    report: &Report,
    migration: &Path,
    format: &ReportFormat,
    out: Option<&Path>,
) -> anyhow::Result<Vec<WrittenReport>> {
    let dir = match out {
        Some(dir) => dir.to_path_buf(),
        None => crate::state::layout::reports_dir(migration),
    };
    std::fs::create_dir_all(&dir)
        .map_err(|err| anyhow::anyhow!("create report directory {}: {err}", dir.display()))?;
    let mut written = Vec::new();
    if matches!(format, ReportFormat::Md | ReportFormat::Both) {
        let path = dir.join(MARKDOWN_FILE_NAME);
        let text = render_markdown(report);
        crate::state::atomic::write_atomic(&path, text.as_bytes())
            .map_err(|err| anyhow::anyhow!("write Markdown listing {}: {err:#}", path.display()))?;
        written.push(WrittenReport {
            path,
            format: "md".to_string(),
        });
    }
    if matches!(format, ReportFormat::Html | ReportFormat::Both) {
        let path = dir.join(HTML_FILE_NAME);
        let text = render_html(report);
        crate::state::atomic::write_atomic(&path, text.as_bytes())
            .map_err(|err| anyhow::anyhow!("write HTML listing {}: {err:#}", path.display()))?;
        written.push(WrittenReport {
            path,
            format: "html".to_string(),
        });
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_only_writes_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let migration = dir.path().join("migration");
        let report = empty_report();
        let written = write_reports(&report, &migration, &ReportFormat::Md, None).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].path, migration.join("reports").join("report.md"));
        let text = std::fs::read_to_string(&written[0].path).unwrap();
        assert!(text.contains("# Migration report"));
    }

    #[test]
    fn explicit_output_wins_over_the_migration_tree() {
        let dir = tempfile::tempdir().unwrap();
        let migration = dir.path().join("migration");
        let custom = dir.path().join("custom");
        let report = empty_report();
        let written =
            write_reports(&report, &migration, &ReportFormat::Both, Some(&custom)).unwrap();
        assert_eq!(written.len(), 2);
        assert!(custom.join("report.md").is_file());
        assert!(custom.join("report.html").is_file());
        assert!(!migration.join("reports").join("report.md").is_file());
    }

    /// Build a minimal model with one missing change.
    fn empty_report() -> Report {
        Report {
            attempt_id: "attempt".to_string(),
            sethu_version: "0.1.0".to_string(),
            generator_name: "vimanam".to_string(),
            generator_version: "1.3.0".to_string(),
            invocation: Vec::new(),
            old_spec_hash: "old".to_string(),
            new_spec_hash: "new".to_string(),
            old_source: "old.json".to_string(),
            new_source: "new.json".to_string(),
            old_git: "unknown".to_string(),
            new_git: "unknown".to_string(),
            repo_path: "/repo".to_string(),
            baseline_commit: "abc".to_string(),
            scope: Vec::new(),
            accounted: false,
            ready: false,
            not_ready: Vec::new(),
            problems: Vec::new(),
            breaking_total: 0,
            review_total: 0,
            non_breaking_total: 0,
            operations: Vec::new(),
            rows: Vec::new(),
            shared_repairs: Vec::new(),
            runs: Vec::new(),
            idioms: Vec::new(),
            unsupported: Vec::new(),
            conversion_notes: Vec::new(),
            limits: Vec::new(),
        }
    }
}
