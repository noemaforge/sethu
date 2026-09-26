//! Checks for `sethu report`.
//!
//! Tests build a scratch consumer repository per case through `sethu
//! init` on the pinned specs, fill ledgers through `sethu record` or by
//! hand, then drive `report` with the working directory set to that
//! repository. Live tests need the released diff binary on PATH and skip
//! with a named reason without it. Crafted ledgers and run artefacts
//! prove that incomplete migrations still list every required change and
//! that injected markup renders as plain text in both listings.

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
fn init_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
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
    dir
}

/// One migration with its change ids and migration directory.
struct Setup {
    /// Guard for the scratch consumer repository.
    _repo: tempfile::TempDir,
    /// Canonical repository path used for state lookups.
    repo: PathBuf,
    /// Required change ids (breaking plus review) in report order.
    required: Vec<String>,
    /// Migration directory holding the manifest and the ledger.
    migration: PathBuf,
}

/// Initialise a scratch repository and read back its required ids.
fn setup() -> Setup {
    setup_with_scope(&[])
}

/// Initialise a scratch repository with extra init flags.
fn setup_with_scope(extra: &[&str]) -> Setup {
    let dir = init_git_repo();
    let repo = dir.path().canonicalize().unwrap();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    let mut init = Command::cargo_bin("sethu").unwrap();
    init.current_dir(&repo)
        .arg("init")
        .arg(&old)
        .arg(&new)
        .arg("--repo")
        .arg(&repo)
        .args(extra);
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
        _repo: dir,
        repo,
        required,
        migration,
    }
}

/// Write one repository file to back an evidence reference.
fn write_repo_file(setup: &Setup, name: &str, body: &str) {
    let path = setup.repo.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();
}

/// Record one outcome through the CLI and assert success.
fn record(setup: &Setup, id: &str, args: &[&str]) {
    let mut full = vec![id, "--outcome"];
    full.extend(args);
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(&setup.repo).arg("record").args(&full);
    cmd.assert().success();
}

/// Build a `sethu report` invocation rooted at one repository.
fn report_cmd(repo: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(repo).arg("report").args(args);
    cmd
}

#[test]
fn incomplete_ledger_lists_every_id_with_missing_marked() {
    if !need_vimanam("incomplete_ledger_lists_every_id_with_missing_marked") {
        return;
    }
    let setup = setup();
    assert_eq!(setup.required.len(), 27);
    write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    for id in setup.required.iter().take(3) {
        record(
            &setup,
            id,
            &["no_usage_found", "--evidence", "trace-notes.md"],
        );
    }

    let out = setup.repo.join("listing");
    report_cmd(&setup.repo, &["--format", "md", "--out"])
        .arg(&out)
        .assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("accounted: no"));

    let markdown = std::fs::read_to_string(out.join("report.md")).unwrap();
    for id in &setup.required {
        assert!(markdown.contains(id), "listing omits required change {id}");
    }
    assert!(markdown.contains("missing"));
    assert!(markdown.contains("Required changes"));
    assert!(markdown.contains("Limitations"));
    assert!(!out.join("report.html").exists());
}

#[test]
fn default_run_writes_both_listings_into_the_migration() {
    if !need_vimanam("default_run_writes_both_listings_into_the_migration") {
        return;
    }
    let setup = setup();
    write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    for id in &setup.required {
        let id = id.clone();
        record(
            &setup,
            &id,
            &["no_usage_found", "--evidence", "trace-notes.md"],
        );
    }

    report_cmd(&setup.repo, &[])
        .assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("accounted: yes"))
        .stdout(predicate::str::contains("ready: yes"));

    let markdown = std::fs::read_to_string(setup.migration.join("reports/report.md")).unwrap();
    let html = std::fs::read_to_string(setup.migration.join("reports/report.html")).unwrap();
    for id in &setup.required {
        assert!(markdown.contains(id), "markdown omits required change {id}");
        assert!(html.contains(id), "html omits required change {id}");
    }
    assert!(markdown.contains("Nullable idiom applications"));
    assert!(html.contains("Nullable idiom applications"));
}

#[test]
fn injection_in_note_and_evidence_renders_as_text() {
    if !need_vimanam("injection_in_note_and_evidence_renders_as_text") {
        return;
    }
    let setup = setup();
    write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    let first = setup.required[0].clone();
    record(
        &setup,
        &first,
        &[
            "unresolved",
            "--evidence",
            "trace-notes.md",
            "--evidence",
            "<img src=x onerror=alert(1)>",
            "--note",
            "<script>alert(1)</script> [evil](http://example.com)",
        ],
    );

    let out = setup.repo.join("listing");
    report_cmd(&setup.repo, &["--format", "both", "--out"])
        .arg(&out)
        .assert()
        .success();

    let markdown = std::fs::read_to_string(out.join("report.md")).unwrap();
    assert!(markdown.contains("`<script>alert(1)</script>"));
    assert!(markdown.contains("[evil](http://example.com)`"));
    assert!(markdown.contains("`<img src=x onerror=alert(1)>`"));

    let html = std::fs::read_to_string(out.join("report.html")).unwrap();
    assert!(!html.contains("<script>"));
    assert!(!html.contains("<img "));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&lt;img "));
}

#[test]
fn injection_in_change_record_renders_as_text() {
    if !need_vimanam("injection_in_change_record_renders_as_text") {
        return;
    }
    let setup = setup();
    write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    let injected = "<script>alert(\"change\")</script>";
    graft_change_record(&setup, injected);
    record(
        &setup,
        injected,
        &["unresolved", "--evidence", "trace-notes.md"],
    );

    let out = setup.repo.join("listing");
    report_cmd(&setup.repo, &["--format", "both", "--out"])
        .arg(&out)
        .assert()
        .success();

    let markdown = std::fs::read_to_string(out.join("report.md")).unwrap();
    assert!(markdown.contains("`<script>alert(\"change\")</script>`"));
    let html = std::fs::read_to_string(out.join("report.html")).unwrap();
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains(injected));
}

/// Clone the first stored change record under an injected id.
///
/// The graft keeps identity hashes intact and only extends the change
/// list, so the listing must render the injected id as plain text.
fn graft_change_record(setup: &Setup, injected: &str) {
    let manifest: AttemptRecord =
        sethu::state::read_state_file(&layout::manifest_path(&setup.migration)).unwrap();
    let root = layout::state_root(&setup.repo);
    let pair = layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash).unwrap();
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    let path = layout::changes_file(&capture);
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let first = document["changes"].as_array().unwrap()[0].clone();
    let mut record = first;
    record["id"] = serde_json::Value::String(injected.to_string());
    document["changes"].as_array_mut().unwrap().push(record);
    let breaking = document["summary"]["breaking"].as_u64().unwrap();
    document["summary"]["breaking"] = serde_json::Value::from(breaking + 1);
    std::fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
}

#[test]
fn crafted_run_artefact_links_evidence_and_marks_missing_runs() {
    if !need_vimanam("crafted_run_artefact_links_evidence_and_marks_missing_runs") {
        return;
    }
    let setup = setup();
    write_repo_file(&setup, "trace-notes.md", "searched wrappers\n");
    let run_id = craft_run(&setup);
    let cited = setup.required[0].clone();
    record(
        &setup,
        &cited,
        &[
            "unresolved",
            "--evidence",
            "trace-notes.md",
            "--evidence",
            &format!("run:{run_id}"),
        ],
    );
    let missing = setup.required[1].clone();
    record(
        &setup,
        &missing,
        &[
            "unresolved",
            "--evidence",
            "trace-notes.md",
            "--evidence",
            "run:does-not-exist",
        ],
    );

    let out = setup.repo.join("listing");
    report_cmd(&setup.repo, &["--format", "md", "--out"])
        .arg(&out)
        .assert()
        .success();

    let markdown = std::fs::read_to_string(out.join("report.md")).unwrap();
    assert!(markdown.contains(&run_id));
    assert!(markdown.contains("missing run artefact"));
    assert!(markdown.contains("Verification runs"));
}

/// Store one minimal run record with a stage and a trace under the migration.
fn craft_run(setup: &Setup) -> String {
    let run_id = "run9c0ffee".to_string();
    let dir = setup.migration.join("runs").join(&run_id);
    let stage = dir.join("picker").join("original-new");
    std::fs::create_dir_all(&stage).unwrap();
    let record = serde_json::json!({
        "run_id": run_id,
        "harness_hash": "abc",
        "harness_changed": false,
        "sethu_version": "0.1.0",
        "nextest_version": "nextest",
        "checks": [
            {"name": "picker", "role": "regression",
             "change_ids": [setup.required[0].clone()], "verified": true,
             "stages": ["pass"]}
        ]
    });
    std::fs::write(
        dir.join("run.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    std::fs::write(
        stage.join("stage.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "check": "picker", "stage": "original-new",
            "verdict": "pass", "detail": "", "parser": "nextest-junit"
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(stage.join("requests.jsonl"), "{\"scenario_id\":\"demo\"}\n").unwrap();
    run_id
}

#[test]
fn attempt_selector_picks_one_migration() {
    if !need_vimanam("attempt_selector_picks_one_migration") {
        return;
    }
    let dir = init_git_repo();
    let repo = dir.path().canonicalize().unwrap();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");
    for scope in [["--scope", "src"], ["--scope", "other"]] {
        let mut init = Command::cargo_bin("sethu").unwrap();
        init.current_dir(&repo)
            .arg("init")
            .arg(&old)
            .arg(&new)
            .arg("--repo")
            .arg(&repo)
            .args(scope);
        init.assert().success();
    }
    let root = layout::state_root(&repo);
    let mut names: Vec<String> = std::fs::read_dir(layout::migrations_dir(&root))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2);

    report_cmd(&repo, &[])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs exactly one"));

    let prefix = &names[0][..12];
    let mut selected = Command::cargo_bin("sethu").unwrap();
    selected
        .current_dir(&repo)
        .arg("--attempt")
        .arg(prefix)
        .arg("report")
        .arg("--format")
        .arg("md");
    selected.assert().success().code(0);
    assert!(
        layout::migration_dir(&root, &names[0])
            .join("reports/report.md")
            .is_file()
    );
    assert!(
        !layout::migration_dir(&root, &names[1])
            .join("reports/report.md")
            .is_file()
    );
}
