//! Worktrees, stub instances, test processes, and run artefacts.
//!
//! One invocation runs three stages per selected check: the baseline
//! commit against old fixtures, the baseline commit against new
//! fixtures, and the patched commit against new fixtures. Each stage
//! gets a detached application worktree with the frozen harness
//! overlaid, plus its own fresh stub instance and trace. Results land
//! under `runs/<run-id>/` beside the manifest, one directory per
//! check and stage, with the test streams, the JUnit report when
//! nextest ran, the stub trace, and a JSON record of everything the
//! verdict rested on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::cli::{SpecVersion, VerifyArgs};
use crate::stub::server::StubServer;
use crate::verify::manifest::{
    CheckSpec, ExpectedDiagnostic, ExpectedExchange, Manifest, Role, freeze_harness, hash_harness,
    load_manifest, read_freeze, select_checks,
};
use crate::verify::outcome::{
    CaseResult, ParserKind, StageVerdict, TestRun, evaluate, looks_like_build_failure, parse_junit,
    parse_libtest, parser_name, stage_met,
};

/// Seconds allowed per test process before it is killed.
const STAGE_TIMEOUT_SECS: u64 = 600;

/// Milliseconds to wait for a settled stub trace after the tests end.
const TRACE_SETTLE_MS: u64 = 5000;

/// Exit code when every selected check verified.
const EXIT_VERIFIED: u8 = 0;

/// Exit code when at least one selected check did not verify.
const EXIT_NOT_VERIFIED: u8 = 4;

/// One stage of the three-stage matrix.
struct StageDef {
    /// Directory name under the check directory.
    name: &'static str,
    /// Application commit checked out for this stage.
    commit: String,
    /// Contract version served by this stage stub.
    spec: SpecVersion,
    /// Scenario directory feeding this stage stub.
    scenarios: PathBuf,
    /// Whether a regression check must fail here with its diagnostic.
    want_red: bool,
}

/// Summary of one check across its three stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckSummary {
    /// Check name from the manifest.
    name: String,
    /// Check role from the manifest.
    role: String,
    /// Contract change records the check covered when the run started.
    ///
    /// The live manifest can change later, so the run keeps its own
    /// copy. Readers map a past run back to its records from here.
    #[serde(default)]
    change_ids: Vec<String>,
    /// Failure signature the red stage required.
    #[serde(default)]
    expected_diagnostic: Option<ExpectedDiagnostic>,
    /// Stub exchanges every stage of this check had to show.
    #[serde(default)]
    expected_exchange: Vec<ExpectedExchange>,
    /// Whether the three stages met the check matrix.
    verified: bool,
    /// Per-stage verdict words in stage order.
    stages: Vec<String>,
}

/// Run record stored as `runs/<run-id>/run.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunRecord {
    /// Unique run identifier and directory name.
    run_id: String,
    /// Manifest path as passed on the command line.
    manifest: String,
    /// Seconds since the Unix epoch when the run started.
    started_epoch_secs: u64,
    /// Seconds since the Unix epoch when the run finished.
    finished_epoch_secs: u64,
    /// Binary version that ran the verification.
    sethu_version: String,
    /// `git --version` output, trimmed.
    git_version: String,
    /// `cargo --version` output, trimmed.
    cargo_version: String,
    /// Nextest version, or a note that the fallback applied.
    nextest_version: String,
    /// Operating system and architecture plus process id.
    environment: String,
    /// Live harness content hash for this run.
    harness_hash: String,
    /// Frozen hash beside the manifest, when one existed.
    frozen_hash: Option<String>,
    /// Whether the harness moved since the freeze.
    harness_changed: bool,
    /// Earlier run ids with a different harness hash.
    superseded_runs: Vec<String>,
    /// Pinned old spec hash from the manifest.
    spec_old_sha256: String,
    /// Pinned new spec hash from the manifest.
    spec_new_sha256: String,
    /// Per-check summaries in manifest order.
    checks: Vec<CheckSummary>,
}

/// Stage record stored beside its test streams and stub trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StageRecord {
    /// Stage name (`original-old`, `original-new`, or `patched-new`).
    stage: String,
    /// Check name from the manifest.
    check: String,
    /// Contract change records the check covered when the run started.
    ///
    /// The live manifest can change later, so each stage keeps its
    /// own copy beside the verdict that rested on it.
    #[serde(default)]
    change_ids: Vec<String>,
    /// Failure signature this stage required, when one applied.
    #[serde(default)]
    expected_diagnostic: Option<ExpectedDiagnostic>,
    /// Stub exchanges this stage had to show.
    #[serde(default)]
    expected_exchange: Vec<ExpectedExchange>,
    /// Application commit checked out for this stage.
    commit: String,
    /// Contract version served by this stage stub.
    spec_version: String,
    /// Pinned spec hash for this stage.
    spec_sha256: String,
    /// Scenario directory feeding this stage stub.
    scenarios_dir: String,
    /// Per-file hashes of the scenario directory.
    scenario_files: Vec<ScenarioFileHash>,
    /// Live harness content hash for this run.
    harness_hash: String,
    /// Exact test command that ran.
    test_command: String,
    /// Which report parser produced the case list.
    parser: String,
    /// Process exit code, when the process exited.
    exit_code: Option<i32>,
    /// Signal that killed the process, when one did.
    signal: Option<i32>,
    /// Whether the run hit the deadline and was killed.
    timed_out: bool,
    /// Whether output shows the harness failed to build.
    build_failed: bool,
    /// Parsed per-test cases with their outputs.
    cases: Vec<StoredCase>,
    /// Verdict word for this stage.
    verdict: String,
    /// Verdict detail for non-green stages.
    detail: String,
    /// Seconds since the Unix epoch when the stage started.
    started_epoch_secs: u64,
    /// Seconds since the Unix epoch when the stage finished.
    finished_epoch_secs: u64,
}

/// One scenario file hash inside a stage record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScenarioFileHash {
    /// File name inside the scenario directory.
    name: String,
    /// Hex hash of the exact file bytes.
    sha256: String,
}

/// One stored test case inside a stage record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCase {
    /// Test case name as reported.
    name: String,
    /// Class or binary name as reported, when present.
    classname: String,
    /// Whether the case passed.
    passed: bool,
    /// Failure message plus captured output.
    output: String,
}

/// Outcome of one spawned process.
struct ProcessOutcome {
    /// Captured standard output.
    stdout: String,
    /// Captured standard error.
    stderr: String,
    /// Process exit code, when the process exited.
    exit_code: Option<i32>,
    /// Signal that killed the process, when one did.
    signal: Option<i32>,
    /// Whether the deadline fired first.
    timed_out: bool,
}

/// Entry point shared by the command module.
///
/// Freezing writes the freeze file first, then the run proceeds, so
/// one command can freeze and prove the frozen harness together.
pub fn run(args: &VerifyArgs, manifest_arg: Option<&Path>) -> anyhow::Result<ExitCode> {
    let Some(manifest_path) = manifest_arg.or(args.manifest.as_deref()) else {
        anyhow::bail!("pass --manifest PATH to name the verification manifest");
    };
    if args.freeze {
        let (manifest, _) = load_manifest(manifest_path)?;
        let frozen = freeze_harness(manifest_path, &manifest)?;
        println!("frozen {}", frozen.harness_hash);
        if args.check.is_empty() {
            return Ok(ExitCode::SUCCESS);
        }
    }
    let (manifest, manifest_dir) = load_manifest(manifest_path)?;
    let selected = select_checks(&manifest, manifest_path, &args.check)?;
    let frozen = read_freeze(manifest_path)?;
    let (live_hash, _) = hash_harness(&manifest.harness).map_err(|error| {
        anyhow::anyhow!("hash harness {}: {error:#}", manifest.harness.display())
    })?;
    let harness_changed = frozen
        .as_ref()
        .is_some_and(|frozen| frozen.harness_hash != live_hash);
    if harness_changed {
        println!("harness changed since the freeze; earlier runs are superseded");
    }
    refuse_dirty_tree(&manifest.repo)?;
    check_commit(&manifest.repo, &manifest.baseline_commit, "baseline")?;
    check_commit(&manifest.repo, &manifest.patched_commit, "patched")?;
    check_spec_identity(&manifest)?;
    // Stage worktree paths must stay absolute. Git resolves a relative
    // `worktree add` path against the consumer repository instead of
    // the process directory, so a relative manifest parent would plant
    // the checkout inside the consumer while cargo runs elsewhere.
    // The manifest loader returns an absolute directory for this.
    let runs_dir = manifest_dir.join(crate::verify::manifest::RUNS_DIR_NAME);
    let run_id = fresh_run_id();
    let run_dir = runs_dir.join(&run_id);
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("create run directory {}", run_dir.display()))?;
    let superseded = superseded_runs(&runs_dir, &run_id, &live_hash)?;
    println!("run {run_id}");
    let started = epoch_secs();
    let nextest = probe_nextest(&manifest.repo);
    let mut summaries = Vec::with_capacity(selected.len());
    let mut overall = true;
    for check in selected {
        let stages = stage_defs(&manifest, check);
        let check_dir = run_dir.join(&check.name);
        let mut words = Vec::with_capacity(stages.len());
        let mut verified = true;
        for stage in &stages {
            let verdict = run_stage(&manifest, check, stage, &check_dir, &nextest)?;
            let met = stage_met(&verdict, stage.want_red);
            words.push(verdict_word(&verdict).to_string());
            println!(
                "check {} {}: {}",
                check.name,
                stage.name,
                verdict_line(&verdict)
            );
            if !met {
                verified = false;
            }
        }
        println!(
            "check {}: {}",
            check.name,
            if verified { "verified" } else { "NOT VERIFIED" }
        );
        if !verified {
            overall = false;
        }
        summaries.push(summarize_check(check, verified, words));
    }
    let record = RunRecord {
        run_id: run_id.clone(),
        manifest: manifest_path.display().to_string(),
        started_epoch_secs: started,
        finished_epoch_secs: epoch_secs(),
        sethu_version: env!("CARGO_PKG_VERSION").to_string(),
        git_version: probe_tool(&manifest.repo, "git", &["--version"]),
        cargo_version: probe_tool(&manifest.repo, "cargo", &["--version"]),
        nextest_version: nextest.clone(),
        environment: format!(
            "{} {} pid={}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::process::id()
        ),
        harness_hash: live_hash,
        frozen_hash: frozen.map(|frozen| frozen.harness_hash),
        harness_changed,
        superseded_runs: superseded,
        spec_old_sha256: manifest.spec_old_sha256.clone(),
        spec_new_sha256: manifest.spec_new_sha256.clone(),
        checks: summaries,
    };
    let mut bytes = serde_json::to_vec_pretty(&record).with_context(|| "encode run record")?;
    bytes.push(b'\n');
    crate::state::atomic::write_atomic(&run_dir.join("run.json"), &bytes)?;
    if overall {
        Ok(ExitCode::from(EXIT_VERIFIED))
    } else {
        Ok(ExitCode::from(EXIT_NOT_VERIFIED))
    }
}

/// Snapshot one check into its run summary.
///
/// The summary copies the mapping the verdict rested on, so a later
/// manifest edit cannot reattribute this run.
fn summarize_check(check: &CheckSpec, verified: bool, stages: Vec<String>) -> CheckSummary {
    CheckSummary {
        name: check.name.clone(),
        role: match check.role {
            Role::Regression => "regression".to_string(),
            Role::Guard => "guard".to_string(),
        },
        change_ids: check.change_ids.clone(),
        expected_diagnostic: check.expected_diagnostic.clone(),
        expected_exchange: check.expected_exchange.clone(),
        verified,
        stages,
    }
}

/// Build the three stage definitions for one check.
///
/// Guards never want red. A regression check wants red only on
/// original sources against new fixtures.
fn stage_defs(manifest: &Manifest, check: &CheckSpec) -> Vec<StageDef> {
    let want_red = check.role == Role::Regression;
    vec![
        StageDef {
            name: "original-old",
            commit: manifest.baseline_commit.clone(),
            spec: SpecVersion::Old,
            scenarios: manifest.scenarios_old.clone(),
            want_red: false,
        },
        StageDef {
            name: "original-new",
            commit: manifest.baseline_commit.clone(),
            spec: SpecVersion::New,
            scenarios: manifest.scenarios_new.clone(),
            want_red,
        },
        StageDef {
            name: "patched-new",
            commit: manifest.patched_commit.clone(),
            spec: SpecVersion::New,
            scenarios: manifest.scenarios_new.clone(),
            want_red: false,
        },
    ]
}

/// Run one check in one stage and record every artefact.
///
/// The worktree is removed before this returns, whether the stage
/// passed or not. The returned verdict already folds in the stub
/// trace.
fn run_stage(
    manifest: &Manifest,
    check: &CheckSpec,
    stage: &StageDef,
    check_dir: &Path,
    nextest: &str,
) -> anyhow::Result<StageVerdict> {
    let started = epoch_secs();
    let stage_dir = check_dir.join(stage.name);
    std::fs::create_dir_all(&stage_dir)
        .with_context(|| format!("create stage directory {}", stage_dir.display()))?;
    let (spec_bytes, label) = crate::stub::embedded_spec(&stage.spec);
    let spec_sha = stage_spec_sha(manifest, &stage.spec);
    let spec_value: serde_json::Value = serde_json::from_slice(spec_bytes)
        .with_context(|| format!("parse embedded {label} contract"))?;
    let scenario_files = hash_scenario_dir(&stage.scenarios)?;
    let scenarios = match crate::stub::load_scenarios(&stage.scenarios, &spec_value, spec_sha) {
        Ok(scenarios) => scenarios,
        Err(error) => {
            let verdict = stage_failure(
                check,
                format!(
                    "scenario directory {} refused: {error:#}",
                    stage.scenarios.display()
                ),
            );
            write_stage_record(
                manifest,
                check,
                stage,
                &stage_dir,
                spec_sha,
                &scenario_files,
                started,
                &empty_run(check, nextest),
                &[],
                verdict.clone(),
            )?;
            return Ok(verdict);
        }
    };
    let by_id: HashMap<String, crate::stub::Scenario> = scenarios
        .iter()
        .map(|scenario| (scenario.id.clone(), scenario.clone()))
        .collect();
    let worktree = fresh_worktree_dir(&stage_dir)?;
    let tree_outcome = create_worktree(&manifest.repo, &stage.commit, &worktree);
    if let Err(error) = tree_outcome {
        let verdict = stage_failure(
            check,
            format!("cannot check out commit {}: {error:#}", stage.commit),
        );
        write_stage_record(
            manifest,
            check,
            stage,
            &stage_dir,
            spec_sha,
            &scenario_files,
            started,
            &empty_run(check, nextest),
            &[],
            verdict.clone(),
        )?;
        return Ok(verdict);
    }
    let verdict = run_in_worktree(
        manifest,
        check,
        stage,
        &stage_dir,
        spec_sha,
        &scenario_files,
        &spec_value,
        label,
        &scenarios,
        &by_id,
        &worktree,
        nextest,
        started,
    );
    let _ = remove_worktree(&manifest.repo, &worktree);
    verdict
}

/// Run the stub plus the test process inside a ready worktree.
#[allow(clippy::too_many_arguments)]
fn run_in_worktree(
    manifest: &Manifest,
    check: &CheckSpec,
    stage: &StageDef,
    stage_dir: &Path,
    spec_sha: &str,
    scenario_files: &[ScenarioFileHash],
    spec_value: &serde_json::Value,
    label: &str,
    scenarios: &[crate::stub::Scenario],
    by_id: &HashMap<String, crate::stub::Scenario>,
    worktree: &Path,
    nextest: &str,
    started: u64,
) -> anyhow::Result<StageVerdict> {
    if let Err(error) = overlay_harness(manifest, worktree) {
        let verdict = stage_failure(
            check,
            format!("cannot overlay the harness into the worktree: {error:#}"),
        );
        write_stage_record(
            manifest,
            check,
            stage,
            stage_dir,
            spec_sha,
            scenario_files,
            started,
            &empty_run(check, nextest),
            &[],
            verdict.clone(),
        )?;
        return Ok(verdict);
    }
    let trace_path = stage_dir.join("requests.jsonl");
    let server = match StubServer::start(
        0,
        scenarios.to_vec(),
        spec_value.clone(),
        label,
        trace_path.clone(),
    ) {
        Ok(server) => server,
        Err(error) => {
            let verdict = stage_failure(check, format!("cannot start the stage stub: {error:#}"));
            write_stage_record(
                manifest,
                check,
                stage,
                stage_dir,
                spec_sha,
                scenario_files,
                started,
                &empty_run(check, nextest),
                &[],
                verdict.clone(),
            )?;
            return Ok(verdict);
        }
    };
    let port = server.local_addr().port();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let served = std::thread::spawn(move || server.serve_until(&flag));
    let junit_path = stage_dir.join("junit.xml");
    let use_nextest = nextest != "absent (libtest fallback applies)";
    let (argv, parser) = if use_nextest {
        if let Err(error) = write_nextest_config(worktree, &junit_path) {
            eprintln!("warning: cannot write nextest profile: {error:#}");
        }
        (
            vec![
                "nextest".to_string(),
                "run".to_string(),
                "--profile".to_string(),
                "sethu".to_string(),
                "-E".to_string(),
                format!("test(={})", check.test),
            ],
            ParserKind::NextestJunit,
        )
    } else {
        (
            vec![
                "test".to_string(),
                "--".to_string(),
                "--exact".to_string(),
                check.test.clone(),
            ],
            ParserKind::LibtestText,
        )
    };
    let outcome = run_cargo(
        worktree,
        &argv,
        port,
        Duration::from_secs(STAGE_TIMEOUT_SECS),
    );
    stop.store(true, Ordering::Relaxed);
    match served.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("warning: stage stub stopped with an error: {error:#}");
        }
        Err(_) => {
            eprintln!("warning: stage stub thread did not stop cleanly");
        }
    }
    wait_for_trace(&trace_path);
    let trace = crate::stub::read_trace(&trace_path).unwrap_or_else(|_| Vec::new());
    let command = format!("cargo {}", argv.join(" "));
    let build_failed =
        outcome.exit_code != Some(0) && looks_like_build_failure(&outcome.stdout, &outcome.stderr);
    let cases = parse_cases(parser, &junit_path, &outcome.stdout);
    let run = TestRun {
        command,
        parser,
        exit_code: outcome.exit_code,
        signal: outcome.signal,
        timed_out: outcome.timed_out,
        build_failed,
        cases,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
    };
    let verdict = evaluate(check, stage.want_red, &run, &trace, by_id);
    write_stage_record(
        manifest,
        check,
        stage,
        stage_dir,
        spec_sha,
        scenario_files,
        started,
        &run,
        &trace,
        verdict.clone(),
    )?;
    Ok(verdict)
}

/// Write the stage record plus the captured test streams.
#[allow(clippy::too_many_arguments)]
fn write_stage_record(
    manifest: &Manifest,
    check: &CheckSpec,
    stage: &StageDef,
    stage_dir: &Path,
    spec_sha: &str,
    scenario_files: &[ScenarioFileHash],
    started: u64,
    run: &TestRun,
    trace: &[crate::stub::TraceEntry],
    verdict: StageVerdict,
) -> anyhow::Result<()> {
    std::fs::write(stage_dir.join("stdout.log"), &run.stdout)
        .with_context(|| format!("write {}", stage_dir.join("stdout.log").display()))?;
    std::fs::write(stage_dir.join("stderr.log"), &run.stderr)
        .with_context(|| format!("write {}", stage_dir.join("stderr.log").display()))?;
    let record = StageRecord {
        stage: stage.name.to_string(),
        check: check.name.clone(),
        change_ids: check.change_ids.clone(),
        expected_diagnostic: check.expected_diagnostic.clone(),
        expected_exchange: check.expected_exchange.clone(),
        commit: stage.commit.clone(),
        spec_version: match stage.spec {
            SpecVersion::Old => "old".to_string(),
            SpecVersion::New => "new".to_string(),
        },
        spec_sha256: spec_sha.to_string(),
        scenarios_dir: stage.scenarios.display().to_string(),
        scenario_files: scenario_files.to_vec(),
        harness_hash: current_harness_hash(manifest),
        test_command: run.command.clone(),
        parser: parser_name(run.parser).to_string(),
        exit_code: run.exit_code,
        signal: run.signal,
        timed_out: run.timed_out,
        build_failed: run.build_failed,
        cases: run
            .cases
            .iter()
            .map(|case| StoredCase {
                name: case.name.clone(),
                classname: case.classname.clone(),
                passed: case.passed,
                output: case.output.clone(),
            })
            .collect(),
        verdict: verdict_word(&verdict).to_string(),
        detail: verdict_detail(&verdict),
        started_epoch_secs: started,
        finished_epoch_secs: epoch_secs(),
    };
    let mut bytes = serde_json::to_vec_pretty(&record).with_context(|| "encode stage record")?;
    bytes.push(b'\n');
    crate::state::atomic::write_atomic(&stage_dir.join("stage.json"), &bytes)?;
    let _ = trace;
    Ok(())
}

/// Read the live harness hash without failing the stage record.
fn current_harness_hash(manifest: &Manifest) -> String {
    hash_harness(&manifest.harness)
        .map(|(hash, _)| hash)
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Build a failure verdict with the right variant for the check role.
fn stage_failure(check: &CheckSpec, reason: String) -> StageVerdict {
    match check.role {
        Role::Regression => StageVerdict::InvalidRed(reason),
        Role::Guard => StageVerdict::Failed(reason),
    }
}

/// Empty test run used when a stage never reaches the test process.
fn empty_run(check: &CheckSpec, nextest: &str) -> TestRun {
    TestRun {
        command: format!("cargo test -- --exact {}", check.test),
        parser: if nextest == "absent (libtest fallback applies)" {
            ParserKind::LibtestText
        } else {
            ParserKind::NextestJunit
        },
        exit_code: None,
        signal: None,
        timed_out: false,
        build_failed: false,
        cases: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
    }
}

/// One-word verdict for records and summaries.
fn verdict_word(verdict: &StageVerdict) -> &str {
    match verdict {
        StageVerdict::Pass => "pass",
        StageVerdict::ExpectedRed => "expected-red",
        StageVerdict::InvalidRed(_) => "invalid-red",
        StageVerdict::Failed(_) => "failed",
    }
}

/// Human line for one stage verdict.
fn verdict_line(verdict: &StageVerdict) -> String {
    match verdict {
        StageVerdict::Pass => "pass".to_string(),
        StageVerdict::ExpectedRed => "expected-red".to_string(),
        StageVerdict::InvalidRed(reason) => format!("invalid-red: {reason}"),
        StageVerdict::Failed(reason) => format!("failed: {reason}"),
    }
}

/// Detail text for records; empty for green stages.
fn verdict_detail(verdict: &StageVerdict) -> String {
    match verdict {
        StageVerdict::Pass | StageVerdict::ExpectedRed => String::new(),
        StageVerdict::InvalidRed(reason) | StageVerdict::Failed(reason) => reason.clone(),
    }
}

/// Pick the pinned spec hash for one stage.
fn stage_spec_sha<'a>(manifest: &'a Manifest, spec: &SpecVersion) -> &'a str {
    match spec {
        SpecVersion::Old => &manifest.spec_old_sha256,
        SpecVersion::New => &manifest.spec_new_sha256,
    }
}

/// Check that the embedded contracts match the manifest identities.
///
/// The stub serves its compiled-in specs, so a manifest that pins
/// different hashes refuses the whole run before any stage starts.
fn check_spec_identity(manifest: &Manifest) -> anyhow::Result<()> {
    for (spec, wanted) in [
        (SpecVersion::Old, manifest.spec_old_sha256.as_str()),
        (SpecVersion::New, manifest.spec_new_sha256.as_str()),
    ] {
        let (bytes, label) = crate::stub::embedded_spec(&spec);
        let actual = crate::stub::body_sha256_hex(bytes);
        if actual != wanted {
            anyhow::bail!(
                "manifest pins {label} spec {wanted} but the bundled contract hashes {actual}"
            );
        }
    }
    Ok(())
}

/// Refuse when the consumer tree holds uncommitted changes.
///
/// The state directory never counts: evidence generation writes
/// there, and counting it would make every run refuse itself. Any
/// other new, modified, or deleted path fails with its name.
fn refuse_dirty_tree(repo: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("status")
        .arg("--porcelain=v1")
        .current_dir(repo)
        .output()
        .with_context(|| format!("run `git status` in {}", repo.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "cannot read consumer status in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let paths = changed_paths(line);
        if paths.is_empty() {
            continue;
        }
        if paths.iter().all(is_state_path) {
            continue;
        }
        anyhow::bail!(
            "uncommitted changes in {} ({line}); commit them to the upgrade branch first",
            repo.display()
        );
    }
    Ok(())
}

/// Split one porcelain line into the paths it touches.
///
/// Rename and copy lines carry two paths around ` -> `. Every other
/// line carries one path after the two status columns.
fn changed_paths(line: &str) -> Vec<&str> {
    let rest = line.get(3..).unwrap_or("").trim();
    if rest.is_empty() {
        return Vec::new();
    }
    if rest.contains(" -> ") {
        return rest.split(" -> ").map(str::trim).collect();
    }
    vec![rest.trim()]
}

/// Report whether a status path lives under the state directory.
fn is_state_path(path: &&str) -> bool {
    let trimmed = path.trim_matches('"');
    trimmed == ".sethu" || trimmed.starts_with(".sethu/")
}

/// Check that one commit exists in the consumer repository.
fn check_commit(repo: &Path, commit: &str, label: &str) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("rev-parse")
        .arg("--verify")
        .arg(format!("{commit}^{{commit}}"))
        .current_dir(repo)
        .output()
        .with_context(|| format!("run `git rev-parse` in {}", repo.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "manifest names an unknown {label} commit {commit} in {}",
            repo.display()
        );
    }
    Ok(())
}

/// Pick a fresh run id from the clock and the process id.
fn fresh_run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_nanos())
        .unwrap_or(0);
    format!("run-{nanos}-{}", std::process::id())
}

/// Seconds since the Unix epoch, or zero when the clock is broken.
fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

/// List earlier runs whose harness hash differs from the live one.
///
/// Earlier runs keep their artefacts. The new run records their ids
/// so readers know they belong to a superseded harness.
fn superseded_runs(runs_dir: &Path, current: &str, live_hash: &str) -> anyhow::Result<Vec<String>> {
    let mut superseded = Vec::new();
    let entries = match std::fs::read_dir(runs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(superseded),
        Err(error) => {
            return Err(
                anyhow::Error::new(error).context(format!("list runs in {}", runs_dir.display()))
            );
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("list {}", runs_dir.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|part| part.to_str()) {
            Some(name) => name,
            None => continue,
        };
        if name == current {
            continue;
        }
        let bytes = match std::fs::read(path.join("run.json")) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let record: RunRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if record.harness_hash != live_hash {
            superseded.push(record.run_id);
        }
    }
    superseded.sort();
    Ok(superseded)
}

/// Hash the scenario files in one directory, in sorted name order.
fn hash_scenario_dir(dir: &Path) -> anyhow::Result<Vec<ScenarioFileHash>> {
    let mut names: Vec<String> = Vec::new();
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("read scenario dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("list {}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let bytes = std::fs::read(dir.join(&name))
            .with_context(|| format!("read scenario file {}", dir.join(&name).display()))?;
        out.push(ScenarioFileHash {
            name,
            sha256: crate::stub::body_sha256_hex(&bytes),
        });
    }
    Ok(out)
}

/// Pick a fresh worktree directory beside the stage directory.
fn fresh_worktree_dir(stage_dir: &Path) -> anyhow::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_nanos())
        .unwrap_or(0);
    Ok(stage_dir.join(format!("worktree-{}-{}", nanos, std::process::id())))
}

/// Create a detached worktree of one commit in a fresh directory.
///
/// The parent directory must exist. Git creates the worktree path
/// itself, so it must not exist yet.
fn create_worktree(repo: &Path, commit: &str, path: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("add")
        .arg("--detach")
        .arg(path)
        .arg(commit)
        .current_dir(repo)
        .output()
        .with_context(|| format!("run `git worktree add` in {}", repo.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "cannot create a worktree of {commit}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Remove one stage worktree, ignoring the result on a best effort.
///
/// Callers run this after the verdict is recorded, so a removal
/// failure only warns instead of hiding the stage result.
fn remove_worktree(repo: &Path, path: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(path)
        .current_dir(repo)
        .output()
        .with_context(|| format!("run `git worktree remove` in {}", repo.display()))?;
    if !output.status.success() {
        eprintln!(
            "warning: cannot remove worktree {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Overlay the frozen harness into an application worktree.
///
/// Every harness file copies over except the two scenario
/// directories, which the driver feeds to each stage stub directly.
/// Test files land at the same relative paths they hold in the
/// harness, so a harness `tests/` directory extends the worktree
/// `tests/` directory.
fn overlay_harness(manifest: &Manifest, worktree: &Path) -> anyhow::Result<()> {
    let old_dir = canonical_dir(&manifest.scenarios_old)?;
    let new_dir = canonical_dir(&manifest.scenarios_new)?;
    copy_harness_tree(
        &manifest.harness,
        &manifest.harness,
        worktree,
        &old_dir,
        &new_dir,
    )
}

/// Canonicalise a scenario directory for overlay comparisons.
///
/// A missing directory reads as none, so manifests that name their
/// scenarios outside the harness skip no subtree.
fn canonical_dir(dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    match dir.canonicalize() {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(anyhow::Error::new(error)
            .context(format!("resolve scenario directory {}", dir.display()))),
    }
}

/// Copy one harness tree into a worktree, skipping scenario subtrees.
fn copy_harness_tree(
    root: &Path,
    dir: &Path,
    worktree: &Path,
    old_dir: &Option<PathBuf>,
    new_dir: &Option<PathBuf>,
) -> anyhow::Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("read harness dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("list {}", dir.display()))?;
        let source = entry.path();
        if source.is_dir() {
            let canonical = source.canonicalize().unwrap_or_else(|_| source.clone());
            if Some(&canonical) == old_dir.as_ref() || Some(&canonical) == new_dir.as_ref() {
                continue;
            }
            let relative = source
                .strip_prefix(root)
                .with_context(|| format!("relativise {}", source.display()))?;
            std::fs::create_dir_all(worktree.join(relative))
                .with_context(|| format!("create {}", worktree.join(relative).display()))?;
            copy_harness_tree(root, &source, worktree, old_dir, new_dir)?;
            continue;
        }
        if !source.is_file() {
            continue;
        }
        let relative = source
            .strip_prefix(root)
            .with_context(|| format!("relativise {}", source.display()))?;
        let target = worktree.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::copy(&source, &target)
            .with_context(|| format!("copy {} to {}", source.display(), target.display()))?;
    }
    Ok(())
}

/// Write the nextest profile that drops JUnit beside the stage.
///
/// The installer owns this profile in real migrations. Throwaway
/// stage worktrees carry a copy so the report parses even when the
/// consumer never installed the pack.
fn write_nextest_config(worktree: &Path, junit_path: &Path) -> anyhow::Result<()> {
    let dir = worktree.join(".config");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let text = format!(
        "[profile.sethu.junit]\npath = \"{}\"\n",
        junit_path.display()
    );
    std::fs::write(dir.join("nextest.toml"), text)
        .with_context(|| format!("write {}", dir.join("nextest.toml").display()))?;
    Ok(())
}

/// Run one cargo command with a stub address and a deadline.
///
/// The child inherits the parent environment plus the stub address
/// and an offline flag, so no stage ever reaches the network. Every
/// spawn sets its working directory and captures both streams.
/// Separate threads drain both pipes while the main thread polls the
/// exit, so verbose output can never wedge the child. Past the
/// deadline the child is killed and reaped before this returns.
fn run_cargo(worktree: &Path, argv: &[String], port: u16, timeout: Duration) -> ProcessOutcome {
    let mut command = std::process::Command::new("cargo");
    command
        .args(argv)
        .current_dir(worktree)
        .env("IMMICH_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("CARGO_NET_OFFLINE", "true");
    let mut child = match command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return ProcessOutcome {
                stdout: String::new(),
                stderr: format!("cannot start cargo: {error}"),
                exit_code: None,
                signal: None,
                timed_out: false,
            };
        }
    };
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_drain = std::thread::spawn(move || drain_stdout(stdout_pipe));
    let stderr_drain = std::thread::spawn(move || drain_stderr(stderr_pipe));
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return ProcessOutcome {
                    stdout: join_drain(stdout_drain),
                    stderr: format!("{}cannot poll cargo: {error}", join_drain(stderr_drain)),
                    exit_code: None,
                    signal: None,
                    timed_out: false,
                };
            }
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return ProcessOutcome {
            stdout: join_drain(stdout_drain),
            stderr: format!(
                "{}cargo did not finish within {} seconds and was killed",
                join_drain(stderr_drain),
                timeout.as_secs()
            ),
            exit_code: None,
            signal: None,
            timed_out: true,
        };
    };
    ProcessOutcome {
        stdout: join_drain(stdout_drain),
        stderr: join_drain(stderr_drain),
        exit_code: status.code(),
        signal: exit_signal(&status),
        timed_out: false,
    }
}

/// Read one captured standard output pipe fully.
fn drain_stdout(pipe: Option<std::process::ChildStdout>) -> String {
    use std::io::Read;
    let mut text = String::new();
    if let Some(mut pipe) = pipe {
        let mut bytes = Vec::new();
        if pipe.read_to_end(&mut bytes).is_ok() {
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    text
}

/// Read one captured standard error pipe fully.
fn drain_stderr(pipe: Option<std::process::ChildStderr>) -> String {
    use std::io::Read;
    let mut text = String::new();
    if let Some(mut pipe) = pipe {
        let mut bytes = Vec::new();
        if pipe.read_to_end(&mut bytes).is_ok() {
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    text
}

/// Collect one drain thread, keeping nothing when it panics.
fn join_drain(thread: std::thread::JoinHandle<String>) -> String {
    thread.join().unwrap_or_else(|_| String::new())
}

/// Read the killing signal of an exit status, when one exists.
#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

/// Read the killing signal of an exit status on other platforms.
#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Wait until the stub trace settles after the test process ends.
///
/// The server appends each entry after its response bytes, so a
/// client holding a full reply can still run ahead of the flush.
/// Polling for a stable size hides no lost entry: anything still
/// missing after the deadline fails the exchange check as before.
fn wait_for_trace(path: &Path) {
    let start = std::time::Instant::now();
    let limit = Duration::from_millis(TRACE_SETTLE_MS);
    let mut last = file_len(path);
    while start.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(100));
        let now = file_len(path);
        if now.is_some() && now == last {
            return;
        }
        last = now;
    }
}

/// Read one file length, or none when the file is absent.
fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|info| info.len())
}

/// Parse the report a stage produced into per-test cases.
///
/// A missing or broken JUnit file reads as no cases, which the
/// evaluator reports as a test that never ran.
fn parse_cases(parser: ParserKind, junit_path: &Path, stdout: &str) -> Vec<CaseResult> {
    match parser {
        ParserKind::NextestJunit => match std::fs::read_to_string(junit_path) {
            Ok(xml) => parse_junit(&xml).unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        ParserKind::LibtestText => parse_libtest(stdout),
    }
}

/// Probe the nextest runner through cargo.
///
/// The version string doubles as the availability signal. The exact
/// word `absent` never appears in a real version line, so the runner
/// compares against the full sentence below.
fn probe_nextest(workdir: &Path) -> String {
    let text = probe_tool(workdir, "cargo", &["nextest", "--version"]);
    if text.starts_with("cargo-nextest ") {
        text
    } else {
        "absent (libtest fallback applies)".to_string()
    }
}

/// Run one `--version` probe and keep its first line.
fn probe_tool(workdir: &Path, program: &str, args: &[&str]) -> String {
    let output = match std::process::Command::new(program)
        .args(args)
        .current_dir(workdir)
        .output()
    {
        Ok(output) => output,
        Err(error) => return format!("unavailable ({error})"),
    };
    if !output.status.success() {
        return "unavailable".to_string();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("unavailable")
        .trim()
        .to_string()
}

/// Parse one stored case list back, used only by tests in this file.
#[cfg(test)]
fn stored_names(cases: &[StoredCase]) -> Vec<&str> {
    cases.iter().map(|case| case.name.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_matrix_wants_red_only_for_regression_on_new() {
        let manifest = Manifest {
            schema_version: 1,
            repo: PathBuf::from("."),
            baseline_commit: "base".to_string(),
            patched_commit: "patched".to_string(),
            harness: PathBuf::from("harness"),
            scenarios_old: PathBuf::from("old"),
            scenarios_new: PathBuf::from("new"),
            spec_old_sha256: "0".repeat(64),
            spec_new_sha256: "1".repeat(64),
            checks: vec![],
        };
        let regression = CheckSpec {
            name: "random-picker".to_string(),
            role: Role::Regression,
            change_ids: vec![],
            test: "verify_random_picker".to_string(),
            expected_diagnostic: Some(crate::verify::manifest::ExpectedDiagnostic::Substring(
                "ids".to_string(),
            )),
            expected_exchange: vec![],
        };
        let stages = stage_defs(&manifest, &regression);
        assert_eq!(stages.len(), 3);
        assert_eq!(stages[0].name, "original-old");
        assert!(!stages[0].want_red);
        assert!(stages[1].want_red);
        assert!(!stages[2].want_red);
        let guard = CheckSpec {
            role: Role::Guard,
            expected_diagnostic: None,
            ..regression
        };
        assert!(
            stage_defs(&manifest, &guard)
                .iter()
                .all(|stage| !stage.want_red)
        );
    }

    #[test]
    fn porcelain_paths_split_renames_and_skip_state() {
        assert_eq!(changed_paths(" M src/lib.rs"), vec!["src/lib.rs"]);
        assert_eq!(
            changed_paths("R  old.rs -> new.rs"),
            vec!["old.rs", "new.rs"]
        );
        assert!(changed_paths("").is_empty());
        assert!(is_state_path(&".sethu/runs/x"));
        assert!(!is_state_path(&"src/lib.rs"));
    }

    #[test]
    fn summarized_check_keeps_mapping_after_manifest_edit() {
        let mut check = CheckSpec {
            name: "random-picker".to_string(),
            role: Role::Regression,
            change_ids: vec!["vc1_real".to_string()],
            test: "verify_random_picker".to_string(),
            expected_diagnostic: Some(crate::verify::manifest::ExpectedDiagnostic::Substring(
                "random picker ids".to_string(),
            )),
            expected_exchange: vec![crate::verify::manifest::ExpectedExchange {
                scenario: "random-search".to_string(),
                method: "POST".to_string(),
                path: "/search/random".to_string(),
            }],
        };
        let summary = summarize_check(&check, true, vec!["expected-red".to_string()]);
        check.change_ids = vec!["vc1_FABRICATED".to_string()];
        check.expected_exchange.clear();
        assert_eq!(summary.change_ids, vec!["vc1_real".to_string()]);
        assert_eq!(summary.expected_exchange.len(), 1);
        let value = serde_json::to_value(&summary).unwrap();
        assert_eq!(value["change_ids"], serde_json::json!(["vc1_real"]));
        let back: CheckSummary = serde_json::from_value(value).unwrap();
        assert_eq!(back.change_ids, vec!["vc1_real".to_string()]);
        assert!(back.expected_diagnostic.is_some());
        assert_eq!(back.expected_exchange.len(), 1);
    }

    #[test]
    fn stage_record_keeps_mapping_after_manifest_edit() {
        let dir = tempfile::tempdir().unwrap();
        let stage_dir = dir.path().join("original-new");
        std::fs::create_dir_all(&stage_dir).unwrap();
        let manifest = Manifest {
            schema_version: 1,
            repo: dir.path().to_path_buf(),
            baseline_commit: "base".to_string(),
            patched_commit: "patched".to_string(),
            harness: dir.path().join("missing-harness"),
            scenarios_old: dir.path().to_path_buf(),
            scenarios_new: dir.path().to_path_buf(),
            spec_old_sha256: "0".repeat(64),
            spec_new_sha256: "1".repeat(64),
            checks: vec![],
        };
        let mut check = CheckSpec {
            name: "random-picker".to_string(),
            role: Role::Regression,
            change_ids: vec!["vc1_real".to_string()],
            test: "verify_random_picker".to_string(),
            expected_diagnostic: Some(crate::verify::manifest::ExpectedDiagnostic::Substring(
                "random picker ids".to_string(),
            )),
            expected_exchange: vec![crate::verify::manifest::ExpectedExchange {
                scenario: "random-search".to_string(),
                method: "POST".to_string(),
                path: "/search/random".to_string(),
            }],
        };
        let stage = StageDef {
            name: "original-new",
            commit: "base".to_string(),
            spec: SpecVersion::New,
            scenarios: dir.path().to_path_buf(),
            want_red: true,
        };
        let run = TestRun {
            command: "cargo nextest run".to_string(),
            parser: ParserKind::NextestJunit,
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            build_failed: false,
            cases: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
        };
        let sha = "a".repeat(64);
        write_stage_record(
            &manifest,
            &check,
            &stage,
            &stage_dir,
            &sha,
            &[],
            0,
            &run,
            &[],
            StageVerdict::Pass,
        )
        .unwrap();
        check.change_ids = vec!["vc1_FABRICATED".to_string()];
        check.expected_exchange.clear();
        let bytes = std::fs::read(stage_dir.join("stage.json")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["change_ids"], serde_json::json!(["vc1_real"]));
        assert_eq!(value["expected_exchange"].as_array().unwrap().len(), 1);
        let back: StageRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.change_ids, vec!["vc1_real".to_string()]);
    }

    #[test]
    fn verdict_words_cover_every_variant() {
        assert_eq!(verdict_word(&StageVerdict::Pass), "pass");
        assert_eq!(verdict_word(&StageVerdict::ExpectedRed), "expected-red");
        assert_eq!(
            verdict_word(&StageVerdict::InvalidRed("x".to_string())),
            "invalid-red"
        );
        assert_eq!(
            verdict_word(&StageVerdict::Failed("x".to_string())),
            "failed"
        );
        assert!(stored_names(&[]).is_empty());
    }
}
