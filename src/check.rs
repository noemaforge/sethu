//! Accounting and readiness for one migration attempt.
//!
//! The checker recomputes the required change set from the attempt capture
//! and revalidates every ledger disposition from disk. Hand edits to stored
//! files get no trust. Unknown outcome words, weak evidence, and verified
//! claims without a passing run all fail here. Accounting reports whether
//! every required change carries exactly one valid disposition. Readiness
//! reports whether any valid disposition still waits on a decision or on
//! open work. The two results stay separate and map to distinct exit codes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use indexmap::IndexMap;
use serde::Serialize;

use crate::provenance::OriginsDocument;
use crate::state::attempt::AttemptRecord;
use crate::state::capture::CaptureRecord;
use crate::state::ledger;
use crate::vimanam::{DiffDocument, Severity};

/// Envelope version of the machine readable check report.
///
/// Readers accept the fields they know and ignore the rest. Adding a field
/// keeps this version. Renaming or removing one bumps it.
pub const OUTPUT_VERSION: u32 = 1;

/// One accounting failure or state mismatch found by the checker.
///
/// Each problem names the change it concerns, or a scope word such as
/// `ledger` or `capture` when no single change applies. Codes stay stable
/// so scripts and later reports can match on them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// Change id the problem concerns, or a scope word for global faults.
    pub id: String,
    /// Stable machine readable code for the failure class.
    pub code: String,
    /// Human readable explanation naming the values involved.
    pub message: String,
}

impl Problem {
    /// Code for a required change with no ledger entry.
    pub const MISSING: &'static str = "missing";
    /// Code for a ledger entry the capture does not contain.
    pub const UNKNOWN: &'static str = "unknown";
    /// Code for an id named more than once where one entry must count.
    pub const DUPLICATE: &'static str = "duplicate";
    /// Code for a ledger entry with an unknown outcome or broken shape.
    pub const MALFORMED_OUTCOME: &'static str = "malformed_outcome";
    /// Code for a ledger file that is not usable JSON or has a bad shape.
    pub const MALFORMED_LEDGER: &'static str = "malformed_ledger";
    /// Code for a disposition whose evidence does not meet its minimum.
    pub const MISSING_EVIDENCE: &'static str = "missing_evidence";
    /// Code for capture input hashes that differ from the manifest binding.
    pub const IDENTITY_MISMATCH: &'static str = "identity_mismatch";
    /// Code for a verified claim with no passing run behind it.
    pub const STALE_VERIFICATION: &'static str = "stale_verification";

    /// Build a problem from its three parts.
    pub fn new(id: &str, code: &str, message: String) -> Self {
        Self {
            id: id.to_string(),
            code: code.to_string(),
            message,
        }
    }
}

/// One required change and the verdict on its ledger entry.
///
/// Entries marked invalid explain themselves through the problem list.
/// Readiness follows only from valid entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryView {
    /// Outcome word stored in the ledger for this change.
    pub outcome: String,
    /// Evidence references stored beside the outcome in file order.
    pub evidence: Vec<String>,
    /// Note stored beside the outcome, when the writer left one.
    pub note: Option<String>,
    /// Whether the entry is structurally valid and backed by evidence.
    pub valid: bool,
    /// Whether the entry resolves the change within the recorded scope.
    pub ready: bool,
}

/// Full result of checking one migration attempt.
///
/// Accounting holds when the problem list is empty. Readiness holds when
/// accounting holds and no valid disposition still waits on a decision or
/// on open work. Dispositions follow required order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evaluation {
    /// Envelope version of this report. Writers always stamp the shared value.
    pub schema_version: u32,
    /// Full id of the checked attempt.
    pub attempt_id: String,
    /// Required change ids in capture report order.
    ///
    /// Only breaking and review changes count. Non-breaking changes stay
    /// outside accounting, so voluntary dispositions for them never appear
    /// here.
    pub required: Vec<String>,
    /// Declared scope paths from the manifest in stored order.
    pub scope: Vec<String>,
    /// Whether every required change carries exactly one valid disposition.
    pub accounted: bool,
    /// Whether every valid disposition resolves its change.
    pub ready: bool,
    /// Required ids whose valid dispositions still block readiness.
    pub not_ready: Vec<String>,
    /// Accounting failures and state mismatches in deterministic order.
    pub problems: Vec<Problem>,
    /// Per change verdicts in required order. Missing changes stay absent.
    pub dispositions: IndexMap<String, EntryView>,
}

/// Loaded state the checker evaluates.
///
/// Callers read every file through the state helpers first. Missing or
/// unreadable capture state is a tool failure for the caller. A missing
/// ledger reads as empty here, so every required change reports missing.
pub struct Inputs<'a> {
    /// Attempt manifest bound to one capture and one consumer state.
    pub manifest: &'a AttemptRecord,
    /// Capture record bound to the same capture id.
    pub capture: &'a CaptureRecord,
    /// Stored origins for the capture, when the caller found the file.
    pub origins: Option<&'a OriginsDocument>,
    /// Parsed change list from the attempt capture.
    pub document: &'a DiffDocument,
    /// Raw ledger bytes, or none when no ledger file exists yet.
    pub ledger_bytes: Option<&'a [u8]>,
    /// Canonical consumer repository path used to resolve evidence.
    pub repo: &'a Path,
}

/// Evaluate one attempt from its loaded state.
///
/// The function never writes. It recomputes the required set from the
/// capture, keeping only breaking and review changes, compares every
/// stored identity hash, parses the ledger without leniency for unknown
/// outcomes, and rechecks every evidence reference against the repository
/// on disk. Ledger entries for known non-breaking changes are voluntary
/// coverage and are accepted without any verdict.
pub fn evaluate(inputs: &Inputs<'_>) -> Evaluation {
    let required: Vec<String> = inputs
        .document
        .changes
        .iter()
        .filter(|item| matches!(item.severity, Severity::Breaking | Severity::Review))
        .map(|item| item.id.clone())
        .collect();
    let required_set: HashSet<&str> = required.iter().map(String::as_str).collect();
    let known_set: HashSet<&str> = inputs
        .document
        .changes
        .iter()
        .map(|item| item.id.as_str())
        .collect();

    let mut problems = Vec::new();

    push_capture_duplicates(inputs.document, &mut problems);
    push_identity_problems(inputs, &mut problems);
    push_origin_problems(inputs, &required, &mut problems);

    let mut dispositions: IndexMap<String, EntryView> = IndexMap::new();
    match inputs.ledger_bytes {
        None => {
            for id in &required {
                problems.push(Problem::new(
                    id,
                    Problem::MISSING,
                    format!("ledger names no disposition for required change {id}"),
                ));
            }
        }
        Some(bytes) => parse_ledger(
            bytes,
            inputs.repo,
            inputs.manifest,
            &required,
            &required_set,
            &known_set,
            &mut problems,
            &mut dispositions,
        ),
    }

    let mut not_ready = Vec::new();
    for id in &required {
        if let Some(view) = dispositions.get(id)
            && view.valid
            && !view.ready
        {
            not_ready.push(id.clone());
        }
    }

    let accounted = problems.is_empty();
    let ready = accounted && not_ready.is_empty();
    Evaluation {
        schema_version: OUTPUT_VERSION,
        attempt_id: inputs.manifest.attempt_id.clone(),
        required,
        scope: inputs.manifest.scope.clone(),
        accounted,
        ready,
        not_ready,
        problems,
        dispositions,
    }
}

/// Map an evaluation to its frozen process exit code.
///
/// Accounting failures exit 4 with or without the readiness flag. Accounted
/// work that is not ready exits 5, but only under the readiness flag.
/// Everything else exits 0. Tool failures never reach this function.
pub fn exit_code(accounted: bool, ready: bool, require_ready: bool) -> std::process::ExitCode {
    if !accounted {
        return std::process::ExitCode::from(4);
    }
    if require_ready && !ready {
        return std::process::ExitCode::from(5);
    }
    std::process::ExitCode::SUCCESS
}

/// Reference to one stored verification run and one check inside it.
///
/// The shape is `run:<run-id>/<check>`. The run id names a stored run
/// directory under the migration. The check names one check from that
/// run record. Both parts are required. A run without a check cannot
/// show which behaviour was exercised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReference {
    /// Stored run directory name.
    pub run_id: String,
    /// Check name from the run record.
    pub check: String,
}

/// Parse a `run:<run-id>/<check>` evidence reference.
///
/// The `run:` prefix is required. The remainder splits on the first
/// slash into a non-empty run id and a non-empty check name. Anything
/// else fails with the reference named and the expected shape shown.
pub fn parse_run_reference(reference: &str) -> anyhow::Result<RunReference> {
    let Some(rest) = reference.strip_prefix("run:") else {
        anyhow::bail!(
            "reference `{reference}` needs the shape `run:<run-id>/<check>` that names the stored run and the check"
        );
    };
    match rest.split_once('/') {
        Some((run_id, check)) if !run_id.is_empty() && !check.is_empty() => Ok(RunReference {
            run_id: run_id.to_string(),
            check: check.to_string(),
        }),
        _ => anyhow::bail!(
            "reference `{reference}` needs the shape `run:<run-id>/<check>` that names the stored run and the check"
        ),
    }
}

/// One expected failure signature as snapshotted in a run artefact.
///
/// A plain string must appear verbatim in the failure output. A regex
/// object must match somewhere in the same output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
enum StoredDiagnostic {
    /// Verbatim substring of the failure output.
    Substring(String),
    /// Regular expression matched against the failure output.
    Pattern {
        /// Regular expression source.
        regex: String,
    },
}

/// One stored test case inside a stage record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, Default)]
struct StoredCase {
    /// Test case name as reported.
    #[serde(default)]
    name: String,
    /// Whether the case passed.
    #[serde(default)]
    passed: bool,
    /// Failure message plus captured output.
    #[serde(default)]
    output: String,
}

/// One expected stub exchange as snapshotted in a run artefact.
///
/// The snapshot freezes what the verdict rested on, so a later manifest
/// edit cannot reattribute the run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct StoredExchange {
    /// Scenario id the trace must show.
    #[serde(default)]
    scenario: String,
    /// Request method the trace must show.
    #[serde(default)]
    method: String,
    /// Request path template the trace must show.
    #[serde(default)]
    path: String,
}

/// One check summary as stored in a run record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, Default)]
struct StoredRunCheck {
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
}

/// One run record as stored beside a verification manifest.
///
/// Only the fields the claim check needs are kept. Unknown fields stay
/// unread.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, Default)]
struct StoredRunRecord {
    /// Unique run identifier and directory name.
    #[serde(default)]
    run_id: String,
    /// Per-check summaries in manifest order.
    #[serde(default)]
    checks: Vec<StoredRunCheck>,
}

/// One stage record as stored beside its test streams and stub trace.
///
/// Only the fields the claim check needs are kept. Unknown fields stay
/// unread.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, Default)]
struct StoredStageRecord {
    /// Check name from the manifest.
    #[serde(default)]
    check: String,
    /// Stage name, such as `original-new`.
    #[serde(default)]
    stage: String,
    /// Contract change records the check covered when the run started.
    #[serde(default)]
    change_ids: Vec<String>,
    /// Failure signature the red stage required, when one applied.
    #[serde(default)]
    expected_diagnostic: Option<StoredDiagnostic>,
    /// Stub exchanges the stage had to show.
    #[serde(default)]
    expected_exchange: Vec<StoredExchange>,
    /// Application commit checked out for this stage.
    #[serde(default)]
    commit: String,
    /// Contract version served by this stage stub.
    #[serde(default)]
    spec_version: String,
    /// Pinned spec hash for this stage.
    #[serde(default)]
    spec_sha256: String,
    /// Scenario directory feeding this stage stub.
    #[serde(default)]
    scenarios_dir: String,
    /// Verdict word for this stage.
    #[serde(default)]
    verdict: String,
    /// Process exit code, when the process exited.
    #[serde(default)]
    exit_code: Option<i32>,
    /// Signal that killed the process, when one did.
    #[serde(default)]
    signal: Option<i32>,
    /// Whether the run hit the deadline and was killed.
    #[serde(default)]
    timed_out: bool,
    /// Whether output shows the harness failed to build.
    #[serde(default)]
    build_failed: bool,
    /// Parsed per-test cases with their outputs.
    #[serde(default)]
    cases: Vec<StoredCase>,
}

/// Deepest directory level searched for stored run artefacts.
///
/// Run records sit at most a few levels down (`runs/<id>/run.json`, or
/// deeper when the manifest lives in a nested harness directory). The
/// bound keeps the walk cheap on large consumer checkouts.
const MAX_RUN_WALK_DEPTH: usize = 8;

/// Validate a `fixed_and_verified` evidence list against stored runs.
///
/// Every `run:<run-id>/<check>` reference must resolve to a stored run
/// under the migration, name a check from that run record, and rest on
/// passing stages whose trace shows the expected exchange. At least one
/// reference must name a regression check that covers the change id. A
/// guard alone cannot show a repair. A check whose scenarios cannot
/// support a claim blocks the claim instead of passing it. The
/// `patched-new` stage must have run on the consumer commit under
/// review. Anything else fails with the cause named.
pub fn validate_fixed_claim(
    repo: &Path,
    manifest: &AttemptRecord,
    change_id: &str,
    evidence: &[String],
) -> anyhow::Result<()> {
    let mut checked = Vec::with_capacity(evidence.len());
    for reference in evidence {
        match ledger::check_reference(repo, reference) {
            Ok(item) => checked.push(item),
            Err(err) => {
                anyhow::bail!("change {change_id} cites unusable evidence: {err:#}");
            }
        }
    }
    let summary = ledger::summarize(&checked);
    ledger::check_evidence(ledger::Outcome::FixedAndVerified, &summary, None)?;
    let mut references = Vec::new();
    for item in &checked {
        if item.kind != ledger::EvidenceKind::Run {
            continue;
        }
        match parse_run_reference(&item.reference) {
            Ok(parsed) => references.push(parsed),
            Err(err) => {
                anyhow::bail!("change {change_id} cites {err:#}");
            }
        }
    }
    if references.is_empty() {
        anyhow::bail!(
            "change {change_id} needs a run reference shaped like `run:<run-id>/<check>` that names the stored run and the check"
        );
    }
    let head = crate::state::attempt::read_baseline_commit(repo).map_err(|err| {
        anyhow::anyhow!(
            "cannot read the consumer commit in {}: {err:#}, so the run cannot be bound to the current state",
            repo.display()
        )
    })?;
    let root = crate::state::layout::state_root(repo);
    let migration = crate::state::layout::migrations_dir(&root).join(&manifest.attempt_id);
    let mut saw_regression = false;
    let mut covered = false;
    for reference in &references {
        let inspected = inspect_run(&root, &migration, manifest, change_id, &head, reference)?;
        if inspected.regression {
            saw_regression = true;
            if inspected.covers {
                covered = true;
            }
        }
    }
    if !saw_regression {
        anyhow::bail!(
            "change {change_id} names only guard checks, and a guard protects unchanged behaviour, so no named check shows the repair"
        );
    }
    if !covered {
        anyhow::bail!(
            "no named check covers change {change_id}, so the named runs cannot verify it"
        );
    }
    Ok(())
}

/// What one referenced check proved about the change.
struct InspectedCheck {
    /// Whether the named check regresses behaviour rather than guarding it.
    regression: bool,
    /// Whether the named check covers the change under review.
    covers: bool,
}

/// Validate one run reference through its stored stages.
///
/// The run record must parse, the check must exist with a known role,
/// and the check must have verified. Every stage must then carry the
/// verdict its role requires, with stored cases and exit codes that
/// back the verdict and the red diagnostic where one applies. The
/// patched commit must match the consumer state under review, and
/// every stage trace must show the expected exchange. Failures name
/// the run, the check, and the stage.
fn inspect_run(
    root: &Path,
    migration: &Path,
    manifest: &AttemptRecord,
    change_id: &str,
    head: &str,
    reference: &RunReference,
) -> anyhow::Result<InspectedCheck> {
    let Some(run_dir) = find_run_dir(migration, &reference.run_id) else {
        anyhow::bail!(
            "change {change_id} names no stored run `{}` under the migration ({})",
            reference.run_id,
            known_run_ids(migration)
        );
    };
    let record = read_run_record(&run_dir).map_err(|err| {
        anyhow::anyhow!(
            "stored run `{}` keeps a run record that is not usable: {err:#}",
            reference.run_id
        )
    })?;
    let Some(check) = record
        .checks
        .iter()
        .find(|item| item.name == reference.check)
    else {
        let known = record
            .checks
            .iter()
            .map(|item| format!("`{}`", item.name))
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "change {change_id} names no check `{}` in stored run `{}` (known: {known})",
            reference.check,
            reference.run_id
        );
    };
    let regression = match check.role.as_str() {
        "regression" => true,
        "guard" => false,
        _ => {
            anyhow::bail!(
                "check `{}` in stored run `{}` has role `{}`, need `regression` or `guard`",
                reference.check,
                reference.run_id,
                check.role
            );
        }
    };
    if !check.verified {
        anyhow::bail!(
            "check `{}` in stored run `{}` did not verify, so it cannot back the claim",
            reference.check,
            reference.run_id
        );
    }
    let stages: [(&str, &str); 3] = if regression {
        [
            ("original-old", "pass"),
            ("original-new", "expected-red"),
            ("patched-new", "pass"),
        ]
    } else {
        [
            ("original-old", "pass"),
            ("original-new", "pass"),
            ("patched-new", "pass"),
        ]
    };
    let mut supported = false;
    for (stage_name, wanted) in stages {
        let stage = read_stage_record(&run_dir, &reference.check, stage_name).map_err(|err| {
            anyhow::anyhow!(
                "check `{}` in stored run `{}` keeps no usable `{stage_name}` stage: {err:#}",
                reference.check,
                reference.run_id
            )
        })?;
        if stage.verdict != wanted {
            anyhow::bail!(
                "stage `{stage_name}` of check `{}` in stored run `{}` reads `{}` but the claim needs `{wanted}`",
                reference.check,
                reference.run_id,
                stage.verdict
            );
        }
        check_stage_cases(
            &run_dir,
            reference,
            &stage,
            regression && stage_name == "original-new",
        )?;
        if stage_name == "patched-new" && stage.commit != head {
            anyhow::bail!(
                "stage `patched-new` of check `{}` in stored run `{}` ran on commit {} but the consumer is at {head}, so the run no longer matches the current state",
                reference.check,
                reference.run_id,
                stage.commit
            );
        }
        if stage.expected_exchange.is_empty() {
            anyhow::bail!(
                "stage `{stage_name}` of check `{}` in stored run `{}` declares no expected exchange, so the trace cannot show the repair",
                reference.check,
                reference.run_id
            );
        }
        if check_stage_exchange(root, manifest, &run_dir, reference, &stage)? {
            supported = true;
        }
    }
    if !supported {
        anyhow::bail!(
            "check `{}` in stored run `{}` exercises only scenarios that cannot support a claim, so the claim is blocked, not passed",
            reference.check,
            reference.run_id
        );
    }
    Ok(InspectedCheck {
        regression,
        covers: regression && check.change_ids.iter().any(|id| id == change_id),
    })
}

/// Check one stored stage test result against its required outcome.
///
/// A killed, signalled, or unbuilt harness fails the stage. An empty
/// case list fails it too: no stored case means no test ran. A green
/// stage needs every stored case passed. A red stage needs a failing
/// case plus the snapshotted diagnostic in the stored output. One
/// invocation runs one check, so every stored case belongs to it.
fn check_stage_cases(
    run_dir: &Path,
    reference: &RunReference,
    stage: &StoredStageRecord,
    want_red: bool,
) -> anyhow::Result<()> {
    if stage.timed_out {
        anyhow::bail!(
            "test process for stage `{}` of check `{}` in stored run `{}` hit the deadline and was killed",
            stage.stage,
            reference.check,
            reference.run_id
        );
    }
    if let Some(signal) = stage.signal {
        anyhow::bail!(
            "test process for stage `{}` of check `{}` in stored run `{}` died on signal {signal}",
            stage.stage,
            reference.check,
            reference.run_id
        );
    }
    if stage.build_failed {
        anyhow::bail!(
            "harness for stage `{}` of check `{}` in stored run `{}` did not build, so the stage shows no behaviour",
            stage.stage,
            reference.check,
            reference.run_id
        );
    }
    if stage.cases.is_empty() {
        anyhow::bail!(
            "stage `{}` of check `{}` in stored run `{}` records no test cases, so no test ran",
            stage.stage,
            reference.check,
            reference.run_id
        );
    }
    if want_red {
        if stage.exit_code == Some(0) {
            anyhow::bail!(
                "stage `{}` of check `{}` in stored run `{}` exited 0, but the red stage needs a failure",
                stage.stage,
                reference.check,
                reference.run_id
            );
        }
        if stage.cases.iter().all(|case| case.passed) {
            anyhow::bail!(
                "stage `{}` of check `{}` in stored run `{}` passed, but the red stage needs a failure",
                stage.stage,
                reference.check,
                reference.run_id
            );
        }
        let Some(wanted) = &stage.expected_diagnostic else {
            anyhow::bail!(
                "stage `{}` of check `{}` in stored run `{}` declares no expected diagnostic, so the failure proves nothing",
                stage.stage,
                reference.check,
                reference.run_id
            );
        };
        let combined = stage_case_output(run_dir, reference, stage);
        if !diagnostic_matches(wanted, &combined) {
            anyhow::bail!(
                "stage `{}` of check `{}` in stored run `{}` failed without the expected diagnostic, so the failure is not the intended break",
                stage.stage,
                reference.check,
                reference.run_id
            );
        }
        return Ok(());
    }
    if let Some(failed) = stage.cases.iter().find(|case| !case.passed) {
        anyhow::bail!(
            "test `{}` failed in stage `{}` of check `{}` in stored run `{}` where a pass was required",
            failed.name,
            stage.stage,
            reference.check,
            reference.run_id
        );
    }
    if stage.exit_code != Some(0) {
        anyhow::bail!(
            "stage `{}` of check `{}` in stored run `{}` exited without a zero status where a pass was recorded",
            stage.stage,
            reference.check,
            reference.run_id
        );
    }
    Ok(())
}

/// Join stored case outputs with the captured test streams.
///
/// The diagnostic may live in a case message or in the process output,
/// so every source joins here before matching. Missing stream files
/// read as empty.
fn stage_case_output(
    run_dir: &Path,
    reference: &RunReference,
    stage: &StoredStageRecord,
) -> String {
    let mut parts = Vec::new();
    for case in &stage.cases {
        if !case.output.trim().is_empty() {
            parts.push(case.output.clone());
        }
    }
    let stage_dir = run_dir.join(&reference.check).join(&stage.stage);
    for name in ["stdout.log", "stderr.log"] {
        if let Ok(text) = std::fs::read_to_string(stage_dir.join(name)) {
            parts.push(text);
        }
    }
    parts.join("\n")
}

/// Report whether stored output carries one expected diagnostic.
fn diagnostic_matches(diagnostic: &StoredDiagnostic, output: &str) -> bool {
    match diagnostic {
        StoredDiagnostic::Substring(text) => output.contains(text),
        StoredDiagnostic::Pattern { regex } => match regex::Regex::new(regex) {
            Ok(pattern) => pattern.is_match(output),
            Err(_) => false,
        },
    }
}

/// Check one stored stage trace against its snapshotted exchanges.
///
/// The spec inputs are rehashed against the stage record, the scenario
/// directory is reloaded, and every expected exchange must appear in
/// the trace as received, validated, and answered with the declared
/// response. Unexpected, invalid, or unanswered entries fail the stage.
/// The result reports whether any expected scenario can support a
/// claim.
fn check_stage_exchange(
    root: &Path,
    manifest: &AttemptRecord,
    run_dir: &Path,
    reference: &RunReference,
    stage: &StoredStageRecord,
) -> anyhow::Result<bool> {
    if !stage.check.is_empty() && stage.check != reference.check {
        anyhow::bail!(
            "stage `{}` of check `{}` in stored run `{}` names check `{}`, so the record does not belong to the claim",
            stage.stage,
            reference.check,
            reference.run_id,
            stage.check
        );
    }
    let pair = crate::state::layout::pair_dir(root, &manifest.old_spec_hash, &manifest.new_spec_hash)
        .map_err(|err| {
            anyhow::anyhow!(
                "cannot locate the spec pair for the claim: {err:#}, so the exchange cannot be rechecked"
            )
        })?;
    let spec_path = match stage.spec_version.as_str() {
        "old" => crate::state::pair::inputs_old_path(&pair),
        "new" => crate::state::pair::inputs_new_path(&pair),
        _ => {
            anyhow::bail!(
                "stage `{}` of check `{}` in stored run `{}` declares spec version `{}`, need `old` or `new`",
                stage.stage,
                reference.check,
                reference.run_id,
                stage.spec_version
            );
        }
    };
    let bytes = std::fs::read(&spec_path).map_err(|err| {
        anyhow::anyhow!(
            "cannot read spec inputs at {}: {err:#}, so the exchange cannot be rechecked",
            spec_path.display()
        )
    })?;
    let actual = crate::state::layout::sha256_hex(&bytes);
    if actual != stage.spec_sha256 {
        anyhow::bail!(
            "spec inputs at {} hash {actual} but the stage ran against {}, so the exchange cannot be rechecked",
            spec_path.display(),
            stage.spec_sha256
        );
    }
    let spec: serde_json::Value = serde_json::from_slice(&bytes).map_err(|err| {
        anyhow::anyhow!(
            "cannot parse spec inputs at {}: {err:#}, so the exchange cannot be rechecked",
            spec_path.display()
        )
    })?;
    let scenarios_dir = Path::new(&stage.scenarios_dir);
    let scenarios =
        crate::stub::load_scenarios(scenarios_dir, &spec, &stage.spec_sha256).map_err(|err| {
            anyhow::anyhow!(
                "scenario directory {} cannot be reloaded: {err:#}, so the exchange cannot be rechecked",
                scenarios_dir.display()
            )
        })?;
    let mut by_id = std::collections::HashMap::new();
    for scenario in &scenarios {
        by_id.insert(scenario.id.as_str(), scenario);
    }
    let stage_dir = run_dir.join(&reference.check).join(&stage.stage);
    let trace = crate::stub::read_trace(&stage_dir.join("requests.jsonl")).map_err(|err| {
        anyhow::anyhow!(
            "trace for stage `{}` of check `{}` in stored run `{}` cannot be read: {err:#}, so the exchange is broken",
            stage.stage,
            reference.check,
            reference.run_id
        )
    })?;
    for exchange in &stage.expected_exchange {
        let Some(scenario) = by_id.get(exchange.scenario.as_str()) else {
            anyhow::bail!(
                "stage `{}` of check `{}` in stored run `{}` expects unknown scenario `{}`",
                stage.stage,
                reference.check,
                reference.run_id,
                exchange.scenario
            );
        };
        let wanted = served_body_hash(&scenario.response.body);
        let hit = trace.iter().any(|entry| {
            entry.scenario_id.as_deref() == Some(exchange.scenario.as_str())
                && entry.method.eq_ignore_ascii_case(&exchange.method)
                && crate::stub::template_matches(&exchange.path, &entry.path)
                && entry.request_valid
                && entry.response_written
                && entry.response_status == scenario.response.status
                && entry.response_body_sha256 == wanted
        });
        if !hit {
            anyhow::bail!(
                "expected exchange `{}` {} {} was not received, validated, and answered in the trace for stage `{}` of check `{}` in stored run `{}`",
                exchange.scenario,
                exchange.method,
                exchange.path,
                stage.stage,
                reference.check,
                reference.run_id
            );
        }
    }
    for entry in &trace {
        match entry.scenario_id.as_deref() {
            None => {
                anyhow::bail!(
                    "unexpected request {} {} reached no scenario in the trace for stage `{}` of check `{}` in stored run `{}`",
                    entry.method,
                    entry.path,
                    stage.stage,
                    reference.check,
                    reference.run_id
                );
            }
            Some(seen) => {
                if !stage
                    .expected_exchange
                    .iter()
                    .any(|wanted| wanted.scenario == seen)
                {
                    anyhow::bail!(
                        "request {} {} hit scenario `{seen}`, which stage `{}` of check `{}` in stored run `{}` does not expect",
                        entry.method,
                        entry.path,
                        stage.stage,
                        reference.check,
                        reference.run_id
                    );
                }
                if !entry.request_valid {
                    anyhow::bail!(
                        "request {} {} for scenario `{seen}` failed validation in the trace for stage `{}` of check `{}` in stored run `{}`",
                        entry.method,
                        entry.path,
                        stage.stage,
                        reference.check,
                        reference.run_id
                    );
                }
                if !entry.response_written {
                    anyhow::bail!(
                        "response for scenario `{seen}` was not fully written in the trace for stage `{}` of check `{}` in stored run `{}`",
                        stage.stage,
                        reference.check,
                        reference.run_id
                    );
                }
            }
        }
    }
    Ok(stage.expected_exchange.iter().any(|exchange| {
        by_id
            .get(exchange.scenario.as_str())
            .is_some_and(|scenario| scenario.supports_claim)
    }))
}

/// Hash a served scenario response body the way the stub hashes it.
///
/// A missing body sends zero bytes. Any other value travels in its
/// canonical JSON form.
fn served_body_hash(body: &Option<serde_json::Value>) -> String {
    match body {
        Some(value) => crate::stub::body_sha256_hex(&serde_json::to_vec(value).unwrap_or_default()),
        None => crate::stub::body_sha256_hex(&[]),
    }
}

/// Find one stored run directory by id under a migration.
///
/// The walk covers the whole migration tree up to a fixed depth, so
/// runs stored beside a nested manifest are found as well as runs in
/// the top-level runs directory. The first sorted hit wins.
fn find_run_dir(migration: &Path, run_id: &str) -> Option<PathBuf> {
    let mut hits = Vec::new();
    collect_run_dirs(migration, 0, run_id, &mut hits);
    hits.sort();
    hits.into_iter().next()
}

/// Collect stored run directories with one id under a directory.
///
/// Pending temp files stay excluded, so interrupted writers never
/// appear in lookups.
fn collect_run_dirs(dir: &Path, depth: usize, run_id: &str, out: &mut Vec<PathBuf>) {
    if depth > MAX_RUN_WALK_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if crate::state::atomic::is_pending_temp(&path) {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some(run_id)
            && path.join("run.json").is_file()
        {
            out.push(path);
        } else {
            collect_run_dirs(&path, depth + 1, run_id, out);
        }
    }
}

/// List stored run ids under a migration for refusal messages.
///
/// The walk mirrors the lookup. Results sort, and the message caps the
/// listing so one crowded tree cannot flood the output.
fn known_run_ids(migration: &Path) -> String {
    let mut ids = Vec::new();
    collect_known_run_ids(migration, 0, &mut ids);
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return "no stored runs were found".to_string();
    }
    const SHOWN: usize = 8;
    let mut text = ids
        .iter()
        .take(SHOWN)
        .map(|id| format!("`{id}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > SHOWN {
        text.push_str(&format!(" ({} more)", ids.len() - SHOWN));
    }
    format!("known: {text}")
}

/// Collect parent names of every run record under a directory.
fn collect_known_run_ids(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_RUN_WALK_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if crate::state::atomic::is_pending_temp(&path) {
            continue;
        }
        if path.is_file() {
            if path.file_name().and_then(|name| name.to_str()) == Some("run.json")
                && let Some(name) = path
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .and_then(|name| name.to_str())
            {
                out.push(name.to_string());
            }
            continue;
        }
        if path.is_dir() {
            collect_known_run_ids(&path, depth + 1, out);
        }
    }
}

/// Read one stored run record.
///
/// A missing or misshapen file fails with the path named, so callers
/// can tell a lost artefact apart from a failed check.
fn read_run_record(run_dir: &Path) -> anyhow::Result<StoredRunRecord> {
    let path = run_dir.join("run.json");
    let bytes =
        std::fs::read(&path).with_context(|| format!("read run record {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse run record {}", path.display()))
}

/// Read one stored stage record.
///
/// A missing or misshapen file fails with the path named, so callers
/// can tell a lost artefact apart from a failed check.
fn read_stage_record(
    run_dir: &Path,
    check: &str,
    stage: &str,
) -> anyhow::Result<StoredStageRecord> {
    let path = run_dir.join(check).join(stage).join("stage.json");
    let bytes =
        std::fs::read(&path).with_context(|| format!("read stage record {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse stage record {}", path.display()))
}

/// Report capture change ids named more than once.
///
/// The capture list is immutable init output. A repeated id means the tree
/// was edited by hand or the generator emitted a corrupt list. Either way
/// accounting cannot trust the required set.
fn push_capture_duplicates(document: &DiffDocument, problems: &mut Vec<Problem>) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for item in &document.changes {
        *counts.entry(item.id.as_str()).or_insert(0) += 1;
    }
    for item in &document.changes {
        if counts[item.id.as_str()] > 1
            && !problems
                .iter()
                .any(|p| p.id == item.id && p.code == Problem::DUPLICATE)
        {
            problems.push(Problem::new(
                &item.id,
                Problem::DUPLICATE,
                format!(
                    "capture change list names change {} {} times, only one entry can count",
                    item.id,
                    counts[item.id.as_str()]
                ),
            ));
        }
    }
}

/// Compare stored input identity hashes against the manifest binding.
///
/// The manifest carries the full spec hashes and the full capture id. The
/// capture record and the parsed change list must agree on all of them.
/// Any disagreement means the tree no longer describes one upgrade.
fn push_identity_problems(inputs: &Inputs<'_>, problems: &mut Vec<Problem>) {
    if inputs.manifest.old_spec_hash != inputs.document.old.file_sha256 {
        problems.push(Problem::new(
            "capture",
            Problem::IDENTITY_MISMATCH,
            "input identity mismatch: manifest old spec hash does not match capture old file hash"
                .to_string(),
        ));
    }
    if inputs.manifest.new_spec_hash != inputs.document.new.file_sha256 {
        problems.push(Problem::new(
            "capture",
            Problem::IDENTITY_MISMATCH,
            "input identity mismatch: manifest new spec hash does not match capture new file hash"
                .to_string(),
        ));
    }
    if inputs.manifest.capture_id != inputs.capture.capture_id {
        problems.push(Problem::new(
            "capture",
            Problem::IDENTITY_MISMATCH,
            "input identity mismatch: manifest capture id does not match stored capture record"
                .to_string(),
        ));
    }
    if inputs.capture.old_spec_hash != inputs.manifest.old_spec_hash
        || inputs.capture.new_spec_hash != inputs.manifest.new_spec_hash
    {
        problems.push(Problem::new(
            "capture",
            Problem::IDENTITY_MISMATCH,
            "input identity mismatch: stored capture record names different spec hashes than the manifest"
                .to_string(),
        ));
    }
}

/// Report required changes missing from the stored origins map.
///
/// Origins cover every change in a fresh capture. A gap means the capture
/// tree was edited by hand after init wrote it.
fn push_origin_problems(inputs: &Inputs<'_>, required: &[String], problems: &mut Vec<Problem>) {
    let Some(origins) = inputs.origins else {
        return;
    };
    for id in required {
        if !origins.origins.contains_key(id) {
            problems.push(Problem::new(
                id,
                Problem::IDENTITY_MISMATCH,
                format!("capture origins list names no origin for change {id}"),
            ));
        }
    }
}

/// One ledger entry after lenient structural parsing.
///
/// The outcome word stays raw here. Callers validate it against the five
/// known words and report anything else as malformed.
struct RawEntry {
    /// Outcome word exactly as stored in the file.
    outcome_word: String,
    /// Evidence references in stored order.
    evidence: Vec<String>,
    /// Stored note, when the writer left one.
    note: Option<String>,
}

/// Parse the ledger bytes and validate every disposition.
///
/// Structural file faults become ledger scope problems. Ids the capture
/// never names become unknown problems in file order. Ledger entries for
/// known non-breaking changes are voluntary coverage and are skipped
/// without a problem or a verdict. Required ids without any entry become
/// missing problems in required order. Valid entries get their evidence
/// rechecked against the repository.
#[allow(clippy::too_many_arguments)]
fn parse_ledger(
    bytes: &[u8],
    repo: &Path,
    manifest: &AttemptRecord,
    required: &[String],
    required_set: &HashSet<&str>,
    known_set: &HashSet<&str>,
    problems: &mut Vec<Problem>,
    dispositions: &mut IndexMap<String, EntryView>,
) {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(err) => {
            problems.push(Problem::new(
                "ledger",
                Problem::MALFORMED_LEDGER,
                format!("ledger file is not usable JSON: {err}"),
            ));
            return;
        }
    };
    let top = match value.as_object() {
        Some(top) => top,
        None => {
            problems.push(Problem::new(
                "ledger",
                Problem::MALFORMED_LEDGER,
                "ledger file must hold a JSON object at the top level".to_string(),
            ));
            return;
        }
    };
    match top
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(found) if found == u64::from(crate::state::SCHEMA_VERSION) => {}
        _ => {
            problems.push(Problem::new(
                "ledger",
                Problem::MALFORMED_LEDGER,
                format!(
                    "ledger schema version is not supported, need {}",
                    crate::state::SCHEMA_VERSION
                ),
            ));
        }
    }
    let entries = match top
        .get("dispositions")
        .and_then(serde_json::Value::as_object)
    {
        Some(entries) => entries,
        None => {
            problems.push(Problem::new(
                "ledger",
                Problem::MALFORMED_LEDGER,
                "ledger dispositions must be an object keyed by change id".to_string(),
            ));
            return;
        }
    };

    if let Some(keys) = scan_disposition_keys(bytes) {
        let mut seen = HashSet::new();
        for key in &keys {
            if known_set.contains(key.as_str()) && !required_set.contains(key.as_str()) {
                continue;
            }
            if !seen.insert(key.clone())
                && !problems
                    .iter()
                    .any(|p| p.id == *key && p.code == Problem::DUPLICATE)
            {
                let total = keys_count(&keys, key);
                problems.push(Problem::new(
                    key,
                    Problem::DUPLICATE,
                    format!(
                        "ledger names change {key} {total} times, only one disposition can count"
                    ),
                ));
            }
        }
    }

    for (id, raw) in entries {
        if !required_set.contains(id.as_str()) {
            if known_set.contains(id.as_str()) {
                continue;
            }
            problems.push(Problem::new(
                id,
                Problem::UNKNOWN,
                format!("ledger names change {id} that the capture does not contain"),
            ));
            continue;
        }
        match parse_entry(id, raw) {
            Ok(entry) => check_entry(id, entry, repo, manifest, problems, dispositions),
            Err(message) => {
                problems.push(Problem::new(id, Problem::MALFORMED_OUTCOME, message));
                dispositions.insert(
                    id.clone(),
                    EntryView {
                        outcome: raw
                            .get("current")
                            .and_then(|current| current.get("outcome"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        evidence: Vec::new(),
                        note: None,
                        valid: false,
                        ready: false,
                    },
                );
            }
        }
    }

    for id in required {
        if !entries.contains_key(id) {
            problems.push(Problem::new(
                id,
                Problem::MISSING,
                format!("ledger names no disposition for required change {id}"),
            ));
        }
    }
}

/// Count occurrences of one key in a scanned key list.
fn keys_count(keys: &[String], wanted: &str) -> usize {
    keys.iter().filter(|key| key.as_str() == wanted).count()
}

/// Parse one disposition value without trusting its shape.
///
/// Only the current entry counts. History stays unread. Evidence must be an
/// array of strings and the note must be a string when present.
fn parse_entry(id: &str, raw: &serde_json::Value) -> Result<RawEntry, String> {
    let current = raw
        .get("current")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("ledger entry for change {id} must hold a current object"))?;
    let outcome_word = current
        .get("outcome")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("ledger entry for change {id} must name its outcome as a word"))?
        .to_string();
    let evidence = match current.get("evidence") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| format!("ledger entry for change {id} must list evidence as an array"))?
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    format!("ledger entry for change {id} must list evidence as strings")
                })
            })
            .collect::<Result<Vec<String>, String>>()?,
    };
    let note = match current.get("note") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(_) => {
            return Err(format!(
                "ledger entry for change {id} must carry its note as a string"
            ));
        }
    };
    Ok(RawEntry {
        outcome_word,
        evidence,
        note,
    })
}

/// Validate one parsed entry and record its verdict.
///
/// Unknown outcome words fail as malformed. Every evidence reference is
/// resolved against the repository again, so deleted files and invented
/// line numbers fail here. Verified claims are rechecked against the
/// stored runs: the run must exist, its check must cover the change and
/// carry passing stages, the patched commit must match the consumer
/// state under review, and every stage trace must show the expected
/// exchange.
fn check_entry(
    id: &str,
    entry: RawEntry,
    repo: &Path,
    manifest: &AttemptRecord,
    problems: &mut Vec<Problem>,
    dispositions: &mut IndexMap<String, EntryView>,
) {
    let outcome = match ledger::Outcome::parse(&entry.outcome_word) {
        Ok(outcome) => outcome,
        Err(_) => {
            problems.push(Problem::new(
                id,
                Problem::MALFORMED_OUTCOME,
                format!(
                    "ledger entry for change {id} names unknown outcome `{}`",
                    entry.outcome_word
                ),
            ));
            dispositions.insert(
                id.to_string(),
                EntryView {
                    outcome: entry.outcome_word,
                    evidence: entry.evidence,
                    note: entry.note,
                    valid: false,
                    ready: false,
                },
            );
            return;
        }
    };

    let mut checked = Vec::with_capacity(entry.evidence.len());
    for reference in &entry.evidence {
        match ledger::check_reference(repo, reference) {
            Ok(item) => checked.push(item),
            Err(err) => {
                problems.push(Problem::new(
                    id,
                    Problem::MISSING_EVIDENCE,
                    format!("ledger entry for change {id} cites unusable evidence: {err:#}"),
                ));
                dispositions.insert(
                    id.to_string(),
                    EntryView {
                        outcome: entry.outcome_word,
                        evidence: entry.evidence,
                        note: entry.note,
                        valid: false,
                        ready: false,
                    },
                );
                return;
            }
        }
    }
    let summary = ledger::summarize(&checked);
    if let Err(err) = ledger::check_evidence(outcome, &summary, entry.note.as_deref()) {
        let (code, prefix) = match outcome {
            ledger::Outcome::FixedAndVerified => (
                Problem::STALE_VERIFICATION,
                "claims verification without a passing run",
            ),
            _ => (Problem::MISSING_EVIDENCE, "carries too little evidence"),
        };
        problems.push(Problem::new(
            id,
            code,
            format!("ledger entry for change {id} {prefix}: {err:#}"),
        ));
        dispositions.insert(
            id.to_string(),
            EntryView {
                outcome: entry.outcome_word,
                evidence: entry.evidence,
                note: entry.note,
                valid: false,
                ready: false,
            },
        );
        return;
    }
    if outcome == ledger::Outcome::FixedAndVerified
        && let Err(err) = validate_fixed_claim(repo, manifest, id, &entry.evidence)
    {
        problems.push(Problem::new(
            id,
            Problem::STALE_VERIFICATION,
            format!("ledger entry for change {id} claims verification without a usable passing run: {err:#}"),
        ));
        dispositions.insert(
            id.to_string(),
            EntryView {
                outcome: entry.outcome_word,
                evidence: entry.evidence,
                note: entry.note,
                valid: false,
                ready: false,
            },
        );
        return;
    }

    let ready = matches!(
        outcome,
        ledger::Outcome::FixedAndVerified
            | ledger::Outcome::UnaffectedInApplication
            | ledger::Outcome::NoUsageFound
    );
    dispositions.insert(
        id.to_string(),
        EntryView {
            outcome: entry.outcome_word,
            evidence: entry.evidence,
            note: entry.note,
            valid: true,
            ready,
        },
    );
}

/// Scan raw ledger bytes for disposition keys in file order.
///
/// Parsed maps drop repeated keys, so duplicate detection needs this raw
/// pass. The scan returns none when the top level holds no usable
/// dispositions object. Callers report that structural fault separately.
fn scan_disposition_keys(bytes: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut scanner = Scanner {
        text: text.as_bytes(),
        pos: 0,
    };
    scanner.skip_whitespace();
    scanner.expect(b'{')?;
    loop {
        scanner.skip_whitespace();
        match scanner.peek() {
            Some(b'}') => return None,
            Some(b'"') => {}
            _ => return None,
        }
        let key = scanner.parse_string()?;
        scanner.skip_whitespace();
        scanner.expect(b':')?;
        if key == "dispositions" {
            scanner.skip_whitespace();
            return scanner.parse_key_list();
        }
        scanner.skip_value()?;
        scanner.skip_whitespace();
        match scanner.peek() {
            Some(b',') => {
                scanner.pos += 1;
            }
            Some(b'}') => return None,
            _ => return None,
        }
    }
}

/// Minimal JSON cursor over ledger bytes for key scanning.
///
/// The cursor only reads object keys and skips values. It never builds a
/// document model, so repeated keys stay visible to the caller.
struct Scanner<'a> {
    /// Raw file bytes being scanned.
    text: &'a [u8],
    /// Byte offset of the next unread byte.
    pos: usize,
}

impl Scanner<'_> {
    /// Skip spaces, tabs, and line breaks.
    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    /// Peek at the next unread byte without consuming it.
    fn peek(&self) -> Option<u8> {
        self.text.get(self.pos).copied()
    }

    /// Consume one expected byte.
    fn expect(&mut self, wanted: u8) -> Option<()> {
        if self.peek() == Some(wanted) {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    /// Parse one JSON string with escape handling.
    fn parse_string(&mut self) -> Option<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self.peek()?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.pos += 1;
                    match self.peek()? {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            self.pos += 1;
                            out.push(self.parse_hex_char()?);
                            continue;
                        }
                        _ => return None,
                    }
                    self.pos += 1;
                }
                0x20..=0x7E => {
                    out.push(byte as char);
                    self.pos += 1;
                }
                _ => {
                    let rest = std::str::from_utf8(&self.text[self.pos..]).ok()?;
                    let ch = rest.chars().next()?;
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    /// Parse four hex digits after a unicode escape into a character.
    fn parse_hex_char(&mut self) -> Option<char> {
        let digits = std::str::from_utf8(self.text.get(self.pos..self.pos + 4)?).ok()?;
        let code = u32::from_str_radix(digits, 16).ok()?;
        self.pos += 4;
        char::from_u32(code).or(Some('\u{FFFD}'))
    }

    /// Skip one JSON value of any shape.
    fn skip_value(&mut self) -> Option<()> {
        self.skip_whitespace();
        match self.peek()? {
            b'"' => {
                self.parse_string()?;
                Some(())
            }
            b'{' => self.skip_balanced(b'{', b'}'),
            b'[' => self.skip_balanced(b'[', b']'),
            b't' => self.skip_word("true"),
            b'f' => self.skip_word("false"),
            b'n' => self.skip_word("null"),
            b'-' | b'0'..=b'9' => {
                while matches!(
                    self.peek(),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.pos += 1;
                }
                Some(())
            }
            _ => None,
        }
    }

    /// Skip a balanced bracket pair with string awareness.
    fn skip_balanced(&mut self, open: u8, close: u8) -> Option<()> {
        self.expect(open)?;
        let mut depth = 1_usize;
        while depth > 0 {
            match self.peek()? {
                b'"' => {
                    self.parse_string()?;
                }
                byte if byte == open => {
                    depth += 1;
                    self.pos += 1;
                }
                byte if byte == close => {
                    depth -= 1;
                    self.pos += 1;
                }
                _ => {
                    self.pos += 1;
                }
            }
        }
        Some(())
    }

    /// Skip one literal word such as true or null.
    fn skip_word(&mut self, word: &str) -> Option<()> {
        if self.text.get(self.pos..self.pos + word.len()) == Some(word.as_bytes()) {
            self.pos += word.len();
            Some(())
        } else {
            None
        }
    }

    /// Parse an object into its keys in file order.
    fn parse_key_list(&mut self) -> Option<Vec<String>> {
        self.expect(b'{')?;
        let mut keys = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek()? {
                b'}' => {
                    self.pos += 1;
                    return Some(keys);
                }
                b'"' => {}
                _ => return None,
            }
            keys.push(self.parse_string()?);
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_value()?;
            self.skip_whitespace();
            match self.peek()? {
                b',' => {
                    self.pos += 1;
                }
                b'}' => {
                    self.pos += 1;
                    return Some(keys);
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::capture::CaptureRecord;
    use crate::vimanam::{ChangeRecord, DiffSummary, EndpointRef, Generator, Severity, SpecSide};

    /// Build a two change diff document bound to two spec hashes.
    fn document(old_hash: &str, new_hash: &str) -> DiffDocument {
        mixed_document(
            old_hash,
            new_hash,
            &[
                ("vc1_first", Severity::Breaking),
                ("vc1_second", Severity::Breaking),
            ],
        )
    }

    /// Build a diff document with explicit severities bound to two hashes.
    fn mixed_document(old_hash: &str, new_hash: &str, rows: &[(&str, Severity)]) -> DiffDocument {
        let breaking = rows
            .iter()
            .filter(|(_, severity)| matches!(severity, Severity::Breaking))
            .count();
        let non_breaking = rows
            .iter()
            .filter(|(_, severity)| matches!(severity, Severity::NonBreaking))
            .count();
        let review = rows.len() - breaking - non_breaking;
        let change = |(id, severity): &(&str, Severity)| ChangeRecord {
            id: id.to_string(),
            endpoint: EndpointRef {
                method: "POST".to_string(),
                path: "/search/random".to_string(),
            },
            kind: crate::vimanam::ChangeKind::ResponseSchemaChanged,
            severity: severity.clone(),
            details: crate::vimanam::ChangeDetails::default(),
        };
        DiffDocument {
            schema_version: 1,
            generator: Generator {
                name: "vimanam".to_string(),
                version: "1.3.0".to_string(),
            },
            old: SpecSide {
                title: "Demo".to_string(),
                version: "1".to_string(),
                file_sha256: old_hash.to_string(),
            },
            new: SpecSide {
                title: "Demo".to_string(),
                version: "2".to_string(),
                file_sha256: new_hash.to_string(),
            },
            summary: DiffSummary {
                endpoints_added: 0,
                endpoints_removed: 0,
                endpoints_changed: 1,
                breaking,
                non_breaking,
                review,
            },
            changes: rows.iter().map(change).collect(),
        }
    }

    /// Build a manifest and capture record bound to two spec hashes.
    fn binding(old_hash: &str, new_hash: &str) -> (AttemptRecord, CaptureRecord) {
        let manifest = AttemptRecord {
            schema_version: crate::state::SCHEMA_VERSION,
            attempt_id: "attempt".to_string(),
            old_spec_hash: old_hash.to_string(),
            new_spec_hash: new_hash.to_string(),
            capture_id: "capture".to_string(),
            repo_path: "/repo".to_string(),
            baseline_commit: "abc".to_string(),
            scope: Vec::new(),
            sethu_version: "0.1.0".to_string(),
        };
        let capture = CaptureRecord {
            schema_version: crate::state::SCHEMA_VERSION,
            capture_id: "capture".to_string(),
            generator_name: "vimanam".to_string(),
            generator_version: "1.3.0".to_string(),
            invocation: Vec::new(),
            vimanam_schema_version: 1,
            old_spec_hash: old_hash.to_string(),
            new_spec_hash: new_hash.to_string(),
        };
        (manifest, capture)
    }

    /// Evaluate with an empty repository and optional ledger bytes.
    fn run(
        manifest: &AttemptRecord,
        capture: &CaptureRecord,
        document: &DiffDocument,
        ledger_bytes: Option<&[u8]>,
        repo: &Path,
    ) -> Evaluation {
        evaluate(&Inputs {
            manifest,
            capture,
            origins: None,
            document,
            ledger_bytes,
            repo,
        })
    }

    #[test]
    fn missing_ledger_reports_every_required_change() {
        let repo = tempfile::tempdir().unwrap();
        let (manifest, capture) = binding(&"a".repeat(64), &"b".repeat(64));
        let document = document(&"a".repeat(64), &"b".repeat(64));
        let report = run(&manifest, &capture, &document, None, repo.path());
        assert!(!report.accounted);
        assert!(!report.ready);
        assert_eq!(report.problems.len(), 2);
        assert!(report.problems.iter().all(|p| p.code == Problem::MISSING));
        assert_eq!(
            exit_code(report.accounted, report.ready, false),
            std::process::ExitCode::from(4)
        );
    }

    #[test]
    fn identity_mismatch_blocks_accounting() {
        let repo = tempfile::tempdir().unwrap();
        let (manifest, capture) = binding(&"a".repeat(64), &"b".repeat(64));
        let document = document(&"a".repeat(64), &"c".repeat(64));
        let report = run(&manifest, &capture, &document, None, repo.path());
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.code == Problem::IDENTITY_MISMATCH)
        );
        assert!(!report.accounted);
    }

    #[test]
    fn verified_claims_without_a_stored_run_are_stale() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("notes.md"), "checked\n").unwrap();
        let (manifest, capture) = binding(&"a".repeat(64), &"b".repeat(64));
        let document = document(&"a".repeat(64), &"b".repeat(64));
        let ledger = serde_json::json!({
            "schema_version": 1,
            "dispositions": {
                "vc1_first": {"current": {"outcome": "fixed_and_verified", "evidence": ["notes.md"], "note": "patched"}},
                "vc1_second": {"current": {"outcome": "no_usage_found", "evidence": ["notes.md"]}}
            }
        });
        let bytes = serde_json::to_vec(&ledger).unwrap();
        let report = run(&manifest, &capture, &document, Some(&bytes), repo.path());
        assert!(!report.accounted);
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.code == Problem::STALE_VERIFICATION)
        );
        assert!(!report.problems.iter().any(|p| p.id == "vc1_second"
            && !p.code.is_empty()
            && p.code == Problem::STALE_VERIFICATION));
    }

    #[test]
    fn decision_required_is_accounted_but_not_ready() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("notes.md"), "checked\n").unwrap();
        let (manifest, capture) = binding(&"a".repeat(64), &"b".repeat(64));
        let document = document(&"a".repeat(64), &"b".repeat(64));
        let ledger = serde_json::json!({
            "schema_version": 1,
            "dispositions": {
                "vc1_first": {"current": {"outcome": "decision_required", "evidence": ["notes.md"], "note": "drop paging or fetch again"}},
                "vc1_second": {"current": {"outcome": "no_usage_found", "evidence": ["notes.md"]}}
            }
        });
        let bytes = serde_json::to_vec(&ledger).unwrap();
        let report = run(&manifest, &capture, &document, Some(&bytes), repo.path());
        assert!(report.accounted);
        assert!(!report.ready);
        assert_eq!(report.not_ready, vec!["vc1_first".to_string()]);
        assert_eq!(
            exit_code(report.accounted, report.ready, false),
            std::process::ExitCode::SUCCESS
        );
        assert_eq!(
            exit_code(report.accounted, report.ready, true),
            std::process::ExitCode::from(5)
        );
    }

    #[test]
    fn duplicate_keys_in_raw_bytes_are_detected() {
        let bytes = br#"{"schema_version": 1, "dispositions": {"vc1_first": {"current": {"outcome": "unresolved", "evidence": []}}, "vc1_first": {"current": {"outcome": "unresolved", "evidence": []}}}}"#;
        let keys = scan_disposition_keys(bytes).unwrap();
        assert_eq!(keys, vec!["vc1_first".to_string(), "vc1_first".to_string()]);
    }

    #[test]
    fn unknown_and_malformed_entries_are_rejected() {
        let repo = tempfile::tempdir().unwrap();
        let (manifest, capture) = binding(&"a".repeat(64), &"b".repeat(64));
        let document = document(&"a".repeat(64), &"b".repeat(64));
        let ledger = serde_json::json!({
            "schema_version": 1,
            "dispositions": {
                "vc1_first": {"current": {"outcome": "bogus", "evidence": []}},
                "vc1_nobody": {"current": {"outcome": "unresolved", "evidence": []}}
            }
        });
        let bytes = serde_json::to_vec(&ledger).unwrap();
        let report = run(&manifest, &capture, &document, Some(&bytes), repo.path());
        assert!(!report.accounted);
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.code == Problem::MALFORMED_OUTCOME)
        );
        assert!(report.problems.iter().any(|p| p.code == Problem::UNKNOWN));
        assert!(report.problems.iter().any(|p| p.code == Problem::MISSING));
    }

    #[test]
    fn exit_codes_follow_the_frozen_map() {
        assert_eq!(
            exit_code(false, false, false),
            std::process::ExitCode::from(4)
        );
        assert_eq!(
            exit_code(false, false, true),
            std::process::ExitCode::from(4)
        );
        assert_eq!(
            exit_code(true, false, false),
            std::process::ExitCode::SUCCESS
        );
        assert_eq!(
            exit_code(true, false, true),
            std::process::ExitCode::from(5)
        );
        assert_eq!(exit_code(true, true, true), std::process::ExitCode::SUCCESS);
    }

    #[test]
    fn required_set_keeps_only_breaking_and_review() {
        let repo = tempfile::tempdir().unwrap();
        let (manifest, capture) = binding(&"a".repeat(64), &"b".repeat(64));
        let document = mixed_document(
            &"a".repeat(64),
            &"b".repeat(64),
            &[
                ("vc1_first", Severity::Breaking),
                ("vc1_second", Severity::Review),
                ("vc1_third", Severity::NonBreaking),
            ],
        );
        std::fs::write(repo.path().join("notes.md"), "checked\n").unwrap();
        let ledger = serde_json::json!({
            "schema_version": 1,
            "dispositions": {
                "vc1_first": {"current": {"outcome": "no_usage_found", "evidence": ["notes.md"]}},
                "vc1_second": {"current": {"outcome": "no_usage_found", "evidence": ["notes.md"]}}
            }
        });
        let bytes = serde_json::to_vec(&ledger).unwrap();
        let report = run(&manifest, &capture, &document, Some(&bytes), repo.path());
        assert_eq!(
            report.required,
            vec!["vc1_first".to_string(), "vc1_second".to_string()]
        );
        assert!(report.accounted);
        assert!(report.ready);
        assert!(report.problems.is_empty());
    }

    #[test]
    fn voluntary_non_breaking_entry_causes_no_problem() {
        let repo = tempfile::tempdir().unwrap();
        let (manifest, capture) = binding(&"a".repeat(64), &"b".repeat(64));
        let document = mixed_document(
            &"a".repeat(64),
            &"b".repeat(64),
            &[
                ("vc1_first", Severity::Breaking),
                ("vc1_second", Severity::Review),
                ("vc1_third", Severity::NonBreaking),
            ],
        );
        std::fs::write(repo.path().join("notes.md"), "checked\n").unwrap();
        let ledger = serde_json::json!({
            "schema_version": 1,
            "dispositions": {
                "vc1_first": {"current": {"outcome": "no_usage_found", "evidence": ["notes.md"]}},
                "vc1_second": {"current": {"outcome": "no_usage_found", "evidence": ["notes.md"]}},
                "vc1_third": {"current": {"outcome": "no_usage_found", "evidence": ["notes.md"]}}
            }
        });
        let bytes = serde_json::to_vec(&ledger).unwrap();
        let report = run(&manifest, &capture, &document, Some(&bytes), repo.path());
        assert!(report.accounted);
        assert!(report.ready);
        assert!(report.problems.is_empty());
        assert!(!report.dispositions.contains_key("vc1_third"));
    }

    #[test]
    fn unknown_id_outside_capture_still_fails() {
        let repo = tempfile::tempdir().unwrap();
        let (manifest, capture) = binding(&"a".repeat(64), &"b".repeat(64));
        let document = mixed_document(
            &"a".repeat(64),
            &"b".repeat(64),
            &[
                ("vc1_first", Severity::Breaking),
                ("vc1_third", Severity::NonBreaking),
            ],
        );
        std::fs::write(repo.path().join("notes.md"), "checked\n").unwrap();
        let ledger = serde_json::json!({
            "schema_version": 1,
            "dispositions": {
                "vc1_first": {"current": {"outcome": "no_usage_found", "evidence": ["notes.md"]}},
                "vc1_nobody": {"current": {"outcome": "unresolved", "evidence": ["notes.md"]}}
            }
        });
        let bytes = serde_json::to_vec(&ledger).unwrap();
        let report = run(&manifest, &capture, &document, Some(&bytes), repo.path());
        assert!(!report.accounted);
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.id == "vc1_nobody" && p.code == Problem::UNKNOWN)
        );
    }

    /// One committed git repository with a pinned code file.
    fn git_repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("app.ts"), "export {};\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        let output = git(&["rev-parse", "HEAD"]);
        let head = String::from_utf8(output.stdout).unwrap();
        (dir, head.trim().to_string())
    }

    /// Embedded contract bytes with their hashes.
    fn spec_pair() -> (Vec<u8>, Vec<u8>, String, String) {
        let (old, _) = crate::stub::embedded_spec(&crate::cli::SpecVersion::Old);
        let (next, _) = crate::stub::embedded_spec(&crate::cli::SpecVersion::New);
        let old_hash = crate::state::layout::sha256_hex(old);
        let next_hash = crate::state::layout::sha256_hex(next);
        (old.to_vec(), next.to_vec(), old_hash, next_hash)
    }

    /// One asset body that validates under both embedded specs.
    fn claim_asset(id: &str) -> serde_json::Value {
        serde_json::json!({
            "checksum": "da39a3ee5e6b4b0d3255bfef95601890afd80709",
            "deviceAssetId": "device-0",
            "deviceId": "device-1",
            "duration": "0:00:01.000000",
            "fileCreatedAt": "2024-09-27T10:00:00.000Z",
            "fileModifiedAt": "2024-09-27T10:00:00.000Z",
            "hasMetadata": true,
            "id": id,
            "isArchived": false,
            "isFavorite": false,
            "isOffline": false,
            "isTrashed": false,
            "localDateTime": "2024-09-27T10:00:00.000Z",
            "originalFileName": "photo-0.jpg",
            "originalPath": "/photos/photo-0.jpg",
            "ownerId": "550e8400-e29b-41d4-a716-446655440001",
            "thumbhash": "3OcRJwh4d3h6eIeIh3h2e3h4gQ",
            "type": "IMAGE",
            "updatedAt": "2024-09-27T10:00:00.000Z",
            "stack": null
        })
    }

    /// Old-contract search response holding one asset.
    fn claim_old_body() -> serde_json::Value {
        serde_json::json!({
            "albums": {"count": 0, "facets": [], "items": [], "total": 0},
            "assets": {
                "count": 1,
                "facets": [],
                "items": [claim_asset("asset-r1")],
                "nextPage": null,
                "total": 1
            }
        })
    }

    /// New-contract random response holding one asset.
    fn claim_new_body() -> serde_json::Value {
        serde_json::Value::Array(vec![claim_asset("asset-r1")])
    }

    /// One scenario file value for a random-search fixture.
    fn claim_scenario(spec_hash: &str, body: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": "random-search",
            "change_ids": ["vc1_claim"],
            "spec_sha256": spec_hash,
            "request": {"method": "POST", "path": "/search/random"},
            "response": {"status": 200, "body": body}
        })
    }

    /// One trace entry for a served random-search exchange.
    fn claim_trace(body: &serde_json::Value) -> serde_json::Value {
        let bytes = serde_json::to_vec(body).unwrap();
        serde_json::json!({
            "scenario_id": "random-search",
            "method": "POST",
            "path": "/search/random",
            "request_valid": true,
            "request_detail": "body matches the contract schema",
            "response_status": 200,
            "response_body_sha256": crate::stub::body_sha256_hex(&bytes),
            "response_written": true
        })
    }

    /// Stored cases matching one fabricated stage verdict.
    ///
    /// A red verdict carries a failing case with the declared
    /// diagnostic. Any other verdict carries a passing case.
    fn stage_cases(verdict: &str) -> serde_json::Value {
        if verdict == "expected-red" {
            serde_json::json!([
                {"name": "verify_random_picker", "passed": false,
                 "output": "assertion failed: random picker ids are wrong"}
            ])
        } else {
            serde_json::json!([
                {"name": "verify_random_picker", "passed": true, "output": ""}
            ])
        }
    }

    /// Stored run world with fabricated artefacts under a git checkout.
    struct ClaimWorld {
        /// Guard for the git repository holding state and scenarios.
        _repo: tempfile::TempDir,
        /// Canonical repository path used for lookups.
        repo: std::path::PathBuf,
        /// Attempt manifest bound to the embedded spec hashes.
        manifest: AttemptRecord,
        /// Capture record bound to the same hashes.
        capture: CaptureRecord,
        /// Diff document with one breaking change.
        document: DiffDocument,
        /// Stored run id under the migration.
        run_id: String,
        /// Migration directory holding the run tree.
        migration: std::path::PathBuf,
    }

    /// Build a world where the stored run fully backs the claim.
    fn claim_world() -> ClaimWorld {
        let (repo_dir, head) = git_repo();
        let repo = repo_dir.path().canonicalize().unwrap();
        let (old_bytes, new_bytes, old_hash, new_hash) = spec_pair();
        let manifest = AttemptRecord {
            schema_version: crate::state::SCHEMA_VERSION,
            attempt_id: "attempt".to_string(),
            old_spec_hash: old_hash.clone(),
            new_spec_hash: new_hash.clone(),
            capture_id: "capture".to_string(),
            repo_path: repo.display().to_string(),
            baseline_commit: head.clone(),
            scope: Vec::new(),
            sethu_version: "0.1.0".to_string(),
        };
        let capture = CaptureRecord {
            schema_version: crate::state::SCHEMA_VERSION,
            capture_id: "capture".to_string(),
            generator_name: "vimanam".to_string(),
            generator_version: "1.3.0".to_string(),
            invocation: Vec::new(),
            vimanam_schema_version: 1,
            old_spec_hash: old_hash.clone(),
            new_spec_hash: new_hash.clone(),
        };
        let document = mixed_document(&old_hash, &new_hash, &[("vc1_claim", Severity::Breaking)]);
        let root = crate::state::layout::state_root(&repo);
        let pair = crate::state::layout::pair_dir(&root, &old_hash, &new_hash).unwrap();
        std::fs::create_dir_all(crate::state::layout::pair_inputs_dir(&pair)).unwrap();
        std::fs::write(crate::state::pair::inputs_old_path(&pair), &old_bytes).unwrap();
        std::fs::write(crate::state::pair::inputs_new_path(&pair), &new_bytes).unwrap();
        let scenarios_old = repo.join("scenarios-old");
        let scenarios_new = repo.join("scenarios-new");
        std::fs::create_dir_all(&scenarios_old).unwrap();
        std::fs::create_dir_all(&scenarios_new).unwrap();
        std::fs::write(
            scenarios_old.join("random.json"),
            serde_json::to_vec_pretty(&claim_scenario(&old_hash, &claim_old_body())).unwrap(),
        )
        .unwrap();
        std::fs::write(
            scenarios_new.join("random.json"),
            serde_json::to_vec_pretty(&claim_scenario(&new_hash, &claim_new_body())).unwrap(),
        )
        .unwrap();
        let migration = crate::state::layout::migration_dir(&root, "attempt");
        let run_id = "run-claim".to_string();
        write_claim_run(
            &migration,
            &run_id,
            &head,
            &old_hash,
            &new_hash,
            &scenarios_old,
            &scenarios_new,
            &claim_old_body(),
            &claim_new_body(),
            "picker",
            "regression",
            true,
            &["vc1_claim".to_string()],
            "pass",
            "expected-red",
            "pass",
        );
        ClaimWorld {
            _repo: repo_dir,
            repo,
            manifest,
            capture,
            document,
            run_id,
            migration,
        }
    }

    /// Write one fabricated run with three stages and clean traces.
    #[allow(clippy::too_many_arguments)]
    fn write_claim_run(
        migration: &std::path::Path,
        run_id: &str,
        head: &str,
        old_hash: &str,
        new_hash: &str,
        scenarios_old: &std::path::Path,
        scenarios_new: &std::path::Path,
        old_body: &serde_json::Value,
        new_body: &serde_json::Value,
        check: &str,
        role: &str,
        verified: bool,
        change_ids: &[String],
        old_verdict: &str,
        red_verdict: &str,
        patched_verdict: &str,
    ) {
        let run_dir = migration.join("runs").join(run_id);
        let record = serde_json::json!({
            "run_id": run_id,
            "harness_hash": "abc",
            "harness_changed": false,
            "sethu_version": "0.1.0",
            "nextest_version": "nextest",
            "checks": [
                {"name": check, "role": role, "change_ids": change_ids,
                 "verified": verified, "stages": []}
            ]
        });
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("run.json"),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
        for (stage, commit, spec, spec_hash, scenarios, body, verdict) in [
            (
                "original-old",
                "baseline",
                "old",
                old_hash,
                scenarios_old,
                old_body,
                old_verdict,
            ),
            (
                "original-new",
                "baseline",
                "new",
                new_hash,
                scenarios_new,
                new_body,
                red_verdict,
            ),
            (
                "patched-new",
                head,
                "new",
                new_hash,
                scenarios_new,
                new_body,
                patched_verdict,
            ),
        ] {
            let stage_dir = run_dir.join(check).join(stage);
            std::fs::create_dir_all(&stage_dir).unwrap();
            let stored = serde_json::json!({
                "stage": stage,
                "check": check,
                "change_ids": change_ids,
                "expected_diagnostic": "random picker ids",
                "expected_exchange": [
                    {"scenario": "random-search", "method": "POST", "path": "/search/random"}
                ],
                "commit": commit,
                "spec_version": spec,
                "spec_sha256": spec_hash,
                "scenarios_dir": scenarios.display().to_string(),
                "verdict": verdict,
                "detail": "",
                "exit_code": if verdict == "expected-red" { 101 } else { 0 },
                "signal": null,
                "timed_out": false,
                "build_failed": false,
                "cases": stage_cases(verdict),
            });
            std::fs::write(
                stage_dir.join("stage.json"),
                serde_json::to_vec_pretty(&stored).unwrap(),
            )
            .unwrap();
            std::fs::write(
                stage_dir.join("requests.jsonl"),
                format!(
                    "{}\n",
                    String::from_utf8(serde_json::to_vec(&claim_trace(body)).unwrap()).unwrap()
                ),
            )
            .unwrap();
        }
    }

    /// Evaluate a world with one fixed claim on the single change.
    fn claim_report(world: &ClaimWorld, evidence: &[&str]) -> Evaluation {
        let evidence: Vec<String> = evidence.iter().map(|item| item.to_string()).collect();
        let ledger = serde_json::json!({
            "schema_version": 1,
            "dispositions": {
                "vc1_claim": {"current": {"outcome": "fixed_and_verified", "evidence": evidence}}
            }
        });
        let bytes = serde_json::to_vec(&ledger).unwrap();
        run(
            &world.manifest,
            &world.capture,
            &world.document,
            Some(&bytes),
            &world.repo,
        )
    }

    /// Read the single problem of a refused claim report.
    fn single_problem(report: &Evaluation) -> &Problem {
        assert!(
            !report.accounted,
            "expected the claim to fail, got: {report:?}"
        );
        assert_eq!(
            report.problems.len(),
            1,
            "expected one problem, got: {report:?}"
        );
        assert_eq!(report.problems[0].code, Problem::STALE_VERIFICATION);
        &report.problems[0]
    }

    #[test]
    fn run_references_parse_run_and_check() {
        let parsed = parse_run_reference("run:run-1/picker").unwrap();
        assert_eq!(parsed.run_id, "run-1");
        assert_eq!(parsed.check, "picker");
        for bad in ["run:only-run", "run:/picker", "run:", "run", "picker", ""] {
            assert!(
                parse_run_reference(bad).is_err(),
                "expected a refusal for `{bad}`"
            );
        }
    }

    #[test]
    fn stored_run_with_a_clean_trace_validates() {
        let world = claim_world();
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        assert!(
            report.accounted,
            "expected no problems, got: {:?}",
            report.problems
        );
        assert!(report.ready);
        assert!(report.problems.is_empty());
        let view = &report.dispositions["vc1_claim"];
        assert!(view.valid);
        assert!(view.ready);
        assert_eq!(
            exit_code(report.accounted, report.ready, true),
            std::process::ExitCode::SUCCESS
        );
    }

    #[test]
    fn missing_run_is_refused_with_known_ids() {
        let world = claim_world();
        let report = claim_report(&world, &["run:run-ghost/picker", "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("no stored run"),
            "got: {}",
            problem.message
        );
        assert!(
            problem.message.contains(&world.run_id),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn unknown_check_is_refused_with_known_names() {
        let world = claim_world();
        let run_ref = format!("run:{}/ghost", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("no check"),
            "got: {}",
            problem.message
        );
        assert!(
            problem.message.contains("picker"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn unverified_check_cannot_back_a_claim() {
        let world = claim_world();
        rewrite_run_check(
            &world,
            "picker",
            "regression",
            false,
            &["vc1_claim".to_string()],
        );
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("did not verify"),
            "got: {}",
            problem.message
        );
    }

    /// Rewrite the single check summary inside a fabricated run record.
    fn rewrite_run_check(
        world: &ClaimWorld,
        check: &str,
        role: &str,
        verified: bool,
        change_ids: &[String],
    ) {
        let path = world
            .migration
            .join("runs")
            .join(&world.run_id)
            .join("run.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record["checks"] = serde_json::json!([
            {"name": check, "role": role, "change_ids": change_ids,
             "verified": verified, "stages": []}
        ]);
        std::fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    }

    #[test]
    fn guard_only_reference_is_refused() {
        let world = claim_world();
        rewrite_run_check(&world, "picker", "guard", true, &["vc1_claim".to_string()]);
        for stage in ["original-old", "original-new", "patched-new"] {
            rewrite_stage_verdict(&world, "picker", stage, "pass");
        }
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("only guard"),
            "got: {}",
            problem.message
        );
    }

    /// Rewrite one fabricated stage verdict with matching cases.
    fn rewrite_stage_verdict(world: &ClaimWorld, check: &str, stage: &str, verdict: &str) {
        let path = world
            .migration
            .join("runs")
            .join(&world.run_id)
            .join(check)
            .join(stage)
            .join("stage.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record["verdict"] = serde_json::Value::String(verdict.to_string());
        record["cases"] = stage_cases(verdict);
        record["exit_code"] = serde_json::json!(if verdict == "expected-red" { 101 } else { 0 });
        std::fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    }

    #[test]
    fn uncovered_change_is_refused() {
        let world = claim_world();
        rewrite_run_check(
            &world,
            "picker",
            "regression",
            true,
            &["vc1_other".to_string()],
        );
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("covers"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn run_on_another_commit_is_stale() {
        let world = claim_world();
        let run_ref = format!("run:{}/picker", world.run_id);
        rewrite_stage_commit(&world, "picker", "patched-new", "deadbeef");
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("no longer matches"),
            "got: {}",
            problem.message
        );
    }

    /// Rewrite one fabricated stage commit.
    fn rewrite_stage_commit(world: &ClaimWorld, check: &str, stage: &str, commit: &str) {
        let path = world
            .migration
            .join("runs")
            .join(&world.run_id)
            .join(check)
            .join(stage)
            .join("stage.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record["commit"] = serde_json::Value::String(commit.to_string());
        std::fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    }

    #[test]
    fn red_stage_without_a_failure_is_refused() {
        let world = claim_world();
        rewrite_stage_verdict(&world, "picker", "original-new", "pass");
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("expected-red"),
            "got: {}",
            problem.message
        );
    }

    /// Patch one field of a fabricated stage record.
    fn patch_stage_json(
        world: &ClaimWorld,
        check: &str,
        stage: &str,
        key: &str,
        value: serde_json::Value,
    ) {
        let path = world
            .migration
            .join("runs")
            .join(&world.run_id)
            .join(check)
            .join(stage)
            .join("stage.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record[key] = value;
        std::fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    }

    #[test]
    fn red_stage_without_the_diagnostic_is_refused() {
        let world = claim_world();
        patch_stage_json(
            &world,
            "picker",
            "original-new",
            "cases",
            serde_json::json!([
                {"name": "verify_random_picker", "passed": false,
                 "output": "connection refused before any request"}
            ]),
        );
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("without the expected diagnostic"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn red_stage_with_zero_exit_is_refused() {
        let world = claim_world();
        patch_stage_json(
            &world,
            "picker",
            "original-new",
            "exit_code",
            serde_json::json!(0),
        );
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("exited 0"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn green_stage_with_a_failing_case_is_refused() {
        let world = claim_world();
        patch_stage_json(
            &world,
            "picker",
            "patched-new",
            "cases",
            serde_json::json!([
                {"name": "verify_random_picker", "passed": false,
                 "output": "assertion failed: random picker ids are wrong"}
            ]),
        );
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("where a pass was required"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn stage_without_cases_is_refused() {
        let world = claim_world();
        patch_stage_json(
            &world,
            "picker",
            "patched-new",
            "cases",
            serde_json::Value::Array(Vec::new()),
        );
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("no test cases"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn broken_trace_exchange_is_refused() {
        let world = claim_world();
        let path = world
            .migration
            .join("runs")
            .join(&world.run_id)
            .join("picker")
            .join("patched-new")
            .join("requests.jsonl");
        std::fs::write(path, "{\"scenario_id\":null,\"method\":\"GET\",\"path\":\"/elsewhere\",\"request_valid\":false,\"request_detail\":\"no scenario matches\",\"response_status\":599,\"response_body_sha256\":\"abc\",\"response_written\":true}\n").unwrap();
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem
                .message
                .contains("not received, validated, and answered")
                || problem.message.contains("unexpected request"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn stage_without_an_expected_exchange_is_refused() {
        let world = claim_world();
        let path = world
            .migration
            .join("runs")
            .join(&world.run_id)
            .join("picker")
            .join("patched-new")
            .join("stage.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record["expected_exchange"] = serde_json::Value::Array(Vec::new());
        std::fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let run_ref = format!("run:{}/picker", world.run_id);
        let report = claim_report(&world, &[&run_ref, "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("no expected exchange"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn bare_run_reference_without_a_check_is_refused() {
        let world = claim_world();
        let report = claim_report(&world, &["run:bare-run", "app.ts:1"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("run:<run-id>/<check>"),
            "got: {}",
            problem.message
        );
    }

    #[test]
    fn free_text_never_satisfies_verification() {
        let world = claim_world();
        let report = claim_report(&world, &["looks fixed to me"]);
        let problem = single_problem(&report);
        assert!(
            problem.message.contains("run:<run-id>/<check>"),
            "got: {}",
            problem.message
        );
    }

    /// One spec whose response schema carries a discriminator.
    fn discriminated_spec() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title": "Things", "version": "1"},
            "paths": {
                "/thing": {
                    "get": {
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "schema": {
                                            "type": "object",
                                            "discriminator": {"propertyName": "kind"},
                                            "properties": {"kind": {"type": "string"}},
                                            "required": ["kind"]
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn unsupported_only_scenarios_block_the_claim() {
        let (repo_dir, head) = git_repo();
        let repo = repo_dir.path().canonicalize().unwrap();
        let spec_bytes = discriminated_spec();
        let spec_hash = crate::state::layout::sha256_hex(&spec_bytes);
        let manifest = AttemptRecord {
            schema_version: crate::state::SCHEMA_VERSION,
            attempt_id: "attempt".to_string(),
            old_spec_hash: spec_hash.clone(),
            new_spec_hash: spec_hash.clone(),
            capture_id: "capture".to_string(),
            repo_path: repo.display().to_string(),
            baseline_commit: head.clone(),
            scope: Vec::new(),
            sethu_version: "0.1.0".to_string(),
        };
        let capture = CaptureRecord {
            schema_version: crate::state::SCHEMA_VERSION,
            capture_id: "capture".to_string(),
            generator_name: "vimanam".to_string(),
            generator_version: "1.3.0".to_string(),
            invocation: Vec::new(),
            vimanam_schema_version: 1,
            old_spec_hash: spec_hash.clone(),
            new_spec_hash: spec_hash.clone(),
        };
        let document = mixed_document(&spec_hash, &spec_hash, &[("vc1_claim", Severity::Breaking)]);
        let root = crate::state::layout::state_root(&repo);
        let pair = crate::state::layout::pair_dir(&root, &spec_hash, &spec_hash).unwrap();
        std::fs::create_dir_all(crate::state::layout::pair_inputs_dir(&pair)).unwrap();
        std::fs::write(crate::state::pair::inputs_old_path(&pair), &spec_bytes).unwrap();
        std::fs::write(crate::state::pair::inputs_new_path(&pair), &spec_bytes).unwrap();
        let body = serde_json::json!({"kind": "widget"});
        let scenarios = repo.join("scenarios");
        std::fs::create_dir_all(&scenarios).unwrap();
        std::fs::write(
            scenarios.join("thing.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "id": "thing",
                "change_ids": ["vc1_claim"],
                "spec_sha256": spec_hash,
                "request": {"method": "GET", "path": "/thing"},
                "response": {"status": 200, "body": body}
            }))
            .unwrap(),
        )
        .unwrap();
        let loaded = crate::stub::load_scenarios(
            &scenarios,
            &serde_json::from_slice::<serde_json::Value>(&spec_bytes).unwrap(),
            &spec_hash,
        )
        .unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(
            !loaded[0].supports_claim,
            "the discriminator scenario must not support a claim"
        );
        let migration = crate::state::layout::migration_dir(&root, "attempt");
        let run_dir = migration.join("runs").join("run-claim");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("run.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "run_id": "run-claim",
                "checks": [
                    {"name": "picker", "role": "regression",
                     "change_ids": ["vc1_claim"], "verified": true, "stages": []}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let trace_bytes = serde_json::to_vec(&serde_json::json!({
            "scenario_id": "thing",
            "method": "GET",
            "path": "/thing",
            "request_valid": true,
            "request_detail": "path matches the contract",
            "response_status": 200,
            "response_body_sha256": crate::stub::body_sha256_hex(&serde_json::to_vec(&body).unwrap()),
            "response_written": true
        }))
        .unwrap();
        for (stage, commit, verdict) in [
            ("original-old", "baseline", "pass"),
            ("original-new", "baseline", "expected-red"),
            ("patched-new", head.as_str(), "pass"),
        ] {
            let stage_dir = run_dir.join("picker").join(stage);
            std::fs::create_dir_all(&stage_dir).unwrap();
            let red = verdict == "expected-red";
            std::fs::write(
                stage_dir.join("stage.json"),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "stage": stage,
                    "check": "picker",
                    "change_ids": ["vc1_claim"],
                    "expected_diagnostic": "thing ids",
                    "expected_exchange": [
                        {"scenario": "thing", "method": "GET", "path": "/thing"}
                    ],
                    "commit": commit,
                    "spec_version": "new",
                    "spec_sha256": spec_hash,
                    "scenarios_dir": scenarios.display().to_string(),
                    "verdict": verdict,
                    "detail": "",
                    "exit_code": if red { 101 } else { 0 },
                    "signal": null,
                    "timed_out": false,
                    "build_failed": false,
                    "cases": [
                        {"name": "verify_thing", "passed": !red,
                         "output": if red { "assertion failed: thing ids are wrong" } else { "" }}
                    ]
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::write(
                stage_dir.join("requests.jsonl"),
                format!("{}\n", String::from_utf8(trace_bytes.clone()).unwrap()),
            )
            .unwrap();
        }
        let ledger = serde_json::json!({
            "schema_version": 1,
            "dispositions": {
                "vc1_claim": {"current": {"outcome": "fixed_and_verified",
                                          "evidence": ["run:run-claim/picker", "app.ts:1"]}}
            }
        });
        let bytes = serde_json::to_vec(&ledger).unwrap();
        let report = run(&manifest, &capture, &document, Some(&bytes), &repo);
        assert!(!report.accounted);
        assert_eq!(report.problems.len(), 1);
        assert_eq!(report.problems[0].code, Problem::STALE_VERIFICATION);
        assert!(
            report.problems[0].message.contains("blocked, not passed"),
            "got: {}",
            report.problems[0].message
        );
    }
}
