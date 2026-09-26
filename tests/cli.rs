//! Integration tests for the sethu CLI.

use assert_cmd::Command;
use predicates::prelude::*;

fn sethu() -> Command {
    Command::cargo_bin("sethu").unwrap()
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
    sethu()
        .args(["install", "/tmp"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn init_stub_exits_1_with_not_available() {
    sethu()
        .args(["init", "old.yaml", "new.yaml"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn changes_stub_exits_1_with_not_available() {
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
    sethu()
        .args(["context", "vc1_abc123"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn record_stub_exits_1_with_not_available() {
    sethu()
        .args(["record", "vc1_abc123", "--outcome", "unresolved"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn check_stub_exits_1_with_not_available() {
    sethu()
        .arg("check")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn stub_stub_exits_1_with_not_available() {
    sethu()
        .args(["stub", "--version", "old"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn verify_stub_exits_1_with_not_available() {
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
    sethu()
        .args(["scan", "/tmp", "--spec", "openapi.yaml"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn capabilities_stub_exits_1_with_not_available() {
    sethu()
        .arg("capabilities")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not available yet"))
        .stdout(predicate::str::is_empty());
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
