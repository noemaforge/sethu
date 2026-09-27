//! Integration tests for `sethu record`.
//!
//! Tests build a scratch consumer repository per case through `sethu
//! init` on the pinned specs, then drive `record` with the working
//! directory set to that repository. Live tests need the released diff
//! binary on PATH and skip with a named reason without it. The demo
//! matrix test also skips when `SETHU_DEMO_REPO` is unset. Under CI
//! either missing prerequisite fails the test instead. Refusals
//! must change nothing, so failing cases assert that no ledger file
//! was written. Tests never touch the real home directory.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};
use sethu::state::attempt::AttemptRecord;
use sethu::state::layout;
use sethu::state::ledger::{Ledger, Outcome};

mod common;

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
///
/// Under CI the absence fails the test instead of skipping it.
fn need_vimanam(test_name: &str) -> bool {
    if vimanam_available() {
        true
    } else {
        common::skip(test_name, "`vimanam` is not on PATH");
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

/// Build a `sethu record` invocation rooted at one repository.
fn record_cmd(repo: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(repo).arg("record").args(args);
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
    Setup {
        _repo,
        repo,
        ids,
        migration,
    }
}

/// Read the stored ledger for a setup.
fn stored_ledger(setup: &Setup) -> Ledger {
    sethu::state::read_state_file(&layout::ledger_path(&setup.migration)).unwrap()
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

#[test]
fn fixed_and_verified_is_refused_without_a_stored_run() {
    if !need_vimanam("fixed_and_verified_is_refused_without_a_stored_run") {
        return;
    }
    let setup = setup();
    let file = write_repo_file(&setup, "app/search.ts", "export {};\n");
    let location = format!("{file}:3");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "fixed_and_verified",
            "--evidence",
            "run:run-ghost/picker",
            "--evidence",
            &location,
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("no stored run"))
    .stderr(predicate::str::contains("run-ghost"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

/// Read the consumer HEAD of a setup repository.
fn head_commit(repo: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// One asset body that validates under both pinned specs.
fn stored_asset(id: &str, index: usize) -> Value {
    json!({
        "checksum": "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        "deviceAssetId": format!("device-{index}"),
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
        "originalFileName": format!("photo-{index}.jpg"),
        "originalPath": format!("/photos/photo-{index}.jpg"),
        "ownerId": "550e8400-e29b-41d4-a716-446655440001",
        "thumbhash": "3OcRJwh4d3h6eIeIh3h2e3h4gQ",
        "type": "IMAGE",
        "updatedAt": "2024-09-27T10:00:00.000Z",
        "stack": null
    })
}

/// Old-contract search response holding two assets.
fn stored_old_body() -> Value {
    let items: Vec<Value> = ["asset-r1", "asset-r2"]
        .iter()
        .enumerate()
        .map(|(index, id)| stored_asset(id, index))
        .collect();
    json!({
        "albums": {"count": 0, "facets": [], "items": [], "total": 0},
        "assets": {
            "count": items.len(),
            "facets": [],
            "items": items,
            "nextPage": null,
            "total": items.len()
        }
    })
}

/// New-contract random response holding two assets.
fn stored_new_body() -> Value {
    Value::Array(
        ["asset-r1", "asset-r2"]
            .iter()
            .enumerate()
            .map(|(index, id)| stored_asset(id, index))
            .collect(),
    )
}

/// Write scenario directories with one valid random-search fixture each.
fn write_stored_scenarios(area: &Path, old_hash: &str, new_hash: &str) -> (PathBuf, PathBuf) {
    let old = area.join("scenarios-old");
    let next = area.join("scenarios-new");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&next).unwrap();
    for (dir, hash, body) in [
        (&old, old_hash, stored_old_body()),
        (&next, new_hash, stored_new_body()),
    ] {
        let scenario = json!({
            "id": "random-search",
            "change_ids": ["vc1_demo"],
            "spec_sha256": hash,
            "request": {"method": "POST", "path": "/search/random"},
            "response": {"status": 200, "body": body}
        });
        std::fs::write(
            dir.join("random.json"),
            serde_json::to_vec_pretty(&scenario).unwrap(),
        )
        .unwrap();
    }
    (old, next)
}

/// One trace line for a served random-search exchange.
fn stored_trace_line(body: &Value) -> String {
    let bytes = serde_json::to_vec(body).unwrap();
    String::from_utf8(
        serde_json::to_vec(&json!({
            "scenario_id": "random-search",
            "method": "POST",
            "path": "/search/random",
            "request_valid": true,
            "request_detail": "body matches the contract schema",
            "response_status": 200,
            "response_body_sha256": sethu::stub::body_sha256_hex(&bytes),
            "response_written": true
        }))
        .unwrap(),
    )
    .unwrap()
}

/// Write one fabricated verified run under a migration.
///
/// The run record names one regression check covering `covered`. Every
/// stage carries a passing verdict with a clean trace, and the patched
/// stage ran on `head`.
#[allow(clippy::too_many_arguments)]
fn write_stored_run(
    migration: &Path,
    run_id: &str,
    head: &str,
    old_hash: &str,
    new_hash: &str,
    scenarios_old: &Path,
    scenarios_new: &Path,
    covered: &[String],
) {
    let run_dir = migration.join("runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    let record = json!({
        "run_id": run_id,
        "harness_hash": "abc",
        "harness_changed": false,
        "sethu_version": "0.1.0",
        "nextest_version": "nextest",
        "checks": [
            {"name": "random-picker", "role": "regression",
             "change_ids": covered, "verified": true, "stages": []}
        ]
    });
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
            stored_old_body(),
            "pass",
        ),
        (
            "original-new",
            "baseline",
            "new",
            new_hash,
            scenarios_new,
            stored_new_body(),
            "expected-red",
        ),
        (
            "patched-new",
            head,
            "new",
            new_hash,
            scenarios_new,
            stored_new_body(),
            "pass",
        ),
    ] {
        let stage_dir = run_dir.join("random-picker").join(stage);
        std::fs::create_dir_all(&stage_dir).unwrap();
        let red = verdict == "expected-red";
        let stored = json!({
            "stage": stage,
            "check": "random-picker",
            "change_ids": covered,
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
            "exit_code": if red { 101 } else { 0 },
            "signal": null,
            "timed_out": false,
            "build_failed": false,
            "cases": [
                {"name": "verify_random_picker", "passed": !red,
                 "output": if red { "assertion failed: random picker ids are wrong" } else { "" }}
            ]
        });
        std::fs::write(
            stage_dir.join("stage.json"),
            serde_json::to_vec_pretty(&stored).unwrap(),
        )
        .unwrap();
        std::fs::write(
            stage_dir.join("requests.jsonl"),
            format!("{}\n", stored_trace_line(&body)),
        )
        .unwrap();
    }
}

/// Manifest hashes of a setup migration.
fn setup_hashes(setup: &Setup) -> (String, String) {
    let manifest: AttemptRecord =
        sethu::state::read_state_file(&layout::manifest_path(&setup.migration)).unwrap();
    (manifest.old_spec_hash, manifest.new_spec_hash)
}

#[test]
fn fixed_and_verified_records_with_a_stored_run() {
    if !need_vimanam("fixed_and_verified_records_with_a_stored_run") {
        return;
    }
    let setup = setup();
    let head = head_commit(&setup.repo);
    let (old_hash, new_hash) = setup_hashes(&setup);
    let area = tempfile::tempdir().unwrap();
    let (scenarios_old, scenarios_new) = write_stored_scenarios(area.path(), &old_hash, &new_hash);
    write_stored_run(
        &setup.migration,
        "run-demo",
        &head,
        &old_hash,
        &new_hash,
        &scenarios_old,
        &scenarios_new,
        &[setup.ids[0].clone()],
    );
    let file = write_repo_file(&setup, "app/search.ts", "export {};\n");
    let location = format!("{file}:3");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "fixed_and_verified",
            "--evidence",
            "run:run-demo/random-picker",
            "--evidence",
            &location,
        ],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("recorded"))
    .stdout(predicate::str::contains("fixed_and_verified"));

    let ledger = stored_ledger(&setup);
    ledger.validate().unwrap();
    let entry = &ledger.dispositions[&setup.ids[0]];
    assert_eq!(entry.current.outcome, Outcome::FixedAndVerified);
    assert_eq!(
        entry.current.evidence,
        vec!["run:run-demo/random-picker".to_string(), location]
    );
}

#[test]
fn fixed_and_verified_is_refused_for_an_uncovered_change() {
    if !need_vimanam("fixed_and_verified_is_refused_for_an_uncovered_change") {
        return;
    }
    let setup = setup();
    let head = head_commit(&setup.repo);
    let (old_hash, new_hash) = setup_hashes(&setup);
    let area = tempfile::tempdir().unwrap();
    let (scenarios_old, scenarios_new) = write_stored_scenarios(area.path(), &old_hash, &new_hash);
    write_stored_run(
        &setup.migration,
        "run-demo",
        &head,
        &old_hash,
        &new_hash,
        &scenarios_old,
        &scenarios_new,
        &["vc1_someone_else".to_string()],
    );
    let file = write_repo_file(&setup, "app/search.ts", "export {};\n");
    let location = format!("{file}:3");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "fixed_and_verified",
            "--evidence",
            "run:run-demo/random-picker",
            "--evidence",
            &location,
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("covers"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn fixed_and_verified_is_refused_for_a_broken_exchange() {
    if !need_vimanam("fixed_and_verified_is_refused_for_a_broken_exchange") {
        return;
    }
    let setup = setup();
    let head = head_commit(&setup.repo);
    let (old_hash, new_hash) = setup_hashes(&setup);
    let area = tempfile::tempdir().unwrap();
    let (scenarios_old, scenarios_new) = write_stored_scenarios(area.path(), &old_hash, &new_hash);
    write_stored_run(
        &setup.migration,
        "run-demo",
        &head,
        &old_hash,
        &new_hash,
        &scenarios_old,
        &scenarios_new,
        &[setup.ids[0].clone()],
    );
    std::fs::write(
        setup
            .migration
            .join("runs")
            .join("run-demo")
            .join("random-picker")
            .join("patched-new")
            .join("requests.jsonl"),
        "",
    )
    .unwrap();
    let file = write_repo_file(&setup, "app/search.ts", "export {};\n");
    let location = format!("{file}:3");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "fixed_and_verified",
            "--evidence",
            "run:run-demo/random-picker",
            "--evidence",
            &location,
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains(
        "not received, validated, and answered",
    ))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn unaffected_passes_with_a_code_location() {
    if !need_vimanam("unaffected_passes_with_a_code_location") {
        return;
    }
    let setup = setup();
    let file = write_repo_file(&setup, "app/search.ts", "export {};\n");
    let location = format!("{file}:3");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unaffected_in_application",
            "--evidence",
            &location,
        ],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("recorded"))
    .stdout(predicate::str::contains("unaffected_in_application"));

    let ledger = stored_ledger(&setup);
    ledger.validate().unwrap();
    assert_eq!(ledger.dispositions.len(), 1);
    let entry = &ledger.dispositions[&setup.ids[0]];
    assert_eq!(entry.current.outcome, Outcome::UnaffectedInApplication);
    assert_eq!(entry.current.evidence, vec![location]);
    assert_eq!(entry.current.note, None);
    assert!(entry.history.is_empty());
}

#[test]
fn unaffected_refuses_an_out_of_range_line_suffix() {
    if !need_vimanam("unaffected_refuses_an_out_of_range_line_suffix") {
        return;
    }
    let setup = setup();
    write_repo_file(&setup, "app.ts", "export {};\n");
    let suffix = "99999999999999999999999";
    let location = format!("app.ts:{suffix}");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unaffected_in_application",
            "--evidence",
            &location,
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains(suffix))
    .stderr(predicate::str::contains("line numbers start at one"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn unaffected_refuses_free_text_only() {
    if !need_vimanam("unaffected_refuses_free_text_only") {
        return;
    }
    let setup = setup();

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unaffected_in_application",
            "--evidence",
            "looks fine",
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("path:line"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn unaffected_refuses_a_missing_file() {
    if !need_vimanam("unaffected_refuses_a_missing_file") {
        return;
    }
    let setup = setup();

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unaffected_in_application",
            "--evidence",
            "missing.ts:1",
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("names no file"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn unaffected_refuses_a_bare_file_without_a_line() {
    if !need_vimanam("unaffected_refuses_a_bare_file_without_a_line") {
        return;
    }
    let setup = setup();
    let notes = write_repo_file(&setup, "search-notes.md", "searched\n");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unaffected_in_application",
            "--evidence",
            &notes,
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("path:line"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn no_usage_found_passes_with_a_trace_file() {
    if !need_vimanam("no_usage_found_passes_with_a_trace_file") {
        return;
    }
    let setup = setup();
    let notes = write_repo_file(&setup, "search-notes.md", "searched wrappers\n");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[1],
            "--outcome",
            "no_usage_found",
            "--evidence",
            &notes,
        ],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("no_usage_found"));

    let ledger = stored_ledger(&setup);
    assert_eq!(
        ledger.dispositions[&setup.ids[1]].current.outcome,
        Outcome::NoUsageFound
    );
}

#[test]
fn no_usage_found_refuses_without_evidence() {
    if !need_vimanam("no_usage_found_refuses_without_evidence") {
        return;
    }
    let setup = setup();

    record_cmd(&setup.repo, &[&setup.ids[0], "--outcome", "no_usage_found"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("search artefact"))
        .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn decision_required_passes_with_evidence_and_note() {
    if !need_vimanam("decision_required_passes_with_evidence_and_note") {
        return;
    }
    let setup = setup();
    let file = write_repo_file(&setup, "app/picker.ts", "export {};\n");
    let location = format!("{file}:7");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "decision_required",
            "--evidence",
            &location,
            "--note",
            "Paging no longer applies, drop it or fetch repeated batches",
        ],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("decision_required"));

    let ledger = stored_ledger(&setup);
    let entry = &ledger.dispositions[&setup.ids[0]];
    assert_eq!(entry.current.outcome, Outcome::DecisionRequired);
    assert!(entry.current.note.as_deref().unwrap().contains("Paging"));
}

#[test]
fn decision_required_refuses_without_a_note() {
    if !need_vimanam("decision_required_refuses_without_a_note") {
        return;
    }
    let setup = setup();
    let notes = write_repo_file(&setup, "search-notes.md", "searched\n");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "decision_required",
            "--evidence",
            &notes,
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("needs a note"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn unresolved_passes_with_evidence() {
    if !need_vimanam("unresolved_passes_with_evidence") {
        return;
    }
    let setup = setup();
    let notes = write_repo_file(&setup, "triage.md", "still failing\n");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unresolved",
            "--evidence",
            &notes,
        ],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("unresolved"));

    let ledger = stored_ledger(&setup);
    assert_eq!(
        ledger.dispositions[&setup.ids[0]].current.outcome,
        Outcome::Unresolved
    );
}

#[test]
fn unresolved_refuses_without_evidence() {
    if !need_vimanam("unresolved_refuses_without_evidence") {
        return;
    }
    let setup = setup();

    record_cmd(&setup.repo, &[&setup.ids[0], "--outcome", "unresolved"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("checkable evidence"))
        .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn unknown_id_is_refused() {
    if !need_vimanam("unknown_id_is_refused") {
        return;
    }
    let setup = setup();
    let notes = write_repo_file(&setup, "triage.md", "tried\n");

    record_cmd(
        &setup.repo,
        &[
            "vc1_0000000000000000",
            "--outcome",
            "unresolved",
            "--evidence",
            &notes,
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("unknown change id"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&setup.migration).exists());
}

#[test]
fn repeated_write_keeps_one_current_entry_with_history() {
    if !need_vimanam("repeated_write_keeps_one_current_entry_with_history") {
        return;
    }
    let setup = setup();
    let notes = write_repo_file(&setup, "triage.md", "tried\n");
    let file = write_repo_file(&setup, "app/search.ts", "export {};\n");
    let location = format!("{file}:3");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unresolved",
            "--evidence",
            &notes,
        ],
    )
    .assert()
    .success();
    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unaffected_in_application",
            "--evidence",
            &location,
        ],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("unaffected_in_application"));

    let ledger = stored_ledger(&setup);
    assert_eq!(ledger.dispositions.len(), 1);
    let entry = &ledger.dispositions[&setup.ids[0]];
    assert_eq!(entry.current.outcome, Outcome::UnaffectedInApplication);
    assert_eq!(entry.current.evidence, vec![location]);
    assert_eq!(entry.history.len(), 1);
    assert_eq!(entry.history[0].outcome, Outcome::Unresolved);
    assert_eq!(entry.history[0].evidence, vec![notes]);

    let raw = std::fs::read(layout::ledger_path(&setup.migration)).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(value["dispositions"].as_object().unwrap().len(), 1);
}

#[test]
fn malformed_outcome_exits_2() {
    record_cmd(
        tempfile::tempdir().unwrap().path(),
        &["vc1_abc123", "--outcome", "bogus"],
    )
    .assert()
    .failure()
    .code(2);
}

#[test]
fn record_without_init_is_refused() {
    if !need_vimanam("record_without_init_is_refused") {
        return;
    }
    let (repo, _) = init_git_repo();

    record_cmd(repo.path(), &["vc1_abc123", "--outcome", "unresolved"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("run `init` first"))
        .stdout(predicate::str::is_empty());
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

#[test]
fn attempt_prefix_writes_into_the_named_migration() {
    if !need_vimanam("attempt_prefix_writes_into_the_named_migration") {
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

    let notes = write_repo_file(&setup, "triage.md", "tried\n");
    attempt_record_cmd(
        &setup.repo,
        &prefix,
        &[
            &setup.ids[0],
            "--outcome",
            "unresolved",
            "--evidence",
            &notes,
        ],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("recorded"))
    .stdout(predicate::str::contains("unresolved"));

    let ledger = stored_ledger(&setup);
    assert_eq!(ledger.dispositions.len(), 1);
    assert!(ledger.dispositions.contains_key(&setup.ids[0]));
    for migration in &migrations {
        if *migration != setup.migration {
            assert!(!layout::ledger_path(migration).exists());
        }
    }
}

#[test]
fn attempt_unknown_value_names_the_value() {
    let repo = bare_migrations(&["aa111111", "aa222222"]);

    attempt_record_cmd(
        repo.path(),
        "deadbeef",
        &["vc1_abc123", "--outcome", "unresolved"],
    )
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

    attempt_record_cmd(
        repo.path(),
        "aa",
        &["vc1_abc123", "--outcome", "unresolved"],
    )
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

    attempt_record_cmd(repo.path(), "", &["vc1_abc123", "--outcome", "unresolved"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("non-empty"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn several_attempts_refuse_without_a_choice() {
    if !need_vimanam("several_attempts_refuse_without_a_choice") {
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

    let notes = write_repo_file(&setup, "triage.md", "tried\n");
    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "unresolved",
            "--evidence",
            &notes,
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("needs exactly one"))
    .stdout(predicate::str::is_empty());
}

/// Run one git command in a directory and keep stdout on success.
fn demo_git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Replace one source block exactly once, commit, and return the commit.
fn commit_edit(repo: &Path, file: &str, old: &str, new: &str, message: &str) -> String {
    let path = repo.join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text.matches(old).count(),
        1,
        "expected one match for the patch anchor in {file}"
    );
    std::fs::write(&path, text.replace(old, new)).unwrap();
    demo_git(repo, &["add", "."]);
    demo_git(
        repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
    demo_git(repo, &["rev-parse", "HEAD"]).trim().to_string()
}

/// Correct repair: random search reads the new array, others keep the parser.
const E2E_PATCH_OLD: &str = r#"    /// Run a random search and return one page of assets.
    pub fn search_random(&self, page: u64) -> Result<SearchResponse, Error> {
        self.post("/search/random", &PageBody { page })
    }"#;

/// Correct repair: random search reads the new array, others keep the parser.
const E2E_PATCH_NEW: &str = r#"    /// Run a random search and return one page of assets.
    pub fn search_random(&self, page: u64) -> Result<SearchResponse, Error> {
        let url = format!("{}/search/random", self.base_url);
        let response = self
            .http
            .post(&url)
            .json(&PageBody { page })
            .send()
            .map_err(Error::Http)?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        let text = response.text().map_err(Error::Http)?;
        if let Ok(items) = serde_json::from_str::<Vec<Asset>>(&text) {
            return Ok(SearchResponse {
                albums: AlbumResults::default(),
                assets: AssetResults {
                    count: items.len() as u64,
                    items,
                },
            });
        }
        parse_search_response(&text)
    }"#;

/// Harness test driving the picker through the stub.
const E2E_HARNESS_TESTS: &str = r#"
use demo_picker::{ImmichClient, Picker};

#[test]
fn verify_random_picker() {
    let client = ImmichClient::from_env().expect("read stub address from environment");
    let mut picker = Picker::new();
    match client.search_random(1) {
        Ok(response) => picker.add_response(&response),
        Err(_) => {}
    }
    let got: Vec<String> = picker.selection().to_vec();
    assert_eq!(
        got,
        vec!["asset-r1".to_string(), "asset-r2".to_string()],
        "random picker ids"
    );
}
"#;

/// Build a `sethu` invocation with an isolated cargo target directory.
fn e2e_sethu(target: &Path) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.env("CARGO_TARGET_DIR", target);
    cmd
}

/// Change ids of the random-search endpoint in capture order.
fn random_ids(changes: &[sethu::vimanam::ChangeRecord]) -> Vec<String> {
    changes
        .iter()
        .filter(|item| item.endpoint.method == "POST" && item.endpoint.path == "/search/random")
        .map(|item| item.id.clone())
        .collect()
}

#[test]
fn demo_matrix_run_records_random_search_repairs() {
    let Some(demo) = common::demo_repo("demo_matrix_run_records_random_search_repairs") else {
        return;
    };
    if !need_vimanam("demo_matrix_run_records_random_search_repairs") {
        return;
    }
    let _clone = demo.checkout();
    let repo = _clone.path().join("consumer").canonicalize().unwrap();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    let target = tempfile::tempdir().unwrap();
    e2e_sethu(target.path())
        .current_dir(&repo)
        .arg("init")
        .arg(&old)
        .arg(&new)
        .arg("--repo")
        .arg(&repo)
        .assert()
        .success();

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
    let random = random_ids(&document.changes);
    assert_eq!(
        random.len(),
        5,
        "expected five random-search records, got {random:?}"
    );
    let required: Vec<String> = document
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
    assert_eq!(required.len(), 27);

    let baseline = demo_git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let patched = commit_edit(
        &repo,
        "src/lib.rs",
        E2E_PATCH_OLD,
        E2E_PATCH_NEW,
        "give random search its own array path",
    );

    let harness = migration.join("harness");
    std::fs::create_dir_all(harness.join("tests")).unwrap();
    std::fs::create_dir_all(harness.join("scenarios").join("old")).unwrap();
    std::fs::create_dir_all(harness.join("scenarios").join("new")).unwrap();
    std::fs::write(
        harness.join("tests").join("harness_checks.rs"),
        E2E_HARNESS_TESTS,
    )
    .unwrap();
    let scenario = |id: &str, sha: &str, request: Value, response: Value| {
        json!({
            "id": id,
            "change_ids": ["vc1_demo"],
            "spec_sha256": sha,
            "request": request,
            "response": {"status": 200, "body": response}
        })
    };
    let post = |path: &str, body: Option<Value>| {
        let mut request = json!({"method": "POST", "path": path});
        if let Some(body) = body {
            request["body"] = body;
        }
        request
    };
    std::fs::write(
        harness.join("scenarios").join("old").join("random.json"),
        serde_json::to_vec_pretty(&scenario(
            "random-search",
            &manifest.old_spec_hash,
            post("/search/random", Some(json!({"page": 1}))),
            stored_old_body(),
        ))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        harness.join("scenarios").join("new").join("random.json"),
        serde_json::to_vec_pretty(&scenario(
            "random-search",
            &manifest.new_spec_hash,
            post("/search/random", None),
            stored_new_body(),
        ))
        .unwrap(),
    )
    .unwrap();
    let manifest_path = migration.join("verify-manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "repo": repo.display().to_string(),
            "baseline_commit": baseline,
            "patched_commit": patched,
            "harness": "harness",
            "scenarios_old": "harness/scenarios/old",
            "scenarios_new": "harness/scenarios/new",
            "spec_old_sha256": manifest.old_spec_hash,
            "spec_new_sha256": manifest.new_spec_hash,
            "checks": [
                {
                    "name": "random-picker",
                    "role": "regression",
                    "change_ids": random,
                    "test": "verify_random_picker",
                    "expected_diagnostic": "random picker ids",
                    "expected_exchange": [
                        {"scenario": "random-search", "method": "POST", "path": "/search/random"}
                    ]
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    e2e_sethu(target.path())
        .args(["verify", "--freeze", "--manifest"])
        .arg(&manifest_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("frozen"));

    e2e_sethu(target.path())
        .args(["verify", "--manifest"])
        .arg(&manifest_path)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("random-picker: verified"));

    let runs = migration.join("runs");
    let run_names: Vec<String> = std::fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(run_names.len(), 1, "expected one run in {}", runs.display());
    let run_ref = format!("run:{}/random-picker", run_names[0]);

    let others: Vec<String> = required
        .iter()
        .filter(|id| !random.contains(id))
        .cloned()
        .collect();
    assert!(!others.is_empty());
    record_cmd(
        &repo,
        &[
            &others[0],
            "--outcome",
            "fixed_and_verified",
            "--evidence",
            &run_ref,
            "--evidence",
            "src/lib.rs:1",
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("covers"))
    .stdout(predicate::str::is_empty());
    record_cmd(
        &repo,
        &[
            &others[1],
            "--outcome",
            "fixed_and_verified",
            "--evidence",
            "run:run-ghost/random-picker",
            "--evidence",
            "src/lib.rs:1",
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("no stored run"))
    .stdout(predicate::str::is_empty());
    assert!(!layout::ledger_path(&migration).exists());

    for id in &random {
        record_cmd(
            &repo,
            &[
                id,
                "--outcome",
                "fixed_and_verified",
                "--evidence",
                &run_ref,
                "--evidence",
                "src/lib.rs:1",
            ],
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("fixed_and_verified"));
    }
    std::fs::write(repo.join("search-notes.md"), "searched wrappers\n").unwrap();
    for id in &others {
        record_cmd(
            &repo,
            &[
                id,
                "--outcome",
                "no_usage_found",
                "--evidence",
                "search-notes.md",
            ],
        )
        .assert()
        .success();
    }

    let mut check = Command::cargo_bin("sethu").unwrap();
    check
        .current_dir(&repo)
        .arg("check")
        .assert()
        .success()
        .code(0)
        .stdout(
            predicate::str::contains("accounted: yes").and(predicate::str::contains("ready: yes")),
        );
    let mut ready = Command::cargo_bin("sethu").unwrap();
    ready
        .current_dir(&repo)
        .arg("check")
        .arg("--require-ready")
        .assert()
        .success()
        .code(0);

    let ledger: Ledger = sethu::state::read_state_file(&layout::ledger_path(&migration)).unwrap();
    assert_eq!(ledger.dispositions.len(), 27);
    for id in &random {
        let entry = &ledger.dispositions[id];
        assert_eq!(entry.current.outcome, Outcome::FixedAndVerified);
        assert!(entry.current.evidence.contains(&run_ref));
    }
}
