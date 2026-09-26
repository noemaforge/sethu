//! Integration tests for `sethu check`.
//!
//! Tests build a scratch consumer repository per case through `sethu
//! init` on the pinned specs, fill the ledger through `sethu record`,
//! then drive `check` with the working directory set to that repository.
//! Live tests need the released diff binary on PATH and skip with a named
//! reason without it. Hand edits to stored files go through JSON reads
//! and rewrites. Tests never touch the real home directory.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use sethu::state::attempt::AttemptRecord;
use sethu::state::layout;

/// Locate a checked in fixture by path under the crate root.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Report whether the released diff binary answers on PATH.
fn vimanam_available() -> bool {
    std::process::Command::new("vimanam")
        .arg("--version")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Skip the calling test with a named reason when the binary is absent.
fn need_vimanam(test_name: &str) -> bool {
    if vimanam_available() {
        true
    } else {
        eprintln!("SKIP {test_name}: `vimanam` is not on PATH");
        false
    }
}

/// Create a scratch consumer repository with one commit.
///
/// Returns the directory guard and the baseline commit hash.
fn init_git_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| -> std::process::Output {
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
    run(&["init", "-q"]);
    std::fs::write(dir.path().join("README.md"), "consumer\n").unwrap();
    run(&["add", "."]);
    run(&[
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=Test",
        "commit",
        "-q",
        "-m",
        "baseline",
    ]);
    let output = run(&["rev-parse", "HEAD"]);
    let commit = String::from_utf8(output.stdout).unwrap();
    (dir, commit.trim().to_string())
}

/// Build a `sethu check` invocation rooted at one repository.
fn check_cmd(repo: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(repo).arg("check").args(args);
    cmd
}

/// One ready migration with its change ids and migration directory.
struct Setup {
    /// Guard for the scratch consumer repository.
    _repo: tempfile::TempDir,
    /// Canonical repository path used for state lookups.
    repo: PathBuf,
    /// Change ids from the attempt capture in report order.
    ids: Vec<String>,
    /// Required change ids (breaking plus review) in report order.
    required: Vec<String>,
    /// Migration directory holding the manifest and the ledger.
    migration: PathBuf,
}

/// Initialise a scratch repository and read back its change ids.
fn setup() -> Setup {
    let (_repo, _) = init_git_repo();
    let repo = _repo.path().canonicalize().unwrap();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    let mut init = Command::cargo_bin("sethu").unwrap();
    init.current_dir(&repo)
        .arg("init")
        .arg(&old)
        .arg(&new)
        .arg("--repo")
        .arg(&repo);
    init.assert().success();

    let root = layout::state_root(&repo);
    let names: Vec<String> = std::fs::read_dir(layout::migrations_dir(&root))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 1);
    let migration = layout::migration_dir(&root, &names[0]);
    let manifest: AttemptRecord =
        sethu::state::read_state_file(&layout::manifest_path(&migration)).unwrap();
    let pair = layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash).unwrap();
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    let raw = std::fs::read(layout::changes_file(&capture)).unwrap();
    let document = sethu::vimanam::parse_diff_output(&raw).unwrap();
    assert!(!document.changes.is_empty());
    let ids = document
        .changes
        .iter()
        .map(|item| item.id.clone())
        .collect();
    let required = document
        .changes
        .iter()
        .filter(|item| {
            matches!(
                item.severity,
                sethu::vimanam::Severity::Breaking | sethu::vimanam::Severity::Review
            )
        })
        .map(|item| item.id.clone())
        .collect();
    Setup {
        _repo,
        repo,
        ids,
        required,
        migration,
    }
}

/// Write one repository file to back an evidence reference.
fn write_repo_file(setup: &Setup, name: &str, body: &str) -> String {
    let path = setup.repo.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();
    name.to_string()
}

/// Record one outcome through the CLI and assert success.
fn record(setup: &Setup, id: &str, args: &[&str]) {
    let mut full = vec![id, "--outcome"];
    full.extend(args);
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(&setup.repo).arg("record").args(&full);
    cmd.assert().success();
}

/// Fill a complete ledger with one pending decision.
///
/// The first change records a pending decision with a note. Every other
/// change records scoped absence backed by the same trace file.
fn fill_complete_ledger(setup: &Setup, trace: &str) {
    let first = setup.ids[0].clone();
    record(
        setup,
        &first,
        &[
            "decision_required",
            "--evidence",
            trace,
            "--note",
            "Paging no longer applies, drop it or fetch repeated batches",
        ],
    );
    for id in setup.ids.iter().skip(1) {
        let id = id.clone();
        record(setup, &id, &["no_usage_found", "--evidence", trace]);
    }
}

/// Read the stored ledger as a generic JSON value.
fn ledger_value(setup: &Setup) -> serde_json::Value {
    let raw = std::fs::read(layout::ledger_path(&setup.migration)).unwrap();
    serde_json::from_slice(&raw).unwrap()
}

/// Rewrite the stored ledger from a generic JSON value.
fn write_ledger_value(setup: &Setup, value: &serde_json::Value) {
    let bytes = serde_json::to_vec_pretty(value).unwrap();
    std::fs::write(layout::ledger_path(&setup.migration), bytes).unwrap();
}

#[test]
fn complete_ledger_with_pending_decision_splits_exits() {
    if !need_vimanam("complete_ledger_with_pending_decision_splits_exits") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    check_cmd(&setup.repo, &[])
        .assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("accounted: yes"))
        .stdout(predicate::str::contains("ready: no"));

    check_cmd(&setup.repo, &["--require-ready"])
        .assert()
        .failure()
        .code(5)
        .stdout(predicate::str::contains("accounted: yes"))
        .stdout(predicate::str::contains("ready: no"));
}

/// Record resolved dispositions for exactly the required changes.
fn fill_required_ledger(setup: &Setup, trace: &str) {
    for id in &setup.required {
        let id = id.clone();
        record(setup, &id, &["no_usage_found", "--evidence", trace]);
    }
}

#[test]
fn required_only_ledger_exits_0_with_27_required() {
    if !need_vimanam("required_only_ledger_exits_0_with_27_required") {
        return;
    }
    let setup = setup();
    assert_eq!(setup.required.len(), 27);
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_required_ledger(&setup, &trace);

    check_cmd(&setup.repo, &[])
        .assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("accounted: yes"))
        .stdout(predicate::str::contains("ready: yes"))
        .stdout(predicate::str::contains("required 27"));

    check_cmd(&setup.repo, &["--require-ready"])
        .assert()
        .success()
        .code(0);

    let assert = check_cmd(&setup.repo, &["--json"])
        .assert()
        .success()
        .code(0);
    let output = assert.get_output();
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["required"].as_array().unwrap().len(), 27);
    assert_eq!(report["problems"].as_array().unwrap().len(), 0);
}

#[test]
fn required_only_ledger_with_blocker_splits_exits() {
    if !need_vimanam("required_only_ledger_with_blocker_splits_exits") {
        return;
    }
    let setup = setup();
    assert_eq!(setup.required.len(), 27);
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    let first = setup.required[0].clone();
    record(
        &setup,
        &first,
        &[
            "decision_required",
            "--evidence",
            &trace,
            "--note",
            "Paging no longer applies, drop it or fetch repeated batches",
        ],
    );
    for id in setup.required.iter().skip(1) {
        let id = id.clone();
        record(&setup, &id, &["no_usage_found", "--evidence", &trace]);
    }

    check_cmd(&setup.repo, &[])
        .assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("accounted: yes"))
        .stdout(predicate::str::contains("ready: no"));

    check_cmd(&setup.repo, &["--require-ready"])
        .assert()
        .failure()
        .code(5)
        .stdout(predicate::str::contains("accounted: yes"))
        .stdout(predicate::str::contains("ready: no"));
}

#[test]
fn voluntary_non_breaking_entry_causes_no_problem() {
    if !need_vimanam("voluntary_non_breaking_entry_causes_no_problem") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_required_ledger(&setup, &trace);

    let extra = setup
        .ids
        .iter()
        .find(|id| !setup.required.contains(id))
        .unwrap()
        .clone();
    record(&setup, &extra, &["no_usage_found", "--evidence", &trace]);

    check_cmd(&setup.repo, &[])
        .assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("accounted: yes"))
        .stdout(predicate::str::contains("required 27"));
}

#[test]
fn hand_deleted_entry_exits_4() {
    if !need_vimanam("hand_deleted_entry_exits_4") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    let removed = setup.ids[1].clone();
    let mut value = ledger_value(&setup);
    value["dispositions"]
        .as_object_mut()
        .unwrap()
        .shift_remove(&removed);
    write_ledger_value(&setup, &value);

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("missing"))
        .stdout(predicate::str::contains(&removed));
}

#[test]
fn unknown_entry_exits_4() {
    if !need_vimanam("unknown_entry_exits_4") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    let mut value = ledger_value(&setup);
    value["dispositions"]["vc1_0000000000000000"] = serde_json::json!({
        "current": {"outcome": "unresolved", "evidence": [trace]}
    });
    write_ledger_value(&setup, &value);

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("unknown"))
        .stdout(predicate::str::contains("vc1_0000000000000000"));
}

#[test]
fn duplicate_entry_exits_4() {
    if !need_vimanam("duplicate_entry_exits_4") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    duplicate_first_disposition(&setup);

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("duplicate"))
        .stdout(predicate::str::contains(&setup.ids[0]));
}

/// Rewrite the ledger with its first disposition key repeated twice.
///
/// Parsed maps drop repeats, so the duplication is done on raw text with
/// a string aware brace matcher. The checker scans the same raw bytes.
fn duplicate_first_disposition(setup: &Setup) {
    let path = layout::ledger_path(&setup.migration);
    let text = std::fs::read_to_string(&path).unwrap();
    let anchor = text.find("\"dispositions\"").unwrap();
    let open = text[anchor..].find('{').unwrap() + anchor;
    let mut pos = open + 1;
    pos = skip_json_whitespace(&text, pos);
    assert_eq!(text[pos..].chars().next().unwrap(), '"');
    let (key, after_key) = read_json_string(&text, pos);
    let colon = text[after_key..].find(':').unwrap() + after_key;
    let mut value_pos = skip_json_whitespace(&text, colon + 1);
    let end = match_json_value(&text, value_pos);
    let entry_text = text[value_pos..end].to_string();
    value_pos = end;
    let mut duplicated = text.clone();
    duplicated.insert_str(end, &format!(",\n    \"{key}\": {entry_text}"));
    std::fs::write(&path, duplicated).unwrap();
    let _ = value_pos;
}

/// Skip whitespace in JSON text and return the next offset.
fn skip_json_whitespace(text: &str, mut pos: usize) -> usize {
    while text[pos..].starts_with([' ', '\t', '\n', '\r']) {
        pos += 1;
    }
    pos
}

/// Read one JSON string starting at its opening quote.
///
/// Returns the decoded value and the offset past the closing quote.
fn read_json_string(text: &str, pos: usize) -> (String, usize) {
    let bytes = text.as_bytes();
    assert_eq!(bytes[pos], b'"');
    let mut out = String::new();
    let mut i = pos + 1;
    while bytes[i] != b'"' {
        if bytes[i] == b'\\' {
            out.push('\\');
            out.push(bytes[i + 1] as char);
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    (out, i + 1)
}

/// Find the offset past one JSON value starting at its first byte.
///
/// The matcher skips over strings, so braces inside text never close
/// the value early.
fn match_json_value(text: &str, pos: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = pos;
    let mut depth = 0_usize;
    let mut in_string = false;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_string {
            if byte == b'\\' {
                i += 2;
                continue;
            }
            if byte == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = true;
                i += 1;
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
        }
    }
    i
}

#[test]
fn malformed_outcome_exits_4() {
    if !need_vimanam("malformed_outcome_exits_4") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    let target = setup.ids[0].clone();
    let mut value = ledger_value(&setup);
    value["dispositions"][&target]["current"]["outcome"] =
        serde_json::Value::String("bogus".to_string());
    write_ledger_value(&setup, &value);

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("malformed_outcome"))
        .stdout(predicate::str::contains(&target));
}

#[test]
fn missing_evidence_exits_4() {
    if !need_vimanam("missing_evidence_exits_4") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    let target = setup.ids[2].clone();
    let mut value = ledger_value(&setup);
    value["dispositions"][&target]["current"]["evidence"] = serde_json::json!([]);
    write_ledger_value(&setup, &value);

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("missing_evidence"))
        .stdout(predicate::str::contains(&target));
}

#[test]
fn deleted_evidence_file_exits_4() {
    if !need_vimanam("deleted_evidence_file_exits_4") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    let target = setup.ids[0].clone();
    record(&setup, &target, &["unresolved", "--evidence", &trace]);
    std::fs::remove_file(setup.repo.join(&trace)).unwrap();

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("missing_evidence"))
        .stdout(predicate::str::contains(&target));
}

#[test]
fn identity_mismatch_exits_4() {
    if !need_vimanam("identity_mismatch_exits_4") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    let manifest: AttemptRecord =
        sethu::state::read_state_file(&layout::manifest_path(&setup.migration)).unwrap();
    let pair = layout::pair_dir(
        &layout::state_root(&setup.repo),
        &manifest.old_spec_hash,
        &manifest.new_spec_hash,
    )
    .unwrap();
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    let path = layout::changes_file(&capture);
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let old_hash = document["old"]["file_sha256"].as_str().unwrap().to_string();
    let mut tampered = old_hash.clone();
    tampered.replace_range(0..1, if old_hash.starts_with('a') { "b" } else { "a" });
    document["old"]["file_sha256"] = serde_json::Value::String(tampered);
    std::fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("identity_mismatch"));
}

#[test]
fn verified_claim_without_a_runner_exits_4() {
    if !need_vimanam("verified_claim_without_a_runner_exits_4") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    let target = setup.ids[3].clone();
    let mut value = ledger_value(&setup);
    value["dispositions"][&target]["current"]["outcome"] =
        serde_json::Value::String("fixed_and_verified".to_string());
    write_ledger_value(&setup, &value);

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("stale_verification"))
        .stdout(predicate::str::contains(&target));
}

#[test]
fn empty_ledger_exits_4() {
    if !need_vimanam("empty_ledger_exits_4") {
        return;
    }
    let setup = setup();

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("missing"))
        .stdout(predicate::str::contains("accounted: no"));
}

#[test]
fn corrupt_ledger_exits_4() {
    if !need_vimanam("corrupt_ledger_exits_4") {
        return;
    }
    let setup = setup();
    std::fs::write(layout::ledger_path(&setup.migration), b"{ not json").unwrap();

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("malformed_ledger"));
}

#[test]
fn json_report_carries_accounting_and_readiness() {
    if !need_vimanam("json_report_carries_accounting_and_readiness") {
        return;
    }
    let setup = setup();
    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger(&setup, &trace);

    let assert = check_cmd(&setup.repo, &["--json"])
        .assert()
        .success()
        .code(0);
    let output = assert.get_output();
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["accounted"], serde_json::Value::Bool(true));
    assert_eq!(report["ready"], serde_json::Value::Bool(false));
    assert_eq!(
        report["required"].as_array().unwrap().len(),
        setup.required.len()
    );
    assert_eq!(report["problems"].as_array().unwrap().len(), 0);
    assert_eq!(report["not_ready"].as_array().unwrap().len(), 1);
}

#[test]
fn check_without_init_exits_1() {
    let (repo, _) = init_git_repo();

    check_cmd(repo.path(), &[])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("run `init` first"));
}

#[test]
fn several_attempts_exit_1() {
    if !need_vimanam("several_attempts_exit_1") {
        return;
    }
    let setup = setup();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");
    let mut second = Command::cargo_bin("sethu").unwrap();
    second
        .current_dir(&setup.repo)
        .arg("init")
        .arg(&old)
        .arg(&new)
        .arg("--repo")
        .arg(&setup.repo)
        .arg("--scope")
        .arg("src");
    second.assert().success();

    check_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("needs exactly one"));
}

/// Build a `sethu --attempt <selector> check` invocation in one repository.
fn attempt_check_cmd(repo: &Path, selector: &str, args: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(repo)
        .arg("--attempt")
        .arg(selector)
        .arg("check")
        .args(args);
    cmd
}

/// Build a `sethu --attempt <selector> record` invocation in one repository.
fn attempt_record_cmd(repo: &Path, selector: &str, args: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(repo)
        .arg("--attempt")
        .arg(selector)
        .arg("record")
        .args(args);
    cmd
}

/// State root with bare migration directories and no manifests.
///
/// Resolution fails before any manifest read, so these directories pin
/// the unknown and ambiguous refusal paths without running init.
fn bare_migrations(names: &[&str]) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let root = layout::state_root(repo.path());
    for name in names {
        std::fs::create_dir_all(layout::migration_dir(&root, name)).unwrap();
    }
    repo
}

/// Initialise a second attempt over the same pair and list every migration.
fn setup_two(setup: &Setup) -> Vec<PathBuf> {
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");
    let mut second = Command::cargo_bin("sethu").unwrap();
    second
        .current_dir(&setup.repo)
        .arg("init")
        .arg(&old)
        .arg(&new)
        .arg("--repo")
        .arg(&setup.repo)
        .arg("--scope")
        .arg("src");
    second.assert().success();
    let mut migrations: Vec<PathBuf> =
        std::fs::read_dir(layout::migrations_dir(&layout::state_root(&setup.repo)))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
    migrations.sort();
    assert_eq!(migrations.len(), 2);
    migrations
}

/// Shortest leading slice of `target` that no other name shares.
fn unique_prefix(target: &str, others: &[String]) -> String {
    for len in 1..=target.len() {
        let prefix = &target[..len];
        if others.iter().all(|name| !name.starts_with(prefix)) {
            return prefix.to_string();
        }
    }
    target.to_string()
}

/// Fill a complete ledger with one pending decision through one selector.
///
/// The first change records a pending decision with a note. Every other
/// change records scoped absence backed by the same trace file. Only the
/// selected migration gains a ledger.
fn fill_complete_ledger_with_attempt(setup: &Setup, selector: &str, trace: &str) {
    let first = setup.ids[0].clone();
    attempt_record_cmd(
        &setup.repo,
        selector,
        &[
            &first,
            "--outcome",
            "decision_required",
            "--evidence",
            trace,
            "--note",
            "Paging no longer applies, drop it or fetch repeated batches",
        ],
    )
    .assert()
    .success();
    for id in setup.ids.iter().skip(1) {
        let id = id.clone();
        attempt_record_cmd(
            &setup.repo,
            selector,
            &[&id, "--outcome", "no_usage_found", "--evidence", trace],
        )
        .assert()
        .success();
    }
}

#[test]
fn attempt_prefix_evaluates_the_named_migration() {
    if !need_vimanam("attempt_prefix_evaluates_the_named_migration") {
        return;
    }
    let setup = setup();
    let migrations = setup_two(&setup);
    let target = setup
        .migration
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let others: Vec<String> = migrations
        .iter()
        .filter(|path| *path != &setup.migration)
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(others.len(), 1);
    let prefix = unique_prefix(&target, &others);
    let other_prefix = unique_prefix(&others[0], std::slice::from_ref(&target));

    let trace = write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    fill_complete_ledger_with_attempt(&setup, &prefix, &trace);

    attempt_check_cmd(&setup.repo, &prefix, &[])
        .assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("accounted: yes"))
        .stdout(predicate::str::contains("ready: no"));

    attempt_check_cmd(&setup.repo, &other_prefix, &[])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("accounted: no"));
}

#[test]
fn attempt_unknown_value_names_the_value() {
    let repo = bare_migrations(&["aa111111", "aa222222"]);

    attempt_check_cmd(repo.path(), "deadbeef", &[])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unknown attempt"))
        .stderr(predicate::str::contains("deadbeef"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn attempt_ambiguous_prefix_names_the_candidates() {
    let repo = bare_migrations(&["aa111111", "aa222222"]);

    attempt_check_cmd(repo.path(), "aa", &[])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("ambiguous"))
        .stderr(predicate::str::contains("aa111111"))
        .stderr(predicate::str::contains("aa222222"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn attempt_empty_value_is_a_usage_error() {
    let repo = bare_migrations(&["aa111111"]);

    attempt_check_cmd(repo.path(), "", &[])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("non-empty"))
        .stdout(predicate::str::is_empty());
}
