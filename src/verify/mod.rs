//! Attributable verification runs for one migration attempt.
//!
//! A verification manifest names the consumer repository, the baseline and
//! patched commits, the harness directory, the scenario directories for
//! each contract version, the pinned spec hashes, and one entry per named
//! check. The runner executes three stages per check (original against
//! old fixtures, original against new fixtures, patched against new
//! fixtures), each with its own fresh stub instance and its own trace.
//! Every stage result is computed from the structured test result plus
//! the stub trace, and every artefact lands under `runs/<run-id>/` next
//! to the manifest.
//!
//! ## Manifest shape
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "repo": "/path/to/consumer",
//!   "baseline_commit": "abc123",
//!   "patched_commit": "def456",
//!   "harness": "harness",
//!   "scenarios_old": "harness/scenarios/old",
//!   "scenarios_new": "harness/scenarios/new",
//!   "spec_old_sha256": "ff2d…",
//!   "spec_new_sha256": "a5c9…",
//!   "checks": [
//!     {
//!       "name": "random-picker",
//!       "role": "regression",
//!       "change_ids": ["vc1_…"],
//!       "test": "verify_random_picker",
//!       "expected_diagnostic": "random picker ids",
//!       "expected_exchange": [
//!         {"scenario": "random-search", "method": "POST", "path": "/search/random"}
//!       ]
//!     },
//!     {
//!       "name": "smart-guard",
//!       "role": "guard",
//!       "change_ids": ["vc1_…"],
//!       "test": "verify_smart_guard",
//!       "expected_exchange": [
//!         {"scenario": "smart-search", "method": "POST", "path": "/search/smart"}
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! Relative paths resolve against the directory holding the manifest
//! file. `expected_diagnostic` is either a substring or an object with
//! one `regex` key. Only `regression` checks declare it.
//!
//! ## Harness layout
//!
//! The harness directory holds `tests/*.rs` integration tests plus the
//! scenario directories named by the manifest. Freezing hashes every
//! file under the harness directory. Staging copies the harness into
//! the application worktree except for the two scenario directories,
//! which the driver reads directly to feed each stage's own stub.

/// Verification manifest, freeze records, and harness hashing.
pub mod manifest;
/// Structured test results plus stub-trace correlation.
pub mod outcome;
/// Worktrees, stub instances, test processes, and run artefacts.
pub mod runner;
