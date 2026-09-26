//! Implementation of `sethu context`.
//!
//! The command shows one change with focused contract detail. It prints the
//! stored record unchanged, readable old and new renderings through the
//! adapter exact operation match, and the raw operation plus its referenced
//! components from each spec. Related operations stay listed, never
//! rendered. Prepare mode writes the same bundle per change into the
//! attempt context directory as plain files for readers that run nothing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use serde_json::Value;

use crate::cli::ContextArgs;
use crate::commands::Status;
use crate::provenance::{Origin, OriginsDocument};
use crate::state::attempt::AttemptRecord;
use crate::state::{atomic, layout, pair};
use crate::vimanam::{ChangeKind, ChangeRecord, DetailLevel};

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Resolved state for one context invocation.
///
/// Every path is checked against the stored full hashes before use. The
/// change list appears twice. The typed records drive decisions and the raw
/// value prints the one record the invocation asked for.
struct Resolved {
    /// Canonical repository root that owns the state tree.
    repo: PathBuf,
    /// Migration directory that holds the attempt context output.
    migration: PathBuf,
    /// Copied old spec inside the pair inputs directory.
    old_spec: PathBuf,
    /// Copied new spec inside the pair inputs directory.
    new_spec: PathBuf,
    /// Parsed old spec for source extraction.
    old_value: Value,
    /// Parsed new spec for source extraction.
    new_value: Value,
    /// Typed change records in capture order.
    changes: Vec<ChangeRecord>,
    /// Raw change list as stored, with key order preserved.
    changes_value: Value,
    /// Shared origins when the capture stores them.
    origins: Option<OriginsDocument>,
}

/// Full context bundle for one change id.
///
/// Optional sides stay none when the endpoint exists on one spec only.
/// Added endpoints render the new spec and removed endpoints the old one.
struct Bundle {
    /// Change id this bundle describes.
    id: String,
    /// Operation label from the record, such as `POST /search/random`.
    operation_label: String,
    /// Unmodified record JSON with a trailing newline.
    record_text: String,
    /// Origin description with a trailing newline.
    origin_text: String,
    /// Related operations, listed and never rendered.
    related_lines: Vec<String>,
    /// Readable old rendering with a trailing newline.
    old_rendering: Option<String>,
    /// Readable new rendering with a trailing newline.
    new_rendering: Option<String>,
    /// Old operation plus components as JSON with a trailing newline.
    old_source_text: Option<String>,
    /// New operation plus components as JSON with a trailing newline.
    new_source_text: Option<String>,
    /// Count of old components in the source fragment.
    old_components: usize,
    /// Count of new components in the source fragment.
    new_components: usize,
}

/// Entry point for `sethu context`.
///
/// The command resolves the single migration under the working directory,
/// checks the id against the attempt capture, and either prints one bundle
/// or writes a whole group. Usage errors name the bad selector. Unknown
/// ids fail with the capture named.
pub fn run(args: &ContextArgs) -> anyhow::Result<ExitCode> {
    let resolved = resolve()?;
    let raw = find_raw_record(&resolved.changes_value, &args.id).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown change id `{}` for this attempt, it names no change in the attempt capture",
            args.id
        )
    })?;
    let record = resolved
        .changes
        .iter()
        .find(|item| item.id == args.id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown change id `{}` for this attempt, it names no change in the attempt capture",
                args.id
            )
        })?
        .clone();
    let detail = crate::context::detail_for(&args.level);

    if let Some(selector) = args.prepare.as_deref() {
        let ids =
            crate::context::resolve_group(selector, &resolved.changes, resolved.origins.as_ref())?;
        if ids.is_empty() {
            anyhow::bail!("prepare selection {selector:?} names no changes");
        }
        return prepare(&resolved, &ids, detail);
    }

    let bundle = build_bundle(&resolved, &record, &raw, detail)?;
    print_bundle(&resolved, &bundle, &args.level)?;
    Ok(ExitCode::SUCCESS)
}

/// Find the single migration under a state root.
///
/// Zero migrations means nothing was initialised here. Several means
/// the choice is ambiguous, and this command refuses to guess. Both
/// cases fail with the directory named.
fn sole_migration(root: &Path) -> anyhow::Result<PathBuf> {
    let dir = layout::migrations_dir(root);
    let mut names = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry =
                    entry.with_context(|| format!("read entry in directory {}", dir.display()))?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if crate::state::atomic::is_pending_temp(&path) {
                    continue;
                }
                if let Some(name) = path.file_name().and_then(|part| part.to_str()) {
                    names.push(name.to_string());
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("list directory {}", dir.display()))
            );
        }
    }
    names.sort();
    match names.len() {
        0 => anyhow::bail!(
            "found no migration attempt under {}, run `init` first",
            dir.display()
        ),
        1 => {
            let migration = dir.join(&names[0]);
            layout::check_manifest_identity(&migration, &names[0])?;
            Ok(migration)
        }
        _ => anyhow::bail!(
            "found {} migration attempts under {}, `context` needs exactly one",
            names.len(),
            dir.display()
        ),
    }
}

/// Resolve every file one context invocation needs.
///
/// The manifest carries full hashes and the full capture id. Every lookup
/// compares those stored values, so a tampered tree fails here instead of
/// rendering context from the wrong change list.
fn resolve() -> anyhow::Result<Resolved> {
    let repo = std::env::current_dir().with_context(|| "read working directory for `context`")?;
    let repo = repo
        .canonicalize()
        .with_context(|| format!("resolve working directory {}", repo.display()))?;
    let root = layout::state_root(&repo);
    let migration = sole_migration(&root)?;
    let manifest: AttemptRecord =
        crate::state::read_state_file(&layout::manifest_path(&migration))?;
    manifest.validate()?;
    let pair_dir = layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash)?;
    layout::check_pair_identity(&pair_dir, &manifest.old_spec_hash, &manifest.new_spec_hash)?;
    let capture = layout::capture_dir(&pair_dir, &manifest.capture_id);
    layout::check_capture_identity(&capture, &manifest.capture_id)?;

    let changes_path = layout::changes_file(&capture);
    let changes_bytes = std::fs::read(&changes_path)
        .with_context(|| format!("read capture change list {}", changes_path.display()))?;
    let document = crate::vimanam::parse_diff_output(&changes_bytes)?;
    let changes_value: Value = serde_json::from_slice(&changes_bytes)
        .with_context(|| format!("parse capture change list {}", changes_path.display()))?;

    let old_spec = pair::inputs_old_path(&pair_dir);
    let new_spec = pair::inputs_new_path(&pair_dir);
    let old_value =
        crate::context::parse_spec_bytes(&pair::read_spec_bytes(&old_spec)?, &old_spec)?;
    let new_value =
        crate::context::parse_spec_bytes(&pair::read_spec_bytes(&new_spec)?, &new_spec)?;

    let origins_path = layout::origins_file(&capture);
    let origins = if origins_path.is_file() {
        Some(crate::provenance::read_origins(&capture)?)
    } else {
        None
    };

    Ok(Resolved {
        repo,
        migration,
        old_spec,
        new_spec,
        old_value,
        new_value,
        changes: document.changes,
        changes_value,
        origins,
    })
}

/// Find the raw stored record for one id.
///
/// The value comes straight from the stored bytes, so ids and severities
/// print exactly as captured. Key order follows the stored document.
fn find_raw_record(changes_value: &Value, id: &str) -> Option<Value> {
    changes_value
        .get("changes")?
        .as_array()?
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
        .cloned()
}

/// Build the full bundle for one change record.
///
/// Added endpoints render the new spec and removed endpoints the old one.
/// Every other kind renders both. Rendering uses the exact operation match,
/// so an unknown operation fails instead of leaking a neighbour.
fn build_bundle(
    resolved: &Resolved,
    record: &ChangeRecord,
    raw: &Value,
    detail: DetailLevel,
) -> anyhow::Result<Bundle> {
    let label = format!("{} {}", record.endpoint.method, record.endpoint.path);
    let (want_old, want_new) = match record.kind {
        ChangeKind::EndpointAdded => (false, true),
        ChangeKind::EndpointRemoved => (true, false),
        _ => (true, true),
    };

    let old_rendering = if want_old {
        Some(
            crate::vimanam::render_operation(
                &resolved.old_spec,
                &record.endpoint.method,
                &record.endpoint.path,
                detail,
                &resolved.repo,
            )
            .with_context(|| format!("render old context for {}", record.id))?,
        )
    } else {
        None
    };
    let new_rendering = if want_new {
        Some(
            crate::vimanam::render_operation(
                &resolved.new_spec,
                &record.endpoint.method,
                &record.endpoint.path,
                detail,
                &resolved.repo,
            )
            .with_context(|| format!("render new context for {}", record.id))?,
        )
    } else {
        None
    };

    let (old_source_text, old_components) = if want_old {
        let fragments = crate::context::extract_fragments(
            &resolved.old_value,
            &record.endpoint.method,
            &record.endpoint.path,
        )
        .with_context(|| format!("extract old source for {}", record.id))?;
        let count = fragments.components.len();
        (Some(encode_json(&fragments)?), count)
    } else {
        (None, 0)
    };
    let (new_source_text, new_components) = if want_new {
        let fragments = crate::context::extract_fragments(
            &resolved.new_value,
            &record.endpoint.method,
            &record.endpoint.path,
        )
        .with_context(|| format!("extract new source for {}", record.id))?;
        let count = fragments.components.len();
        (Some(encode_json(&fragments)?), count)
    } else {
        (None, 0)
    };

    let (origin_text, related_lines) = origin_block(&resolved.origins, record, &resolved.changes);
    let mut record_text = serde_json::to_string_pretty(raw)
        .with_context(|| format!("encode stored record {}", record.id))?;
    record_text.push('\n');

    Ok(Bundle {
        id: record.id.clone(),
        operation_label: label,
        record_text,
        origin_text,
        related_lines,
        old_rendering,
        new_rendering,
        old_source_text,
        new_source_text,
        old_components,
        new_components,
    })
}

/// Encode a value as pretty JSON with a trailing newline.
fn encode_json<T: serde::Serialize>(value: &T) -> anyhow::Result<String> {
    let mut text =
        serde_json::to_string_pretty(value).with_context(|| "encode source fragments as JSON")?;
    text.push('\n');
    Ok(text)
}

/// Describe the origin of one record and list its related operations.
///
/// Component origins list the other operations and change ids that share
/// the component. Operation origins list the operations that still
/// reference the old schema. Both stay listed, never rendered.
fn origin_block(
    origins: &Option<OriginsDocument>,
    record: &ChangeRecord,
    changes: &[ChangeRecord],
) -> (String, Vec<String>) {
    let Some(document) = origins else {
        return (
            "origins not recorded for this capture\n".to_string(),
            Vec::new(),
        );
    };
    let Some(origin) = document.origins.get(&record.id) else {
        return ("no origin stored for this change\n".to_string(), Vec::new());
    };
    match origin {
        Origin::Component { name } => {
            let mut related: Vec<(String, String)> = Vec::new();
            for (other_id, other) in &document.origins {
                if other_id == &record.id {
                    continue;
                }
                if matches!(other, Origin::Component { name: other_name } if other_name == name) {
                    let label = changes
                        .iter()
                        .find(|item| item.id == *other_id)
                        .map(|item| format!("{} {}", item.endpoint.method, item.endpoint.path))
                        .unwrap_or_else(|| "unknown operation".to_string());
                    related.push((label, other_id.clone()));
                }
            }
            related.sort();
            related.dedup();
            let lines: Vec<String> = related
                .iter()
                .map(|(label, id)| format!("{label} ({id})"))
                .collect();
            (
                format!("shared component {name}\nthe change comes from a named component used across operations\n"),
                lines,
            )
        }
        Origin::Operation {
            position,
            old,
            new,
            still_references,
        } => (
            format!(
                "operation reference switch at {position}\nwas {old}\nnow {new}\nthe components themselves may be unchanged\n"
            ),
            still_references.clone(),
        ),
        Origin::Unknown => (
            "unknown origin, neither a shared component nor a switched reference explains this change\n"
                .to_string(),
            Vec::new(),
        ),
    }
}

/// Print one bundle to stdout.
///
/// Sections longer than the chunk threshold land as numbered chunk files
/// under the attempt context directory. The output names those files where
/// the section would have printed.
fn print_bundle(
    resolved: &Resolved,
    bundle: &Bundle,
    level: &crate::cli::ContextLevel,
) -> anyhow::Result<()> {
    let word = crate::context::level_word(level);
    println!("## change {}", bundle.id);
    print!("{}", bundle.record_text);
    println!("## operation {}", bundle.operation_label);
    print!("{}", bundle.origin_text);
    print_section(
        resolved,
        bundle,
        &format!("## old rendering ({word}, old spec)"),
        bundle.old_rendering.as_deref(),
        "rendering-old",
        "md",
        "absent, the endpoint first appears in the new spec",
    )?;
    print_section(
        resolved,
        bundle,
        &format!("## new rendering ({word}, new spec)"),
        bundle.new_rendering.as_deref(),
        "rendering-new",
        "md",
        "absent, the endpoint no longer exists in the new spec",
    )?;
    print_section(
        resolved,
        bundle,
        &format!(
            "## old source (operation plus {} components)",
            bundle.old_components
        ),
        bundle.old_source_text.as_deref(),
        "source-old",
        "json",
        "absent, the endpoint first appears in the new spec",
    )?;
    print_section(
        resolved,
        bundle,
        &format!(
            "## new source (operation plus {} components)",
            bundle.new_components
        ),
        bundle.new_source_text.as_deref(),
        "source-new",
        "json",
        "absent, the endpoint no longer exists in the new spec",
    )?;
    println!("## related operations");
    if bundle.related_lines.is_empty() {
        println!("none");
    } else {
        for line in &bundle.related_lines {
            println!("- {line}");
        }
    }
    Ok(())
}

/// Print one section inline or spill it into chunk files.
///
/// Small sections print as is. Large sections split into numbered chunk
/// files under the attempt context directory for this change. Only files
/// land there, never anything a reader would need to execute.
fn print_section(
    resolved: &Resolved,
    bundle: &Bundle,
    heading: &str,
    content: Option<&str>,
    stem: &str,
    ext: &str,
    absent_note: &str,
) -> anyhow::Result<()> {
    println!("{heading}");
    let Some(text) = content else {
        println!("{absent_note}");
        return Ok(());
    };
    if text.len() <= crate::context::CHUNK_THRESHOLD_BYTES {
        print!("{text}");
        return Ok(());
    }
    let dir = layout::context_dir(&resolved.migration).join(&bundle.id);
    let paths = write_chunked(&dir, stem, ext, text)?;
    println!(
        "section too large ({} bytes), split into {} chunk files under {}",
        text.len(),
        paths.len(),
        dir.display()
    );
    for path in &paths {
        println!("- {}", path.display());
    }
    Ok(())
}

/// Write one bundle per id into the attempt context directory.
///
/// Every id resolves through the same bundle builder as single output, so
/// files and printed context agree. Only data files land here. Nothing
/// written needs to run.
fn prepare(resolved: &Resolved, ids: &[String], detail: DetailLevel) -> anyhow::Result<ExitCode> {
    let context_dir = layout::context_dir(&resolved.migration);
    let mut files_written = 0;
    for id in ids {
        let raw = find_raw_record(&resolved.changes_value, id).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown change id `{id}` for this attempt, it names no change in the attempt capture"
            )
        })?;
        let record = resolved
            .changes
            .iter()
            .find(|item| item.id == *id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown change id `{id}` for this attempt, it names no change in the attempt capture"
                )
            })?
            .clone();
        let bundle = build_bundle(resolved, &record, &raw, detail)?;
        let dir = context_dir.join(id);
        let mut files = write_chunked(&dir, "record", "json", &bundle.record_text)?;
        files.extend(write_chunked(
            &dir,
            "origin",
            "txt",
            &origin_file_text(&bundle),
        )?);
        if let Some(text) = bundle.old_rendering.as_deref() {
            files.extend(write_chunked(&dir, "rendering-old", "md", text)?);
        }
        if let Some(text) = bundle.new_rendering.as_deref() {
            files.extend(write_chunked(&dir, "rendering-new", "md", text)?);
        }
        if let Some(text) = bundle.old_source_text.as_deref() {
            files.extend(write_chunked(&dir, "source-old", "json", text)?);
        }
        if let Some(text) = bundle.new_source_text.as_deref() {
            files.extend(write_chunked(&dir, "source-new", "json", text)?);
        }
        files_written += files.len();
        println!("prepared {id} ({} files)", files.len());
        for path in &files {
            println!("- {}", path.display());
        }
    }
    println!(
        "prepared {} changes ({} files) into {}",
        ids.len(),
        files_written,
        context_dir.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Render the origin file text for one bundle.
///
/// The file joins the origin description with the related list, so tracers
/// read both from one place.
fn origin_file_text(bundle: &Bundle) -> String {
    let mut text = bundle.origin_text.clone();
    text.push_str("related operations\n");
    if bundle.related_lines.is_empty() {
        text.push_str("none\n");
    } else {
        for line in &bundle.related_lines {
            text.push_str("- ");
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

/// Write content as one file or as numbered chunk files.
///
/// Content that fits the threshold lands as `stem.ext`. Larger content
/// splits on line boundaries into `stem-chunk-NN.ext` files. Writes are
/// atomic, so readers see whole files or nothing.
fn write_chunked(dir: &Path, stem: &str, ext: &str, content: &str) -> anyhow::Result<Vec<PathBuf>> {
    let parts = crate::context::split_chunks(content, crate::context::CHUNK_THRESHOLD_BYTES);
    if parts.len() == 1 {
        let path = dir.join(format!("{stem}.{ext}"));
        atomic::write_atomic(&path, parts[0].as_bytes())?;
        return Ok(vec![path]);
    }
    let mut paths = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        let path = dir.join(format!("{stem}-chunk-{:02}.{ext}", index + 1));
        atomic::write_atomic(&path, part.as_bytes())?;
        paths.push(path);
    }
    Ok(paths)
}
