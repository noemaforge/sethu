//! Walk a spec repository tag history and rank candidate upgrade pairs.
//!
//! The walk lists release tags in version order, reads one spec revision
//! per tag with `git show`, and diffs adjacent revisions through the
//! contract diff adapter. Nothing is ever checked out. Each diffed pair
//! gains a heuristic score from its change kinds and its shared origins,
//! so the most instructive upgrade stands first. A score never claims that
//! a consumer is affected. Missing specs, moved specs, and parse failures
//! are reported by tag. They are never skipped quietly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use indexmap::IndexMap;

use crate::provenance::{Origin, compute_origins};
use crate::vimanam::{ChangeKind, ChangeOperation, ChangeTarget, DiffDocument};

/// Counter that keeps concurrent scan temp directories apart.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Inputs for one tag history walk.
#[derive(Debug, Clone)]
pub struct ScanRequest {
    /// Repository to walk. Every git spawn runs with this directory.
    pub repo: PathBuf,
    /// Spec path inside the repository, or a glob such as `specs/*.json`.
    /// Only `*` and `?` act as wildcards. Other characters match literally.
    pub spec: String,
    /// Tag pattern handed to `git tag --list`. `*` walks every tag.
    pub pattern: String,
}

/// Resolution of one tag to spec bytes.
#[derive(Debug, Clone)]
pub enum SpecState {
    /// The spec was read at this path with this SHA-256 hex hash.
    Found {
        /// Repository relative path that supplied the bytes.
        path: String,
        /// Lowercase hex SHA-256 of the raw bytes.
        hash: String,
        /// Raw spec bytes exactly as `git show` printed them.
        bytes: Vec<u8>,
    },
    /// No spec matched at this tag. `moved_to` names same file names
    /// found elsewhere at the tag in sorted order. It stays empty when
    /// nothing with the same name exists.
    Missing {
        /// Same file names found elsewhere at the tag.
        moved_to: Vec<String>,
    },
    /// The bytes at this path failed JSON and YAML parsing. The error
    /// is folded to one line.
    Unparseable {
        /// Repository relative path that failed to parse.
        path: String,
        /// Lowercase hex SHA-256 of the raw bytes.
        hash: String,
        /// One line parse error.
        error: String,
    },
    /// A glob matched several files at this tag. The tag stays out of
    /// every pair diff until the glob names one file.
    Ambiguous {
        /// Sorted repository relative paths that matched the glob.
        matches: Vec<String>,
    },
}

/// One resolved spec revision for one tag in version order.
#[derive(Debug, Clone)]
pub struct TagSpec {
    /// Tag name as listed by git.
    pub tag: String,
    /// Full commit hash the tag points at.
    pub commit: String,
    /// How the spec pattern resolved at this tag.
    pub state: SpecState,
}

/// One diffed adjacent tag pair with its heuristic score.
#[derive(Debug, Clone)]
pub struct CandidatePair {
    /// One based rank among diffed pairs. Lower numbers stand first.
    pub rank: usize,
    /// Older tag of the pair.
    pub old_tag: String,
    /// Newer tag of the pair.
    pub new_tag: String,
    /// Commit the older tag points at.
    pub old_commit: String,
    /// Commit the newer tag points at.
    pub new_commit: String,
    /// Repository relative spec path diffed on both sides.
    pub spec_path: String,
    /// SHA-256 hex of the older spec bytes.
    pub old_hash: String,
    /// SHA-256 hex of the newer spec bytes.
    pub new_hash: String,
    /// Generator version the diff document reports.
    pub tool_version: String,
    /// Changes classed as breaking.
    pub breaking: usize,
    /// Changes classed as needing review.
    pub review: usize,
    /// Changes classed as non breaking.
    pub non_breaking: usize,
    /// Heuristic demo value score. Higher stands first.
    pub score: u64,
    /// Human reason for the score, built from the counted signals.
    pub rationale: String,
}

/// One adjacent tag pair that could not be diffed.
#[derive(Debug, Clone)]
pub struct PairFailure {
    /// Older tag of the pair.
    pub old_tag: String,
    /// Newer tag of the pair.
    pub new_tag: String,
    /// One line reason the pair stayed out of the ranking.
    pub reason: String,
}

/// Full result of one tag history walk in deterministic order.
#[derive(Debug, Clone)]
pub struct ScanReport {
    /// Repository the walk read, as requested.
    pub repo: PathBuf,
    /// Spec path or glob the walk resolved per tag.
    pub spec: String,
    /// Tag pattern the walk listed.
    pub pattern: String,
    /// Version the diff binary probe reported.
    pub tool_version: String,
    /// One entry per listed tag in version order.
    pub specs: Vec<TagSpec>,
    /// Diffed pairs in rank order.
    pub pairs: Vec<CandidatePair>,
    /// Undiffed adjacent pairs in tag order.
    pub failures: Vec<PairFailure>,
}

/// Run one tag history walk and rank its diffed pairs.
///
/// Tags sort in version order. Adjacent tags with a readable spec on both
/// sides diff through the contract diff adapter. Every other adjacent
/// pair becomes a failure entry that names its cause. Missing specs and
/// parse failures appear by tag. Output order is fully deterministic.
pub fn run_scan(request: &ScanRequest) -> anyhow::Result<ScanReport> {
    if !request.repo.is_dir() {
        anyhow::bail!("cannot scan {}: not a directory", request.repo.display());
    }
    let probed = crate::vimanam::probe_vimanam(&request.repo).with_context(|| {
        format!(
            "probe the diff binary before scanning {}",
            request.repo.display()
        )
    })?;
    let mut tags = list_tags(&request.repo, &request.pattern)?;
    if tags.is_empty() {
        anyhow::bail!(
            "no tags match pattern {:?} in {}",
            request.pattern,
            request.repo.display()
        );
    }
    sort_tags_version_order(&mut tags);
    let mut specs = Vec::with_capacity(tags.len());
    for tag in &tags {
        let commit = tag_commit(&request.repo, tag)?;
        let state = resolve_spec(&request.repo, tag, &request.spec)?;
        specs.push(TagSpec {
            tag: tag.clone(),
            commit,
            state,
        });
    }
    let mut pairs = Vec::new();
    let mut failures = Vec::new();
    for window in specs.windows(2) {
        let (old, new) = (&window[0], &window[1]);
        match diff_specs(&request.repo, old, new)? {
            PairOutcome::Candidate(pair) => pairs.push(pair),
            PairOutcome::Failure(failure) => failures.push(failure),
        }
    }
    rank_pairs(&mut pairs);
    Ok(ScanReport {
        repo: request.repo.clone(),
        spec: request.spec.clone(),
        pattern: request.pattern.clone(),
        tool_version: probed.to_string(),
        specs,
        pairs,
        failures,
    })
}

/// Render the report as deterministic human lines.
///
/// Candidates print in rank order. Failures, missing specs, and parse
/// failures print in tag order. The closing notes state that the ranking
/// is a heuristic and that a candidate proves no consumer impact.
pub fn render_report(report: &ScanReport) -> Vec<String> {
    let mut lines = vec![
        format!("spec {} in {}", report.spec, report.repo.display()),
        format!("tool vimanam {}", report.tool_version),
        format!(
            "tags in version order: {}",
            report
                .specs
                .iter()
                .map(|entry| entry.tag.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ];
    let total = report.pairs.len();
    for pair in &report.pairs {
        lines.push(format!(
            "candidate {} of {total}: {} -> {} score {}",
            pair.rank, pair.old_tag, pair.new_tag, pair.score
        ));
        lines.push(format!(
            "  old commit {} spec {} hash {}",
            pair.old_commit, pair.spec_path, pair.old_hash
        ));
        lines.push(format!(
            "  new commit {} spec {} hash {}",
            pair.new_commit, pair.spec_path, pair.new_hash
        ));
        lines.push(format!(
            "  changes {} (breaking {}, review {}, non-breaking {})",
            pair.breaking + pair.review + pair.non_breaking,
            pair.breaking,
            pair.review,
            pair.non_breaking
        ));
        lines.push(format!("  rationale: {}", pair.rationale));
    }
    if total == 0 {
        lines.push("no adjacent pair carried a readable spec on both sides".to_string());
    }
    for failure in &report.failures {
        lines.push(format!(
            "pair {} -> {} not diffed: {}",
            failure.old_tag, failure.new_tag, failure.reason
        ));
    }
    for entry in &report.specs {
        match &entry.state {
            SpecState::Found { .. } | SpecState::Ambiguous { .. } => {}
            SpecState::Missing { moved_to } => {
                if moved_to.is_empty() {
                    lines.push(format!(
                        "missing {} ({}): spec {} not present at tag",
                        entry.tag, entry.commit, report.spec
                    ));
                } else {
                    lines.push(format!(
                        "missing {} ({}): spec {} not present at tag (same name found at {})",
                        entry.tag,
                        entry.commit,
                        report.spec,
                        moved_to.join(", ")
                    ));
                }
            }
            SpecState::Unparseable { path, error, .. } => {
                lines.push(format!(
                    "unparseable {} ({}): spec {path} failed to parse: {error}",
                    entry.tag, entry.commit
                ));
            }
        }
        if let SpecState::Ambiguous { matches } = &entry.state {
            lines.push(format!(
                "ambiguous {} ({}): pattern {} matched several specs: {}",
                entry.tag,
                entry.commit,
                report.spec,
                matches.join(", ")
            ));
        }
    }
    lines.push(
        "note: ranking is a heuristic. The biggest diff is not automatically the best demo."
            .to_string(),
    );
    lines.push("note: a candidate proves no real consumer impact.".to_string());
    lines
}

/// Result of attempting one adjacent pair.
enum PairOutcome {
    /// Both sides diffed and scored.
    Candidate(CandidatePair),
    /// The pair stayed out of the ranking with a reason.
    Failure(PairFailure),
}

/// Run one git command and demand success.
///
/// Every spawn sets the repository as its working directory and captures
/// both streams. A failure carries the trimmed stderr text.
fn git_stdout(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("run git {} in {}", args.join(" "), repo.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), fold_line(&detail));
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("read git {} output as text", args.join(" ")))
}

/// Run one git command that may fail and return its stdout.
///
/// The caller decides what a failure means. Both streams stay captured
/// and the working directory is always the repository.
fn git_show(repo: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("run git {} in {}", args.join(" "), repo.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), fold_line(&detail));
    }
    Ok(output.stdout)
}

/// List tags matching the pattern in git order.
///
/// The pattern passes straight to `git tag --list`, so its wildcards
/// follow git rules. Version ordering happens in a later step.
fn list_tags(repo: &Path, pattern: &str) -> anyhow::Result<Vec<String>> {
    let text = git_stdout(repo, &["tag", "--list", pattern])
        .with_context(|| format!("list tags matching {pattern:?} in {}", repo.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Read the commit one tag points at.
fn tag_commit(repo: &Path, tag: &str) -> anyhow::Result<String> {
    let text = git_stdout(repo, &["rev-parse", &format!("{tag}^{{commit}}")])
        .with_context(|| format!("read commit for tag {tag:?}"))?;
    Ok(text.trim().to_string())
}

/// List every file at one tag in sorted order.
fn files_at_tag(repo: &Path, tag: &str) -> anyhow::Result<Vec<String>> {
    let bytes = git_show(repo, &["ls-tree", "-r", "--name-only", tag])
        .with_context(|| format!("list files at tag {tag:?}"))?;
    let text = String::from_utf8(bytes)
        .with_context(|| format!("read file list at tag {tag:?} as text"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Resolve the spec pattern to bytes at one tag.
///
/// A literal path reads through `git show`. A pattern with `*` or `?`
/// matches against the tag file list. A literal path that is absent
/// reports same named files elsewhere, so a moved spec shows its new
/// location instead of vanishing quietly.
fn resolve_spec(repo: &Path, tag: &str, spec: &str) -> anyhow::Result<SpecState> {
    if has_glob_chars(spec) {
        let files = files_at_tag(repo, tag)?;
        let mut matches: Vec<String> = files
            .into_iter()
            .filter(|name| match_glob(spec, name))
            .collect();
        matches.sort();
        matches.dedup();
        match matches.len() {
            0 => Ok(SpecState::Missing {
                moved_to: Vec::new(),
            }),
            1 => {
                let path = matches.remove(0);
                let bytes = git_show(repo, &["show", &format!("{tag}:{path}")])
                    .with_context(|| format!("read spec {path:?} at tag {tag:?}"))?;
                Ok(classify_bytes(&path, bytes))
            }
            _ => Ok(SpecState::Ambiguous { matches }),
        }
    } else {
        match git_show(repo, &["show", &format!("{tag}:{spec}")]) {
            Ok(bytes) => Ok(classify_bytes(spec, bytes)),
            Err(_) => {
                let moved_to = files_at_tag(repo, tag).map(|files| {
                    let base = spec.rsplit('/').next().unwrap_or(spec);
                    let mut found: Vec<String> = files
                        .into_iter()
                        .filter(|name| name.rsplit('/').next() == Some(base))
                        .filter(|name| name != spec)
                        .collect();
                    found.sort();
                    found.dedup();
                    found
                });
                match moved_to {
                    Ok(moved_to) => Ok(SpecState::Missing { moved_to }),
                    Err(first) => Err(first.context(format!("read spec {spec:?} at tag {tag:?}"))),
                }
            }
        }
    }
}

/// Hash parsed bytes and mark unparseable content.
///
/// Parsing accepts JSON and falls back to YAML. The error folds to one
/// line so reports stay deterministic and readable.
fn classify_bytes(path: &str, bytes: Vec<u8>) -> SpecState {
    let hash = crate::state::layout::sha256_hex(&bytes);
    match parse_spec_bytes(&bytes) {
        Ok(_) => SpecState::Found {
            path: path.to_string(),
            hash,
            bytes,
        },
        Err(error) => SpecState::Unparseable {
            path: path.to_string(),
            hash,
            error: fold_line(&format!("{error:#}")),
        },
    }
}

/// Parse raw spec bytes as JSON, falling back to YAML.
fn parse_spec_bytes(bytes: &[u8]) -> anyhow::Result<serde_json::Value> {
    match serde_json::from_slice(bytes) {
        Ok(value) => Ok(value),
        Err(json_err) => {
            serde_norway::from_slice(bytes).with_context(|| format!("parse spec bytes: {json_err}"))
        }
    }
}

/// Fold text to one line for single line report fields.
fn fold_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Report whether a spec pattern uses wildcard characters.
fn has_glob_chars(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

/// Match text against a glob with `*` and `?` wildcards.
///
/// `*` crosses directory separators. Matching is byte by byte over
/// characters and stays deterministic.
pub fn match_glob(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    glob_match(&pattern, &text)
}

/// Recursive glob matcher behind the public wrapper.
fn glob_match(pattern: &[char], text: &[char]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }
    if pattern[0] == '*' {
        let mut rest = pattern;
        while rest.first() == Some(&'*') {
            rest = &rest[1..];
        }
        if rest.is_empty() {
            return true;
        }
        for split in 0..=text.len() {
            if glob_match(rest, &text[split..]) {
                return true;
            }
        }
        return false;
    }
    if text.is_empty() {
        return false;
    }
    if pattern[0] == '?' || pattern[0] == text[0] {
        return glob_match(&pattern[1..], &text[1..]);
    }
    false
}

/// Sort tags in place in version order.
///
/// A leading `v` falls away before parsing. Numeric segments compare by
/// value and the rest compares as text. Tags without any version shape
/// sort lexically before versioned tags. The tag text breaks every tie,
/// so the order is total and deterministic.
pub fn sort_tags_version_order(tags: &mut [String]) {
    tags.sort_by(|first, second| {
        version_key(first)
            .cmp(&version_key(second))
            .then_with(|| first.cmp(second))
    });
}

/// Build the sort key of one tag.
fn version_key(tag: &str) -> (u8, Vec<(u64, String)>, String) {
    let body = tag.strip_prefix('v').or_else(|| tag.strip_prefix('V'));
    let Some(body) = body else {
        let plain = tag.to_string();
        if is_version_body(tag) {
            return (1, version_parts(tag), plain);
        }
        return (0, Vec::new(), plain);
    };
    if is_version_body(body) {
        (1, version_parts(body), tag.to_string())
    } else {
        (0, Vec::new(), tag.to_string())
    }
}

/// Report whether a tag body starts with a digit.
fn is_version_body(body: &str) -> bool {
    body.chars()
        .next()
        .is_some_and(|head| head.is_ascii_digit())
}

/// Split a version body into numeric and text segment pairs.
fn version_parts(body: &str) -> Vec<(u64, String)> {
    body.split('.')
        .map(|segment| {
            let digits: String = segment
                .chars()
                .take_while(|head| head.is_ascii_digit())
                .collect();
            let number = digits.parse::<u64>().unwrap_or(0);
            let rest = segment[digits.len()..].to_string();
            (number, rest)
        })
        .collect()
}

/// Diff one adjacent tag pair when both sides carry readable specs.
///
/// Pairs with a missing, ambiguous, or unparseable side become failures
/// that name the cause. Diff tool failures become failures that carry
/// the tool diagnostic. Nothing here writes to the scanned repository.
fn diff_specs(repo: &Path, old: &TagSpec, new: &TagSpec) -> anyhow::Result<PairOutcome> {
    let failure = |reason: String| {
        PairOutcome::Failure(PairFailure {
            old_tag: old.tag.clone(),
            new_tag: new.tag.clone(),
            reason,
        })
    };
    let (old_path, old_bytes) = match &old.state {
        SpecState::Found { path, bytes, .. } => (path.clone(), bytes.clone()),
        SpecState::Missing { .. } => {
            return Ok(failure(format!("spec missing at {}", old.tag)));
        }
        SpecState::Unparseable { path, error, .. } => {
            return Ok(failure(format!(
                "spec {path} at {} failed to parse: {error}",
                old.tag
            )));
        }
        SpecState::Ambiguous { .. } => {
            return Ok(failure(format!(
                "spec pattern matched several files at {}",
                old.tag
            )));
        }
    };
    let (new_path, new_bytes) = match &new.state {
        SpecState::Found { path, bytes, .. } => (path.clone(), bytes.clone()),
        SpecState::Missing { .. } => {
            return Ok(failure(format!("spec missing at {}", new.tag)));
        }
        SpecState::Unparseable { path, error, .. } => {
            return Ok(failure(format!(
                "spec {path} at {} failed to parse: {error}",
                new.tag
            )));
        }
        SpecState::Ambiguous { .. } => {
            return Ok(failure(format!(
                "spec pattern matched several files at {}",
                new.tag
            )));
        }
    };
    if old_path != new_path {
        return Ok(failure(format!(
            "spec path moved from {old_path} to {new_path}"
        )));
    }
    let old_value = parse_spec_bytes(&old_bytes)
        .with_context(|| format!("parse resolved spec at {}", old.tag))?;
    let new_value = parse_spec_bytes(&new_bytes)
        .with_context(|| format!("parse resolved spec at {}", new.tag))?;
    let scratch = scratch_dir()?;
    let cleanup = || {
        let _ = std::fs::remove_dir_all(&scratch);
    };
    let outcome = run_pair_diff(
        repo, &scratch, old, new, &old_path, &old_bytes, &new_bytes, &old_value, &new_value,
    );
    cleanup();
    match outcome {
        Ok(pair) => Ok(PairOutcome::Candidate(pair)),
        Err(error) => {
            let reason = fold_line(&format!("{error:#}"));
            Ok(failure(format!("diff failed: {reason}")))
        }
    }
}

/// Create a fresh scratch directory for one pair diff.
fn scratch_dir() -> anyhow::Result<PathBuf> {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("sethu-scan-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create scratch directory {}", dir.display()))?;
    Ok(dir)
}

/// Write both specs to scratch files, diff them, and score the pair.
///
/// The diff runs through the shared adapter, which is the only code
/// that invokes the diff binary. Shared origins come from the provenance
/// module. Pointer-only groupings stay candidate correlations and never
/// count as fan-out.
#[allow(clippy::too_many_arguments)]
fn run_pair_diff(
    repo: &Path,
    scratch: &Path,
    old: &TagSpec,
    new: &TagSpec,
    spec_path: &str,
    old_bytes: &[u8],
    new_bytes: &[u8],
    old_value: &serde_json::Value,
    new_value: &serde_json::Value,
) -> anyhow::Result<CandidatePair> {
    let old_file = scratch.join("old.json");
    let new_file = scratch.join("new.json");
    std::fs::write(&old_file, old_bytes)
        .with_context(|| format!("write scratch spec {}", old_file.display()))?;
    std::fs::write(&new_file, new_bytes)
        .with_context(|| format!("write scratch spec {}", new_file.display()))?;
    let document = crate::vimanam::run_diff(&old_file, &new_file, false, repo)
        .with_context(|| format!("diff {} with {}", old.tag, new.tag))?;
    let origins = compute_origins(old_value, new_value, &document.changes);
    let signals = score_document(&document, &origins.origins);
    Ok(CandidatePair {
        rank: 0,
        old_tag: old.tag.clone(),
        new_tag: new.tag.clone(),
        old_commit: old.commit.clone(),
        new_commit: new.commit.clone(),
        spec_path: spec_path.to_string(),
        old_hash: crate::state::layout::sha256_hex(old_bytes),
        new_hash: crate::state::layout::sha256_hex(new_bytes),
        tool_version: document.generator.version.clone(),
        breaking: document.summary.breaking,
        review: document.summary.review,
        non_breaking: document.summary.non_breaking,
        score: signals.score,
        rationale: signals.rationale,
    })
}

/// Counted demo value signals of one diff.
struct ScoredSignals {
    /// Heuristic score built from the counted signals.
    score: u64,
    /// Human reason for the score.
    rationale: String,
}

/// Score one diff from its change kinds and shared origins.
///
/// Removed endpoints weigh most, then response shape changes, removed
/// properties, and newly required fields. Type changes elsewhere add a
/// smaller weight without double counting response records. Shared
/// origin fan-out adds one bonus per operation past the first behind a
/// changed component. Pointer-only groupings are counted as candidate
/// correlations and never count as fan-out.
fn score_document(document: &DiffDocument, origins: &IndexMap<String, Origin>) -> ScoredSignals {
    let mut endpoints_removed = 0usize;
    let mut response_shape = 0usize;
    let mut removed_properties = 0usize;
    let mut newly_required = 0usize;
    let mut type_changes = 0usize;
    for record in &document.changes {
        if record.kind == ChangeKind::EndpointRemoved {
            endpoints_removed += 1;
        }
        if record.kind == ChangeKind::ResponseSchemaChanged {
            response_shape += 1;
        }
        if let Some(change) = record.details.schema_change.as_ref() {
            if change.target == ChangeTarget::Property
                && change.operation == ChangeOperation::Removed
            {
                removed_properties += 1;
            }
            if change.target == ChangeTarget::RequiredMember
                && change.operation == ChangeOperation::Added
            {
                newly_required += 1;
            }
            if change.target == ChangeTarget::Type
                && record.kind != ChangeKind::ResponseSchemaChanged
            {
                type_changes += 1;
            }
        }
    }
    let fan_out = largest_fan_out(document, origins);
    let correlations = count_candidate_correlations(document, origins);
    let mut score = 10 * endpoints_removed as u64
        + 5 * response_shape as u64
        + 4 * removed_properties as u64
        + 4 * newly_required as u64
        + 2 * type_changes as u64;
    let mut parts = Vec::new();
    if endpoints_removed > 0 {
        parts.push(plural(
            endpoints_removed,
            "removed endpoint",
            "removed endpoints",
        ));
    }
    if response_shape > 0 {
        parts.push(plural(
            response_shape,
            "response shape change",
            "response shape changes",
        ));
    }
    if removed_properties > 0 {
        parts.push(plural(
            removed_properties,
            "removed property",
            "removed properties",
        ));
    }
    if newly_required > 0 {
        parts.push(plural(
            newly_required,
            "newly required field",
            "newly required fields",
        ));
    }
    if type_changes > 0 {
        parts.push(plural(type_changes, "type change", "type changes"));
    }
    if let Some((name, operations)) = &fan_out
        && *operations >= 2
    {
        score += 3 * (*operations as u64 - 1);
        parts.push(format!("fan-out {operations} via {name}"));
    }
    if correlations > 0 {
        parts.push(plural(
            correlations,
            "candidate correlation",
            "candidate correlations",
        ));
    }
    if parts.is_empty() {
        parts.push("no ranked signals".to_string());
    }
    ScoredSignals {
        score,
        rationale: parts.join(", "),
    }
}

/// Count helper for rationale words.
fn plural(count: usize, singular: &str, plural_word: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural_word}")
    }
}

/// Find the largest shared component fan-out in one diff.
///
/// Fan-out means one changed component reaching N operations. The
/// answer names the component and its distinct operation count. Ties
/// resolve to the smallest component name. Operation origins and
/// unknown origins never count.
fn largest_fan_out(
    document: &DiffDocument,
    origins: &IndexMap<String, Origin>,
) -> Option<(String, usize)> {
    let mut members: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for record in &document.changes {
        if let Some(Origin::Component { name }) = origins.get(&record.id) {
            members
                .entry(name.clone())
                .or_default()
                .insert((record.endpoint.method.clone(), record.endpoint.path.clone()));
        }
    }
    let mut best: Option<(String, usize)> = None;
    for (name, operations) in &members {
        let count = operations.len();
        let better = match &best {
            None => true,
            Some((_, held)) => count > *held,
        };
        if better {
            best = Some((name.clone(), count));
        }
    }
    best
}

/// Count pointer-only groupings that span several operations.
///
/// Records with an unknown origin that share one schema pointer read as
/// candidate correlations. They never count as fan-out, since one
/// pointer can come from unrelated components.
fn count_candidate_correlations(
    document: &DiffDocument,
    origins: &IndexMap<String, Origin>,
) -> usize {
    let mut groups: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for record in &document.changes {
        let unknown = origins
            .get(&record.id)
            .is_none_or(|origin| *origin == Origin::Unknown);
        if !unknown {
            continue;
        }
        if let Some(change) = record.details.schema_change.as_ref() {
            groups
                .entry(change.pointer.clone())
                .or_default()
                .insert((record.endpoint.method.clone(), record.endpoint.path.clone()));
        }
    }
    groups.values().filter(|members| members.len() >= 2).count()
}

/// Order diffed pairs by score and stamp ranks.
///
/// Higher scores stand first. Ties resolve by tag order, with the older
/// tag first and then the newer tag, so reruns agree byte for byte.
fn rank_pairs(pairs: &mut [CandidatePair]) {
    let mut order: Vec<usize> = (0..pairs.len()).collect();
    order.sort_by(|&first, &second| {
        pairs[second]
            .score
            .cmp(&pairs[first].score)
            .then_with(|| pairs[first].old_tag.cmp(&pairs[second].old_tag))
            .then_with(|| pairs[first].new_tag.cmp(&pairs[second].new_tag))
    });
    let mut ranked: Vec<CandidatePair> = order
        .into_iter()
        .enumerate()
        .map(|(rank, index)| {
            let mut pair = pairs[index].clone();
            pair.rank = rank + 1;
            pair
        })
        .collect();
    ranked.sort_by_key(|pair| pair.rank);
    pairs.clone_from_slice(&ranked);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_order_sorts_release_tags_numerically() {
        let mut tags = vec![
            "v1.10.0".to_string(),
            "v1.2.0".to_string(),
            "latest".to_string(),
            "v1.2.0".to_string(),
            "v2.0.0".to_string(),
            "v1.2.10".to_string(),
        ];
        sort_tags_version_order(&mut tags);
        assert_eq!(
            tags,
            vec!["latest", "v1.2.0", "v1.2.0", "v1.2.10", "v1.10.0", "v2.0.0",]
        );
    }

    #[test]
    fn version_order_accepts_bare_numbers() {
        let mut tags = vec!["2.0".to_string(), "10.0".to_string(), "1.9.3".to_string()];
        sort_tags_version_order(&mut tags);
        assert_eq!(tags, vec!["1.9.3", "2.0", "10.0"]);
    }

    #[test]
    fn glob_match_supports_stars_and_questions() {
        assert!(match_glob("*", "anything/at/all.json"));
        assert!(match_glob("specs/*.json", "specs/api.json"));
        assert!(match_glob("specs/*.json", "specs/nested/api.json"));
        assert!(!match_glob("specs/*.json", "specs/api.yaml"));
        assert!(match_glob("api.???", "api.yml"));
        assert!(!match_glob("api.???", "api.json5"));
        assert!(match_glob("openapi.yaml", "openapi.yaml"));
        assert!(!match_glob("openapi.yaml", "openapi.yml"));
    }

    #[test]
    fn folded_lines_hold_no_whitespace_runs() {
        assert_eq!(
            fold_line("git show failed:\n  fatal: bad tag"),
            "git show failed: fatal: bad tag"
        );
    }
}
