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
use std::path::Path;

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
fn parse_ledger(
    bytes: &[u8],
    repo: &Path,
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
            Ok(entry) => check_entry(id, entry, repo, problems, dispositions),
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
/// line numbers fail here. Verified claims always fail until a verification
/// runner exists that can back them with a passing run.
fn check_entry(
    id: &str,
    entry: RawEntry,
    repo: &Path,
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

    let ready = matches!(
        outcome,
        ledger::Outcome::UnaffectedInApplication | ledger::Outcome::NoUsageFound
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
    fn verified_claims_are_stale_without_a_runner() {
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
}
