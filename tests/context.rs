//! Integration tests for `sethu context`.
//!
//! Tests build scratch state through `sethu init` on the checked in Immich
//! specs, then read context for one change. Live tests need the released
//! diff binary on PATH and skip with a named reason without it. The gate
//! provides the binary. One test uses small synthetic specs to cover an
//! added endpoint.

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

/// Run `sethu init` on two specs inside one consumer repository.
fn init_pair(old: &Path, new: &Path, repo: &Path) {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.arg("init").arg(old).arg(new).arg("--repo").arg(repo);
    cmd.assert().success();
}

/// Read the attempt manifest, capture directory, and typed changes.
fn load_attempt(repo: &Path) -> (PathBuf, PathBuf, Vec<sethu::vimanam::ChangeRecord>) {
    let root = layout::state_root(&repo.canonicalize().unwrap());
    let dir = layout::migrations_dir(&root);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 1, "expected one attempt");
    let migration = dir.join(&names[0]);
    let manifest: AttemptRecord =
        sethu::state::read_state_file(&layout::manifest_path(&migration)).unwrap();
    let pair = layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash).unwrap();
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    let raw = std::fs::read(layout::changes_file(&capture)).unwrap();
    let document = sethu::vimanam::parse_diff_output(&raw).unwrap();
    (migration, capture, document.changes)
}

/// Run `sethu context` with the repository as working directory.
fn context_cmd(repo: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.arg("context").args(args).current_dir(repo);
    cmd
}

/// Find the first response change id for one operation.
fn response_id_for(changes: &[sethu::vimanam::ChangeRecord], method: &str, path: &str) -> String {
    changes
        .iter()
        .find(|record| {
            record.endpoint.method == method
                && record.endpoint.path == path
                && matches!(
                    record.kind,
                    sethu::vimanam::ChangeKind::ResponseSchemaChanged
                )
        })
        .unwrap()
        .id
        .clone()
}

#[test]
fn random_search_context_lists_neighbours_as_related_only() {
    if !need_vimanam("random_search_context_lists_neighbours_as_related_only") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    init_pair(
        &fixture("immich/old.json"),
        &fixture("immich/new.json"),
        repo,
    );
    let (_, _, changes) = load_attempt(repo);
    let id = response_id_for(&changes, "POST", "/search/random");

    let output = context_cmd(repo, &[&id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains(&id));
    assert!(stdout.contains("POST /search/random"));
    assert!(stdout.contains("SearchResponseDto"));

    let (shown, related) = stdout
        .split_once("## related operations")
        .expect("output names the related section");
    assert!(
        !shown.contains("/search/metadata"),
        "renderings and fragments must not include the metadata endpoint"
    );
    assert!(
        !shown.contains("/search/smart"),
        "renderings and fragments must not include the smart endpoint"
    );
    assert!(related.contains("POST /search/metadata"));
    assert!(related.contains("POST /search/smart"));

    context_cmd(
        repo,
        &["vc1_deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("unknown change id"));
}

#[test]
fn exact_operation_match_fails_instead_of_leaking() {
    if !need_vimanam("exact_operation_match_fails_instead_of_leaking") {
        return;
    }
    let err = sethu::vimanam::render_operation(
        &fixture("immich/old.json"),
        "POST",
        "/search/nope",
        sethu::vimanam::DetailLevel::Standard,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("matched no endpoint"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn prepare_writes_a_complete_set() {
    if !need_vimanam("prepare_writes_a_complete_set") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    init_pair(
        &fixture("immich/old.json"),
        &fixture("immich/new.json"),
        repo,
    );
    let (migration, _, changes) = load_attempt(repo);
    let required = sethu::context::required_ids(&changes);
    assert!(!required.is_empty());
    let id = response_id_for(&changes, "POST", "/search/random");
    assert!(required.contains(&id));

    context_cmd(repo, &[&id, "--prepare", "all"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "prepared {} changes",
            required.len()
        )));

    let context_dir = layout::context_dir(&migration);
    let mut dirs: Vec<String> = std::fs::read_dir(&context_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    dirs.sort();
    assert_eq!(dirs, {
        let mut wanted = required.clone();
        wanted.sort();
        wanted
    });

    for wanted in &required {
        let dir = context_dir.join(wanted);
        for name in [
            "record.json",
            "origin.txt",
            "rendering-old.md",
            "rendering-new.md",
            "source-old.json",
            "source-new.json",
        ] {
            assert!(
                dir.join(name).is_file(),
                "missing {name} for {wanted} in {}",
                dir.display()
            );
        }
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("record.json")).unwrap()).unwrap();
        assert_eq!(record["id"].as_str(), Some(wanted.as_str()));
        for name in ["source-old.json", "source-new.json"] {
            let source: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap();
            assert!(source.get("operation").is_some(), "{name} for {wanted}");
            assert!(source.get("components").is_some(), "{name} for {wanted}");
        }
    }

    let random_dir = context_dir.join(&id);
    let origin = std::fs::read_to_string(random_dir.join("origin.txt")).unwrap();
    assert!(origin.contains("POST /search/metadata"));
    assert!(origin.contains("POST /search/smart"));
    let rendering = std::fs::read_to_string(random_dir.join("rendering-old.md")).unwrap();
    assert!(rendering.contains("POST /search/random"));
    assert!(!rendering.contains("/search/metadata"));
}

#[test]
fn added_endpoint_renders_new_side_only() {
    if !need_vimanam("added_endpoint_renders_new_side_only") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.json");
    let new = dir.path().join("new.json");
    std::fs::write(
        &old,
        r#"{"openapi":"3.0.0","info":{"title":"Demo","version":"1"},"paths":{}}"#,
    )
    .unwrap();
    std::fs::write(
        &new,
        r#"{"openapi":"3.0.0","info":{"title":"Demo","version":"2"},"paths":{"/thing":{"get":{"operationId":"getThing","responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"type":"object"}}}}}}}}}"#,
    )
    .unwrap();
    init_pair(&old, &new, repo);
    let (_, _, changes) = load_attempt(repo);
    let id = changes
        .iter()
        .find(|record| matches!(record.kind, sethu::vimanam::ChangeKind::EndpointAdded))
        .unwrap()
        .id
        .clone();

    let output = context_cmd(repo, &[&id, "--level", "overview"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("## new rendering (overview, new spec)"));
    assert!(stdout.contains("GET /thing"));
    assert!(stdout.contains("first appears in the new spec"));
    assert!(!stdout.contains("no longer exists"));
}
