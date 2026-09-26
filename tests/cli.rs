//! Integration tests for the sethu CLI.

use assert_cmd::Command;
use predicates::prelude::*;
use sethu::state::attempt::AttemptRecord;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn sethu() -> Command {
    Command::cargo_bin("sethu").unwrap()
}

/// Locate a checked in fixture by path under the crate root.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Report whether the released diff binary answers on PATH.
fn diff_binary_present() -> bool {
    std::process::Command::new("vimanam")
        .arg("--version")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Read the capabilities document the binary reports.
fn capabilities_json() -> serde_json::Value {
    let assert = sethu().args(["capabilities", "--json"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(&stdout).unwrap()
}

/// Report whether one command is implemented in this build.
///
/// Per-command stub cases return early once their command flips to
/// available. The generic case below covers every command that is
/// still planned, so a newly implemented command skips its old case
/// instead of failing it.
fn is_available(name: &str) -> bool {
    capabilities_json()["commands"][name]["status"] == "available"
}

/// Arguments that reach the stub error for one command.
///
/// Every entry names the command plus the flags its stub case
/// already uses, so the generic case below stays in step with them.
fn stub_invocation(name: &str) -> Option<Vec<&'static str>> {
    match name {
        "install" => Some(vec!["install", "/tmp"]),
        "changes" => Some(vec!["changes"]),
        "context" => Some(vec!["context", "vc1_abc123"]),
        "check" => Some(vec!["check"]),
        "verify" => Some(vec!["verify"]),
        "report" => Some(vec!["report"]),
        "scan" => Some(vec!["scan", "/tmp", "--spec", "openapi.yaml"]),
        _ => None,
    }
}

/// Run one git invocation inside a scratch repository.
fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn help_lists_all_subcommands() {
    let mut cmd = sethu();
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("changes"))
        .stdout(predicate::str::contains("context"))
        .stdout(predicate::str::contains("record"))
        .stdout(predicate::str::contains("check"))
        .stdout(predicate::str::contains("stub"))
        .stdout(predicate::str::contains("verify"))
        .stdout(predicate::str::contains("report"))
        .stdout(predicate::str::contains("scan"))
        .stdout(predicate::str::contains("capabilities"));
}

#[test]
fn install_help_succeeds() {
    sethu().args(["install", "--help"]).assert().success();
}

#[test]
fn init_help_succeeds() {
    sethu().args(["init", "--help"]).assert().success();
}

#[test]
fn changes_help_succeeds() {
    sethu().args(["changes", "--help"]).assert().success();
}

#[test]
fn context_help_succeeds() {
    sethu().args(["context", "--help"]).assert().success();
}

#[test]
fn record_help_succeeds() {
    sethu().args(["record", "--help"]).assert().success();
}

#[test]
fn check_help_succeeds() {
    sethu().args(["check", "--help"]).assert().success();
}

#[test]
fn stub_help_succeeds() {
    sethu().args(["stub", "--help"]).assert().success();
}

#[test]
fn verify_help_succeeds() {
    sethu().args(["verify", "--help"]).assert().success();
}

#[test]
fn report_help_succeeds() {
    sethu().args(["report", "--help"]).assert().success();
}

#[test]
fn scan_help_succeeds() {
    sethu().args(["scan", "--help"]).assert().success();
}

#[test]
fn capabilities_help_succeeds() {
    sethu().args(["capabilities", "--help"]).assert().success();
}

#[test]
fn install_stub_exits_1_with_not_available() {
    if is_available("install") {
        return;
    }
    sethu()
        .args(["install", "/tmp"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn init_list_reports_no_attempts_with_success() {
    let specs = tempfile::tempdir().unwrap();
    let old = specs.path().join("old.json");
    let new = specs.path().join("new.json");
    std::fs::write(
        &old,
        r#"{"openapi":"3.0.0","info":{"title":"Demo","version":"1"},"paths":{}}"#,
    )
    .unwrap();
    std::fs::write(
        &new,
        r#"{"openapi":"3.0.0","info":{"title":"Demo","version":"2"},"paths":{}}"#,
    )
    .unwrap();
    let repo = tempfile::tempdir().unwrap();

    sethu()
        .arg("init")
        .arg(&old)
        .arg(&new)
        .arg("--list")
        .arg("--repo")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("no attempts"));
    assert!(!repo.path().join(".sethu").exists());
}

#[test]
fn changes_stub_exits_1_with_not_available() {
    if is_available("changes") {
        return;
    }
    sethu()
        .arg("changes")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn context_stub_exits_1_with_not_available() {
    if is_available("context") {
        return;
    }
    sethu()
        .args(["context", "vc1_abc123"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn record_unresolved_reports_recorded_with_success() {
    if !diff_binary_present() {
        eprintln!("SKIP record_unresolved_reports_recorded_with_success: `vimanam` is not on PATH");
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    git(scratch.path(), &["init", "-q"]);
    std::fs::write(scratch.path().join("README.md"), "consumer\n").unwrap();
    git(scratch.path(), &["add", "."]);
    git(
        scratch.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "baseline",
        ],
    );
    let repo = scratch.path().canonicalize().unwrap();

    sethu()
        .current_dir(&repo)
        .arg("init")
        .arg(fixture("immich/old.json"))
        .arg(fixture("immich/new.json"))
        .arg("--repo")
        .arg(&repo)
        .assert()
        .success();

    let root = sethu::state::layout::state_root(&repo);
    let names: Vec<String> = std::fs::read_dir(sethu::state::layout::migrations_dir(&root))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 1);
    let migration = sethu::state::layout::migration_dir(&root, &names[0]);
    let manifest: AttemptRecord =
        sethu::state::read_state_file(&sethu::state::layout::manifest_path(&migration)).unwrap();
    let pair =
        sethu::state::layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash)
            .unwrap();
    let capture = sethu::state::layout::capture_dir(&pair, &manifest.capture_id);
    let raw = std::fs::read(sethu::state::layout::changes_file(&capture)).unwrap();
    let document = sethu::vimanam::parse_diff_output(&raw).unwrap();
    assert!(!document.changes.is_empty());
    let id = document.changes[0].id.clone();

    std::fs::write(repo.join("triage.md"), "still failing\n").unwrap();
    sethu()
        .current_dir(&repo)
        .arg("record")
        .arg(&id)
        .arg("--outcome")
        .arg("unresolved")
        .arg("--evidence")
        .arg("triage.md")
        .assert()
        .success()
        .stdout(predicate::str::contains("recorded"))
        .stdout(predicate::str::contains(id.as_str()))
        .stdout(predicate::str::contains("unresolved"));
    assert!(sethu::state::layout::ledger_path(&migration).exists());
}

#[test]
fn check_stub_exits_1_with_not_available() {
    if is_available("check") {
        return;
    }
    sethu()
        .arg("check")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn stub_empty_scenarios_prints_readiness_line() {
    let scenarios = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    let binary = assert_cmd::cargo::cargo_bin("sethu");
    let mut child = std::process::Command::new(binary)
        .arg("stub")
        .arg("--version")
        .arg("old")
        .arg("--port")
        .arg("0")
        .arg("--scenarios")
        .arg(scenarios.path())
        .current_dir(run.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        let first = lines.next().map(|line| line.unwrap()).unwrap_or_default();
        sender.send(first).unwrap();
    });
    let readiness = receiver
        .recv_timeout(Duration::from_secs(15))
        .expect("stub prints a readiness line");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        readiness.starts_with("sethu-stub ready version=old addr=127.0.0.1:"),
        "unexpected readiness line: {readiness:?}"
    );
    assert!(
        readiness.contains("scenarios=0"),
        "unexpected readiness line: {readiness:?}"
    );
}

#[test]
fn verify_stub_exits_1_with_not_available() {
    if is_available("verify") {
        return;
    }
    sethu()
        .arg("verify")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn report_stub_exits_1_with_not_available() {
    if is_available("report") {
        return;
    }
    sethu()
        .arg("report")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn scan_stub_exits_1_with_not_available() {
    if is_available("scan") {
        return;
    }
    sethu()
        .args(["scan", "/tmp", "--spec", "openapi.yaml"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn planned_commands_exit_1_with_not_available() {
    let document = capabilities_json();
    let commands = document["commands"].as_object().unwrap();
    let mut planned = 0;
    for (name, entry) in commands {
        if entry["status"] != "planned" {
            continue;
        }
        let argv = stub_invocation(name)
            .unwrap_or_else(|| panic!("no stub invocation for planned command {name}"));
        sethu()
            .args(argv)
            .assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains("not available yet"))
            .stdout(predicate::str::is_empty());
        planned += 1;
    }
    assert!(planned > 0, "expected at least one planned command");
}

#[test]
fn capabilities_reports_table_with_success() {
    sethu()
        .arg("capabilities")
        .assert()
        .success()
        .stdout(predicate::str::contains("vimanam"))
        .stdout(predicate::str::contains("git"))
        .stdout(predicate::str::contains("init: available"));
}

#[test]
fn unknown_flag_exits_2() {
    sethu()
        .arg("--this-flag-does-not-exist")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn record_without_outcome_exits_2() {
    sethu()
        .args(["record", "vc1_abc123"])
        .assert()
        .failure()
        .code(2);
}
