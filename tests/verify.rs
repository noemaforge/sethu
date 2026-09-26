//! Checks for `sethu verify`.
//!
//! The tests prove the three-stage matrix on throwaway clones of the
//! real demo consumer: the random picker check goes green, expected
//! red, green, both guards pass in every stage, a naive shared-parser
//! patch fails a guard, and a dead stub yields an invalid red. Every
//! run uses real stub instances on loopback and fixture scenarios
//! validated against the pinned specs. Nothing writes to the live
//! demo checkout. Heavy tests share one target directory and one
//! lock, so sequential cargo builds reuse compiled dependencies.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};

/// Live demo consumer checkout the tests clone but never modify.
const DEMO_REPO: &str = "/home/nryn/work/sethu-demo-picker";

/// Full hash of the pinned old spec, matching the checked in fixture.
const OLD_SHA: &str = "ff2d4e2a7c35cbcf0ef0b8ca50158bf7711bc200a80f4cdcf0a51208403631c5";
/// Full hash of the pinned new spec, matching the checked in fixture.
const NEW_SHA: &str = "a5c98c5cf35f9b42a412a3aaee7c1e1f597279cc7c8a88cc090d7a7e21aedd26";

/// Lock that serialises the heavy cargo-building tests.
static HEAVY: Mutex<()> = Mutex::new(());

/// Shared target directory reused by every heavy test process.
static TARGET: OnceLock<PathBuf> = OnceLock::new();

/// Shared target directory, leaked for the life of the test process.
fn shared_target_dir() -> PathBuf {
    TARGET
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_path_buf();
            std::mem::forget(dir);
            path
        })
        .clone()
}

/// Build the test command for the sethu binary with a shared target dir.
fn sethu() -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.env("CARGO_TARGET_DIR", shared_target_dir());
    cmd
}

/// Run one git command in a directory and keep stdout on success.
fn git(dir: &Path, args: &[&str]) -> String {
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

/// Clone the demo consumer into a scratch directory.
///
/// The clone carries the full history, so baseline and patched
/// commits resolve exactly like they would in the live checkout.
fn clone_demo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("consumer");
    let output = std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(DEMO_REPO)
        .arg(&target)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    git(&target, &["config", "user.email", "test@example.com"]);
    git(&target, &["config", "user.name", "Test"]);
    dir
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
    git(repo, &["add", "."]);
    git(
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
    git(repo, &["rev-parse", "HEAD"]).trim().to_string()
}

/// One valid asset with a null stack, exercising the nullable idiom.
fn full_asset(id: &str, index: usize) -> Value {
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

/// One old-contract search response holding the given asset IDs.
fn search_response(ids: &[&str]) -> Value {
    let items: Vec<Value> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| full_asset(id, index))
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

/// One new-contract random response holding the given asset IDs.
fn asset_array(ids: &[&str]) -> Value {
    Value::Array(
        ids.iter()
            .enumerate()
            .map(|(index, id)| full_asset(id, index))
            .collect(),
    )
}

/// Harness integration tests driving the picker through the stub.
///
/// The random check tolerates a failed decode and asserts the picked
/// IDs, so the red stage fails on the assertion message rather than
/// on a raw parsing error.
const HARNESS_TESTS: &str = r#"
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

#[test]
fn verify_smart_guard() {
    let client = ImmichClient::from_env().expect("read stub address from environment");
    let response = client.search_smart("sunset").expect("run smart search");
    let mut picker = Picker::new();
    picker.add_response(&response);
    let got: Vec<String> = picker.selection().to_vec();
    assert_eq!(got, vec!["asset-s1".to_string()], "smart picker ids");
}

#[test]
fn verify_metadata_guard() {
    let client = ImmichClient::from_env().expect("read stub address from environment");
    let response = client
        .search_metadata("photo-0.jpg")
        .expect("run metadata search");
    let mut picker = Picker::new();
    picker.add_response(&response);
    let got: Vec<String> = picker.selection().to_vec();
    assert_eq!(got, vec!["asset-m1".to_string()], "metadata picker ids");
}
"#;

/// Write the harness directory with tests and both scenario sets.
fn write_harness(root: &Path) {
    let harness = root.join("harness");
    let tests = harness.join("tests");
    let old = harness.join("scenarios").join("old");
    let next = harness.join("scenarios").join("new");
    for dir in [&tests, &old, &next] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(tests.join("harness_checks.rs"), HARNESS_TESTS).unwrap();
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
        old.join("random.json"),
        serde_json::to_vec_pretty(&scenario(
            "random-search",
            OLD_SHA,
            post("/search/random", Some(json!({"page": 1}))),
            search_response(&["asset-r1", "asset-r2"]),
        ))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        old.join("smart.json"),
        serde_json::to_vec_pretty(&scenario(
            "smart-search",
            OLD_SHA,
            post("/search/smart", Some(json!({"query": "sunset"}))),
            search_response(&["asset-s1"]),
        ))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        old.join("metadata.json"),
        serde_json::to_vec_pretty(&scenario(
            "metadata-search",
            OLD_SHA,
            post(
                "/search/metadata",
                Some(json!({"originalFileName": "photo-0.jpg"})),
            ),
            search_response(&["asset-m1"]),
        ))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        next.join("random.json"),
        serde_json::to_vec_pretty(&scenario(
            "random-search",
            NEW_SHA,
            post("/search/random", None),
            asset_array(&["asset-r1", "asset-r2"]),
        ))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        next.join("smart.json"),
        serde_json::to_vec_pretty(&scenario(
            "smart-search",
            NEW_SHA,
            post("/search/smart", Some(json!({"query": "sunset"}))),
            search_response(&["asset-s1"]),
        ))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        next.join("metadata.json"),
        serde_json::to_vec_pretty(&scenario(
            "metadata-search",
            NEW_SHA,
            post(
                "/search/metadata",
                Some(json!({"originalFileName": "photo-0.jpg"})),
            ),
            search_response(&["asset-m1"]),
        ))
        .unwrap(),
    )
    .unwrap();
}

/// Write a verification manifest naming the three demo checks.
fn write_manifest(root: &Path, repo: &Path, baseline: &str, patched: &str) -> PathBuf {
    let manifest = json!({
        "schema_version": 1,
        "repo": repo.display().to_string(),
        "baseline_commit": baseline,
        "patched_commit": patched,
        "harness": "harness",
        "scenarios_old": "harness/scenarios/old",
        "scenarios_new": "harness/scenarios/new",
        "spec_old_sha256": OLD_SHA,
        "spec_new_sha256": NEW_SHA,
        "checks": [
            {
                "name": "random-picker",
                "role": "regression",
                "change_ids": ["vc1_demo_random"],
                "test": "verify_random_picker",
                "expected_diagnostic": "random picker ids",
                "expected_exchange": [
                    {"scenario": "random-search", "method": "POST", "path": "/search/random"}
                ]
            },
            {
                "name": "smart-guard",
                "role": "guard",
                "change_ids": ["vc1_demo_smart"],
                "test": "verify_smart_guard",
                "expected_exchange": [
                    {"scenario": "smart-search", "method": "POST", "path": "/search/smart"}
                ]
            },
            {
                "name": "metadata-guard",
                "role": "guard",
                "change_ids": ["vc1_demo_metadata"],
                "test": "verify_metadata_guard",
                "expected_exchange": [
                    {"scenario": "metadata-search", "method": "POST", "path": "/search/metadata"}
                ]
            }
        ]
    });
    let path = root.join("verify-manifest.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    path
}

/// Correct repair: random search reads the new array, others keep the parser.
const CORRECT_PATCH_OLD: &str = r#"    /// Run a random search and return one page of assets.
    pub fn search_random(&self, page: u64) -> Result<SearchResponse, Error> {
        self.post("/search/random", &PageBody { page })
    }"#;

/// Correct repair: random search reads the new array, others keep the parser.
const CORRECT_PATCH_NEW: &str = r#"    /// Run a random search and return one page of assets.
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

/// Naive repair: the shared parser expects an array everywhere.
const NAIVE_PATCH_OLD: &str = r#"pub fn parse_search_response(body: &str) -> Result<SearchResponse, Error> {
    serde_json::from_str(body).map_err(Error::Parse)
}"#;

/// Naive repair: the shared parser expects an array everywhere.
const NAIVE_PATCH_NEW: &str = r#"pub fn parse_search_response(body: &str) -> Result<SearchResponse, Error> {
    let items: Vec<Asset> = serde_json::from_str(body).map_err(Error::Parse)?;
    Ok(SearchResponse {
        albums: AlbumResults::default(),
        assets: AssetResults {
            count: items.len() as u64,
            items,
        },
    })
}"#;

/// Read the single run record under a manifest directory.
fn only_run(manifest_dir: &Path) -> (PathBuf, Value) {
    let runs = manifest_dir.join("runs");
    let names: Vec<String> = std::fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 1, "expected one run in {}", runs.display());
    let dir = runs.join(&names[0]);
    let record: Value =
        serde_json::from_slice(&std::fs::read(dir.join("run.json")).unwrap()).unwrap();
    (dir, record)
}

/// Read one stage record for a check and stage name.
fn stage_record(run_dir: &Path, check: &str, stage: &str) -> Value {
    let path = run_dir.join(check).join(stage).join("stage.json");
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
}

#[test]
fn regression_goes_green_red_green_with_guards() {
    let _guard = HEAVY.lock().unwrap();
    let _clone = clone_demo();
    let repo = _clone.path().join("consumer");
    let baseline = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let patched = commit_edit(
        &repo,
        "src/lib.rs",
        CORRECT_PATCH_OLD,
        CORRECT_PATCH_NEW,
        "give random search its own array path",
    );
    let area = tempfile::tempdir().unwrap();
    write_harness(area.path());
    let manifest = write_manifest(area.path(), &repo, &baseline, &patched);

    sethu()
        .args(["verify", "--freeze", "--manifest"])
        .arg(&manifest)
        .assert()
        .success()
        .stdout(predicate::str::contains("frozen"));
    assert!(area.path().join("verify-manifest.json.frozen").is_file());

    sethu()
        .args(["verify", "--manifest"])
        .arg(&manifest)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("random-picker: verified"))
        .stdout(predicate::str::contains("smart-guard: verified"))
        .stdout(predicate::str::contains("metadata-guard: verified"))
        .stdout(predicate::str::contains("expected-red"));

    let (run_dir, record) = only_run(area.path());
    assert_eq!(record["harness_changed"], false);
    let checks = record["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 3);
    for check in checks {
        assert_eq!(check["verified"], true, "{check}");
    }
    let stages = ["original-old", "original-new", "patched-new"];
    let wanted = ["pass", "expected-red", "pass"];
    for (stage, verdict) in stages.iter().zip(wanted.iter()) {
        let stored = stage_record(&run_dir, "random-picker", stage);
        assert_eq!(stored["verdict"], *verdict, "{stage}");
        assert!(!stored["cases"].as_array().unwrap().is_empty());
        assert_eq!(stored["parser"], "nextest-junit");
        let trace: Vec<Value> = std::fs::read_to_string(
            run_dir
                .join("random-picker")
                .join(stage)
                .join("requests.jsonl"),
        )
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
        assert!(!trace.is_empty(), "{stage} trace is empty");
        assert!(
            trace
                .iter()
                .all(|entry: &Value| entry["scenario_id"] == "random-search"),
            "{stage} trace holds another check"
        );
        assert!(
            trace
                .iter()
                .all(|entry: &Value| entry["request_valid"] == true),
            "{stage} trace holds an invalid request"
        );
        assert!(
            Path::new(&run_dir.join("random-picker").join(stage).join("junit.xml")).is_file(),
            "{stage} junit is missing"
        );
    }
    for guard in ["smart-guard", "metadata-guard"] {
        for stage in stages {
            let stored = stage_record(&run_dir, guard, stage);
            assert_eq!(stored["verdict"], "pass", "{guard} {stage}");
        }
    }
}

#[test]
fn naive_shared_parser_patch_fails_a_guard() {
    let _guard = HEAVY.lock().unwrap();
    let _clone = clone_demo();
    let repo = _clone.path().join("consumer");
    let baseline = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let patched = commit_edit(
        &repo,
        "src/lib.rs",
        NAIVE_PATCH_OLD,
        NAIVE_PATCH_NEW,
        "parse every search as an array",
    );
    let area = tempfile::tempdir().unwrap();
    write_harness(area.path());
    let manifest = write_manifest(area.path(), &repo, &baseline, &patched);

    sethu()
        .args(["verify", "--freeze", "--manifest"])
        .arg(&manifest)
        .assert()
        .success();

    sethu()
        .args(["verify", "--manifest"])
        .arg(&manifest)
        .assert()
        .code(4)
        .stdout(predicate::str::contains("NOT VERIFIED"));

    let (run_dir, record) = only_run(area.path());
    let checks = record["checks"].as_array().unwrap();
    let random = checks
        .iter()
        .find(|check| check["name"] == "random-picker")
        .unwrap();
    assert_eq!(random["verified"], true);
    let guards: Vec<&Value> = checks
        .iter()
        .filter(|check| check["name"] != "random-picker")
        .collect();
    assert!(
        guards.iter().any(|check| check["verified"] == false),
        "a naive shared-parser patch must fail a guard"
    );
    let mut failed_detail = false;
    for guard in ["smart-guard", "metadata-guard"] {
        let stored = stage_record(&run_dir, guard, "patched-new");
        if stored["verdict"] == "failed" {
            failed_detail = true;
        }
    }
    assert!(failed_detail, "a guard must fail on the patched commit");
}

#[test]
fn freeze_detects_harness_change_and_supersedes() {
    let _guard = HEAVY.lock().unwrap();
    let _clone = clone_demo();
    let repo = _clone.path().join("consumer");
    let baseline = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let patched = commit_edit(
        &repo,
        "src/lib.rs",
        CORRECT_PATCH_OLD,
        CORRECT_PATCH_NEW,
        "give random search its own array path",
    );
    let area = tempfile::tempdir().unwrap();
    write_harness(area.path());
    let manifest = write_manifest(area.path(), &repo, &baseline, &patched);

    sethu()
        .args(["verify", "--freeze", "--manifest"])
        .arg(&manifest)
        .assert()
        .success();
    sethu()
        .args(["verify", "--manifest"])
        .arg(&manifest)
        .args(["--check", "random-picker"])
        .assert()
        .code(0);
    let runs = area.path().join("runs");
    let first: String = std::fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .pop()
        .unwrap();

    let harness_tests = area.path().join("harness/tests/harness_checks.rs");
    let text = std::fs::read_to_string(&harness_tests).unwrap();
    std::fs::write(&harness_tests, format!("{text}\n// harness tweak\n")).unwrap();

    sethu()
        .args(["verify", "--manifest"])
        .arg(&manifest)
        .args(["--check", "random-picker"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("superseded"));
    let names: Vec<String> = std::fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 2);
    let second = names.iter().find(|name| *name != &first).unwrap();
    let record: Value =
        serde_json::from_slice(&std::fs::read(runs.join(second).join("run.json")).unwrap())
            .unwrap();
    assert_eq!(record["harness_changed"], true);
    assert_eq!(record["superseded_runs"], json!([first]));
}

#[test]
fn dirty_tree_refuses_verify() {
    let _clone = clone_demo();
    let repo = _clone.path().join("consumer");
    let baseline = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let area = tempfile::tempdir().unwrap();
    write_harness(area.path());
    let manifest = write_manifest(area.path(), &repo, &baseline, &baseline);
    std::fs::write(repo.join("scratch-note.txt"), "uncommitted\n").unwrap();

    sethu()
        .args(["verify", "--manifest"])
        .arg(&manifest)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("uncommitted"));
    assert!(
        !area.path().join("runs").exists(),
        "a refused run must leave no artefacts"
    );
}

#[test]
fn stopped_stub_yields_invalid_red() {
    let _guard = HEAVY.lock().unwrap();
    let _clone = clone_demo();
    let repo = _clone.path().join("consumer");
    let baseline = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let area = tempfile::tempdir().unwrap();
    write_harness(area.path());
    let harness = area.path().join("harness");

    let worktree = area.path().join("dead-worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().unwrap(),
            &baseline,
        ],
    );
    let source = harness.join("tests").join("harness_checks.rs");
    std::fs::copy(&source, worktree.join("tests").join("harness_checks.rs")).unwrap();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = probe.local_addr().unwrap().port();
    drop(probe);
    let output = std::process::Command::new("cargo")
        .arg("test")
        .arg("--")
        .arg("--exact")
        .arg("verify_random_picker")
        .current_dir(&worktree)
        .env("IMMICH_BASE_URL", format!("http://127.0.0.1:{dead_port}"))
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TARGET_DIR", shared_target_dir())
        .output()
        .unwrap();
    assert!(!output.status.success(), "the dead stub must fail the test");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    git(
        &repo,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );

    let cases = sethu::verify::outcome::parse_libtest(&stdout);
    let matched: Vec<&sethu::verify::outcome::CaseResult> = cases
        .iter()
        .filter(|case| case.name == "verify_random_picker")
        .collect();
    assert_eq!(matched.len(), 1);
    assert!(!matched[0].passed);
    let run = sethu::verify::outcome::TestRun {
        command: "cargo test -- --exact verify_random_picker".to_string(),
        parser: sethu::verify::outcome::ParserKind::LibtestText,
        exit_code: output.status.code(),
        signal: None,
        timed_out: false,
        build_failed: false,
        cases,
        stdout,
        stderr: String::new(),
    };
    let check = sethu::verify::manifest::CheckSpec {
        name: "random-picker".to_string(),
        role: sethu::verify::manifest::Role::Regression,
        change_ids: vec![],
        test: "verify_random_picker".to_string(),
        expected_diagnostic: Some(sethu::verify::manifest::ExpectedDiagnostic::Substring(
            "random picker ids".to_string(),
        )),
        expected_exchange: vec![sethu::verify::manifest::ExpectedExchange {
            scenario: "random-search".to_string(),
            method: "POST".to_string(),
            path: "/search/random".to_string(),
        }],
    };
    let scenarios = std::collections::HashMap::from([(
        "random-search".to_string(),
        sethu::stub::load_scenarios(
            &harness.join("scenarios").join("new"),
            &serde_json::from_slice::<Value>(
                sethu::stub::embedded_spec(&sethu::cli::SpecVersion::New).0,
            )
            .unwrap(),
            NEW_SHA,
        )
        .unwrap()
        .into_iter()
        .find(|scenario| scenario.id == "random-search")
        .unwrap(),
    )]);
    let verdict = sethu::verify::outcome::evaluate(&check, true, &run, &[], &scenarios);
    assert!(
        matches!(verdict, sethu::verify::outcome::StageVerdict::InvalidRed(_)),
        "a stopped stub must give invalid_red, got {verdict:?}"
    );
}

#[test]
fn unknown_check_name_fails_with_known_names() {
    let area = tempfile::tempdir().unwrap();
    let repo = area.path().join("consumer");
    let manifest = write_manifest(area.path(), &repo, "base", "patched");
    sethu()
        .args(["verify", "--manifest"])
        .arg(&manifest)
        .args(["--check", "ghost"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ghost"))
        .stderr(predicate::str::contains("random-picker"));
}

#[test]
fn missing_manifest_flag_is_a_usage_error() {
    sethu()
        .args(["verify"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--manifest"));
}

#[test]
fn wrong_spec_identity_refuses_before_stages() {
    let _clone = clone_demo();
    let repo = _clone.path().join("consumer");
    let baseline = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let area = tempfile::tempdir().unwrap();
    write_harness(area.path());
    let manifest = write_manifest(area.path(), &repo, &baseline, &baseline);
    let mut value: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    value["spec_new_sha256"] = json!("0".repeat(64));
    std::fs::write(&manifest, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    sethu()
        .args(["verify", "--manifest"])
        .arg(&manifest)
        .assert()
        .failure()
        .stderr(predicate::str::contains("pins"));
    assert!(
        !area.path().join("runs").exists(),
        "a refused run must leave no artefacts"
    );
}

#[test]
fn relative_manifest_path_verifies_without_consumer_litter() {
    let _guard = HEAVY.lock().unwrap();
    let _clone = clone_demo();
    let repo = _clone.path().join("consumer");
    let baseline = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let patched = commit_edit(
        &repo,
        "src/lib.rs",
        CORRECT_PATCH_OLD,
        CORRECT_PATCH_NEW,
        "give random search its own array path",
    );
    let area = tempfile::tempdir().unwrap();
    write_harness(area.path());
    write_manifest(area.path(), &repo, &baseline, &patched);

    sethu()
        .args(["verify", "--freeze", "--manifest", "verify-manifest.json"])
        .current_dir(area.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("frozen"));

    sethu()
        .args(["verify", "--manifest", "verify-manifest.json"])
        .current_dir(area.path())
        .assert()
        .code(0)
        .stdout(predicate::str::contains("random-picker: verified"));

    let (run_dir, record) = only_run(area.path());
    for check in record["checks"].as_array().unwrap() {
        assert_eq!(check["verified"], true, "{check}");
    }
    let red = stage_record(&run_dir, "random-picker", "original-new");
    assert_eq!(red["verdict"], "expected-red");
    assert!(
        !red["cases"].as_array().unwrap().is_empty(),
        "the red stage must run cases instead of reporting zero parsed"
    );
    assert!(
        !repo.join("runs").exists(),
        "a relative manifest must not plant run directories in the consumer"
    );
    let list = git(&repo, &["worktree", "list", "--porcelain"]);
    let trees = list
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count();
    assert_eq!(trees, 1, "no stage worktree may linger: {list}");
}
