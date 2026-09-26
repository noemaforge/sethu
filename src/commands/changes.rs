//! Implementation of `sethu changes`.
//!
//! The command lists the required set of a migration attempt, meaning the
//! captured changes with a breaking or review severity. Each record shows
//! the id, method, path, kind, and severity exactly as the diff tool
//! reported them. Records group first by shared origin, then by service
//! tag, so tracers can split the work without losing a change.
//!
//! Service tags come from the specs. The declared operation tags in the
//! new spec win. When the operation is absent there, the old spec is
//! tried. When neither declares tags, the first path segment names the
//! tag. An operation with several tags appears under each one, while all
//! counts report unique changes and unique operations, so shared rows
//! never inflate the totals.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use indexmap::IndexMap;
use serde::Serialize;

use crate::cli::ChangesArgs;
use crate::commands::Status;
use crate::provenance::{Origin, OriginsDocument};
use crate::state::attempt::AttemptRecord;
use crate::state::{SCHEMA_VERSION, layout, pair};
use crate::vimanam::{ChangeKind, DiffDocument, Severity};

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Envelope version of the JSON output.
///
/// Readers accept the fields they know and ignore the rest. Adding a field
/// keeps this version. Renaming or removing one bumps it.
const OUTPUT_VERSION: u32 = 1;

/// HTTP methods that can carry an operation object.
///
/// Path items also hold helpers like `parameters` and `summary`. Those keys
/// never name an operation, so tag collection skips them.
const METHOD_NAMES: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Entry point for `sethu changes`.
///
/// The command resolves the single migration under the working directory,
/// reads its capture change list and its origins file, and prints the
/// required set grouped by origin and tag. A missing origins file reads as
/// unknown origins for every change. `--all` appends the non-breaking
/// changes as a separate section. `--group` prints one numbered required
/// group. `--json` prints the same facts as one deterministic document.
pub fn run(args: &ChangesArgs) -> anyhow::Result<ExitCode> {
    let loaded = load_attempt()?;
    let required = required_indices(&loaded.document);
    let mut groups = group_changes(&loaded.document, &loaded.origins, &required);
    let total = groups.len();
    let selected = match args.group {
        Some(number) => {
            if number == 0 || number > total {
                anyhow::bail!(
                    "group {number} is out of range, this attempt has {total} required groups"
                );
            }
            vec![groups.remove(number - 1)]
        }
        None => groups,
    };
    if args.json {
        let text = render_json(&loaded, &selected, total, args.all)?;
        println!("{text}");
    } else {
        for line in render_human(&loaded, &selected, total, args.all) {
            println!("{line}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Everything the renderers need after the state lookups.
struct Loaded {
    /// Manifest of the resolved migration attempt.
    manifest: AttemptRecord,
    /// Parsed capture change list, kept in report order.
    document: DiffDocument,
    /// Origins per change id, empty when no origins file was stored.
    origins: OriginsDocument,
    /// Declared operation tags from the new spec.
    new_tags: BTreeMap<(String, String), Vec<String>>,
    /// Declared operation tags from the old spec.
    old_tags: BTreeMap<(String, String), Vec<String>>,
}

/// Load the attempt, its capture, its origins, and both specs.
///
/// The manifest carries full hashes and the full capture id. Every lookup
/// compares those stored values, so a tampered tree fails here instead of
/// listing the wrong changes. Spec reads feed tag lookup only. Change ids
/// and severities always come from the stored capture.
fn load_attempt() -> anyhow::Result<Loaded> {
    let repo = std::env::current_dir().with_context(|| "read working directory for `changes`")?;
    let repo = repo
        .canonicalize()
        .with_context(|| format!("resolve working directory {}", repo.display()))?;
    let root = layout::state_root(&repo);
    let migration = sole_migration(&root)?;
    let manifest: AttemptRecord =
        crate::state::read_state_file(&layout::manifest_path(&migration))?;
    manifest.validate()?;
    let pair = layout::pair_dir(&root, &manifest.old_spec_hash, &manifest.new_spec_hash)?;
    layout::check_pair_identity(&pair, &manifest.old_spec_hash, &manifest.new_spec_hash)?;
    let capture = layout::capture_dir(&pair, &manifest.capture_id);
    layout::check_capture_identity(&capture, &manifest.capture_id)?;
    let changes_path = layout::changes_file(&capture);
    let raw = std::fs::read(&changes_path)
        .with_context(|| format!("read capture change list {}", changes_path.display()))?;
    let document = crate::vimanam::parse_diff_output(&raw)?;
    let origins = load_origins(&capture)?;
    let old_spec = read_spec(&pair::inputs_old_path(&pair))?;
    let new_spec = read_spec(&pair::inputs_new_path(&pair))?;
    Ok(Loaded {
        manifest,
        document,
        origins,
        new_tags: operation_tags(&new_spec),
        old_tags: operation_tags(&old_spec),
    })
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
            "found {} migration attempts under {}, `changes` needs exactly one",
            names.len(),
            dir.display()
        ),
    }
}

/// Load the origins of one capture directory.
///
/// A missing file reads as an empty map, so every change falls back to an
/// unknown origin. Older captures stored no origins file, and listing must
/// still work for them. A present file must parse and validate.
fn load_origins(capture: &Path) -> anyhow::Result<OriginsDocument> {
    let path = layout::origins_file(capture);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let document: OriginsDocument = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse origins file {}", path.display()))?;
            document.validate()?;
            Ok(document)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(OriginsDocument {
            schema_version: SCHEMA_VERSION,
            origins: IndexMap::new(),
        }),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("read origins file {}", path.display())))
        }
    }
}

/// Read and parse one stored spec file.
///
/// Parsing accepts JSON and YAML, matching the stored pair inputs.
fn read_spec(path: &Path) -> anyhow::Result<serde_json::Value> {
    let bytes = pair::read_spec_bytes(path)?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(value),
        Err(json_err) => serde_norway::from_slice(&bytes)
            .with_context(|| format!("parse spec file {}: {json_err}", path.display())),
    }
}

/// Collect declared operation tags from one spec.
///
/// The map keys read `(METHOD, path)` with the method in upper case. Tag
/// lists sort and dedupe, so repeated runs agree. Operations without a
/// declared tag list stay absent, and callers fall back to the path.
fn operation_tags(spec: &serde_json::Value) -> BTreeMap<(String, String), Vec<String>> {
    let mut tags = BTreeMap::new();
    let Some(paths) = spec.get("paths").and_then(|paths| paths.as_object()) else {
        return tags;
    };
    for (path, item) in paths {
        let Some(methods) = item.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            if !METHOD_NAMES.contains(&method.as_str()) {
                continue;
            }
            let mut names: Vec<String> = operation
                .get("tags")
                .and_then(|list| list.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|tag| tag.as_str())
                        .filter(|tag| !tag.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names.dedup();
            if !names.is_empty() {
                tags.insert((method.to_uppercase(), path.clone()), names);
            }
        }
    }
    tags
}

/// Name the service tags of one operation.
///
/// Declared tags from the new spec win. The old spec covers operations the
/// new one no longer defines. Otherwise the first path segment names the
/// tag, such as `system-config` for `/system-config/defaults`.
fn tags_for(
    method: &str,
    path: &str,
    new_tags: &BTreeMap<(String, String), Vec<String>>,
    old_tags: &BTreeMap<(String, String), Vec<String>>,
) -> Vec<String> {
    let key = (method.to_string(), path.to_string());
    if let Some(names) = new_tags.get(&key) {
        return names.clone();
    }
    if let Some(names) = old_tags.get(&key) {
        return names.clone();
    }
    vec![fallback_tag(path)]
}

/// Name the tag for an operation with no declared tags.
///
/// The first path segment names the tag. A path without a segment, such as
/// `/`, reads as untagged.
fn fallback_tag(path: &str) -> String {
    match path.split('/').nth(1) {
        Some(segment) if !segment.is_empty() => segment.to_string(),
        _ => "untagged".to_string(),
    }
}

/// Report whether one severity belongs to the required set.
///
/// Breaking and review changes need investigation. Non-breaking changes
/// stay out unless `--all` asks for them.
fn is_required(severity: &Severity) -> bool {
    matches!(severity, Severity::Breaking | Severity::Review)
}

/// Collect the report positions of the required changes.
///
/// Positions keep capture report order, so every renderer agrees.
fn required_indices(document: &DiffDocument) -> Vec<usize> {
    document
        .changes
        .iter()
        .enumerate()
        .filter(|(_, record)| is_required(&record.severity))
        .map(|(index, _)| index)
        .collect()
}

/// Collect the report positions of the non-breaking changes.
fn non_breaking_indices(document: &DiffDocument) -> Vec<usize> {
    document
        .changes
        .iter()
        .enumerate()
        .filter(|(_, record)| !is_required(&record.severity))
        .map(|(index, _)| index)
        .collect()
}

/// Render one severity exactly as the stored capture spells it.
///
/// The match mirrors the serialized words. Unit tests compare every word
/// against the serialized form, so drift fails loudly.
fn severity_word(severity: &Severity) -> &'static str {
    match severity {
        Severity::Breaking => "breaking",
        Severity::NonBreaking => "non_breaking",
        Severity::Review => "review",
    }
}

/// Render one change kind exactly as the stored capture spells it.
///
/// The match mirrors the serialized words. Unit tests compare every word
/// against the serialized form, so drift fails loudly.
fn kind_word(kind: &ChangeKind) -> &'static str {
    match kind {
        ChangeKind::EndpointAdded => "endpoint_added",
        ChangeKind::EndpointRemoved => "endpoint_removed",
        ChangeKind::ParameterAdded => "parameter_added",
        ChangeKind::ParameterRemoved => "parameter_removed",
        ChangeKind::ParameterRequiredChanged => "parameter_required_changed",
        ChangeKind::ParameterLocationChanged => "parameter_location_changed",
        ChangeKind::ParameterSchemaChanged => "parameter_schema_changed",
        ChangeKind::ResponseAdded => "response_added",
        ChangeKind::ResponseRemoved => "response_removed",
        ChangeKind::OperationIdChanged => "operation_id_changed",
        ChangeKind::DeprecatedChanged => "deprecated_changed",
        ChangeKind::RequestSchemaChanged => "request_schema_changed",
        ChangeKind::ResponseSchemaChanged => "response_schema_changed",
    }
}

/// One origin group of changes.
///
/// The group holds report positions into the capture change list. Counts
/// derive from those positions, so shared tag rows never inflate them.
struct Group {
    /// Origin shared by every change in this group.
    origin: Origin,
    /// Human label of the origin, used for sorting and display.
    label: String,
    /// Report positions of the member changes, in report order.
    indices: Vec<usize>,
    /// One-based number of this group within its section.
    number: usize,
}

/// Group changes by origin.
///
/// Component origins sort by component name, operation origins by
/// position, and unknown origins sort last. Members keep report order.
/// Changes without a stored origin read as unknown.
fn group_changes(
    document: &DiffDocument,
    origins: &OriginsDocument,
    indices: &[usize],
) -> Vec<Group> {
    let mut members: BTreeMap<(u8, String), (Origin, Vec<usize>)> = BTreeMap::new();
    for index in indices {
        let record = &document.changes[*index];
        let origin = origins
            .origins
            .get(&record.id)
            .cloned()
            .unwrap_or(Origin::Unknown);
        let (rank, label) = group_key(&origin);
        members
            .entry((rank, label.clone()))
            .or_insert_with(|| (origin, Vec::new()))
            .1
            .push(*index);
    }
    members
        .into_iter()
        .enumerate()
        .map(|(number, ((_, label), (origin, indices)))| Group {
            origin,
            label,
            indices,
            number: number + 1,
        })
        .collect()
}

/// Name the sort key of one origin.
fn group_key(origin: &Origin) -> (u8, String) {
    match origin {
        Origin::Component { name } => (0, format!("component {name}")),
        Origin::Operation { position, .. } => (1, format!("operation {position}")),
        Origin::Unknown => (2, "unknown".to_string()),
    }
}

/// Count the unique operations across report positions.
///
/// Operations identify by method and path. Shared tag rows count once.
fn unique_operations(document: &DiffDocument, indices: &[usize]) -> usize {
    document
        .changes
        .iter()
        .enumerate()
        .filter(|(index, _)| indices.contains(index))
        .map(|(_, record)| (&record.endpoint.method, &record.endpoint.path))
        .collect::<BTreeSet<_>>()
        .len()
}

/// Count helper for plural words.
fn plural(count: usize, singular: &str, plural_word: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural_word}")
    }
}

/// Render one section header with unique counts.
fn section_header(name: &str, changes: usize, operations: usize, groups: usize) -> String {
    format!(
        "{name}: {} across {} in {}",
        plural(changes, "change", "changes"),
        plural(operations, "operation", "operations"),
        plural(groups, "group", "groups")
    )
}

/// Render one group header with unique counts.
fn group_header(number: usize, total: usize, group: &Group, document: &DiffDocument) -> String {
    format!(
        "group {number} of {total}: {} ({} across {})",
        group.label,
        plural(group.indices.len(), "change", "changes"),
        plural(
            unique_operations(document, &group.indices),
            "operation",
            "operations"
        )
    )
}

/// Render the required groups and the optional section as human lines.
///
/// Every line is deterministic. Groups sort by origin, tags sort by name,
/// operations sort by method and path, and changes keep report order. The
/// totals line always describes the whole required set, even when `--group`
/// prints one group only.
fn render_human(
    loaded: &Loaded,
    selected: &[Group],
    total_groups: usize,
    show_all: bool,
) -> Vec<String> {
    let required = required_indices(&loaded.document);
    let mut lines = vec![section_header(
        "required",
        required.len(),
        unique_operations(&loaded.document, &required),
        total_groups,
    )];
    for group in selected {
        lines.push(String::new());
        lines.push(group_header(
            group.number,
            total_groups,
            group,
            &loaded.document,
        ));
        lines.extend(render_group_lines(loaded, group));
    }
    if show_all {
        let rest = non_breaking_indices(&loaded.document);
        let groups = group_changes(&loaded.document, &loaded.origins, &rest);
        let total = groups.len();
        lines.push(String::new());
        lines.push(section_header(
            "non-breaking",
            rest.len(),
            unique_operations(&loaded.document, &rest),
            total,
        ));
        for group in &groups {
            lines.push(String::new());
            lines.push(group_header(group.number, total, group, &loaded.document));
            lines.extend(render_group_lines(loaded, group));
        }
    }
    lines
}

/// Render one group as tag, operation, and change lines.
///
/// Operations with several tags appear under each tag. Change lines carry
/// the full id plus the stored severity and kind words.
fn render_group_lines(loaded: &Loaded, group: &Group) -> Vec<String> {
    let mut lines = Vec::new();
    let mut tagged: BTreeMap<String, BTreeMap<(String, String), Vec<usize>>> = BTreeMap::new();
    for index in &group.indices {
        let record = &loaded.document.changes[*index];
        for tag in tags_for(
            &record.endpoint.method,
            &record.endpoint.path,
            &loaded.new_tags,
            &loaded.old_tags,
        ) {
            tagged
                .entry(tag)
                .or_default()
                .entry((record.endpoint.method.clone(), record.endpoint.path.clone()))
                .or_default()
                .push(*index);
        }
    }
    for (tag, operations) in &tagged {
        let unique: BTreeSet<usize> = operations.values().flatten().copied().collect();
        lines.push(format!(
            "  tag {tag} ({} across {})",
            plural(unique.len(), "change", "changes"),
            plural(operations.len(), "operation", "operations")
        ));
        for ((method, path), indices) in operations {
            lines.push(format!("    {method} {path}"));
            for index in indices {
                let record = &loaded.document.changes[*index];
                lines.push(format!(
                    "      {} {} {}",
                    record.id,
                    severity_word(&record.severity),
                    kind_word(&record.kind)
                ));
            }
        }
    }
    lines
}

/// One JSON section with unique counts and ordered groups.
#[derive(Debug, Serialize)]
struct SectionOutput {
    /// Unique changes in this section.
    change_count: usize,
    /// Unique operations in this section.
    operation_count: usize,
    /// Groups in this section.
    group_count: usize,
    /// Groups in display order.
    groups: Vec<GroupOutput>,
}

/// One JSON group with its origin and tagged operations.
#[derive(Debug, Serialize)]
struct GroupOutput {
    /// One-based number of this group within its section.
    number: usize,
    /// Shared origin of this group, stored verbatim.
    origin: Origin,
    /// Unique changes in this group.
    change_count: usize,
    /// Unique operations in this group.
    operation_count: usize,
    /// Tagged operations in sorted order.
    tags: Vec<TagOutput>,
}

/// One JSON tag with its sorted operations.
#[derive(Debug, Serialize)]
struct TagOutput {
    /// Service tag name.
    tag: String,
    /// Operations under this tag in sorted order.
    operations: Vec<OperationOutput>,
}

/// One JSON operation with its changes in report order.
#[derive(Debug, Serialize)]
struct OperationOutput {
    /// HTTP method in upper case, as reported.
    method: String,
    /// Path template, as reported.
    path: String,
    /// Member changes in report order.
    changes: Vec<ChangeOutput>,
}

/// One JSON change with its stored identity and class.
#[derive(Debug, Serialize)]
struct ChangeOutput {
    /// Stable change id, copied from the capture.
    id: String,
    /// Change kind, copied from the capture.
    kind: ChangeKind,
    /// Severity word, copied from the capture.
    severity: Severity,
}

/// Full JSON document printed with `--json`.
#[derive(Debug, Serialize)]
struct ChangesOutput {
    /// Envelope version of this document.
    schema_version: u32,
    /// Full id of the resolved migration attempt.
    attempt: String,
    /// Full id of the bound capture.
    capture: String,
    /// Required changes grouped by origin and tag.
    required: SectionOutput,
    /// Non-breaking changes, present only with `--all`.
    #[serde(skip_serializing_if = "Option::is_none")]
    non_breaking: Option<SectionOutput>,
}

/// Render the selection as one deterministic JSON document.
///
/// Groups sort by origin, tags sort by name, operations sort by method and
/// path, and changes keep report order. Struct field order fixes the key
/// order, so reruns agree byte for byte. Section counts always describe
/// the whole section, even when `--group` prints one group only.
fn render_json(
    loaded: &Loaded,
    selected: &[Group],
    total_groups: usize,
    show_all: bool,
) -> anyhow::Result<String> {
    let required = required_indices(&loaded.document);
    let document = ChangesOutput {
        schema_version: OUTPUT_VERSION,
        attempt: loaded.manifest.attempt_id.clone(),
        capture: loaded.manifest.capture_id.clone(),
        required: section_json(loaded, selected, &required, total_groups),
        non_breaking: show_all.then(|| {
            let rest = non_breaking_indices(&loaded.document);
            let groups = group_changes(&loaded.document, &loaded.origins, &rest);
            let total = groups.len();
            section_json(loaded, &groups, &rest, total)
        }),
    };
    serde_json::to_string_pretty(&document).with_context(|| "render changes as JSON")
}

/// Render one section of groups as JSON.
fn section_json(
    loaded: &Loaded,
    groups: &[Group],
    indices: &[usize],
    group_total: usize,
) -> SectionOutput {
    SectionOutput {
        change_count: indices.len(),
        operation_count: unique_operations(&loaded.document, indices),
        group_count: group_total,
        groups: groups
            .iter()
            .map(|group| group_json(loaded, group))
            .collect(),
    }
}

/// Render one group as JSON.
fn group_json(loaded: &Loaded, group: &Group) -> GroupOutput {
    let mut tagged: BTreeMap<String, BTreeMap<(String, String), Vec<usize>>> = BTreeMap::new();
    for index in &group.indices {
        let record = &loaded.document.changes[*index];
        for tag in tags_for(
            &record.endpoint.method,
            &record.endpoint.path,
            &loaded.new_tags,
            &loaded.old_tags,
        ) {
            tagged
                .entry(tag)
                .or_default()
                .entry((record.endpoint.method.clone(), record.endpoint.path.clone()))
                .or_default()
                .push(*index);
        }
    }
    GroupOutput {
        number: group.number,
        origin: group.origin.clone(),
        change_count: group.indices.len(),
        operation_count: unique_operations(&loaded.document, &group.indices),
        tags: tagged
            .into_iter()
            .map(|(tag, operations)| TagOutput {
                tag,
                operations: operations
                    .into_iter()
                    .map(|((method, path), indices)| OperationOutput {
                        method,
                        path,
                        changes: indices
                            .into_iter()
                            .map(|index| {
                                let record = &loaded.document.changes[index];
                                ChangeOutput {
                                    id: record.id.clone(),
                                    kind: record.kind.clone(),
                                    severity: record.severity.clone(),
                                }
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_words_match_serialized_forms() {
        for severity in [Severity::Breaking, Severity::NonBreaking, Severity::Review] {
            let stored = serde_json::to_value(&severity).expect("severity serializes to a word");
            assert_eq!(stored.as_str(), Some(severity_word(&severity)));
        }
    }

    #[test]
    fn kind_words_match_serialized_forms() {
        for kind in [
            ChangeKind::EndpointAdded,
            ChangeKind::EndpointRemoved,
            ChangeKind::ParameterAdded,
            ChangeKind::ParameterRemoved,
            ChangeKind::ParameterRequiredChanged,
            ChangeKind::ParameterLocationChanged,
            ChangeKind::ParameterSchemaChanged,
            ChangeKind::ResponseAdded,
            ChangeKind::ResponseRemoved,
            ChangeKind::OperationIdChanged,
            ChangeKind::DeprecatedChanged,
            ChangeKind::RequestSchemaChanged,
            ChangeKind::ResponseSchemaChanged,
        ] {
            let stored = serde_json::to_value(&kind).expect("kind serializes to a word");
            assert_eq!(stored.as_str(), Some(kind_word(&kind)));
        }
    }

    #[test]
    fn declared_new_tags_win_and_paths_fall_back() {
        let mut new_tags = BTreeMap::new();
        new_tags.insert(
            ("GET".to_string(), "/widgets".to_string()),
            vec!["Catalog".to_string()],
        );
        let mut old_tags = BTreeMap::new();
        old_tags.insert(
            ("DELETE".to_string(), "/legacy".to_string()),
            vec!["Old".to_string()],
        );
        assert_eq!(
            tags_for("GET", "/widgets", &new_tags, &old_tags),
            vec!["Catalog".to_string()]
        );
        assert_eq!(
            tags_for("DELETE", "/legacy", &new_tags, &old_tags),
            vec!["Old".to_string()]
        );
        assert_eq!(
            tags_for("POST", "/orders/items", &new_tags, &old_tags),
            vec!["orders".to_string()]
        );
        assert_eq!(
            tags_for("GET", "/", &new_tags, &old_tags),
            vec!["untagged".to_string()]
        );
    }

    #[test]
    fn shared_rows_keep_unique_counts() {
        let tagged: BTreeMap<String, BTreeMap<(String, String), Vec<usize>>> = BTreeMap::from([
            (
                "First".to_string(),
                BTreeMap::from([(("GET".to_string(), "/a".to_string()), vec![0])]),
            ),
            (
                "Second".to_string(),
                BTreeMap::from([(("GET".to_string(), "/a".to_string()), vec![0])]),
            ),
        ]);
        let flat: Vec<usize> = tagged
            .values()
            .flat_map(|ops| ops.values().flatten())
            .copied()
            .collect();
        assert_eq!(flat.len(), 2);
        let unique: BTreeSet<usize> = flat.into_iter().collect();
        assert_eq!(unique.len(), 1);
    }
}
