//! Integration tests for `sethu changes`.
//!
//! Tests build a scratch consumer repository per case through `sethu
//! init` on the pinned specs, then drive `changes` with the working
//! directory set to that repository. Live tests need the released diff
//! binary on PATH and skip with a named reason without it. Tests never
//! touch the real home directory.

use std::collections::{BTreeMap, BTreeSet};
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

/// One ready migration with its capture document.
struct Setup {
    /// Guard for the scratch consumer repository.
    _repo: tempfile::TempDir,
    /// Canonical repository path used for state lookups.
    repo: PathBuf,
    /// Parsed capture change list in report order.
    document: sethu::vimanam::DiffDocument,
    /// Migration directory holding the manifest.
    migration: PathBuf,
}

/// Initialise a scratch repository and read back its capture.
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
    Setup {
        _repo,
        repo,
        document,
        migration,
    }
}

/// Build a `sethu changes` invocation rooted at one repository.
fn changes_cmd(repo: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(repo).arg("changes").args(args);
    cmd
}

/// Run `changes` with arguments and return its stdout.
fn changes_stdout(setup: &Setup, args: &[&str]) -> String {
    let assert = changes_cmd(&setup.repo, args).assert().success();
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

/// Change ids with a breaking or review severity in report order.
fn required_ids(document: &sethu::vimanam::DiffDocument) -> Vec<String> {
    document
        .changes
        .iter()
        .filter(|record| {
            matches!(
                record.severity,
                sethu::vimanam::Severity::Breaking | sethu::vimanam::Severity::Review
            )
        })
        .map(|record| record.id.clone())
        .collect()
}

/// Change ids with a non-breaking severity in report order.
fn non_breaking_ids(document: &sethu::vimanam::DiffDocument) -> Vec<String> {
    document
        .changes
        .iter()
        .filter(|record| matches!(record.severity, sethu::vimanam::Severity::NonBreaking))
        .map(|record| record.id.clone())
        .collect()
}

/// Read the change lines of human output.
///
/// Change lines carry the full id first, indented past the operation line.
fn output_change_lines(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter(|line| line.starts_with("      vc1_"))
        .collect()
}

/// Split human output at the non-breaking section header.
fn split_sections(stdout: &str) -> (Vec<&str>, Vec<&str>) {
    let mut required = Vec::new();
    let mut rest = Vec::new();
    let mut in_rest = false;
    for line in stdout.lines() {
        if line.starts_with("non-breaking:") {
            in_rest = true;
            continue;
        }
        if in_rest {
            rest.push(line);
        } else {
            required.push(line);
        }
    }
    (required, rest)
}

/// Count the group headers in human output lines.
fn group_header_count(lines: &[&str]) -> usize {
    lines
        .iter()
        .filter(|line| line.starts_with("group "))
        .count()
}

/// The required set lists every breaking and review id with its facts.
///
/// The pinned capture grounds the expected count. Each printed line names
/// the method, path, kind, and severity exactly as the capture stores
/// them.
#[test]
fn required_set_lists_every_breaking_and_review_id() {
    if !need_vimanam("required_set_lists_every_breaking_and_review_id") {
        return;
    }
    let setup = setup();
    let wanted = required_ids(&setup.document);
    assert_eq!(wanted.len(), 27);

    let stdout = changes_stdout(&setup, &[]);
    let lines = output_change_lines(&stdout);
    assert_eq!(lines.len(), wanted.len());
    let mut by_id = BTreeMap::new();
    for record in &setup.document.changes {
        by_id.insert(record.id.as_str(), record);
    }
    // Change lines carry the id, severity, and kind. The operation line
    // above them carries the method and path, so the scan tracks it.
    let mut current: Option<(&str, &str)> = None;
    let mut checked = 0;
    for line in stdout.lines() {
        if line.starts_with("      vc1_") {
            let mut parts = line.split_whitespace();
            let id = parts.next().unwrap();
            let severity = parts.next().unwrap();
            let kind = parts.next().unwrap();
            assert!(parts.next().is_none(), "change line carries extra fields");
            let record = by_id.get(id).unwrap_or_else(|| panic!("unknown id {id}"));
            let stored_severity = serde_json::to_value(&record.severity).unwrap();
            let stored_kind = serde_json::to_value(&record.kind).unwrap();
            assert_eq!(severity, stored_severity.as_str().unwrap());
            assert_eq!(kind, stored_kind.as_str().unwrap());
            let (method, path) = current.unwrap_or_else(|| panic!("change {id} has no operation"));
            assert_eq!(method, record.endpoint.method);
            assert_eq!(path, record.endpoint.path);
            checked += 1;
        } else if line.starts_with("    ") && !line.starts_with("      ") {
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap();
            let path = parts.next().unwrap();
            assert!(
                parts.next().is_none(),
                "operation line carries extra fields"
            );
            current = Some((method, path));
        }
    }
    assert_eq!(checked, wanted.len());
    let operations: BTreeSet<(&str, &str)> = wanted
        .iter()
        .map(|id| {
            let record = by_id.get(id.as_str()).unwrap();
            (
                record.endpoint.method.as_str(),
                record.endpoint.path.as_str(),
            )
        })
        .collect();
    assert!(stdout.contains("27 changes"));
    assert!(stdout.contains(&format!("{} operations", operations.len())));
}

/// Grouping neither drops nor duplicates a required id.
///
/// Every required id appears exactly once across the printed groups, and
/// no non-breaking id leaks into the default output.
#[test]
fn grouping_neither_drops_nor_duplicates_a_required_id() {
    if !need_vimanam("grouping_neither_drops_nor_duplicates_a_required_id") {
        return;
    }
    let setup = setup();
    let wanted = required_ids(&setup.document);
    let unwanted: BTreeSet<String> = non_breaking_ids(&setup.document).into_iter().collect();

    let stdout = changes_stdout(&setup, &[]);
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for line in output_change_lines(&stdout) {
        let id = line.split_whitespace().next().unwrap();
        assert!(
            !unwanted.contains(id),
            "non-breaking id {id} leaks into the required set"
        );
        *seen.entry(id).or_default() += 1;
    }
    let mut shown: Vec<&str> = seen.keys().copied().collect();
    shown.sort_unstable();
    let mut expected: Vec<&str> = wanted.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(shown, expected);
    assert!(
        seen.values().all(|count| *count == 1),
        "a required id appears in several groups"
    );
}

/// `--all` adds the non-breaking changes in a separate section.
///
/// The required section keeps its exact rows, and the new section holds
/// every non-breaking id once.
#[test]
fn all_adds_non_breaking_in_a_separate_section() {
    if !need_vimanam("all_adds_non_breaking_in_a_separate_section") {
        return;
    }
    let setup = setup();
    let wanted = required_ids(&setup.document);
    let extra = non_breaking_ids(&setup.document);
    assert!(!extra.is_empty());

    let plain = changes_stdout(&setup, &[]);
    let full = changes_stdout(&setup, &["--all"]);
    let (required_lines, rest_lines) = split_sections(&full);
    assert!(!rest_lines.is_empty(), "--all prints a second section");

    let plain_ids: Vec<&str> = output_change_lines(&plain)
        .iter()
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    let required_part = required_lines.join("\n");
    let required_ids_shown: Vec<&str> = output_change_lines(&required_part)
        .iter()
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(required_ids_shown, plain_ids);
    assert_eq!(required_ids_shown.len(), wanted.len());

    let rest_part = rest_lines.join("\n");
    let mut shown: Vec<&str> = output_change_lines(&rest_part)
        .iter()
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    shown.sort_unstable();
    let mut expected: Vec<&str> = extra.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(shown, expected);
}

/// `--group` prints one numbered group.
///
/// The numbered groups partition the required set. An out of range number
/// fails with the valid range named.
#[test]
fn group_prints_one_numbered_group() {
    if !need_vimanam("group_prints_one_numbered_group") {
        return;
    }
    let setup = setup();
    let wanted = required_ids(&setup.document);

    let plain = changes_stdout(&setup, &[]);
    let total = group_header_count(&plain.lines().collect::<Vec<&str>>());
    assert!(total > 1);

    let mut union = BTreeSet::new();
    for number in 1..=total {
        let stdout = changes_stdout(&setup, &[&format!("--group={number}")]);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(group_header_count(&lines), 1);
        assert!(stdout.contains(&format!("group {number} of {total}")));
        for line in output_change_lines(&stdout) {
            let id = line.split_whitespace().next().unwrap();
            assert!(
                union.insert(id.to_string()),
                "id {id} appears in several groups"
            );
        }
    }
    let mut shown: Vec<String> = union.into_iter().collect();
    shown.sort_unstable();
    let mut expected = wanted.clone();
    expected.sort_unstable();
    assert_eq!(shown, expected);

    for bad in ["0", &format!("{}", total + 1)] {
        changes_cmd(&setup.repo, &[&format!("--group={bad}")])
            .assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains("out of range"))
            .stdout(predicate::str::is_empty());
    }
}

/// `--json` output is deterministic across runs.
///
/// Two reruns agree byte for byte. The parsed document covers every
/// required id once with stored severities and kinds, and hides the
/// non-breaking section unless `--all` asks for it.
#[test]
fn json_output_is_deterministic() {
    if !need_vimanam("json_output_is_deterministic") {
        return;
    }
    let setup = setup();
    let wanted = required_ids(&setup.document);

    let first = changes_stdout(&setup, &["--json"]);
    let second = changes_stdout(&setup, &["--json"]);
    assert_eq!(first, second);

    let document: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(document["required"]["change_count"], wanted.len() as u64);
    assert!(document.get("non_breaking").is_none());

    let mut by_id = BTreeMap::new();
    for record in &setup.document.changes {
        by_id.insert(record.id.as_str(), record);
    }
    let mut shown = Vec::new();
    for group in document["required"]["groups"].as_array().unwrap() {
        for tag in group["tags"].as_array().unwrap() {
            for operation in tag["operations"].as_array().unwrap() {
                for change in operation["changes"].as_array().unwrap() {
                    let id = change["id"].as_str().unwrap();
                    let record = by_id.get(id).unwrap();
                    assert_eq!(
                        change["severity"],
                        serde_json::to_value(&record.severity).unwrap()
                    );
                    assert_eq!(change["kind"], serde_json::to_value(&record.kind).unwrap());
                    assert_eq!(operation["method"], record.endpoint.method);
                    assert_eq!(operation["path"], record.endpoint.path);
                    shown.push(id.to_string());
                }
            }
        }
    }
    shown.sort_unstable();
    let mut expected = wanted.clone();
    expected.sort_unstable();
    assert_eq!(shown, expected);

    let full = changes_stdout(&setup, &["--json", "--all"]);
    let full_document: serde_json::Value = serde_json::from_str(&full).unwrap();
    assert_eq!(
        full_document["non_breaking"]["change_count"],
        non_breaking_ids(&setup.document).len() as u64
    );
    let again = changes_stdout(&setup, &["--json", "--all"]);
    assert_eq!(full, again);
}

/// A missing origins file reads as unknown origins for every change.
///
/// Older captures store no origins file, and listing must still work for
/// them without failing.
#[test]
fn missing_origins_lists_everything_as_unknown() {
    if !need_vimanam("missing_origins_lists_everything_as_unknown") {
        return;
    }
    let setup = setup();
    let wanted = required_ids(&setup.document);

    let root = layout::state_root(&setup.repo);
    let manifest: AttemptRecord =
        sethu::state::read_state_file(&layout::manifest_path(&setup.migration)).unwrap();
    let pair = layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash).unwrap();
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    let origins = layout::origins_file(&capture);
    assert!(origins.is_file());
    std::fs::remove_file(&origins).unwrap();

    let stdout = changes_stdout(&setup, &[]);
    assert!(stdout.contains("group 1 of 1: unknown"));
    let mut shown: Vec<&str> = output_change_lines(&stdout)
        .iter()
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    shown.sort_unstable();
    let mut expected: Vec<&str> = wanted.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(shown, expected);
}

#[test]
fn changes_without_init_is_refused() {
    let (repo, _) = init_git_repo();

    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.current_dir(repo.path()).arg("changes");
    cmd.assert()
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

    changes_cmd(&setup.repo, &[])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("needs exactly one"))
        .stdout(predicate::str::is_empty());
}
