//! Integration tests for `sethu capabilities`.
//!
//! The tests run the built binary and read its output. Tests that need the
//! released diff binary skip the version specific checks with a named reason
//! when it is absent from PATH. Tests that simulate a missing or outdated
//! binary override PATH for the child process only, through a temporary
//! directory. They never touch the real home directory.

use std::os::unix::fs::PermissionsExt;

use assert_cmd::Command;
use sethu::vimanam::MINIMUM_VERSION;

/// Every command the document must list, in dispatch order.
const COMMANDS: [&str; 10] = [
    "install", "init", "changes", "context", "record", "check", "stub", "verify", "report", "scan",
];

/// Commands that invoke the contract diff tool at runtime.
const DIFF_COMMANDS: [&str; 4] = ["init", "changes", "context", "scan"];

/// Build the test command for the sethu binary.
fn sethu() -> Command {
    Command::cargo_bin("sethu").unwrap()
}

/// Check whether the released diff binary answers on PATH.
fn diff_binary_present() -> bool {
    std::process::Command::new("vimanam")
        .arg("--version")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Run `sethu capabilities --json` and parse stdout as JSON.
///
/// A given path replaces PATH for the child only, so the test can hide
/// binaries without touching the parent process environment.
fn json_output(path_override: Option<&std::path::Path>) -> serde_json::Value {
    let mut cmd = sethu();
    cmd.arg("capabilities");
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }
    cmd.arg("--json");
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(&stdout).unwrap()
}

/// Check the contract that every unavailable entry carries a reason.
fn assert_reasons(value: &serde_json::Value) {
    let commands = value["commands"].as_object().unwrap();
    assert_eq!(commands.len(), COMMANDS.len());
    for name in COMMANDS {
        let entry = &commands[name];
        assert!(entry.is_object(), "{name} is missing from the document");
        let status = entry["status"].as_str().unwrap();
        assert!(
            ["available", "planned", "unavailable"].contains(&status),
            "{name} has an unknown status {status:?}"
        );
        if status == "unavailable" {
            let reason = entry.get("reason").and_then(|text| text.as_str()).unwrap();
            assert!(!reason.is_empty(), "{name} is unavailable without a reason");
        }
    }
}

#[test]
fn json_matches_documented_shape() {
    let value = json_output(None);

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["sethu"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["pack"], env!("CARGO_PKG_VERSION"));
    assert_reasons(&value);

    if !diff_binary_present() {
        eprintln!("SKIP version checks: `vimanam` is not on PATH");
        return;
    }
    assert_eq!(value["vimanam"]["found"], true);
    assert_eq!(value["vimanam"]["supported"], true);
    assert!(value["vimanam"]["version"].as_str().is_some());
    for name in DIFF_COMMANDS {
        assert_ne!(value["commands"][name]["status"], "unavailable");
    }
}

#[test]
fn missing_binary_marks_dependents_unavailable() {
    let empty = tempfile::tempdir().unwrap();
    let value = json_output(Some(empty.path()));

    assert_eq!(value["vimanam"]["found"], false);
    assert_eq!(value["vimanam"]["supported"], false);
    assert_reasons(&value);

    let minimum = MINIMUM_VERSION.to_string();
    for name in DIFF_COMMANDS {
        let entry = &value["commands"][name];
        assert_eq!(entry["status"], "unavailable");
        let reason = entry["reason"].as_str().unwrap();
        assert!(reason.contains("vimanam"), "{name} reason hides the cause");
        assert!(
            reason.contains(&minimum),
            "{name} reason hides the minimum version"
        );
    }
    for name in COMMANDS {
        if DIFF_COMMANDS.contains(&name) {
            continue;
        }
        assert_ne!(
            value["commands"][name]["status"], "unavailable",
            "{name} needs no diff binary"
        );
    }
}

#[test]
fn outdated_binary_reports_unsupported_version() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("vimanam");
    std::fs::write(&script, "#!/bin/sh\necho 'vimanam 1.2.0'\n").unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();

    let value = json_output(Some(dir.path()));

    assert_eq!(value["vimanam"]["found"], true);
    assert_eq!(value["vimanam"]["version"], "1.2.0");
    assert_eq!(value["vimanam"]["supported"], false);
    assert_reasons(&value);

    for name in DIFF_COMMANDS {
        let entry = &value["commands"][name];
        assert_eq!(entry["status"], "unavailable");
        let reason = entry["reason"].as_str().unwrap();
        assert!(reason.contains("1.2.0"), "{name} reason hides the version");
    }
}

#[test]
fn human_table_prints_same_facts() {
    let assert = sethu().arg("capabilities").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
    assert!(stdout.contains("vimanam"));
    assert!(stdout.contains("git"));
    for name in COMMANDS {
        assert!(stdout.contains(name), "{name} is missing from the table");
    }
}

#[test]
fn human_table_names_missing_binary_reason() {
    let empty = tempfile::tempdir().unwrap();
    let mut cmd = sethu();
    cmd.arg("capabilities");
    cmd.env("PATH", empty.path());
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(stdout.contains("init: unavailable"));
    assert!(stdout.contains("vimanam"));
}
