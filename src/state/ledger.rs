//! Outcome ledger for one migration attempt.
//!
//! The ledger keeps one current disposition per change id. Each update
//! replaces the current entry and appends the replaced entry to a history
//! array in the same file. There is no separate history directory. Only
//! the record flow writes this file. Later flows read it back and check
//! it again, since files can be edited by hand.
//!
//! Every write goes through the shared atomic writer, so readers see the
//! old file or the new file, never a mix.

use std::path::Path;

use anyhow::Context;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Outcome words accepted by the ledger.
///
/// The words match the command line values exactly. Parsing refuses
/// anything outside these five.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The change was fixed and the fix was verified by a passing run.
    FixedAndVerified,
    /// The application is not affected within the inspected scope.
    UnaffectedInApplication,
    /// No usage was found within the inspected scope.
    NoUsageFound,
    /// A product decision is needed before this can be resolved.
    DecisionRequired,
    /// The outcome is still unknown or a fix is still failing.
    Unresolved,
}

impl Outcome {
    /// Parse an outcome word.
    ///
    /// The match is exact and lowercase with underscores. Anything else
    /// fails and names the five accepted words.
    pub fn parse(word: &str) -> anyhow::Result<Self> {
        match word {
            "fixed_and_verified" => Ok(Self::FixedAndVerified),
            "unaffected_in_application" => Ok(Self::UnaffectedInApplication),
            "no_usage_found" => Ok(Self::NoUsageFound),
            "decision_required" => Ok(Self::DecisionRequired),
            "unresolved" => Ok(Self::Unresolved),
            _ => anyhow::bail!(
                "unknown outcome `{word}`, expected `fixed_and_verified`, `unaffected_in_application`, `no_usage_found`, `decision_required`, or `unresolved`"
            ),
        }
    }

    /// Render the outcome word.
    ///
    /// The result matches the command line value exactly.
    pub fn as_word(self) -> &'static str {
        match self {
            Self::FixedAndVerified => "fixed_and_verified",
            Self::UnaffectedInApplication => "unaffected_in_application",
            Self::NoUsageFound => "no_usage_found",
            Self::DecisionRequired => "decision_required",
            Self::Unresolved => "unresolved",
        }
    }

    /// Translate a command line outcome into a ledger outcome.
    ///
    /// Both sides name the same five words, so the mapping is one to one.
    pub fn from_cli(outcome: &crate::cli::Outcome) -> Self {
        match outcome {
            crate::cli::Outcome::FixedAndVerified => Self::FixedAndVerified,
            crate::cli::Outcome::UnaffectedInApplication => Self::UnaffectedInApplication,
            crate::cli::Outcome::NoUsageFound => Self::NoUsageFound,
            crate::cli::Outcome::DecisionRequired => Self::DecisionRequired,
            crate::cli::Outcome::Unresolved => Self::Unresolved,
        }
    }
}

impl std::fmt::Display for Outcome {
    /// Format the outcome as its word.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_word())
    }
}

/// Resolved shape of one evidence reference.
///
/// Free text never counts toward any minimum. Run references name a
/// verification run, which no outcome can verify yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    /// A `path:line` location whose file exists in the repository.
    Code,
    /// A repository file path, such as a saved search artefact.
    File,
    /// A `run:` reference to a verification run.
    Run,
    /// Anything else, including prose and missing files.
    Text,
}

/// One evidence reference after shape and existence checks.
///
/// The reference keeps its original spelling. The kind reflects what the
/// reference proved: shape plus a file check against the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedEvidence {
    /// The reference exactly as passed on the command line.
    pub reference: String,
    /// The resolved shape of the reference.
    pub kind: EvidenceKind,
}

/// Counts of checked evidence by kind.
///
/// Rules read these counts instead of raw strings, so prose can never
/// satisfy a minimum on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EvidenceSummary {
    /// References that pin a line in an existing repository file.
    pub code: usize,
    /// References that name an existing repository file.
    pub files: usize,
    /// References that name a verification run.
    pub runs: usize,
}

impl EvidenceSummary {
    /// Count references that name checkable repository content.
    ///
    /// Code locations and files both count. Run references and prose do not.
    pub fn checkable(self) -> usize {
        self.code + self.files
    }
}

/// Check whether a line suffix names a real line number.
///
/// The suffix must be all digits and at least one. Line zero is refused.
fn is_line_number(text: &str) -> bool {
    match text.parse::<u64>() {
        Ok(number) => number >= 1,
        Err(_) => false,
    }
}

/// Check whether a suffix is all digits, whatever its value.
///
/// A digit suffix marks a location claim even when the value is out of
/// range, so line zero fails loudly instead of reading as prose.
fn is_digit_suffix(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Resolve one evidence reference against the repository.
///
/// A `run:` reference with a non empty id counts as a run. A `path:line`
/// reference must name an existing file, and a missing file fails with
/// the reference named, since the shape claims a location. An out of
/// range digit suffix fails the same way. A bare path
/// that names an existing file counts as a file. Everything else reads
/// as prose and counts toward nothing.
pub fn check_reference(repo: &Path, reference: &str) -> anyhow::Result<CheckedEvidence> {
    if let Some(id) = reference.strip_prefix("run:")
        && !id.is_empty()
    {
        return Ok(CheckedEvidence {
            reference: reference.to_string(),
            kind: EvidenceKind::Run,
        });
    }
    if let Some((path, line)) = reference.rsplit_once(':')
        && !path.is_empty()
        && is_digit_suffix(line)
    {
        if !is_line_number(line) {
            anyhow::bail!("evidence `{reference}` names line `{line}`, line numbers start at one");
        }
        let target = repo.join(path);
        if !target.is_file() {
            anyhow::bail!("evidence `{reference}` names no file under the repository");
        }
        return Ok(CheckedEvidence {
            reference: reference.to_string(),
            kind: EvidenceKind::Code,
        });
    }
    if !reference.is_empty() && repo.join(reference).is_file() {
        return Ok(CheckedEvidence {
            reference: reference.to_string(),
            kind: EvidenceKind::File,
        });
    }
    Ok(CheckedEvidence {
        reference: reference.to_string(),
        kind: EvidenceKind::Text,
    })
}

/// Count checked evidence by kind.
///
/// The summary feeds the per outcome rules.
pub fn summarize(evidence: &[CheckedEvidence]) -> EvidenceSummary {
    let mut summary = EvidenceSummary::default();
    for item in evidence {
        match item.kind {
            EvidenceKind::Code => summary.code += 1,
            EvidenceKind::File => summary.files += 1,
            EvidenceKind::Run => summary.runs += 1,
            EvidenceKind::Text => {}
        }
    }
    summary
}

/// Report whether an optional note carries any non blank text.
fn has_note(note: Option<&str>) -> bool {
    note.is_some_and(|text| !text.trim().is_empty())
}

/// Check the evidence for one outcome.
///
/// Verified closure needs a run reference that names a stored run and
/// a check, plus a pinned code location. The deeper run validation
/// (the run exists, its check covers the change, the commit matches,
/// and the trace shows the exchange) runs at write and check time
/// against the stored artefacts. Unaffected closure needs a line
/// pinned code location. Scoped absence needs a repository file or a
/// pinned location. Open outcomes need a checkable reference, and the
/// pending decision also needs a note that states what is being asked.
pub fn check_evidence(
    outcome: Outcome,
    summary: &EvidenceSummary,
    note: Option<&str>,
) -> anyhow::Result<()> {
    match outcome {
        Outcome::FixedAndVerified => {
            if summary.runs == 0 {
                anyhow::bail!(
                    "outcome `fixed_and_verified` needs at least one run reference shaped like `run:<run-id>/<check>` that names the stored run and the check"
                );
            }
            if summary.code == 0 {
                anyhow::bail!(
                    "outcome `fixed_and_verified` needs at least one code location shaped like `path:line` that names a file in the repository"
                );
            }
        }
        Outcome::UnaffectedInApplication => {
            if summary.code == 0 {
                anyhow::bail!(
                    "outcome `unaffected_in_application` needs at least one code location shaped like `path:line` that names a file in the repository"
                );
            }
        }
        Outcome::NoUsageFound => {
            if summary.checkable() == 0 {
                anyhow::bail!(
                    "outcome `no_usage_found` needs at least one search artefact, a repository file or a `path:line` location"
                );
            }
        }
        Outcome::DecisionRequired => {
            if summary.checkable() == 0 {
                anyhow::bail!(
                    "outcome `decision_required` needs at least one checkable evidence reference, such as a `path:line` location or a repository file"
                );
            }
            if !has_note(note) {
                anyhow::bail!(
                    "outcome `decision_required` needs a note that states the ambiguity, the options, and the decision needed"
                );
            }
        }
        Outcome::Unresolved => {
            if summary.checkable() == 0 {
                anyhow::bail!(
                    "outcome `unresolved` needs at least one checkable evidence reference, such as a `path:line` location or a repository file"
                );
            }
        }
    }
    Ok(())
}

/// One stored disposition entry.
///
/// Evidence keeps the original reference spellings in order. The note
/// carries the human judgement that the references alone cannot state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Recorded outcome for the change.
    pub outcome: Outcome,
    /// Evidence references in the order they were passed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// Optional note stored beside the outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Entry {
    /// Create a disposition entry from its parts.
    pub fn new(outcome: Outcome, evidence: Vec<String>, note: Option<String>) -> Self {
        Self {
            outcome,
            evidence,
            note,
        }
    }
}

/// Current disposition plus replaced history for one change id.
///
/// A repeated write replaces the current entry and appends the replaced
/// entry here. The history never gains a second current entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disposition {
    /// The disposition that counts now.
    pub current: Entry,
    /// Replaced dispositions, oldest first.
    #[serde(default)]
    pub history: Vec<Entry>,
}

/// Outcome ledger stored inside a migration directory.
///
/// Dispositions stay keyed by change id in insertion order. The map
/// keeps write order stable across reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Dispositions per change id in insertion order.
    pub dispositions: IndexMap<String, Disposition>,
}

impl Ledger {
    /// Create an empty ledger stamped with the shared schema version.
    pub fn empty() -> Self {
        Self {
            schema_version: crate::state::SCHEMA_VERSION,
            dispositions: IndexMap::new(),
        }
    }

    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        crate::state::check_schema_version(self.schema_version, "ledger")
    }

    /// Record one disposition for a change id.
    ///
    /// The first write creates the entry with an empty history. A
    /// repeated write replaces the current entry and appends the
    /// replaced entry to the history in the same file. A repeated write
    /// never creates a second current entry.
    pub fn record(&mut self, id: &str, entry: Entry) {
        match self.dispositions.entry(id.to_string()) {
            indexmap::map::Entry::Occupied(mut slot) => {
                let replaced = std::mem::replace(&mut slot.get_mut().current, entry);
                slot.get_mut().history.push(replaced);
            }
            indexmap::map::Entry::Vacant(slot) => {
                slot.insert(Disposition {
                    current: entry,
                    history: Vec::new(),
                });
            }
        }
    }
}

/// Load the ledger for one migration directory.
///
/// A missing file reads as an empty ledger. A stored file is parsed and
/// validated before callers trust it. Anything else fails with the file
/// named.
pub fn load_ledger(migration: &Path) -> anyhow::Result<Ledger> {
    let path = crate::state::layout::ledger_path(migration);
    if !path.is_file() {
        return Ok(Ledger::empty());
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read ledger {}", path.display()))?;
    let ledger: Ledger =
        serde_json::from_slice(&bytes).context(format!("parse ledger {}", path.display()))?;
    ledger.validate()?;
    Ok(ledger)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_for(code: usize, files: usize, runs: usize) -> EvidenceSummary {
        EvidenceSummary { code, files, runs }
    }

    #[test]
    fn outcome_words_parse_and_render() {
        for word in [
            "fixed_and_verified",
            "unaffected_in_application",
            "no_usage_found",
            "decision_required",
            "unresolved",
        ] {
            let parsed = Outcome::parse(word).unwrap();
            assert_eq!(parsed.as_word(), word);
            assert_eq!(parsed.to_string(), word);
        }
        assert!(Outcome::parse("fixed").is_err());
        assert!(Outcome::parse("").is_err());
        assert!(Outcome::parse("FIXED_AND_VERIFIED").is_err());
    }

    #[test]
    fn outcome_serialises_as_its_word() {
        let entry = Entry::new(Outcome::NoUsageFound, Vec::new(), None);
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["outcome"], "no_usage_found");
        let back: Entry = serde_json::from_value(value).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn references_resolve_by_shape_and_existence() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("search.ts"), "export {};\n").unwrap();
        std::fs::write(repo.path().join("notes.md"), "searched\n").unwrap();

        let code = check_reference(repo.path(), "search.ts:12").unwrap();
        assert_eq!(code.kind, EvidenceKind::Code);
        let file = check_reference(repo.path(), "notes.md").unwrap();
        assert_eq!(file.kind, EvidenceKind::File);
        let run = check_reference(repo.path(), "run:smoke").unwrap();
        assert_eq!(run.kind, EvidenceKind::Run);
        let prose = check_reference(repo.path(), "looks fine").unwrap();
        assert_eq!(prose.kind, EvidenceKind::Text);
        let missing_bare = check_reference(repo.path(), "absent.md").unwrap();
        assert_eq!(missing_bare.kind, EvidenceKind::Text);
        assert!(check_reference(repo.path(), "missing.ts:1").is_err());
        assert!(check_reference(repo.path(), "search.ts:0").is_err());
    }

    #[test]
    fn out_of_range_line_suffix_names_the_rejected_value() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("app.ts"), "export {};\n").unwrap();
        let suffix = "99999999999999999999999";
        let reference = format!("app.ts:{suffix}");
        let err = check_reference(repo.path(), &reference).unwrap_err();
        assert!(
            err.to_string().contains(suffix),
            "rejection names the rejected suffix, got: {err}"
        );
        assert!(err.to_string().contains("line numbers start at one"));
    }

    #[test]
    fn verified_closure_needs_a_run_and_a_code_location() {
        assert!(
            check_evidence(
                Outcome::FixedAndVerified,
                &summary_for(1, 0, 1),
                Some("note")
            )
            .is_ok()
        );
        let missing_run =
            check_evidence(Outcome::FixedAndVerified, &summary_for(1, 0, 0), None).unwrap_err();
        assert!(
            missing_run.to_string().contains("run:<run-id>/<check>"),
            "refusal names the expected shape, got: {missing_run}"
        );
        let missing_code =
            check_evidence(Outcome::FixedAndVerified, &summary_for(0, 0, 2), None).unwrap_err();
        assert!(
            missing_code.to_string().contains("path:line"),
            "refusal names the expected shape, got: {missing_code}"
        );
        assert!(check_evidence(Outcome::FixedAndVerified, &summary_for(0, 2, 0), None).is_err());
        assert!(
            check_evidence(Outcome::FixedAndVerified, &EvidenceSummary::default(), None).is_err()
        );
    }

    #[test]
    fn unaffected_needs_a_code_location() {
        assert!(
            check_evidence(
                Outcome::UnaffectedInApplication,
                &summary_for(1, 0, 0),
                None
            )
            .is_ok()
        );
        assert!(
            check_evidence(
                Outcome::UnaffectedInApplication,
                &summary_for(0, 2, 0),
                None
            )
            .is_err()
        );
        assert!(
            check_evidence(
                Outcome::UnaffectedInApplication,
                &EvidenceSummary::default(),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn absence_needs_a_search_artefact() {
        assert!(check_evidence(Outcome::NoUsageFound, &summary_for(0, 1, 0), None).is_ok());
        assert!(check_evidence(Outcome::NoUsageFound, &summary_for(1, 0, 0), None).is_ok());
        assert!(
            check_evidence(Outcome::NoUsageFound, &summary_for(0, 0, 3), None).is_err(),
            "run references alone must not satisfy the rule"
        );
        assert!(check_evidence(Outcome::NoUsageFound, &EvidenceSummary::default(), None).is_err());
    }

    #[test]
    fn pending_decision_needs_evidence_and_a_note() {
        assert!(
            check_evidence(
                Outcome::DecisionRequired,
                &summary_for(0, 1, 0),
                Some("pick one")
            )
            .is_ok()
        );
        assert!(check_evidence(Outcome::DecisionRequired, &summary_for(0, 1, 0), None).is_err());
        assert!(
            check_evidence(Outcome::DecisionRequired, &summary_for(0, 1, 0), Some("  ")).is_err()
        );
        assert!(
            check_evidence(
                Outcome::DecisionRequired,
                &EvidenceSummary::default(),
                Some("pick one")
            )
            .is_err()
        );
    }

    #[test]
    fn open_work_needs_evidence_but_no_note() {
        assert!(check_evidence(Outcome::Unresolved, &summary_for(1, 0, 0), None).is_ok());
        assert!(
            check_evidence(Outcome::Unresolved, &summary_for(1, 0, 0), Some("tried x")).is_ok()
        );
        assert!(check_evidence(Outcome::Unresolved, &EvidenceSummary::default(), None).is_err());
    }

    #[test]
    fn repeated_writes_keep_one_current_entry_with_history() {
        let mut ledger = Ledger::empty();
        ledger.validate().unwrap();
        ledger.record(
            "vc1_aaa",
            Entry::new(Outcome::Unresolved, vec!["notes.md".to_string()], None),
        );
        ledger.record(
            "vc1_aaa",
            Entry::new(
                Outcome::UnaffectedInApplication,
                vec!["search.ts:4".to_string()],
                Some("wrapper".to_string()),
            ),
        );
        ledger.record(
            "vc1_bbb",
            Entry::new(Outcome::Unresolved, vec!["notes.md".to_string()], None),
        );
        assert_eq!(ledger.dispositions.len(), 2);
        let first = &ledger.dispositions["vc1_aaa"];
        assert_eq!(first.current.outcome, Outcome::UnaffectedInApplication);
        assert_eq!(first.history.len(), 1);
        assert_eq!(first.history[0].outcome, Outcome::Unresolved);
        assert_eq!(ledger.dispositions["vc1_bbb"].history.len(), 0);

        let bytes = serde_json::to_vec_pretty(&ledger).unwrap();
        let back: Ledger = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, ledger);
    }

    #[test]
    fn dispositions_keep_insertion_order_across_round_trip() {
        let mut ledger = Ledger::empty();
        for id in ["vc1_ccc", "vc1_aaa", "vc1_bbb"] {
            ledger.record(id, Entry::new(Outcome::Unresolved, Vec::new(), None));
        }
        let order: Vec<String> = ledger.dispositions.keys().cloned().collect();
        assert_eq!(order, vec!["vc1_ccc", "vc1_aaa", "vc1_bbb"]);
        let bytes = serde_json::to_vec_pretty(&ledger).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let mut positions = Vec::new();
        for id in &order {
            positions.push(text.find(id.as_str()).unwrap());
        }
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "serialized ledger keeps insertion order, got positions {positions:?}"
        );
        let back: Ledger = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, ledger);
    }
}
