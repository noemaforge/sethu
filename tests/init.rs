//! Integration tests for `sethu init`.
//!
//! Tests run the built binary against the checked in Immich specs with a
//! scratch consumer repository per test. Live tests need the released
//! diff binary on PATH and skip with a named reason without it. The gate
//! provides the binary. One test replaces it with a version shim to prove
//! that a new generator version opens a new capture and a new attempt.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use sethu::state::attempt::AttemptRecord;
use sethu::state::capture::CaptureRecord;
use sethu::state::layout;
use sethu::state::pair::PairRecord;

/// Full SHA-256 of the pinned old spec, from the fixture notice.
const OLD_SHA: &str = "ff2d4e2a7c35cbcf0ef0b8ca50158bf7711bc200a80f4cdcf0a51208403631c5";
/// Full SHA-256 of the pinned new spec, from the fixture notice.
const NEW_SHA: &str = "a5c98c5cf35f9b42a412a3aaee7c1e1f597279cc7c8a88cc090d7a7e21aedd26";

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

/// Build a `sethu init` invocation with explicit old, new, and repo.
fn init_cmd(old: &Path, new: &Path, repo: &Path) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.arg("init").arg(old).arg(new).arg("--repo").arg(repo);
    cmd
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

/// Read the change ids from a stored `changes.json` file.
fn stored_change_ids(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    let document = sethu::vimanam::parse_diff_output(&bytes).unwrap();
    document
        .changes
        .iter()
        .map(|item| item.id.clone())
        .collect()
}

/// Read the single attempt manifest stored under a state root.
fn only_attempt(root: &Path) -> (PathBuf, AttemptRecord) {
    let dir = layout::migrations_dir(root);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 1, "expected one attempt in {}", dir.display());
    let migration = dir.join(&names[0]);
    let manifest: AttemptRecord =
        sethu::state::read_state_file(&layout::manifest_path(&migration)).unwrap();
    (migration, manifest)
}

#[test]
fn initialises_immich_pair_with_pinned_hashes() {
    if !need_vimanam("initialises_immich_pair_with_pinned_hashes") {
        return;
    }
    let (_repo, baseline) = init_git_repo();
    let repo = _repo.path();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    init_cmd(&old, &new, repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));

    let root = layout::state_root(&repo.canonicalize().unwrap());
    let pair = layout::pair_dir(&root, OLD_SHA, NEW_SHA).unwrap();
    assert_eq!(
        pair.file_name().unwrap().to_str().unwrap(),
        "ff2d4e2a7c35-a5c98c5cf35f"
    );
    let stored: PairRecord = sethu::state::read_state_file(&layout::pair_file(&pair)).unwrap();
    stored.validate().unwrap();
    assert_eq!(stored.old_spec_hash, OLD_SHA);
    assert_eq!(stored.new_spec_hash, NEW_SHA);
    assert_eq!(
        std::fs::read(layout::pair_inputs_dir(&pair).join("old.json")).unwrap(),
        std::fs::read(&old).unwrap()
    );
    assert_eq!(
        std::fs::read(layout::pair_inputs_dir(&pair).join("new.json")).unwrap(),
        std::fs::read(&new).unwrap()
    );

    let captures: Vec<String> = std::fs::read_dir(layout::pair_captures_dir(&pair))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(captures.len(), 1);
    let capture = layout::capture_dir(&pair, &captures[0]);
    let record: CaptureRecord =
        sethu::state::read_state_file(&layout::capture_file(&capture)).unwrap();
    record.validate().unwrap();
    assert_eq!(record.generator_name, "vimanam");
    assert_eq!(record.generator_version, "1.3.0");
    assert_eq!(record.vimanam_schema_version, 1);
    assert_eq!(record.old_spec_hash, OLD_SHA);

    let raw = std::fs::read(layout::changes_file(&capture)).unwrap();
    let raw_value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(raw_value["changes"].as_array().unwrap().len(), 46);
    let document = sethu::vimanam::parse_diff_output(&raw).unwrap();
    assert_eq!(document.summary.breaking, 26);
    assert_eq!(document.summary.review, 1);
    assert_eq!(document.summary.non_breaking, 19);
    let fixture_bytes =
        std::fs::read(fixture("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json")).unwrap();
    let expected = sethu::vimanam::parse_diff_output(&fixture_bytes).unwrap();
    let live: Vec<&str> = document
        .changes
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    let wanted: Vec<&str> = expected
        .changes
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(live, wanted);

    let origins = sethu::provenance::read_origins(&capture).unwrap();
    origins.validate().unwrap();
    assert_eq!(origins.origins.len(), document.changes.len());
    for item in &document.changes {
        assert!(
            origins.origins.contains_key(item.id.as_str()),
            "fresh capture is missing an origin for {}",
            item.id
        );
    }

    let (_, manifest) = only_attempt(&root);
    manifest.validate().unwrap();
    assert_eq!(manifest.old_spec_hash, OLD_SHA);
    assert_eq!(manifest.new_spec_hash, NEW_SHA);
    assert_eq!(manifest.capture_id, captures[0]);
    assert_eq!(manifest.baseline_commit, baseline);
    assert!(manifest.scope.is_empty());
    assert_eq!(
        manifest.repo_path,
        repo.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(manifest.sethu_version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn rerun_resumes_and_keeps_ledger() {
    if !need_vimanam("rerun_resumes_and_keeps_ledger") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    init_cmd(&old, &new, repo).assert().success();
    let root = layout::state_root(&repo.canonicalize().unwrap());
    let pair = layout::pair_dir(&root, OLD_SHA, NEW_SHA).unwrap();
    let (attempt_dir, manifest) = only_attempt(&root);
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    let changes_before = std::fs::read(layout::changes_file(&capture)).unwrap();
    let record_before = std::fs::read(layout::capture_file(&capture)).unwrap();
    let origins_before = std::fs::read(layout::origins_file(&capture)).unwrap();

    let mut entries = indexmap::IndexMap::new();
    entries.insert(
        "sentinel".to_string(),
        sethu::state::LedgerEntry::new("unresolved", None),
    );
    let ledger = sethu::state::LedgerFile::new(entries);
    sethu::state::write_state_file(&layout::ledger_path(&attempt_dir), &ledger).unwrap();
    let ledger_before = std::fs::read(layout::ledger_path(&attempt_dir)).unwrap();

    init_cmd(&old, &new, repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("resumed"))
        .stdout(predicate::str::contains("reused"));

    let (_, again) = only_attempt(&root);
    assert_eq!(again.attempt_id, manifest.attempt_id);
    assert_eq!(
        std::fs::read(layout::changes_file(&capture)).unwrap(),
        changes_before
    );
    assert_eq!(
        std::fs::read(layout::capture_file(&capture)).unwrap(),
        record_before
    );
    assert_eq!(
        std::fs::read(layout::origins_file(&capture)).unwrap(),
        origins_before
    );
    assert_eq!(
        std::fs::read(layout::ledger_path(&attempt_dir)).unwrap(),
        ledger_before
    );
}

#[test]
fn changed_generator_version_produces_new_capture_and_attempt() {
    if !need_vimanam("changed_generator_version_produces_new_capture_and_attempt") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    init_cmd(&old, &new, repo).assert().success();
    let root = layout::state_root(&repo.canonicalize().unwrap());
    let pair = layout::pair_dir(&root, OLD_SHA, NEW_SHA).unwrap();
    let (_, first) = only_attempt(&root);
    let first_changes = std::fs::read(layout::changes_file(&layout::capture_dir(
        &pair,
        &first.capture_id,
    )))
    .unwrap();

    let bin = tempfile::tempdir().unwrap();
    let raw = std::fs::read(fixture("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json")).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    value["generator"]["version"] = serde_json::json!("9.9.9");
    let shim_output = bin.path().join("diff.json");
    std::fs::write(&shim_output, serde_json::to_vec(&value).unwrap()).unwrap();
    let shim = bin.path().join("vimanam");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nif [ \"x$1\" = \"x--version\" ]; then\n  echo \"vimanam 9.9.9\"\nelse\n  cat \"{}\"\nfi\n",
            shim_output.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&shim).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shim, perms).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = init_cmd(&old, &new, repo);
    cmd.env("PATH", &path)
        .env("SETHU_SHIM_OUTPUT", &shim_output);
    cmd.assert().success();

    let captures: Vec<String> = std::fs::read_dir(layout::pair_captures_dir(&pair))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(captures.len(), 2);
    let names: Vec<String> = std::fs::read_dir(layout::migrations_dir(&root))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(
        !names.contains(&first.attempt_id)
            || names
                .iter()
                .filter(|name| *name == &first.attempt_id)
                .count()
                == 1
    );

    let mut versions = Vec::new();
    for id in &captures {
        let record: CaptureRecord =
            sethu::state::read_state_file(&layout::capture_file(&layout::capture_dir(&pair, id)))
                .unwrap();
        versions.push(record.generator_version.clone());
        if *id == first.capture_id {
            assert_eq!(
                std::fs::read(layout::changes_file(&layout::capture_dir(&pair, id))).unwrap(),
                first_changes
            );
        } else {
            assert_eq!(
                stored_change_ids(&layout::changes_file(&layout::capture_dir(&pair, id))),
                stored_change_ids(&layout::changes_file(&layout::capture_dir(
                    &pair,
                    &first.capture_id
                )))
            );
        }
    }
    versions.sort();
    assert_eq!(versions, vec!["1.3.0", "9.9.9"]);
}

#[test]
fn refuses_external_refs() {
    if !need_vimanam("refuses_external_refs") {
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
        r#"{"openapi":"3.0.0","info":{"title":"Demo","version":"2"},"paths":{"/x":{"get":{"responses":{"200":{"description":"ok","content":{"application/json":{"schema":{"$ref":"https://example.com/common.json#/Thing"}}}}}}}}}"#,
    )
    .unwrap();

    init_cmd(&old, &new, repo)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("external reference"))
        .stdout(predicate::str::is_empty());
    assert!(!layout::pairs_dir(&layout::state_root(repo)).exists());
}

#[test]
fn fails_clearly_outside_a_git_repo() {
    if !need_vimanam("fails_clearly_outside_a_git_repo") {
        return;
    }
    let plain = tempfile::tempdir().unwrap();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    init_cmd(&old, &new, plain.path())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("baseline"));
}

#[test]
fn scope_change_creates_new_attempt_and_reorder_resumes() {
    if !need_vimanam("scope_change_creates_new_attempt_and_reorder_resumes") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    init_cmd(&old, &new, repo).assert().success();
    let root = layout::state_root(&repo.canonicalize().unwrap());
    let (_, bare) = only_attempt(&root);

    let mut scoped = init_cmd(&old, &new, repo);
    scoped.arg("--scope").arg("src");
    scoped
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));
    let attempts = sethu::state::attempt::find_attempts_for_pair(&root, OLD_SHA, NEW_SHA).unwrap();
    assert_eq!(attempts.len(), 2);
    let moved = attempts
        .iter()
        .find(|(_, manifest)| manifest.attempt_id != bare.attempt_id)
        .unwrap();
    assert_eq!(moved.1.scope, vec!["src".to_string()]);
    assert_eq!(moved.1.capture_id, bare.capture_id);

    let mut reordered = init_cmd(&old, &new, repo);
    reordered
        .arg("--scope")
        .arg("tests")
        .arg("--scope")
        .arg("src");
    reordered.assert().success();
    let mut flipped = init_cmd(&old, &new, repo);
    flipped
        .arg("--scope")
        .arg("src")
        .arg("--scope")
        .arg("tests");
    flipped
        .assert()
        .success()
        .stdout(predicate::str::contains("resumed"));
    let attempts = sethu::state::attempt::find_attempts_for_pair(&root, OLD_SHA, NEW_SHA).unwrap();
    assert_eq!(attempts.len(), 3);
}

#[test]
fn repeated_work_never_rewrites_stored_capture() {
    if !need_vimanam("repeated_work_never_rewrites_stored_capture") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    init_cmd(&old, &new, repo).assert().success();
    let root = layout::state_root(&repo.canonicalize().unwrap());
    let pair = layout::pair_dir(&root, OLD_SHA, NEW_SHA).unwrap();
    let (_, manifest) = only_attempt(&root);
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    let changes_before = std::fs::read(layout::changes_file(&capture)).unwrap();
    let record_before = std::fs::read(layout::capture_file(&capture)).unwrap();

    init_cmd(&old, &new, repo).assert().success();
    let mut scoped = init_cmd(&old, &new, repo);
    scoped.arg("--scope").arg("elsewhere");
    scoped.assert().success();
    let mut listed = init_cmd(&old, &new, repo);
    listed.arg("--list");
    listed.assert().success();

    assert_eq!(
        std::fs::read(layout::changes_file(&capture)).unwrap(),
        changes_before
    );
    assert_eq!(
        std::fs::read(layout::capture_file(&capture)).unwrap(),
        record_before
    );
}

#[test]
fn tampered_severity_blocks_reuse() {
    if !need_vimanam("tampered_severity_blocks_reuse") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    init_cmd(&old, &new, repo).assert().success();
    let root = layout::state_root(&repo.canonicalize().unwrap());
    let pair = layout::pair_dir(&root, OLD_SHA, NEW_SHA).unwrap();
    let (_, manifest) = only_attempt(&root);
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    let changes_path = layout::changes_file(&capture);

    let raw = std::fs::read(&changes_path).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let changes = value.get_mut("changes").unwrap().as_array_mut().unwrap();
    assert!(!changes.is_empty());
    let severity = changes[0].get("severity").unwrap().as_str().unwrap();
    let flipped = if severity == "breaking" {
        "non_breaking"
    } else {
        "breaking"
    };
    changes[0]["severity"] = serde_json::json!(flipped);
    let mut tampered = serde_json::to_vec_pretty(&value).unwrap();
    tampered.push(b'\n');
    std::fs::write(&changes_path, &tampered).unwrap();

    init_cmd(&old, &new, repo)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("differs from the fresh diff"))
        .stdout(predicate::str::is_empty());

    let kept = std::fs::read(&changes_path).unwrap();
    let document = sethu::vimanam::parse_diff_output(&kept).unwrap();
    assert_eq!(
        document.changes[0].severity,
        if flipped == "breaking" {
            sethu::vimanam::Severity::Breaking
        } else {
            sethu::vimanam::Severity::NonBreaking
        }
    );
}

#[test]
fn list_shows_attempts_for_pair() {
    if !need_vimanam("list_shows_attempts_for_pair") {
        return;
    }
    let (_repo, _) = init_git_repo();
    let repo = _repo.path();
    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");

    init_cmd(&old, &new, repo).assert().success();
    let root = layout::state_root(&repo.canonicalize().unwrap());
    let (_, manifest) = only_attempt(&root);

    let mut listed = init_cmd(&old, &new, repo);
    listed.arg("--list");
    listed
        .assert()
        .success()
        .stdout(predicate::str::contains(&manifest.attempt_id));

    let empty = tempfile::tempdir().unwrap();
    let mut none = init_cmd(&old, &new, empty.path());
    none.arg("--list");
    none.assert().success().stdout(predicate::str::is_empty());
}
