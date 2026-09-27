//! Shared access to the demo consumer for integration tests.
//!
//! The demo consumer lives in its own repository. The environment
//! variable `SETHU_DEMO_REPO` names it as a local path or a git URL.
//! Tests that need it skip with a named reason when the variable is
//! unset or empty, so a plain test run passes on a fresh clone.
//! Under CI the same condition fails the test instead. A skipped test
//! counts as a pass, so a silent skip there would hide the workflow.

use std::path::{Path, PathBuf};

/// Environment variable naming the demo consumer repository.
pub const DEMO_REPO_ENV: &str = "SETHU_DEMO_REPO";

/// Reviewed demo consumer commit every test starts from.
pub const DEMO_COMMIT: &str = "1577abbecb68e36b4064a5e5d9447c267aa1b415";

/// Environment variable that CI runners set to mark an automated build.
pub const CI_ENV: &str = "CI";

/// Report whether the tests run under CI.
///
/// GitHub Actions and most other runners set `CI=true`. An empty value,
/// `false` or `0` counts as a local run.
pub fn running_in_ci() -> bool {
    std::env::var(CI_ENV)
        .map(|value| {
            let value = value.trim();
            !value.is_empty() && value != "false" && value != "0"
        })
        .unwrap_or(false)
}

/// Skip the calling test with a named reason, or fail it under CI.
///
/// A local run prints a `SKIP` line to stderr and the caller returns
/// early. Under CI the test panics with the same reason, since nextest
/// reports an early return as a pass and hides its stderr.
pub fn skip(test_name: &str, reason: &str) {
    assert!(
        !running_in_ci(),
        "{test_name} cannot skip under CI: {reason}. Provide the missing prerequisite to the test step."
    );
    eprintln!("SKIP {test_name}: {reason}");
}

/// Subdirectory of cargo's integration test scratch area that holds the
/// demo consumer's build outputs.
const DEMO_TARGET: &str = "demo-consumer-target";

/// Lock file beside [`DEMO_TARGET`] that gives one test at a time the
/// shared build directory.
const DEMO_LOCK: &str = "demo-consumer-target.lock";

/// Build directory every demo test hands to cargo as `CARGO_TARGET_DIR`.
///
/// The path is fixed under Sethu's own `target/`, so it holds one demo
/// consumer build, never grows per run, and `cargo clean` removes it.
/// Every test reuses the compiled dependencies. Only the consumer crate
/// itself rebuilds when a stage's sources differ. A test hands it to
/// cargo only while it holds a [`DemoBuild`].
pub fn demo_target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(DEMO_TARGET)
}

/// Lock file that [`DemoBuild`] holds.
pub fn demo_lock_path() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(DEMO_LOCK)
}

/// Exclusive hold on the shared demo build directory.
///
/// Nextest runs every test in its own process, so the hold is an
/// operating system file lock rather than an in-process mutex. The
/// lock is released when the value drops or the process dies.
///
/// Cargo's own build lock only takes turns per build. It does not stop a
/// stale reuse. Cargo judges the consumer crate fresh when its sources
/// are older than the last output, whatever directory they sit in. A
/// clone made before another test's build would then run that test's
/// binary. Taking this hold before cloning keeps every source tree newer
/// than every output a different test wrote.
pub struct DemoBuild {
    _lock: std::fs::File,
}

impl DemoBuild {
    /// Wait for the shared demo build directory and hold it.
    ///
    /// Call this before [`DemoRepo::checkout`], and keep the value alive
    /// until the test's last cargo build finishes.
    pub fn acquire() -> DemoBuild {
        let path = demo_lock_path();
        let scratch = Path::new(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(scratch)
            .unwrap_or_else(|error| panic!("step create {} failed: {error}", scratch.display()));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap_or_else(|error| panic!("step open {} failed: {error}", path.display()));
        file.lock()
            .unwrap_or_else(|error| panic!("step lock {} failed: {error}", path.display()));
        DemoBuild { _lock: file }
    }
}

/// Resolved location of the demo consumer repository.
pub struct DemoRepo {
    source: String,
}

/// Resolve the demo consumer, or skip the calling test with a named reason.
///
/// Under CI a missing variable fails the test through [`skip`].
///
/// A value naming an existing local directory is made absolute, since
/// the clone runs from a scratch directory. Any other value passes to
/// git unchanged as a URL.
pub fn demo_repo(test_name: &str) -> Option<DemoRepo> {
    let value = std::env::var(DEMO_REPO_ENV).unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        skip(
            test_name,
            &format!("`{DEMO_REPO_ENV}` is not set to the demo consumer repository"),
        );
        return None;
    }
    let local = PathBuf::from(value);
    let source = match local.canonicalize() {
        Ok(path) if path.is_dir() => path.to_string_lossy().into_owned(),
        _ => value.to_string(),
    };
    Some(DemoRepo { source })
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

impl DemoRepo {
    /// Clone the demo consumer into a scratch directory at the reviewed commit.
    ///
    /// The copy sits at `consumer` under the returned directory. The clone
    /// carries the full history, and its default branch is reset to the
    /// reviewed commit. Baseline and patched commits therefore resolve
    /// exactly like they would in the source checkout. The source itself
    /// is never written.
    ///
    /// A test that builds the copy takes [`DemoBuild::acquire`] first.
    pub fn checkout(&self) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("hold the consumer copy");
        let target = dir.path().join("consumer");
        let output = std::process::Command::new("git")
            .arg("clone")
            .arg("-q")
            .arg(&self.source)
            .arg(&target)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|error| panic!("step clone the demo consumer cannot start: {error}"));
        assert!(
            output.status.success(),
            "step clone the demo consumer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        git(&target, &["reset", "-q", "--hard", DEMO_COMMIT]);
        let head = git(&target, &["rev-parse", "HEAD"]);
        assert_eq!(
            head.trim(),
            DEMO_COMMIT,
            "step pin the demo consumer must land on the reviewed commit"
        );
        git(&target, &["config", "user.email", "test@example.com"]);
        git(&target, &["config", "user.name", "Test"]);
        dir
    }
}
