//! Implementation of `sethu init`.

use std::process::ExitCode;

use anyhow::Context;

use crate::cli::InitArgs;
use crate::commands::Status;
use crate::state::{attempt, capture, layout, pair};
use crate::state::{attempt::NewAttempt, capture::NewCapture};

/// Build-time status of this command.
#[allow(dead_code)]
pub const STATUS: Status = Status::Available;

/// Entry point for `sethu init`.
///
/// The command hashes both specs, refuses external references, stores
/// the pair with its provenance, runs the contract diff, stores the
/// output as an immutable capture, and binds one capture to the consumer
/// repository state as a migration attempt. A rerun with identical
/// inputs resumes the same attempt. Any change starts a new attempt and
/// leaves stored captures and ledgers alone. `list` mode prints stored
/// attempts for the pair and changes nothing.
pub fn run(args: &InitArgs) -> anyhow::Result<ExitCode> {
    if args.list {
        return list_attempts(args);
    }
    // Consumer state first, so a bad repository fails before any write.
    let repo = attempt::canonical_repo_path(&args.repo)?;
    let baseline = attempt::read_baseline_commit(&repo)?;
    let root = layout::state_root(&repo);

    let old_bytes = pair::read_spec_bytes(&args.old)?;
    let new_bytes = pair::read_spec_bytes(&args.new)?;
    pair::reject_external_refs(&old_bytes, &args.old)?;
    pair::reject_external_refs(&new_bytes, &args.new)?;
    let old_hash = layout::sha256_hex(&old_bytes);
    let new_hash = layout::sha256_hex(&new_bytes);

    let (pair_dir, pair_new) = pair::ensure_pair(
        &root, &args.old, &args.new, &old_bytes, &new_bytes, &old_hash, &new_hash,
    )?;

    // The adapter probes the minimum version and parses the report.
    // Records keep the ids and severities exactly as reported.
    let probed = crate::vimanam::probe_vimanam(&repo)?;
    let document = crate::vimanam::run_diff(&args.old, &args.new, false, &repo)?;
    if probed.to_string() != document.generator.version {
        eprintln!(
            "warning: `vimanam --version` reports {probed} but the diff reports {}",
            document.generator.version
        );
    }
    let invocation = vec![
        "vimanam".to_string(),
        "diff".to_string(),
        args.old.display().to_string(),
        args.new.display().to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let mut changes_bytes = serde_json::to_vec_pretty(&document)
        .with_context(|| "encode stored change list as JSON")?;
    changes_bytes.push(b'\n');

    let capture_id = capture::capture_id_for(
        &document.generator.name,
        &document.generator.version,
        &invocation,
        document.schema_version,
        &old_hash,
        &new_hash,
    );
    let fresh_capture = NewCapture {
        capture_id: &capture_id,
        generator_name: &document.generator.name,
        generator_version: &document.generator.version,
        invocation: &invocation,
        vimanam_schema_version: document.schema_version,
        old_spec_hash: &old_hash,
        new_spec_hash: &new_hash,
        changes: &document,
        changes_bytes: &changes_bytes,
    };
    let (_, capture_new) = capture::find_or_create_capture(&pair_dir, &fresh_capture)?;

    let scope = attempt::normalize_scope(&args.scope);
    let sethu_version = attempt::current_sethu_version();
    let repo_text = repo.display().to_string();
    let attempt_id = attempt::attempt_id_for(
        &old_hash,
        &new_hash,
        &capture_id,
        &repo_text,
        &baseline,
        &scope,
        &sethu_version,
    );
    let fresh_attempt = NewAttempt {
        attempt_id: &attempt_id,
        old_spec_hash: &old_hash,
        new_spec_hash: &new_hash,
        capture_id: &capture_id,
        repo_path: &repo_text,
        baseline_commit: &baseline,
        scope: &scope,
        sethu_version: &sethu_version,
    };
    let (_, attempt_new) = attempt::find_or_create_attempt(&root, &fresh_attempt)?;

    println!("pair {}", layout::pair_dir_name(&old_hash, &new_hash)?);
    println!(
        "capture {capture_id} {}",
        if capture_new { "created" } else { "reused" }
    );
    println!(
        "attempt {attempt_id} {}",
        if attempt_new { "created" } else { "resumed" }
    );
    eprintln!(
        "sethu {sethu_version} with {} {} over {} changes",
        document.generator.name,
        document.generator.version,
        document.changes.len()
    );
    eprintln!("baseline commit {baseline}");
    if pair_new {
        eprintln!("stored new pair {}", pair_dir.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// List stored attempts for one spec pair.
///
/// The command hashes both specs, scans stored manifests for the same
/// full hashes, and prints one line per attempt in id order. It writes
/// nothing and needs no git checkout beyond the state directory.
fn list_attempts(args: &InitArgs) -> anyhow::Result<ExitCode> {
    let repo = attempt::canonical_repo_path(&args.repo)?;
    let root = layout::state_root(&repo);
    let old_bytes = pair::read_spec_bytes(&args.old)?;
    let new_bytes = pair::read_spec_bytes(&args.new)?;
    let old_hash = layout::sha256_hex(&old_bytes);
    let new_hash = layout::sha256_hex(&new_bytes);
    let attempts = attempt::find_attempts_for_pair(&root, &old_hash, &new_hash)?;
    for (_, manifest) in &attempts {
        let scope = if manifest.scope.is_empty() {
            "-".to_string()
        } else {
            manifest.scope.join(",")
        };
        println!(
            "{} capture {} baseline {} scope {scope} sethu {}",
            manifest.attempt_id,
            manifest.capture_id,
            manifest.baseline_commit,
            manifest.sethu_version
        );
    }
    if attempts.is_empty() {
        eprintln!(
            "no attempts for pair {}",
            layout::pair_dir_name(&old_hash, &new_hash)?
        );
    }
    Ok(ExitCode::SUCCESS)
}
