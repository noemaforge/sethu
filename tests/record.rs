//! Integration tests for `sethu record`.
//!
//! Tests build a scratch consumer repository per case through `sethu
//! init` on the pinned specs, then drive `record` with the working
//! directory set to that repository. Live tests need the released diff
//! binary on PATH and skip with a named reason without it. Refusals
//! must change nothing, so failing cases assert that no ledger file
//! was written. Tests never touch the real home directory.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use sethu::commands::record::ledger::{Ledger, Outcome};
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
fn fixed_and_verified_is_refused_without_a_runner() {
    if !need_vimanam("fixed_and_verified_is_refused_without_a_runner") {
        return;
    }
    let setup = setup();
    let notes = write_repo_file(&setup, "verify-notes.md", "checked\n");

    record_cmd(
        &setup.repo,
        &[
            &setup.ids[0],
            "--outcome",
            "fixed_and_verified",
            "--evidence",
            &format!("run:smoke-{notes}"),
            "--evidence",
            &notes,
            "--note",
            "patched the picker",
        ],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("verification runner"))
    .stderr(predicate::str::contains("has not landed"))
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
