//! Scripted run of the whole upgrade workflow without assistance.
//!
//! The test drives the built binary through the workflow order on one
//! throwaway clone of the real demo consumer: capabilities, install,
//! init with the pinned specs, changes, context preparation for every
//! group, tracer style records, harness freeze, the checked in correct
//! repair to a green verification, a default masking probe, the naive
//! repair to a failed guard, and finally the blocked check and the
//! report. The live demo checkout is never modified. The test needs
//! the released diff binary on PATH and skips with a named reason
//! without it. One consumer copy serves the whole script.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use assert_cmd::Command;
use serde_json::{Value, json};

/// Live demo consumer checkout the test clones but never modifies.
const DEMO_REPO: &str = "/home/nryn/work/sethu-demo-picker";

/// Name of the single scripted workflow test, used in skip messages.
const TEST_NAME: &str = "full_workflow_without_assistance";

/// Lock that serialises the heavy cargo-building workflow.
static HEAVY: Mutex<()> = Mutex::new(());

/// Shared target directory reused by every cargo build the script starts.
///
/// The static publishes the path only. The directory itself is owned by
/// a cleanup guard held for the whole run, never by this static, since
/// a static value is never dropped and would strand the directory.
static TARGET: OnceLock<PathBuf> = OnceLock::new();

/// Cleanup guard for the shared target directory.
///
/// Dropping the guard removes the directory, so no cargo-sized dir
/// survives the run, even when the workflow fails partway. Crash
/// leftovers keep their process id in the name and stay safe to delete.
struct TargetCleanup {
    path: PathBuf,
}

impl Drop for TargetCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Shared target path for every cargo build the script starts.
///
/// The path is scoped to this process id, so concurrent runs never
/// share a directory.
fn shared_target_dir() -> PathBuf {
    TARGET
        .get_or_init(|| {
            std::env::temp_dir().join(format!("sethu-e2e-target-{}", std::process::id()))
        })
        .clone()
}

/// Publish the shared target path and hand back its cleanup guard.
///
/// The caller holds the guard until the run ends. Dropping it removes
/// the directory.
fn hold_shared_target_dir() -> TargetCleanup {
    let path = shared_target_dir();
    std::fs::create_dir_all(&path).expect("keep a shared cargo target directory");
    TargetCleanup { path }
}

/// Build the test command for the sethu binary with a shared target dir.
///
/// The directory flows into verification stages through the environment,
/// so incremental builds stay cheap across every stage and probe.
fn sethu() -> Command {
    let mut cmd = Command::cargo_bin("sethu").expect("locate the sethu binary");
    cmd.env("CARGO_TARGET_DIR", shared_target_dir());
    cmd
}

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

/// Run one git command in a directory and keep stdout on success.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("step git {args:?} cannot start: {error}"));
    assert!(
        output.status.success(),
        "step git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("read git stdout as text")
}

/// Clone the demo consumer into a scratch directory.
///
/// The clone carries the full history, so baseline and patched commits
/// resolve exactly like they would in the live checkout. The live
/// checkout itself is never written.
fn clone_demo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("hold the consumer copy");
    let target = dir.path().join("consumer");
    let output = std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(DEMO_REPO)
        .arg(&target)
        .current_dir(dir.path())
        .output()
        .unwrap_or_else(|error| panic!("step clone the demo consumer cannot start: {error}"));
    assert!(
        output.status.success(),
        "step clone the demo consumer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    git(&target, &["config", "user.email", "test@example.com"]);
    git(&target, &["config", "user.name", "Test"]);
    dir
}

/// Run one workflow step that must succeed and return its streams.
///
/// The step name appears in the failure message, so a regression
/// points at the first broken command instead of a bare exit code.
fn step(repo: &Path, name: &str, args: &[&str]) -> (String, String) {
    let decision = sethu().current_dir(repo).args(args).assert();
    let output = decision.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "step {name} exited {}: stdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    (stdout, stderr)
}

/// Run one workflow step that must fail with an exit code and streams.
fn step_fails(repo: &Path, name: &str, args: &[&str], code: i32) -> (String, String) {
    let decision = sethu().current_dir(repo).args(args).assert();
    let output = decision.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(code),
        "step {name} exited {}: stdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    (stdout, stderr)
}

/// Record one outcome and fail loudly when the ledger refuses it.
fn record(repo: &Path, id: &str, outcome: &str, evidence: &[&str], note: Option<&str>) {
    let mut args = vec!["record", id, "--outcome", outcome];
    for reference in evidence {
        args.push("--evidence");
        args.push(reference);
    }
    if let Some(text) = note {
        args.push("--note");
        args.push(text);
    }
    let (stdout, _) = step(repo, &format!("record {id} as {outcome}"), &args);
    assert!(
        stdout.contains("recorded") && stdout.contains(outcome),
        "step record {id} as {outcome} printed no confirmation: {stdout}"
    );
}

/// Required change ids in capture report order with their endpoints.
fn required_changes(repo: &Path) -> Vec<sethu::vimanam::ChangeRecord> {
    let root = sethu::state::layout::state_root(repo);
    let dir = sethu::state::layout::migrations_dir(&root);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .expect("list stored migrations")
        .map(|entry| {
            entry
                .expect("read a migration entry")
                .file_name()
                .to_str()
                .expect("read a migration name")
                .to_string()
        })
        .collect();
    assert_eq!(names.len(), 1, "expected one migration, found {names:?}");
    let migration = sethu::state::layout::migration_dir(&root, &names[0]);
    let manifest: sethu::state::attempt::AttemptRecord =
        sethu::state::read_state_file(&sethu::state::layout::manifest_path(&migration))
            .expect("read the attempt manifest");
    let pair =
        sethu::state::layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash)
            .expect("locate the spec pair");
    let capture = sethu::state::layout::capture_dir(&pair, &manifest.capture_id);
    let raw = std::fs::read(sethu::state::layout::changes_file(&capture))
        .expect("read the stored change list");
    let document = sethu::vimanam::parse_diff_output(&raw).expect("parse the stored change list");
    document
        .changes
        .into_iter()
        .filter(|item| {
            matches!(
                item.severity,
                sethu::vimanam::Severity::Breaking | sethu::vimanam::Severity::Review
            )
        })
        .collect()
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
fn write_harness(migration: &Path, old_sha: &str, new_sha: &str, random_ids: &[String]) {
    let harness = migration.join("harness");
    let tests = harness.join("tests");
    let old = harness.join("scenarios").join("old");
    let next = harness.join("scenarios").join("new");
    for dir in [&tests, &old, &next] {
        std::fs::create_dir_all(dir).expect("create a harness directory");
    }
    std::fs::write(tests.join("harness_checks.rs"), HARNESS_TESTS)
        .expect("write the harness tests");
    let changes: Vec<String> = random_ids.to_vec();
    let scenario = |id: &str, sha: &str, request: Value, response: Value| {
        json!({
            "id": id,
            "change_ids": changes,
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
            old_sha,
            post("/search/random", Some(json!({"page": 1}))),
            search_response(&["asset-r1", "asset-r2"]),
        ))
        .expect("encode the old random scenario"),
    )
    .expect("write the old random scenario");
    std::fs::write(
        old.join("smart.json"),
        serde_json::to_vec_pretty(&scenario(
            "smart-search",
            old_sha,
            post("/search/smart", Some(json!({"query": "sunset"}))),
            search_response(&["asset-s1"]),
        ))
        .expect("encode the old smart scenario"),
    )
    .expect("write the old smart scenario");
    std::fs::write(
        old.join("metadata.json"),
        serde_json::to_vec_pretty(&scenario(
            "metadata-search",
            old_sha,
            post(
                "/search/metadata",
                Some(json!({"originalFileName": "photo-0.jpg"})),
            ),
            search_response(&["asset-m1"]),
        ))
        .expect("encode the old metadata scenario"),
    )
    .expect("write the old metadata scenario");
    std::fs::write(
        next.join("random.json"),
        serde_json::to_vec_pretty(&scenario(
            "random-search",
            new_sha,
            post("/search/random", None),
            asset_array(&["asset-r1", "asset-r2"]),
        ))
        .expect("encode the new random scenario"),
    )
    .expect("write the new random scenario");
    std::fs::write(
        next.join("smart.json"),
        serde_json::to_vec_pretty(&scenario(
            "smart-search",
            new_sha,
            post("/search/smart", Some(json!({"query": "sunset"}))),
            search_response(&["asset-s1"]),
        ))
        .expect("encode the new smart scenario"),
    )
    .expect("write the new smart scenario");
    std::fs::write(
        next.join("metadata.json"),
        serde_json::to_vec_pretty(&scenario(
            "metadata-search",
            new_sha,
            post(
                "/search/metadata",
                Some(json!({"originalFileName": "photo-0.jpg"})),
            ),
            search_response(&["asset-m1"]),
        ))
        .expect("encode the new metadata scenario"),
    )
    .expect("write the new metadata scenario");
}

/// Write a verification manifest naming the three picker checks.
#[allow(clippy::too_many_arguments)]
fn write_manifest(
    migration: &Path,
    name: &str,
    repo: &Path,
    baseline: &str,
    patched: &str,
    old_sha: &str,
    new_sha: &str,
    random_ids: &[String],
) -> PathBuf {
    let manifest = json!({
        "schema_version": 1,
        "repo": repo.display().to_string(),
        "baseline_commit": baseline,
        "patched_commit": patched,
        "harness": "harness",
        "scenarios_old": "harness/scenarios/old",
        "scenarios_new": "harness/scenarios/new",
        "spec_old_sha256": old_sha,
        "spec_new_sha256": new_sha,
        "checks": [
            {
                "name": "random-picker",
                "role": "regression",
                "change_ids": random_ids,
                "test": "verify_random_picker",
                "expected_diagnostic": "random picker ids",
                "expected_exchange": [
                    {"scenario": "random-search", "method": "POST", "path": "/search/random"}
                ]
            },
            {
                "name": "smart-guard",
                "role": "guard",
                "change_ids": [],
                "test": "verify_smart_guard",
                "expected_exchange": [
                    {"scenario": "smart-search", "method": "POST", "path": "/search/smart"}
                ]
            },
            {
                "name": "metadata-guard",
                "role": "guard",
                "change_ids": [],
                "test": "verify_metadata_guard",
                "expected_exchange": [
                    {"scenario": "metadata-search", "method": "POST", "path": "/search/metadata"}
                ]
            }
        ]
    });
    let path = migration.join(name);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&manifest).expect("encode the manifest"),
    )
    .expect("write the verification manifest");
    path
}

/// List stored run ids beside a manifest file.
fn run_ids(manifest: &Path) -> Vec<String> {
    let dir = manifest
        .parent()
        .expect("read the manifest parent")
        .join("runs");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|_| panic!("list stored runs in {}", dir.display()))
        .map(|entry| {
            entry
                .expect("read a run entry")
                .file_name()
                .to_str()
                .expect("read a run name")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// Serve a new-contract object with no asset list and prove the picker
/// check still fails on its assertion message.
///
/// The consumer parser defaults missing fields, so an object without
/// an asset list could look like an empty page. The probe runs the
/// real harness check against exactly that body and requires the
/// failure to carry the expected signature instead of passing.
fn probe_default_masking(repo: &Path, baseline: &str, harness_tests: &Path) {
    let area = tempfile::tempdir().expect("hold the probe stub trace");
    let trace_path = area.path().join("requests.jsonl");
    let spec: Value = serde_json::from_slice(
        &std::fs::read(fixture("immich/new.json")).expect("read the pinned new spec"),
    )
    .expect("parse the pinned new spec");
    let scenario = sethu::stub::Scenario {
        id: "random-search".to_string(),
        change_ids: Vec::new(),
        request: sethu::stub::ScenarioRequest {
            method: "POST".to_string(),
            path: "/search/random".to_string(),
            query: indexmap::IndexMap::new(),
            headers: indexmap::IndexMap::new(),
            body: None,
        },
        response: sethu::stub::ScenarioResponse {
            status: 200,
            headers: indexmap::IndexMap::new(),
            body: Some(json!({
                "albums": {"count": 0, "facets": [], "items": [], "total": 0}
            })),
        },
        unsupported: Vec::new(),
        supports_claim: false,
    };
    let server =
        sethu::stub::server::StubServer::start(0, vec![scenario], spec, "new", trace_path.clone())
            .expect("start the probe stub on loopback");
    let port = server.local_addr().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let served = std::thread::spawn(move || server.serve_until(&flag));

    let worktree = area.path().join("probe-worktree");
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().expect("render the probe worktree path"),
            baseline,
        ],
    );
    std::fs::create_dir_all(worktree.join("tests")).expect("create the probe tests directory");
    std::fs::copy(
        harness_tests,
        worktree.join("tests").join("harness_checks.rs"),
    )
    .expect("overlay the harness tests into the probe worktree");
    let output = std::process::Command::new("cargo")
        .arg("test")
        .arg("--")
        .arg("--exact")
        .arg("verify_random_picker")
        .current_dir(&worktree)
        .env("IMMICH_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TARGET_DIR", shared_target_dir())
        .output()
        .expect("run the probe picker check");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    served
        .join()
        .expect("stop the probe stub thread")
        .expect("serve the probe exchange cleanly");
    git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            worktree.to_str().expect("render the probe worktree path"),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "step probe the default masking body passed the picker check, want a failure: stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("random picker ids") || stderr.contains("random picker ids"),
        "step probe the default masking body failed without the expected signature: stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while sethu::stub::read_trace(&trace_path)
        .map(|entries| entries.is_empty())
        .unwrap_or(true)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "step probe the default masking body left no stub trace in {}",
            trace_path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let trace = sethu::stub::read_trace(&trace_path).expect("read the probe stub trace");
    assert_eq!(
        trace.len(),
        1,
        "step probe the default masking body traced {} exchanges, want one",
        trace.len()
    );
    let entry = &trace[0];
    assert_eq!(entry.scenario_id.as_deref(), Some("random-search"));
    assert!(entry.request_valid, "the probe request must validate");
    assert!(
        entry.response_written,
        "the probe response must be fully written"
    );
}

/// Dropping the cleanup guard removes its directory.
#[test]
fn shared_target_cleanup_removes_its_directory() {
    let _serial = HEAVY.lock().expect("hold the workflow lock");
    let probe =
        std::env::temp_dir().join(format!("sethu-e2e-cleanup-probe-{}", std::process::id()));
    {
        std::fs::create_dir_all(&probe).expect("create the probe directory");
        let _cleanup = TargetCleanup {
            path: probe.clone(),
        };
    }
    assert!(
        !probe.exists(),
        "dropping the cleanup guard must remove the directory"
    );
}

#[test]
fn full_workflow_without_assistance() {
    if !vimanam_available() {
        eprintln!("SKIP {TEST_NAME}: `vimanam` is not on PATH");
        return;
    }
    let _guard = HEAVY.lock().expect("hold the workflow lock");
    let _target = hold_shared_target_dir();

    let holder = clone_demo();
    let repo = holder
        .path()
        .join("consumer")
        .canonicalize()
        .expect("resolve the consumer copy");

    let (stdout, _) = step(&repo, "capabilities", &["capabilities", "--json"]);
    let capabilities: Value =
        serde_json::from_str(&stdout).expect("parse the capabilities document");
    assert_eq!(
        capabilities["commands"]["install"]["status"], "available",
        "step capabilities must report an installable workflow"
    );

    let (stdout, _) = step(
        &repo,
        "install",
        &["install", repo.to_str().expect("render the repo")],
    );
    assert!(
        stdout.contains("installed into"),
        "step install printed no destination: {stdout}"
    );
    assert!(
        repo.join(".bob/commands/api-upgrade.md").is_file(),
        "step install left no command file"
    );
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "record the pack installation",
        ],
    );
    let baseline = git(&repo, &["rev-parse", "HEAD"]);
    let baseline = baseline.trim().to_string();

    let old = fixture("immich/old.json");
    let new = fixture("immich/new.json");
    let (stdout, _) = step(
        &repo,
        "init",
        &[
            "init",
            old.to_str().expect("render the old spec"),
            new.to_str().expect("render the new spec"),
            "--repo",
            repo.to_str().expect("render the repo"),
        ],
    );
    assert!(
        stdout.contains("created"),
        "step init printed no creation: {stdout}"
    );

    let required = required_changes(&repo);
    assert_eq!(required.len(), 27, "expected 27 required changes");
    let mut random_response = Vec::new();
    let mut page_removal = Vec::new();
    let mut image = Vec::new();
    for item in &required {
        let is_random = item.endpoint.method == "POST" && item.endpoint.path == "/search/random";
        let is_response = matches!(item.kind, sethu::vimanam::ChangeKind::ResponseSchemaChanged);
        if is_random && is_response {
            random_response.push(item.id.clone());
        } else if is_random {
            page_removal.push(item.id.clone());
        } else {
            image.push(item.id.clone());
        }
    }
    assert_eq!(
        random_response.len(),
        4,
        "expected four random response records"
    );
    assert_eq!(page_removal.len(), 1, "expected one page removal record");
    assert_eq!(image.len(), 22, "expected twenty two image config records");

    let (stdout, _) = step(&repo, "changes", &["changes"]);
    assert!(
        stdout.contains("27 changes"),
        "step changes hid required work: {stdout}"
    );
    let (stdout, _) = step(&repo, "changes json", &["changes", "--json"]);
    let document: Value = serde_json::from_str(&stdout).expect("parse the changes document");
    assert_eq!(document["required"]["change_count"], 27);
    let groups = document["required"]["group_count"]
        .as_u64()
        .expect("read the required group count") as usize;
    assert!(groups >= 2, "expected several groups, found {groups}");

    let first = required[0].id.clone();
    for number in 1..=groups {
        let (stdout, _) = step(
            &repo,
            &format!("context prepare group {number}"),
            &["context", &first, "--prepare", &number.to_string()],
        );
        assert!(
            stdout.contains("prepared"),
            "step context prepare group {number} printed no confirmation: {stdout}"
        );
    }
    let root = sethu::state::layout::state_root(&repo);
    let migration = {
        let dir = sethu::state::layout::migrations_dir(&root);
        let names: Vec<String> = std::fs::read_dir(&dir)
            .expect("list stored migrations")
            .map(|entry| {
                entry
                    .expect("read a migration entry")
                    .file_name()
                    .to_str()
                    .expect("read a migration name")
                    .to_string()
            })
            .collect();
        assert_eq!(names.len(), 1);
        sethu::state::layout::migration_dir(&root, &names[0])
    };
    let context_dir = sethu::state::layout::context_dir(&migration);
    let mut prepared: Vec<String> = std::fs::read_dir(&context_dir)
        .unwrap_or_else(|_| panic!("list prepared context in {}", context_dir.display()))
        .map(|entry| {
            entry
                .expect("read a context entry")
                .file_name()
                .to_str()
                .expect("read a context name")
                .to_string()
        })
        .collect();
    prepared.sort();
    let mut wanted: Vec<String> = required.iter().map(|item| item.id.clone()).collect();
    wanted.sort();
    assert_eq!(
        prepared, wanted,
        "prepared context must cover every required change"
    );
    assert!(
        context_dir.join(&first).join("record.json").is_file(),
        "prepared context holds no stored record"
    );

    std::fs::write(
        repo.join("trace-notes.md"),
        "inspected src, wrappers, generated clients, and tests\n",
    )
    .expect("write the search artefact");
    for id in &random_response {
        record(
            &repo,
            id,
            "unresolved",
            &["trace-notes.md"],
            Some("random search repair is pending verification"),
        );
    }
    for id in &image {
        record(&repo, id, "no_usage_found", &["trace-notes.md"], None);
    }
    record(
        &repo,
        &page_removal[0],
        "decision_required",
        &["trace-notes.md"],
        Some(
            "Random search no longer pages. Drop paging and accept one batch, make repeated calls and remove duplicates, or switch endpoints for stable paging. Decide the picker more behaviour.",
        ),
    );
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "record the tracer findings",
        ],
    );

    let manifest: sethu::state::attempt::AttemptRecord =
        sethu::state::read_state_file(&sethu::state::layout::manifest_path(&migration))
            .expect("read the attempt manifest");
    git(&repo, &["checkout", "-q", "-b", "sethu/upgrade-demo"]);
    let applied = std::process::Command::new("git")
        .arg("apply")
        .arg(fixture("e2e/correct.patch"))
        .current_dir(&repo)
        .output()
        .expect("apply the checked in correct repair");
    assert!(
        applied.status.success(),
        "step apply the correct repair failed: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "give random search its own array path",
        ],
    );
    let patched = git(&repo, &["rev-parse", "HEAD"]);
    let patched = patched.trim().to_string();

    write_harness(
        &migration,
        &manifest.old_spec_hash,
        &manifest.new_spec_hash,
        &random_response,
    );
    let manifest_path = write_manifest(
        &migration,
        "verify-manifest.json",
        &repo,
        &baseline,
        &patched,
        &manifest.old_spec_hash,
        &manifest.new_spec_hash,
        &random_response,
    );
    let (stdout, _) = step(
        &repo,
        "verify freeze",
        &[
            "verify",
            "--freeze",
            "--manifest",
            manifest_path.to_str().expect("render the manifest path"),
        ],
    );
    assert!(
        stdout.contains("frozen"),
        "step verify freeze printed no confirmation: {stdout}"
    );
    assert!(
        Path::new(&format!("{}.frozen", manifest_path.display())).is_file(),
        "step verify freeze left no freeze file"
    );
    let (stdout, _) = step(
        &repo,
        "verify",
        &[
            "verify",
            "--manifest",
            manifest_path.to_str().expect("render the manifest path"),
        ],
    );
    assert!(
        stdout.contains("random-picker: verified"),
        "step verify left the regression unverified: {stdout}"
    );
    assert!(
        stdout.contains("smart-guard: verified") && stdout.contains("metadata-guard: verified"),
        "step verify left a guard unverified: {stdout}"
    );
    assert!(
        stdout.contains("expected-red"),
        "step verify showed no red stage: {stdout}"
    );
    assert_eq!(run_ids(&manifest_path).len(), 1, "expected one stored run");

    probe_default_masking(
        &repo,
        &baseline,
        &migration.join("harness/tests/harness_checks.rs"),
    );

    let run_id = run_ids(&manifest_path)[0].clone();
    let run_ref = format!("run:{run_id}/random-picker");
    for id in &random_response {
        record(
            &repo,
            id,
            "fixed_and_verified",
            &[&run_ref, "src/lib.rs:1"],
            None,
        );
    }

    git(&repo, &["checkout", "-q", "sethu/upgrade-demo~1"]);
    git(&repo, &["checkout", "-q", "-b", "sethu/naive-demo"]);
    let applied = std::process::Command::new("git")
        .arg("apply")
        .arg(fixture("e2e/naive.patch"))
        .current_dir(&repo)
        .output()
        .expect("apply the checked in naive repair");
    assert!(
        applied.status.success(),
        "step apply the naive repair failed: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "parse every search as an array",
        ],
    );
    let naive = git(&repo, &["rev-parse", "HEAD"]);
    let naive = naive.trim().to_string();
    let naive_manifest = write_manifest(
        &migration,
        "verify-manifest-naive.json",
        &repo,
        &baseline,
        &naive,
        &manifest.old_spec_hash,
        &manifest.new_spec_hash,
        &random_response,
    );
    let (stdout, _) = step(
        &repo,
        "verify freeze naive",
        &[
            "verify",
            "--freeze",
            "--manifest",
            naive_manifest.to_str().expect("render the naive manifest"),
        ],
    );
    assert!(stdout.contains("frozen"));
    let before = run_ids(&naive_manifest);
    let (stdout, _) = step_fails(
        &repo,
        "verify naive",
        &[
            "verify",
            "--manifest",
            naive_manifest.to_str().expect("render the naive manifest"),
        ],
        4,
    );
    assert!(
        stdout.contains("NOT VERIFIED"),
        "step verify naive passed the shared parser repair: {stdout}"
    );
    let after = run_ids(&naive_manifest);
    let fresh: Vec<String> = after
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(fresh.len(), 1, "expected one naive run, found {fresh:?}");
    let mut guard_failed = false;
    for guard in ["smart-guard", "metadata-guard"] {
        let stage_path = naive_manifest
            .parent()
            .expect("read the manifest parent")
            .join("runs")
            .join(&fresh[0])
            .join(guard)
            .join("patched-new")
            .join("stage.json");
        let stage: Value =
            serde_json::from_slice(&std::fs::read(&stage_path).unwrap_or_else(|_| {
                panic!("read the naive {guard} stage in {}", stage_path.display())
            }))
            .expect("parse the naive guard stage");
        if stage["verdict"] == "failed" {
            guard_failed = true;
        }
    }
    assert!(
        guard_failed,
        "the naive repair must fail a guard on the patched commit"
    );
    git(&repo, &["checkout", "-q", "sethu/upgrade-demo"]);
    assert_eq!(
        git(&repo, &["rev-parse", "HEAD"]).trim(),
        patched,
        "the workflow must end on the repaired commit"
    );

    let (stdout, _) = step(&repo, "check", &["check"]);
    assert!(
        stdout.contains("accounted: yes"),
        "step check lost accounting: {stdout}"
    );
    assert!(
        stdout.contains("ready: no"),
        "step check reported a ready migration: {stdout}"
    );
    let (stdout, _) = step_fails(
        &repo,
        "check require ready",
        &["check", "--require-ready"],
        5,
    );
    assert!(
        stdout.contains("accounted: yes"),
        "step check require ready lost accounting: {stdout}"
    );
    assert!(
        stdout.contains("ready: no"),
        "step check require ready reported ready: {stdout}"
    );
    assert!(
        stdout.contains(&page_removal[0]),
        "step check require ready names no page decision: {stdout}"
    );

    let (stdout, _) = step(&repo, "report", &["report"]);
    assert!(
        stdout.contains("accounted: yes"),
        "step report lost accounting: {stdout}"
    );
    let markdown = std::fs::read_to_string(migration.join("reports/report.md"))
        .expect("read the markdown listing");
    let html = std::fs::read_to_string(migration.join("reports/report.html"))
        .expect("read the html listing");
    for id in &wanted {
        assert!(
            markdown.contains(id),
            "step report omits required change {id}"
        );
        assert!(
            html.contains(id),
            "step report html omits required change {id}"
        );
    }
    for word in ["fixed_and_verified", "no_usage_found", "decision_required"] {
        assert!(
            markdown.contains(word),
            "step report omits disposition {word}"
        );
    }
    assert!(
        markdown.contains(&run_id),
        "step report omits the verification run"
    );
}
