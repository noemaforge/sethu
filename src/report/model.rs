//! Reviewable migration model assembled from validated state.
//!
//! Assembly reads the attempt manifest, the pair provenance, the capture
//! record and change list, the stored origins, and the ledger through the
//! shared state helpers. It then revalidates the ledger through the check
//! evaluator, so the model never trusts hand edits. Every content block
//! the listing needs lives here. Rendering stays in the sibling module.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;

use crate::check::{Evaluation, Inputs, Problem, evaluate};
use crate::provenance::{Origin, OriginsDocument};
use crate::state::attempt::AttemptRecord;
use crate::state::capture::CaptureRecord;
use crate::state::layout;
use crate::state::ledger::{self, CheckedEvidence, EvidenceKind, Ledger};
use crate::state::pair::PairRecord;
use crate::stub::schema::{Direction, Rule, SpecIndex};
use crate::vimanam::{ChangeKind, DiffDocument, Severity};

use super::runs::{DiscoveredRun, discover_runs};

/// Known behaviour limits of the contract diff tool.
///
/// The listing states these instead of hiding them, so readers never
/// mistake a silent tool boundary for application safety.
const TOOL_LIMITS: &[&str] = &[
    "Only the first media type of a body or a response is compared.",
    "A path template rename appears as a removal plus an addition.",
    "Choice members are compared by index.",
    "In newer specs, nullability written as a type union is not detected. Only the nullable keyword is.",
];

/// One operation touched by the required changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRow {
    /// Operation label, such as `POST /search/random`.
    pub operation: String,
    /// Required changes at this operation.
    pub required: usize,
}

/// One cited verification run behind a ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLink {
    /// Evidence reference exactly as stored in the ledger.
    pub reference: String,
    /// Discovered run id, when a stored run matches the reference.
    pub run_id: Option<String>,
    /// Whether the matched run verified its checks.
    pub verified: Option<bool>,
    /// Stage verdicts of the matched run for the citing check.
    pub stages: Vec<String>,
    /// True when no stored run matches the reference.
    pub missing: bool,
    /// True when the matched run is superseded by a harness change.
    pub superseded: bool,
}

/// One required change with its validated disposition and evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRow {
    /// Stable change id from the capture.
    pub id: String,
    /// HTTP method from the capture.
    pub method: String,
    /// Path template from the capture.
    pub path: String,
    /// Change class word from the capture.
    pub kind: String,
    /// Severity word from the capture.
    pub severity: String,
    /// Short change detail, such as a status or a pointer.
    pub detail: String,
    /// Shared origin in plain words.
    pub origin: String,
    /// Stored outcome word, or none when the ledger names none.
    pub outcome: Option<String>,
    /// Whether the disposition is structurally valid and backed.
    pub valid: bool,
    /// Whether the valid disposition resolves the change.
    pub ready: bool,
    /// Stored note, when the writer left one.
    pub note: Option<String>,
    /// Replaced dispositions kept in the same ledger file.
    pub history_len: usize,
    /// Evidence references naming a repository code location.
    pub code_refs: Vec<String>,
    /// Evidence references naming a repository file.
    pub file_refs: Vec<String>,
    /// Evidence references naming a verification run.
    pub run_links: Vec<RunLink>,
    /// Evidence references that count as prose alone.
    pub text_refs: Vec<String>,
    /// Stored run ids whose checks cover this change.
    pub covering_runs: Vec<String>,
    /// Other required ids sharing at least one evidence reference.
    pub shared_with: Vec<String>,
    /// Problem messages the checker raised for this change.
    pub problems: Vec<String>,
}

/// One evidence reference shared by several required changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedRepair {
    /// Evidence reference exactly as stored.
    pub reference: String,
    /// Required ids citing the reference, in report order.
    pub ids: Vec<String>,
}

/// One nullable idiom application found by the schema converter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdiomApplication {
    /// Which stored spec carries the idiom.
    pub spec: String,
    /// Component holding the converted node.
    pub component: String,
    /// Pointer to the converted node.
    pub location: String,
    /// Short factual note from the converter.
    pub detail: String,
}

/// One unsupported construct found by the schema converter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedNote {
    /// Which stored spec carries the construct.
    pub spec: String,
    /// Component holding the converted node.
    pub component: String,
    /// Pointer to the converted node.
    pub location: String,
    /// Kind word of the construct.
    pub kind: String,
    /// Short factual note from the converter.
    pub detail: String,
}

/// Full reviewable model of one migration attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Full id of the reported attempt.
    pub attempt_id: String,
    /// Binary version that created the attempt.
    pub sethu_version: String,
    /// Diff tool name from the capture record.
    pub generator_name: String,
    /// Diff tool version from the capture record.
    pub generator_version: String,
    /// Exact diff tool invocation from the capture record.
    pub invocation: Vec<String>,
    /// Full hash of the old spec.
    pub old_spec_hash: String,
    /// Full hash of the new spec.
    pub new_spec_hash: String,
    /// Provenance of the old spec file.
    pub old_source: String,
    /// Provenance of the new spec file.
    pub new_source: String,
    /// Old spec origin remote, or unknown outside a repository.
    pub old_git: String,
    /// New spec origin remote, or unknown outside a repository.
    pub new_git: String,
    /// Canonical consumer repository path from the manifest.
    pub repo_path: String,
    /// Consumer baseline commit from the manifest.
    pub baseline_commit: String,
    /// Declared scope paths from the manifest.
    pub scope: Vec<String>,
    /// Whether every required change carries a valid disposition.
    pub accounted: bool,
    /// Whether every valid disposition resolves its change.
    pub ready: bool,
    /// Required ids blocking readiness.
    pub not_ready: Vec<String>,
    /// Checker problems in deterministic order.
    pub problems: Vec<Problem>,
    /// Breaking changes across the whole capture.
    pub breaking_total: usize,
    /// Review changes across the whole capture.
    pub review_total: usize,
    /// Non-breaking changes across the whole capture.
    pub non_breaking_total: usize,
    /// Unique operations across the required changes.
    pub operations: Vec<OperationRow>,
    /// Every required change in capture order.
    pub rows: Vec<ChangeRow>,
    /// Evidence references shared by several required changes.
    pub shared_repairs: Vec<SharedRepair>,
    /// Verification runs found under the migration.
    pub runs: Vec<DiscoveredRun>,
    /// Nullable idiom applications in the stored specs.
    pub idioms: Vec<IdiomApplication>,
    /// Unsupported constructs in the stored specs.
    pub unsupported: Vec<UnsupportedNote>,
    /// Converter or loader notes that are limitations, not failures.
    pub conversion_notes: Vec<String>,
    /// Known tool limits stated plainly.
    pub limits: Vec<String>,
}

/// Loaded files behind one report.
///
/// Callers read every file through the state helpers first. Missing or
/// unreadable capture state is a tool failure for the caller, raised with
/// the file named.
struct Loaded {
    /// Attempt manifest bound to one capture and one consumer state.
    manifest: AttemptRecord,
    /// Pair provenance for both input specs.
    pair: PairRecord,
    /// Capture record bound to the same capture id.
    capture: CaptureRecord,
    /// Parsed change list from the attempt capture.
    document: DiffDocument,
    /// Stored origins for the capture, when the file exists.
    origins: Option<OriginsDocument>,
    /// Raw ledger bytes, or none when no ledger file exists yet.
    ledger_bytes: Option<Vec<u8>>,
    /// Typed ledger for notes and history, or empty when absent.
    ledger: Ledger,
    /// Pair directory holding the copied input specs.
    pair_dir: std::path::PathBuf,
    /// Verification runs found under the migration.
    runs: Vec<DiscoveredRun>,
}

/// Assemble the reviewable model for one migration.
///
/// The function never writes. It revalidates the ledger through the check
/// evaluator and marks missing or stale evidence instead of omitting it.
/// An incomplete migration still yields a useful listing of every
/// required change.
pub fn assemble(repo: &Path, migration: &Path) -> anyhow::Result<Report> {
    let loaded = load(repo, migration)?;
    let evaluation = evaluate(&Inputs {
        manifest: &loaded.manifest,
        capture: &loaded.capture,
        origins: loaded.origins.as_ref(),
        document: &loaded.document,
        ledger_bytes: loaded.ledger_bytes.as_deref(),
        repo,
    });
    Ok(build(&loaded, &evaluation, repo))
}

/// Load every state file the model reads.
///
/// Pair and capture records are read through the state helpers. A missing
/// ledger reads as empty, so every required change reports missing. A
/// missing origins file reads as none, so older captures still report.
fn load(repo: &Path, migration: &Path) -> anyhow::Result<Loaded> {
    let root = layout::state_root(repo);
    let manifest: AttemptRecord = crate::state::read_state_file(&layout::manifest_path(migration))?;
    manifest.validate()?;
    let pair = layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash)?;
    let pair_record: PairRecord = crate::state::read_state_file(&layout::pair_file(&pair))?;
    pair_record.validate()?;
    let capture_dir = layout::capture_dir(&pair, &manifest.capture_id);
    let capture_record: CaptureRecord =
        crate::state::read_state_file(&layout::capture_file(&capture_dir))?;
    capture_record.validate()?;
    let changes_path = layout::changes_file(&capture_dir);
    let raw = std::fs::read(&changes_path)
        .with_context(|| format!("read capture change list {}", changes_path.display()))?;
    let document = crate::vimanam::parse_diff_output(&raw)?;
    let origins =
        match crate::state::read_state_file::<OriginsDocument>(&layout::origins_file(&capture_dir))
        {
            Ok(origins) => {
                origins.validate()?;
                Some(origins)
            }
            Err(err) => {
                let missing = err.chain().any(|cause| {
                    matches!(cause.downcast_ref::<std::io::Error>(),
                    Some(io) if io.kind() == std::io::ErrorKind::NotFound)
                });
                if missing {
                    None
                } else {
                    return Err(err);
                }
            }
        };
    let ledger_path = layout::ledger_path(migration);
    let ledger_bytes = match std::fs::read(&ledger_path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("read ledger {}", ledger_path.display()))
            );
        }
    };
    let ledger = ledger::load_ledger(migration)?;
    let runs = discover_runs(migration);
    Ok(Loaded {
        manifest,
        pair: pair_record,
        capture: capture_record,
        document,
        origins,
        ledger_bytes,
        ledger,
        pair_dir: pair,
        runs,
    })
}

/// Build the model from loaded state and a fresh evaluation.
///
/// Every required change gets a row in capture order, including changes
/// with no ledger entry. Ledger notes appear verbatim. The function adds
/// no impact claim the ledger does not already record.
fn build(loaded: &Loaded, evaluation: &Evaluation, repo: &Path) -> Report {
    let required: BTreeSet<&str> = evaluation.required.iter().map(String::as_str).collect();
    let mut operations: BTreeMap<String, usize> = BTreeMap::new();
    for item in &loaded.document.changes {
        if required.contains(item.id.as_str()) {
            *operations
                .entry(format!("{} {}", item.endpoint.method, item.endpoint.path))
                .or_insert(0) += 1;
        }
    }
    let mut rows = Vec::with_capacity(evaluation.required.len());
    for id in &evaluation.required {
        rows.push(build_row(loaded, evaluation, repo, id));
    }
    let shared_repairs = shared_repairs(&rows);
    for group in &shared_repairs {
        for id in &group.ids {
            if let Some(row) = rows.iter_mut().find(|row| &row.id == id) {
                row.shared_with = group
                    .ids
                    .iter()
                    .filter(|other| *other != id)
                    .cloned()
                    .collect();
            }
        }
    }
    let (idioms, unsupported, conversion_notes) = conversion_findings(&loaded.pair_dir);
    Report {
        attempt_id: loaded.manifest.attempt_id.clone(),
        sethu_version: loaded.manifest.sethu_version.clone(),
        generator_name: loaded.capture.generator_name.clone(),
        generator_version: loaded.capture.generator_version.clone(),
        invocation: loaded.capture.invocation.clone(),
        old_spec_hash: loaded.manifest.old_spec_hash.clone(),
        new_spec_hash: loaded.manifest.new_spec_hash.clone(),
        old_source: loaded.pair.old_provenance.source.clone(),
        new_source: loaded.pair.new_provenance.source.clone(),
        old_git: git_label(
            &loaded.pair.old_provenance.git_remote,
            &loaded.pair.old_provenance.git_commit,
        ),
        new_git: git_label(
            &loaded.pair.new_provenance.git_remote,
            &loaded.pair.new_provenance.git_commit,
        ),
        repo_path: loaded.manifest.repo_path.clone(),
        baseline_commit: loaded.manifest.baseline_commit.clone(),
        scope: loaded.manifest.scope.clone(),
        accounted: evaluation.accounted,
        ready: evaluation.ready,
        not_ready: evaluation.not_ready.clone(),
        problems: evaluation.problems.clone(),
        breaking_total: loaded.document.summary.breaking,
        review_total: loaded.document.summary.review,
        non_breaking_total: loaded.document.summary.non_breaking,
        operations: operations
            .into_iter()
            .map(|(operation, required)| OperationRow {
                operation,
                required,
            })
            .collect(),
        rows,
        shared_repairs,
        runs: loaded.runs.clone(),
        idioms,
        unsupported,
        conversion_notes,
        limits: TOOL_LIMITS.iter().map(|limit| limit.to_string()).collect(),
    }
}

/// Build one row for a required change.
///
/// The capture supplies identity, the evaluation supplies the verdict,
/// and the typed ledger supplies the note and the history count. Run
/// references resolve against the discovered runs. A reference with no
/// matching run is marked missing, never dropped.
fn build_row(loaded: &Loaded, evaluation: &Evaluation, repo: &Path, id: &str) -> ChangeRow {
    let record = loaded.document.changes.iter().find(|item| item.id == id);
    let view = evaluation.dispositions.get(id);
    let stored = loaded.ledger.dispositions.get(id);
    let mut code_refs = Vec::new();
    let mut file_refs = Vec::new();
    let mut run_links = Vec::new();
    let mut text_refs = Vec::new();
    if let Some(view) = view {
        for reference in &view.evidence {
            let checked = ledger::check_reference(repo, reference).unwrap_or(CheckedEvidence {
                reference: reference.clone(),
                kind: EvidenceKind::Text,
            });
            match checked.kind {
                EvidenceKind::Code => code_refs.push(reference.clone()),
                EvidenceKind::File => file_refs.push(reference.clone()),
                EvidenceKind::Run => run_links.push(link_run(reference, &loaded.runs)),
                EvidenceKind::Text => text_refs.push(reference.clone()),
            }
        }
    }
    let mut covering = BTreeSet::new();
    for run in &loaded.runs {
        for check in &run.checks {
            if check.change_ids.iter().any(|item| item == id) {
                covering.insert(run.run_id.clone());
            }
        }
    }
    let problems: Vec<String> = evaluation
        .problems
        .iter()
        .filter(|problem| problem.id == id)
        .map(|problem| format!("{}: {}", problem.code, problem.message))
        .collect();
    ChangeRow {
        id: id.to_string(),
        method: record
            .map(|item| item.endpoint.method.clone())
            .unwrap_or_default(),
        path: record
            .map(|item| item.endpoint.path.clone())
            .unwrap_or_default(),
        kind: record.map(|item| kind_word(&item.kind)).unwrap_or_default(),
        severity: record
            .map(|item| severity_word(&item.severity))
            .unwrap_or_default(),
        detail: record.map(detail_line).unwrap_or_default(),
        origin: origin_line(loaded.origins.as_ref(), id),
        outcome: view.map(|view| view.outcome.clone()),
        valid: view.is_some_and(|view| view.valid),
        ready: view.is_some_and(|view| view.ready),
        note: view.and_then(|view| view.note.clone()),
        history_len: stored.map(|entry| entry.history.len()).unwrap_or(0),
        code_refs,
        file_refs,
        run_links,
        text_refs,
        covering_runs: covering.into_iter().collect(),
        shared_with: Vec::new(),
        problems,
    }
}

/// Link one stored run reference to the discovered runs.
///
/// Matching resolves the run id and check name independently. Run ids may
/// use a unique prefix. A missing or ambiguous run or check stays unlinked.
/// A matched run with a changed harness remains marked superseded.
fn link_run(reference: &str, runs: &[DiscoveredRun]) -> RunLink {
    let (wanted, named_check) = match crate::check::parse_run_reference(reference) {
        Ok(parsed) => (parsed.run_id, Some(parsed.check)),
        Err(_) => match reference.strip_prefix("run:") {
            Some(wanted) if !wanted.is_empty() && !wanted.contains('/') => {
                (wanted.to_string(), None)
            }
            _ => return missing_run_link(reference),
        },
    };
    let mut hits = runs.iter().filter(|run| run.run_id.starts_with(&wanted));
    let first = hits.next();
    let ambiguous = hits.next().is_some();
    match first {
        Some(run) if !ambiguous => RunLink {
            reference: reference.to_string(),
            run_id: Some(run.run_id.clone()),
            verified: named_check
                .as_ref()
                .and_then(|name| {
                    let mut checks = run.checks.iter().filter(|check| check.name == *name);
                    let check = checks.next()?;
                    checks.next().is_none().then_some(check.verified)
                })
                .or_else(|| {
                    named_check
                        .is_none()
                        .then(|| run.checks.iter().all(|check| check.verified))
                }),
            stages: match named_check.as_ref() {
                Some(name) => {
                    let mut checks = run.checks.iter().filter(|check| check.name == *name);
                    let check = checks.next();
                    if checks.next().is_some() {
                        return missing_run_link(reference);
                    }
                    let Some(check) = check else {
                        return missing_run_link(reference);
                    };
                    check
                        .stages
                        .iter()
                        .map(|stage| format!("{} {}: {}", check.name, stage.stage, stage.verdict))
                        .collect()
                }
                None => run
                    .checks
                    .iter()
                    .flat_map(|check| {
                        check.stages.iter().map(|stage| {
                            format!("{} {}: {}", check.name, stage.stage, stage.verdict)
                        })
                    })
                    .collect(),
            },
            missing: false,
            superseded: run.harness_changed,
        },
        _ => missing_run_link(reference),
    }
}

/// Keep unresolved references visible without attributing another run or check.
fn missing_run_link(reference: &str) -> RunLink {
    RunLink {
        reference: reference.to_string(),
        run_id: None,
        verified: None,
        stages: Vec::new(),
        missing: true,
        superseded: false,
    }
}

/// Group required changes that cite the same evidence reference.
///
/// Groups hold at least two changes. Both lists sort, so shared repairs
/// render deterministically. Callers fill each row's partner list from
/// these groups.
fn shared_repairs(rows: &[ChangeRow]) -> Vec<SharedRepair> {
    let mut by_reference: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in rows {
        let mut seen = BTreeSet::new();
        for reference in row
            .code_refs
            .iter()
            .chain(row.file_refs.iter())
            .chain(row.run_links.iter().map(|link| &link.reference))
        {
            if seen.insert(reference.as_str()) {
                by_reference
                    .entry(reference.as_str())
                    .or_default()
                    .insert(row.id.as_str());
            }
        }
        for reference in &row.text_refs {
            if seen.insert(reference.as_str()) {
                by_reference
                    .entry(reference.as_str())
                    .or_default()
                    .insert(row.id.as_str());
            }
        }
    }
    by_reference
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(reference, ids)| SharedRepair {
            reference: reference.to_string(),
            ids: ids.into_iter().map(str::to_string).collect(),
        })
        .collect()
}

/// Render one change class as its stored word.
fn kind_word(kind: &ChangeKind) -> String {
    match kind {
        ChangeKind::EndpointAdded => "endpoint_added".to_string(),
        ChangeKind::EndpointRemoved => "endpoint_removed".to_string(),
        ChangeKind::ParameterAdded => "parameter_added".to_string(),
        ChangeKind::ParameterRemoved => "parameter_removed".to_string(),
        ChangeKind::ParameterRequiredChanged => "parameter_required_changed".to_string(),
        ChangeKind::ParameterLocationChanged => "parameter_location_changed".to_string(),
        ChangeKind::ParameterSchemaChanged => "parameter_schema_changed".to_string(),
        ChangeKind::ResponseAdded => "response_added".to_string(),
        ChangeKind::ResponseRemoved => "response_removed".to_string(),
        ChangeKind::OperationIdChanged => "operation_id_changed".to_string(),
        ChangeKind::DeprecatedChanged => "deprecated_changed".to_string(),
        ChangeKind::RequestSchemaChanged => "request_schema_changed".to_string(),
        ChangeKind::ResponseSchemaChanged => "response_schema_changed".to_string(),
    }
}

/// Render one severity as its stored word.
fn severity_word(severity: &Severity) -> String {
    match severity {
        Severity::Breaking => "breaking".to_string(),
        Severity::NonBreaking => "non_breaking".to_string(),
        Severity::Review => "review".to_string(),
    }
}

/// Summarise one change record in a single line.
///
/// The line names the status, parameter, or schema pointer the record
/// concerns. It never interprets the change, only locates it.
fn detail_line(record: &crate::vimanam::ChangeRecord) -> String {
    let details = &record.details;
    if let Some(status) = &details.status {
        if let Some(change) = &details.schema_change {
            return format!("status {status} schema {}", change.pointer);
        }
        return format!("status {status}");
    }
    if let Some(name) = &details.name {
        if let Some(location) = &details.location {
            return format!("parameter {name} in {location}");
        }
        return format!("parameter {name}");
    }
    if let Some(change) = &details.schema_change {
        return format!("schema {}", change.pointer);
    }
    String::new()
}

/// Render one stored origin in plain words.
fn origin_line(origins: Option<&OriginsDocument>, id: &str) -> String {
    let Some(document) = origins else {
        return "origin not recorded".to_string();
    };
    match document.origins.get(id) {
        Some(Origin::Component { name }) => format!("shared component {name}"),
        Some(Origin::Operation {
            position,
            old,
            new,
            still_references,
        }) => {
            if still_references.is_empty() {
                format!("operation reference at {position} ({old} to {new})")
            } else {
                format!(
                    "operation reference at {position} ({old} to {new}), still used by {}",
                    still_references.join(", ")
                )
            }
        }
        Some(Origin::Unknown) | None => "unknown origin".to_string(),
    }
}

/// Label one spec origin from its remote and commit.
fn git_label(remote: &str, commit: &str) -> String {
    if remote == crate::state::pair::UNKNOWN && commit == crate::state::pair::UNKNOWN {
        return "unknown".to_string();
    }
    format!("{remote} at {commit}")
}

/// Find converter findings across both stored specs.
///
/// Every named component of each stored spec converts in both request
/// and response directions. Nullable idiom applications and unsupported
/// constructs are collected with their component and location. A spec
/// that cannot be read or converted yields a limitation note instead of
/// failing the whole listing.
fn conversion_findings(pair: &Path) -> (Vec<IdiomApplication>, Vec<UnsupportedNote>, Vec<String>) {
    let mut idioms = Vec::new();
    let mut unsupported = Vec::new();
    let mut notes = Vec::new();
    for (label, path) in [
        ("old", crate::state::pair::inputs_old_path(pair)),
        ("new", crate::state::pair::inputs_new_path(pair)),
    ] {
        match read_spec(&path) {
            Ok(spec) => convert_spec(label, &spec, &mut idioms, &mut unsupported, &mut notes),
            Err(err) => notes.push(format!(
                "converter skipped the {label} spec at {}: {err:#}",
                path.display()
            )),
        }
    }
    idioms.sort_by(|left, right| {
        (&left.spec, &left.component, &left.location, &left.detail).cmp(&(
            &right.spec,
            &right.component,
            &right.location,
            &right.detail,
        ))
    });
    idioms.dedup();
    unsupported.sort_by(|left, right| {
        (
            &left.spec,
            &left.component,
            &left.location,
            &left.kind,
            &left.detail,
        )
            .cmp(&(
                &right.spec,
                &right.component,
                &right.location,
                &right.kind,
                &right.detail,
            ))
    });
    unsupported.dedup();
    (idioms, unsupported, notes)
}

/// Read one stored spec as parsed data.
///
/// The stored inputs are JSON. The error names the file.
fn read_spec(path: &Path) -> anyhow::Result<serde_json::Value> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read stored spec {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse stored spec {}", path.display()))
}

/// Convert every named component of one spec and collect the findings.
///
/// A component that fails to convert yields a limitation note naming the
/// component. Successful conversions contribute their idiom applications
/// and their unsupported constructs.
fn convert_spec(
    label: &str,
    spec: &serde_json::Value,
    idioms: &mut Vec<IdiomApplication>,
    unsupported: &mut Vec<UnsupportedNote>,
    notes: &mut Vec<String>,
) {
    let index = match SpecIndex::from_spec(spec) {
        Ok(index) => index,
        Err(err) => {
            notes.push(format!("converter skipped the {label} spec index: {err:#}"));
            return;
        }
    };
    let mut names: Vec<String> = index_names(spec);
    names.sort();
    names.dedup();
    for name in &names {
        for direction in [Direction::Request, Direction::Response] {
            let converted = crate::stub::schema::convert_named(&index, name, direction);
            match converted {
                Ok(document) => {
                    for application in &document.applications {
                        if application.rule == Rule::NullableReference {
                            idioms.push(IdiomApplication {
                                spec: label.to_string(),
                                component: name.clone(),
                                location: application.location.clone(),
                                detail: application.detail.clone(),
                            });
                        }
                    }
                    for issue in &document.unsupported {
                        unsupported.push(UnsupportedNote {
                            spec: label.to_string(),
                            component: name.clone(),
                            location: issue.location.clone(),
                            kind: unsupported_word(&issue.kind),
                            detail: issue.detail.clone(),
                        });
                    }
                }
                Err(err) => {
                    notes.push(format!(
                        "converter skipped component {name} of the {label} spec: {err:#}"
                    ));
                }
            }
        }
    }
}

/// List component names of one parsed spec in document order.
///
/// Both the components map and the older definitions map count, so the
/// converter visits the same names the stub index holds.
fn index_names(spec: &serde_json::Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(map) = spec
        .get("components")
        .and_then(|components| components.get("schemas"))
        .and_then(serde_json::Value::as_object)
    {
        names.extend(map.keys().cloned());
    }
    if let Some(map) = spec
        .get("definitions")
        .and_then(serde_json::Value::as_object)
    {
        names.extend(map.keys().cloned());
    }
    names
}

/// Render one unsupported kind as its stored word.
fn unsupported_word(kind: &crate::stub::schema::UnsupportedKind) -> String {
    match kind {
        crate::stub::schema::UnsupportedKind::Discriminator => "discriminator".to_string(),
        crate::stub::schema::UnsupportedKind::NullableWithoutType => {
            "nullable_without_type".to_string()
        }
        crate::stub::schema::UnsupportedKind::ExternalRef => "external_ref".to_string(),
        crate::stub::schema::UnsupportedKind::Format => "format".to_string(),
        crate::stub::schema::UnsupportedKind::Xml => "xml".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::runs::{DiscoveredCheck, DiscoveredRun, StageInfo};

    fn stage(check: &str, stage: &str, verdict: &str) -> StageInfo {
        StageInfo {
            check: check.to_string(),
            stage: stage.to_string(),
            verdict: verdict.to_string(),
            detail: String::new(),
            parser: "nextest-junit".to_string(),
            trace_entries: 1,
        }
    }

    fn check(name: &str, verified: bool, stages: Vec<StageInfo>) -> DiscoveredCheck {
        DiscoveredCheck {
            name: name.to_string(),
            role: "regression".to_string(),
            change_ids: vec!["vc1_example".to_string()],
            verified,
            stages,
        }
    }

    fn run(run_id: &str, harness_changed: bool) -> DiscoveredRun {
        DiscoveredRun {
            run_id: run_id.to_string(),
            relative_dir: format!("runs/{run_id}"),
            harness_hash: "abc".to_string(),
            harness_changed,
            sethu_version: "0.1.0".to_string(),
            nextest_version: "nextest".to_string(),
            checks: vec![
                check(
                    "picker",
                    true,
                    vec![stage("picker", "original-old", "pass")],
                ),
                check(
                    "unrelated",
                    false,
                    vec![stage("unrelated", "original-new", "invalid_red")],
                ),
            ],
        }
    }

    #[test]
    fn named_run_reference_links_only_the_named_check() {
        let runs = vec![run("run-123", true)];

        let link = link_run("run:run-123/picker", &runs);

        assert_eq!(link.run_id.as_deref(), Some("run-123"));
        assert_eq!(link.verified, Some(true));
        assert_eq!(link.stages, vec!["picker original-old: pass"]);
        assert!(!link.missing);
        assert!(link.superseded);
    }

    #[test]
    fn absent_named_check_and_ambiguous_run_prefix_do_not_link() {
        let runs = vec![run("run-123", false), run("run-1234", false)];

        for reference in [
            "run:run-1234/absent",
            "run:run-12/picker",
            "run:run-123/picker",
        ] {
            let link = link_run(reference, &runs);
            assert!(link.missing, "{reference} must not link ambiguously");
            assert_eq!(link.run_id, None);
            assert!(link.stages.is_empty());
        }

        let mut duplicate_check = run("solo", false);
        duplicate_check.checks.push(check(
            "picker",
            true,
            vec![stage("picker", "patched-new", "pass")],
        ));
        let link = link_run("run:solo/picker", &[duplicate_check]);
        assert!(link.missing);
        assert_eq!(link.run_id, None);
    }
}
