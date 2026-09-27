//! Shared access to the demo consumer for integration tests.
//!
//! The demo consumer lives in its own repository. The environment
//! variable `SETHU_DEMO_REPO` names it as a local path or a git URL.
//! Tests that need it skip with a named reason when the variable is
//! unset or empty, so a plain test run passes on a fresh clone.

use std::path::{Path, PathBuf};

/// Environment variable naming the demo consumer repository.
pub const DEMO_REPO_ENV: &str = "SETHU_DEMO_REPO";

/// Reviewed demo consumer commit every test starts from.
pub const DEMO_COMMIT: &str = "1577abbecb68e36b4064a5e5d9447c267aa1b415";

/// Resolved location of the demo consumer repository.
pub struct DemoRepo {
    source: String,
}

/// Resolve the demo consumer, or skip the calling test with a named reason.
///
/// A value naming an existing local directory is made absolute, since
/// the clone runs from a scratch directory. Any other value passes to
/// git unchanged as a URL.
pub fn demo_repo(test_name: &str) -> Option<DemoRepo> {
    let value = std::env::var(DEMO_REPO_ENV).unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        eprintln!("SKIP {test_name}: `{DEMO_REPO_ENV}` is not set to the demo consumer repository");
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
