//! Markdown and static HTML rendering for the migration model.
//!
//! Both renderers read the same assembled model and escape every
//! untrusted string. Ledger notes, evidence references, and change ids
//! reach the page only through the escape helpers, so injected markup
//! renders as plain text in both formats.

use super::model::{ChangeRow, Report};

/// Escape one string for HTML text and attribute use.
///
/// The function replaces the five structural characters. Everything else
/// passes through unchanged, including non-ASCII text.
pub fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for glyph in text.chars() {
        match glyph {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(glyph),
        }
    }
    out
}

/// Escape one string for Markdown rendering.
///
/// The value is fenced as an inline code span, so every byte stays
/// verbatim in the source and renders literally. The fence grows past
/// any backtick run inside the value, with padding when the value
/// itself starts or ends with a backtick. Whitespace collapses only in
/// table and list layouts through the caller-side helper below.
pub fn md_escape(text: &str) -> String {
    let mut longest = 0_usize;
    let mut run = 0_usize;
    for glyph in text.chars() {
        if glyph == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    if text.starts_with('`') || text.ends_with('`') {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// Collapse whitespace runs to single spaces for one-line layouts.
///
/// List items and table cells stay on one line. The caller escapes the
/// result, so markup in the collapsed text still renders literally.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render the full model as Markdown.
pub fn render_markdown(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("# Migration report\n\n");
    push_field(&mut out, "Attempt", &report.attempt_id);
    out.push('\n');
    render_scope_markdown(&mut out, report);
    render_accounting_markdown(&mut out, report);
    render_totals_markdown(&mut out, report);
    render_changes_markdown(&mut out, report);
    render_shared_markdown(&mut out, report);
    render_runs_markdown(&mut out, report);
    render_decisions_markdown(&mut out, report);
    render_limitations_markdown(&mut out, report);
    out
}

/// Push one bulleted field with an escaped value.
fn push_field(out: &mut String, name: &str, value: &str) {
    out.push_str("- ");
    out.push_str(name);
    out.push_str(": ");
    out.push_str(&one_line(&md_escape(value)));
    out.push('\n');
}

/// Render scope, provenance, and tool versions as Markdown.
fn render_scope_markdown(out: &mut String, report: &Report) {
    out.push_str("## Scope and provenance\n\n");
    push_field(out, "Consumer repository", &report.repo_path);
    push_field(out, "Baseline commit", &report.baseline_commit);
    let scope = if report.scope.is_empty() {
        "-".to_string()
    } else {
        report.scope.join(", ")
    };
    push_field(out, "Scope", &scope);
    push_field(out, "Old spec", &report.old_source);
    push_field(out, "Old spec hash", &report.old_spec_hash);
    push_field(out, "Old spec origin", &report.old_git);
    push_field(out, "New spec", &report.new_source);
    push_field(out, "New spec hash", &report.new_spec_hash);
    push_field(out, "New spec origin", &report.new_git);
    push_field(out, "Sethu version", &report.sethu_version);
    push_field(
        out,
        "Diff tool",
        &format!("{} {}", report.generator_name, report.generator_version),
    );
    let invocation = if report.invocation.is_empty() {
        "-".to_string()
    } else {
        report.invocation.join(" ")
    };
    push_field(out, "Diff invocation", &invocation);
    out.push('\n');
}

/// Render accounting and readiness as Markdown.
fn render_accounting_markdown(out: &mut String, report: &Report) {
    out.push_str("## Accounting and readiness\n\n");
    let valid = report.rows.iter().filter(|row| row.valid).count();
    let accounted = if report.accounted {
        "yes".to_string()
    } else {
        format!(
            "no ({valid} of {} valid, {} problems)",
            report.rows.len(),
            report.problems.len()
        )
    };
    push_field(out, "Accounted", &accounted);
    let ready = if report.ready {
        "yes".to_string()
    } else {
        format!("no ({} blockers)", report.not_ready.len())
    };
    push_field(out, "Ready", &ready);
    if report.problems.is_empty() {
        out.push_str("- Problems: none\n");
    } else {
        out.push_str("- Problems:\n");
        for problem in &report.problems {
            out.push_str("  - ");
            out.push_str(&one_line(&md_escape(&format!(
                "{} {}: {}",
                problem.code, problem.id, problem.message
            ))));
            out.push('\n');
        }
    }
    if report.not_ready.is_empty() {
        out.push_str("- Blockers: none\n");
    } else {
        out.push_str("- Blockers:\n");
        for id in &report.not_ready {
            out.push_str("  - ");
            out.push_str(&one_line(&md_escape(id)));
            out.push('\n');
        }
    }
    out.push('\n');
}

/// Render severity totals and affected operations as Markdown.
fn render_totals_markdown(out: &mut String, report: &Report) {
    out.push_str("## Severity totals and affected operations\n\n");
    push_field(out, "Breaking", &report.breaking_total.to_string());
    push_field(out, "Review", &report.review_total.to_string());
    push_field(out, "Non-breaking", &report.non_breaking_total.to_string());
    push_field(out, "Required", &report.rows.len().to_string());
    push_field(
        out,
        "Unique affected operations",
        &report.operations.len().to_string(),
    );
    for operation in &report.operations {
        out.push_str("- ");
        out.push_str(&one_line(&md_escape(&format!(
            "{} ({} required changes)",
            operation.operation, operation.required
        ))));
        out.push('\n');
    }
    out.push('\n');
}

/// Render every required change as Markdown.
fn render_changes_markdown(out: &mut String, report: &Report) {
    out.push_str("## Required changes\n\n");
    for row in &report.rows {
        out.push_str("### ");
        out.push_str(&one_line(&md_escape(&row.id)));
        out.push('\n');
        out.push('\n');
        render_change_fields(out, row);
        out.push('\n');
    }
}

/// Render one required change as bulleted Markdown fields.
fn render_change_fields(out: &mut String, row: &ChangeRow) {
    let endpoint = if row.method.is_empty() && row.path.is_empty() {
        "-".to_string()
    } else {
        format!("{} {}", row.method, row.path).trim().to_string()
    };
    push_field(out, "Endpoint", &endpoint);
    push_field(out, "Kind", &row.kind);
    push_field(out, "Severity", &row.severity);
    if !row.detail.is_empty() {
        push_field(out, "Detail", &row.detail);
    }
    push_field(out, "Origin", &row.origin);
    match &row.outcome {
        Some(outcome) => {
            let state = if row.valid {
                if row.ready {
                    "valid, ready"
                } else {
                    "valid, not ready"
                }
            } else {
                "invalid"
            };
            push_field(out, "Disposition", &format!("{outcome} ({state})"));
        }
        None => push_field(out, "Disposition", "missing, no ledger entry"),
    }
    if let Some(note) = &row.note {
        push_field(out, "Note", note);
    }
    if row.history_len > 0 {
        push_field(out, "Replaced dispositions", &row.history_len.to_string());
    }
    if row.code_refs.is_empty() {
        out.push_str("- Code references: none\n");
    } else {
        out.push_str("- Code references:\n");
        for reference in &row.code_refs {
            out.push_str("  - ");
            out.push_str(&one_line(&md_escape(reference)));
            out.push('\n');
        }
    }
    if !row.file_refs.is_empty() {
        out.push_str("- File references:\n");
        for reference in &row.file_refs {
            out.push_str("  - ");
            out.push_str(&one_line(&md_escape(reference)));
            out.push('\n');
        }
    }
    if row.run_links.is_empty() {
        out.push_str("- Run evidence: none\n");
    } else {
        out.push_str("- Run evidence:\n");
        for link in &row.run_links {
            out.push_str("  - ");
            out.push_str(&one_line(&md_escape(&run_link_line(link))));
            out.push('\n');
        }
    }
    if !row.text_refs.is_empty() {
        out.push_str("- Prose references (never evidence on their own):\n");
        for reference in &row.text_refs {
            out.push_str("  - ");
            out.push_str(&one_line(&md_escape(reference)));
            out.push('\n');
        }
    }
    if !row.covering_runs.is_empty() {
        push_field(out, "Covering runs", &row.covering_runs.join(", "));
    }
    if !row.shared_with.is_empty() {
        push_field(out, "Shared repair with", &row.shared_with.join(", "));
    }
    if row.problems.is_empty() {
        if !row.valid {
            out.push_str("- Evidence state: missing or stale, see the problem list\n");
        }
    } else {
        out.push_str("- Evidence state:\n");
        for problem in &row.problems {
            out.push_str("  - ");
            out.push_str(&one_line(&md_escape(problem)));
            out.push('\n');
        }
    }
}

/// Render one run link as a single plain line.
fn run_link_line(link: &super::model::RunLink) -> String {
    if link.missing {
        return format!("{} (missing run artefact)", link.reference);
    }
    let id = link.run_id.as_deref().unwrap_or("unknown");
    let verdict = match link.verified {
        Some(true) => "verified",
        Some(false) => "not verified",
        None => "unknown",
    };
    let mut line = format!("{} (run {id}, {verdict}", link.reference);
    if link.superseded {
        line.push_str(", superseded by a harness change");
    }
    if !link.stages.is_empty() {
        line.push_str("; ");
        line.push_str(&link.stages.join("; "));
    }
    line.push(')');
    line
}

/// Render shared repairs as Markdown.
fn render_shared_markdown(out: &mut String, report: &Report) {
    out.push_str("## Shared repairs\n\n");
    if report.shared_repairs.is_empty() {
        out.push_str("No evidence reference covers more than one required change.\n\n");
        return;
    }
    for group in &report.shared_repairs {
        out.push_str("- ");
        out.push_str(&one_line(&md_escape(&format!(
            "{} covers {}",
            group.reference,
            group.ids.join(", ")
        ))));
        out.push('\n');
    }
    out.push('\n');
}

/// Render discovered verification runs as Markdown.
fn render_runs_markdown(out: &mut String, report: &Report) {
    out.push_str("## Verification runs\n\n");
    if report.runs.is_empty() {
        out.push_str("No stored verification runs were found under the migration.\n\n");
        return;
    }
    for run in &report.runs {
        out.push_str("### ");
        out.push_str(&one_line(&md_escape(&format!("Run {}", run.run_id))));
        out.push('\n');
        out.push('\n');
        push_field(out, "Location", &run.relative_dir);
        push_field(out, "Harness hash", &run.harness_hash);
        push_field(
            out,
            "Harness changed since freeze",
            if run.harness_changed { "yes" } else { "no" },
        );
        if !run.sethu_version.is_empty() {
            push_field(out, "Sethu version", &run.sethu_version);
        }
        if !run.nextest_version.is_empty() {
            push_field(out, "Test runner", &run.nextest_version);
        }
        for check in &run.checks {
            out.push_str("- ");
            out.push_str(&one_line(&md_escape(&format!(
                "check {} ({}, {})",
                check.name,
                if check.role.is_empty() {
                    "unknown role"
                } else {
                    check.role.as_str()
                },
                if check.verified {
                    "verified"
                } else {
                    "not verified"
                }
            ))));
            out.push('\n');
            if !check.change_ids.is_empty() {
                out.push_str("  - covers ");
                out.push_str(&one_line(&md_escape(&check.change_ids.join(", "))));
                out.push('\n');
            }
            for stage in &check.stages {
                out.push_str("  - ");
                out.push_str(&one_line(&md_escape(&format!(
                    "{}: {} ({} trace entries)",
                    stage.stage, stage.verdict, stage.trace_entries
                ))));
                out.push('\n');
            }
        }
        out.push('\n');
    }
}

/// Render remaining decisions as Markdown.
fn render_decisions_markdown(out: &mut String, report: &Report) {
    out.push_str("## Remaining decisions\n\n");
    let open: Vec<&ChangeRow> = report
        .rows
        .iter()
        .filter(|row| {
            row.outcome.as_deref() == Some("decision_required")
                || row.outcome.as_deref() == Some("unresolved")
                || row.outcome.is_none()
        })
        .collect();
    if open.is_empty() {
        out.push_str("No change waits on a decision or on open work.\n\n");
        return;
    }
    for row in open {
        out.push_str("- ");
        let state = row.outcome.as_deref().unwrap_or("missing");
        match &row.note {
            Some(note) => out.push_str(&one_line(&md_escape(&format!(
                "{} ({state}): {note}",
                row.id
            )))),
            None => out.push_str(&one_line(&md_escape(&format!("{} ({state})", row.id)))),
        }
        out.push('\n');
    }
    out.push('\n');
}

/// Render limitations and converter findings as Markdown.
fn render_limitations_markdown(out: &mut String, report: &Report) {
    out.push_str("## Limitations\n\n");
    out.push_str("The diff tool compares contracts only. It never decides whether one application is affected.\n\n");
    for limit in &report.limits {
        out.push_str("- ");
        out.push_str(&one_line(&md_escape(limit)));
        out.push('\n');
    }
    out.push_str("- Fixed fixtures model the contracted behaviour. They are not proof about the live service.\n");
    out.push_str("- Scoped absence findings hold only within the recorded scope and revision.\n");
    for note in &report.conversion_notes {
        out.push_str("- ");
        out.push_str(&one_line(&md_escape(note)));
        out.push('\n');
    }
    out.push('\n');
    out.push_str("## Nullable idiom applications\n\n");
    out.push_str("The converter reads a nullable reference as a reference that also allows null. Each application below records where that reading applied.\n\n");
    if report.idioms.is_empty() {
        out.push_str("No nullable idiom application was recorded in the stored specs.\n\n");
    } else {
        for idiom in &report.idioms {
            out.push_str("- ");
            out.push_str(&one_line(&md_escape(&format!(
                "{} spec, component {}, {}: {}",
                idiom.spec, idiom.component, idiom.location, idiom.detail
            ))));
            out.push('\n');
        }
        out.push('\n');
    }
    if !report.unsupported.is_empty() {
        out.push_str("## Unsupported constructs\n\n");
        out.push_str("A scenario that touches one of these cannot support a verification claim on its own.\n\n");
        for issue in &report.unsupported {
            out.push_str("- ");
            out.push_str(&one_line(&md_escape(&format!(
                "{} spec, component {}, {} ({}): {}",
                issue.spec, issue.component, issue.location, issue.kind, issue.detail
            ))));
            out.push('\n');
        }
        out.push('\n');
    }
}

/// Render the full model as one static HTML file.
pub fn render_html(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>Migration report</title>\n<style>\n");
    out.push_str("body{font-family:sans-serif;max-width:72rem;margin:2rem auto;padding:0 1rem;line-height:1.5;}\n");
    out.push_str("table{border-collapse:collapse;width:100%;margin:1rem 0;}\n");
    out.push_str(
        "th,td{border:1px solid #999;padding:0.4rem 0.6rem;text-align:left;vertical-align:top;}\n",
    );
    out.push_str(
        "th{background:#eee;}\ncode{font-size:0.9em;}\n.missing{color:#900;font-weight:bold;}\n",
    );
    out.push_str("</style>\n</head>\n<body>\n<h1>Migration report</h1>\n");
    html_paragraph(&mut out, &format!("Attempt {}", report.attempt_id));
    render_scope_html(&mut out, report);
    render_accounting_html(&mut out, report);
    render_totals_html(&mut out, report);
    render_changes_html(&mut out, report);
    render_shared_html(&mut out, report);
    render_runs_html(&mut out, report);
    render_decisions_html(&mut out, report);
    render_limitations_html(&mut out, report);
    out.push_str("</body>\n</html>\n");
    out
}

/// Push one escaped paragraph.
fn html_paragraph(out: &mut String, text: &str) {
    out.push_str("<p>");
    out.push_str(&html_escape(text));
    out.push_str("</p>\n");
}

/// Push one table row of escaped cells.
fn html_row(out: &mut String, cells: &[String]) {
    out.push_str("<tr>");
    for cell in cells {
        out.push_str("<td>");
        out.push_str(&html_escape(cell));
        out.push_str("</td>");
    }
    out.push_str("</tr>\n");
}

/// Render scope, provenance, and tool versions as HTML.
fn render_scope_html(out: &mut String, report: &Report) {
    out.push_str("<h2>Scope and provenance</h2>\n<table>\n");
    let scope = if report.scope.is_empty() {
        "-".to_string()
    } else {
        report.scope.join(", ")
    };
    let invocation = if report.invocation.is_empty() {
        "-".to_string()
    } else {
        report.invocation.join(" ")
    };
    for (name, value) in [
        ("Consumer repository", report.repo_path.clone()),
        ("Baseline commit", report.baseline_commit.clone()),
        ("Scope", scope),
        ("Old spec", report.old_source.clone()),
        ("Old spec hash", report.old_spec_hash.clone()),
        ("Old spec origin", report.old_git.clone()),
        ("New spec", report.new_source.clone()),
        ("New spec hash", report.new_spec_hash.clone()),
        ("New spec origin", report.new_git.clone()),
        ("Sethu version", report.sethu_version.clone()),
        (
            "Diff tool",
            format!("{} {}", report.generator_name, report.generator_version),
        ),
        ("Diff invocation", invocation),
    ] {
        html_row(out, &[name.to_string(), value]);
    }
    out.push_str("</table>\n");
}

/// Render accounting and readiness as HTML.
fn render_accounting_html(out: &mut String, report: &Report) {
    out.push_str("<h2>Accounting and readiness</h2>\n<table>\n");
    let valid = report.rows.iter().filter(|row| row.valid).count();
    let accounted = if report.accounted {
        "yes".to_string()
    } else {
        format!(
            "no ({} of {} valid, {} problems)",
            valid,
            report.rows.len(),
            report.problems.len()
        )
    };
    let ready = if report.ready {
        "yes".to_string()
    } else {
        format!("no ({} blockers)", report.not_ready.len())
    };
    html_row(out, &["Accounted".to_string(), accounted]);
    html_row(out, &["Ready".to_string(), ready]);
    out.push_str("</table>\n");
    if report.problems.is_empty() {
        html_paragraph(out, "Problems: none.");
    } else {
        out.push_str("<h3>Problems</h3>\n<ul>\n");
        for problem in &report.problems {
            out.push_str("<li>");
            out.push_str(&html_escape(&format!(
                "{} {}: {}",
                problem.code, problem.id, problem.message
            )));
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
    if !report.not_ready.is_empty() {
        out.push_str("<h3>Blockers</h3>\n<ul>\n");
        for id in &report.not_ready {
            out.push_str("<li>");
            out.push_str(&html_escape(id));
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
}

/// Render severity totals and affected operations as HTML.
fn render_totals_html(out: &mut String, report: &Report) {
    out.push_str("<h2>Severity totals and affected operations</h2>\n<table>\n");
    for (name, value) in [
        ("Breaking", report.breaking_total.to_string()),
        ("Review", report.review_total.to_string()),
        ("Non-breaking", report.non_breaking_total.to_string()),
        ("Required", report.rows.len().to_string()),
        (
            "Unique affected operations",
            report.operations.len().to_string(),
        ),
    ] {
        html_row(out, &[name.to_string(), value]);
    }
    out.push_str("</table>\n<ul>\n");
    for operation in &report.operations {
        out.push_str("<li>");
        out.push_str(&html_escape(&format!(
            "{} ({} required changes)",
            operation.operation, operation.required
        )));
        out.push_str("</li>\n");
    }
    out.push_str("</ul>\n");
}

/// Render every required change as HTML.
fn render_changes_html(out: &mut String, report: &Report) {
    out.push_str("<h2>Required changes</h2>\n<table>\n<tr><th>Change</th><th>Contract</th><th>Disposition</th><th>Evidence</th></tr>\n");
    for row in &report.rows {
        out.push_str("<tr><td>");
        out.push_str(&html_escape(&row.id));
        out.push_str("</td><td>");
        out.push_str(&html_escape(&change_contract_line(row)));
        out.push_str("</td><td>");
        out.push_str(&html_escape(&change_disposition_line(row)));
        if let Some(note) = &row.note {
            out.push_str(" Note: ");
            out.push_str(&html_escape(note));
        }
        out.push_str("</td><td>");
        out.push_str(&html_escape(&change_evidence_line(row)));
        out.push_str("</td></tr>\n");
    }
    out.push_str("</table>\n");
    for row in &report.rows {
        out.push_str("<h3>");
        out.push_str(&html_escape(&row.id));
        out.push_str("</h3>\n<ul>\n");
        for (name, value) in change_detail_lines(row) {
            out.push_str("<li>");
            out.push_str(&html_escape(&format!("{name}: {value}")));
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
}

/// Render one change contract cell in plain words.
fn change_contract_line(row: &ChangeRow) -> String {
    let mut line = if row.method.is_empty() && row.path.is_empty() {
        "unknown endpoint".to_string()
    } else {
        format!("{} {}", row.method, row.path).trim().to_string()
    };
    line.push_str(&format!(", {}, {}", row.kind, row.severity));
    if !row.detail.is_empty() {
        line.push_str(&format!(", {}", row.detail));
    }
    line.push_str(&format!(". {}", row.origin));
    line.push('.');
    line
}

/// Render one disposition cell in plain words.
fn change_disposition_line(row: &ChangeRow) -> String {
    match &row.outcome {
        Some(outcome) => {
            let state = if row.valid {
                if row.ready {
                    "valid, ready"
                } else {
                    "valid, not ready"
                }
            } else {
                "invalid"
            };
            format!("{outcome} ({state})")
        }
        None => "missing, no ledger entry".to_string(),
    }
}

/// Render one evidence cell in plain words.
fn change_evidence_line(row: &ChangeRow) -> String {
    let mut parts = Vec::new();
    for reference in &row.code_refs {
        parts.push(format!("code {reference}"));
    }
    for reference in &row.file_refs {
        parts.push(format!("file {reference}"));
    }
    for link in &row.run_links {
        parts.push(format!("run {}", run_link_line(link)));
    }
    for reference in &row.text_refs {
        parts.push(format!("prose {reference}"));
    }
    for id in &row.covering_runs {
        parts.push(format!("covering run {id}"));
    }
    if !row.shared_with.is_empty() {
        parts.push(format!("shared repair with {}", row.shared_with.join(", ")));
    }
    for problem in &row.problems {
        parts.push(format!("evidence state: {problem}"));
    }
    if parts.is_empty() {
        if row.valid {
            return "none recorded".to_string();
        }
        return "missing or stale, see the problem list".to_string();
    }
    parts.join("; ")
}

/// Render one change detail list for its own section.
fn change_detail_lines(row: &ChangeRow) -> Vec<(String, String)> {
    let endpoint = if row.method.is_empty() && row.path.is_empty() {
        "-".to_string()
    } else {
        format!("{} {}", row.method, row.path).trim().to_string()
    };
    let mut lines = vec![
        ("Endpoint".to_string(), endpoint),
        ("Kind".to_string(), row.kind.clone()),
        ("Severity".to_string(), row.severity.clone()),
        ("Origin".to_string(), row.origin.clone()),
        ("Disposition".to_string(), change_disposition_line(row)),
    ];
    if !row.detail.is_empty() {
        lines.push(("Detail".to_string(), row.detail.clone()));
    }
    if let Some(note) = &row.note {
        lines.push(("Note".to_string(), note.clone()));
    }
    if row.history_len > 0 {
        lines.push((
            "Replaced dispositions".to_string(),
            row.history_len.to_string(),
        ));
    }
    if row.code_refs.is_empty() {
        lines.push(("Code references".to_string(), "none".to_string()));
    } else {
        lines.push(("Code references".to_string(), row.code_refs.join(", ")));
    }
    if !row.file_refs.is_empty() {
        lines.push(("File references".to_string(), row.file_refs.join(", ")));
    }
    if row.run_links.is_empty() {
        lines.push(("Run evidence".to_string(), "none".to_string()));
    } else {
        lines.push((
            "Run evidence".to_string(),
            row.run_links
                .iter()
                .map(run_link_line)
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    if !row.text_refs.is_empty() {
        lines.push(("Prose references".to_string(), row.text_refs.join(", ")));
    }
    if !row.covering_runs.is_empty() {
        lines.push(("Covering runs".to_string(), row.covering_runs.join(", ")));
    }
    if !row.shared_with.is_empty() {
        lines.push(("Shared repair with".to_string(), row.shared_with.join(", ")));
    }
    for problem in &row.problems {
        lines.push(("Evidence state".to_string(), problem.clone()));
    }
    lines
}

/// Render shared repairs as HTML.
fn render_shared_html(out: &mut String, report: &Report) {
    out.push_str("<h2>Shared repairs</h2>\n");
    if report.shared_repairs.is_empty() {
        html_paragraph(
            out,
            "No evidence reference covers more than one required change.",
        );
        return;
    }
    out.push_str("<ul>\n");
    for group in &report.shared_repairs {
        out.push_str("<li>");
        out.push_str(&html_escape(&format!(
            "{} covers {}",
            group.reference,
            group.ids.join(", ")
        )));
        out.push_str("</li>\n");
    }
    out.push_str("</ul>\n");
}

/// Render discovered verification runs as HTML.
fn render_runs_html(out: &mut String, report: &Report) {
    out.push_str("<h2>Verification runs</h2>\n");
    if report.runs.is_empty() {
        html_paragraph(
            out,
            "No stored verification runs were found under the migration.",
        );
        return;
    }
    for run in &report.runs {
        out.push_str("<h3>");
        out.push_str(&html_escape(&format!("Run {}", run.run_id)));
        out.push_str("</h3>\n<table>\n");
        html_row(out, &["Location".to_string(), run.relative_dir.clone()]);
        html_row(out, &["Harness hash".to_string(), run.harness_hash.clone()]);
        html_row(
            out,
            &[
                "Harness changed since freeze".to_string(),
                if run.harness_changed {
                    "yes".to_string()
                } else {
                    "no".to_string()
                },
            ],
        );
        if !run.sethu_version.is_empty() {
            html_row(
                out,
                &["Sethu version".to_string(), run.sethu_version.clone()],
            );
        }
        if !run.nextest_version.is_empty() {
            html_row(
                out,
                &["Test runner".to_string(), run.nextest_version.clone()],
            );
        }
        out.push_str("</table>\n<ul>\n");
        for check in &run.checks {
            let role = if check.role.is_empty() {
                "unknown role"
            } else {
                check.role.as_str()
            };
            out.push_str("<li>");
            out.push_str(&html_escape(&format!(
                "check {} ({}, {})",
                check.name,
                role,
                if check.verified {
                    "verified"
                } else {
                    "not verified"
                }
            )));
            if !check.change_ids.is_empty() {
                out.push_str(&html_escape(&format!(
                    " covers {}",
                    check.change_ids.join(", ")
                )));
            }
            out.push_str("</li>\n");
            for stage in &check.stages {
                out.push_str("<li>");
                out.push_str(&html_escape(&format!(
                    "{}: {} ({} trace entries)",
                    stage.stage, stage.verdict, stage.trace_entries
                )));
                out.push_str("</li>\n");
            }
        }
        out.push_str("</ul>\n");
    }
}

/// Render remaining decisions as HTML.
fn render_decisions_html(out: &mut String, report: &Report) {
    out.push_str("<h2>Remaining decisions</h2>\n");
    let open: Vec<&ChangeRow> = report
        .rows
        .iter()
        .filter(|row| {
            row.outcome.as_deref() == Some("decision_required")
                || row.outcome.as_deref() == Some("unresolved")
                || row.outcome.is_none()
        })
        .collect();
    if open.is_empty() {
        html_paragraph(out, "No change waits on a decision or on open work.");
        return;
    }
    out.push_str("<ul>\n");
    for row in open {
        let state = row.outcome.as_deref().unwrap_or("missing");
        out.push_str("<li>");
        match &row.note {
            Some(note) => out.push_str(&html_escape(&format!("{} ({state}): {note}", row.id))),
            None => out.push_str(&html_escape(&format!("{} ({state})", row.id))),
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ul>\n");
}

/// Render limitations and converter findings as HTML.
fn render_limitations_html(out: &mut String, report: &Report) {
    out.push_str("<h2>Limitations</h2>\n");
    html_paragraph(
        out,
        "The diff tool compares contracts only. It never decides whether one application is affected.",
    );
    out.push_str("<ul>\n");
    for limit in &report.limits {
        out.push_str("<li>");
        out.push_str(&html_escape(limit));
        out.push_str("</li>\n");
    }
    for limit in [
        "Fixed fixtures model the contracted behaviour. They are not proof about the live service.",
        "Scoped absence findings hold only within the recorded scope and revision.",
    ] {
        out.push_str("<li>");
        out.push_str(&html_escape(limit));
        out.push_str("</li>\n");
    }
    for note in &report.conversion_notes {
        out.push_str("<li>");
        out.push_str(&html_escape(note));
        out.push_str("</li>\n");
    }
    out.push_str("</ul>\n<h2>Nullable idiom applications</h2>\n");
    html_paragraph(
        out,
        "The converter reads a nullable reference as a reference that also allows null. Each application below records where that reading applied.",
    );
    if report.idioms.is_empty() {
        html_paragraph(
            out,
            "No nullable idiom application was recorded in the stored specs.",
        );
    } else {
        out.push_str("<ul>\n");
        for idiom in &report.idioms {
            out.push_str("<li>");
            out.push_str(&html_escape(&format!(
                "{} spec, component {}, {}: {}",
                idiom.spec, idiom.component, idiom.location, idiom.detail
            )));
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
    if !report.unsupported.is_empty() {
        out.push_str("<h2>Unsupported constructs</h2>\n");
        html_paragraph(
            out,
            "A scenario that touches one of these cannot support a verification claim on its own.",
        );
        out.push_str("<ul>\n");
        for issue in &report.unsupported {
            out.push_str("<li>");
            out.push_str(&html_escape(&format!(
                "{} spec, component {}, {} ({}): {}",
                issue.spec, issue.component, issue.location, issue.kind, issue.detail
            )));
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_covers_structural_characters() {
        assert_eq!(
            html_escape("<script>alert(\"x\")</script>"),
            "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;"
        );
        assert_eq!(html_escape("a&b'c"), "a&amp;b&#39;c");
        assert_eq!(html_escape("plain"), "plain");
    }

    #[test]
    fn md_escape_fences_values_as_code_spans() {
        assert_eq!(md_escape("vc1_plain"), "`vc1_plain`");
        assert_eq!(
            md_escape("[evil](http://example.com) <b>bold</b> *em*"),
            "`[evil](http://example.com) <b>bold</b> *em*`"
        );
        assert_eq!(md_escape("has `tick"), "``has `tick``");
        assert_eq!(md_escape("`edge`"), "`` `edge` ``");
    }

    #[test]
    fn injection_renders_as_text_in_both_formats() {
        let report = super::super::model::Report {
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
            breaking_total: 1,
            review_total: 0,
            non_breaking_total: 0,
            operations: Vec::new(),
            rows: vec![super::super::model::ChangeRow {
                id: "<script>alert(1)</script>".to_string(),
                method: "GET".to_string(),
                path: "/x".to_string(),
                kind: "response_schema_changed".to_string(),
                severity: "breaking".to_string(),
                detail: String::new(),
                origin: "unknown origin".to_string(),
                outcome: Some("unresolved".to_string()),
                valid: false,
                ready: false,
                note: Some("[click](http://evil) <img src=x>".to_string()),
                history_len: 0,
                code_refs: Vec::new(),
                file_refs: Vec::new(),
                run_links: Vec::new(),
                text_refs: vec!["<b>prose</b>".to_string()],
                covering_runs: Vec::new(),
                shared_with: Vec::new(),
                problems: vec!["missing_evidence: too little".to_string()],
            }],
            shared_repairs: Vec::new(),
            runs: Vec::new(),
            idioms: Vec::new(),
            unsupported: Vec::new(),
            conversion_notes: Vec::new(),
            limits: vec!["Only the first media type is compared.".to_string()],
        };
        let markdown = render_markdown(&report);
        assert!(markdown.contains("`<script>alert(1)</script>`"));
        assert!(markdown.contains("`[click](http://evil) <img src=x>`"));
        let html = render_html(&report);
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img "));
    }
}
