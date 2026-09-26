//! Focused contract context for one change.
//!
//! The command layer resolves state and prints output. This module holds the
//! pieces that stay testable without state. Spec parsing, operation lookup,
//! reference closure, chunk splitting, and group resolution all live here.
//! Every helper stays deterministic so repeated runs agree byte for byte.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;

use crate::cli::ContextLevel;
use crate::provenance::{Origin, OriginsDocument};
use crate::vimanam::{ChangeRecord, DetailLevel, Severity};

/// HTTP methods that can carry an operation object.
///
/// Path items also hold helpers like `parameters` and `summary`. Those keys
/// never name an operation, so lookup skips them.
const METHOD_NAMES: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Largest section kept inline before chunk files take over.
///
/// Sections above this size land as numbered chunk files under the attempt
/// context directory. Printed output names those files instead.
pub const CHUNK_THRESHOLD_BYTES: usize = 64_000;

/// Raw operation plus every schema component it reaches.
///
/// The operation keeps its parsed shape from the spec. Components map their
/// original names to their raw objects. References inside stay spelled as
/// the spec wrote them, so details stay checkable against the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceFragments {
    /// Raw operation object from `paths`, with refs unresolved.
    pub operation: Value,
    /// Referenced components by original name, transitively closed.
    pub components: BTreeMap<String, Value>,
}

/// Map a ladder level to the rendering detail it requests.
///
/// Overview asks for the summary map. Endpoint asks for standard detail.
/// Schema asks for full detail. Schema level never trims. The adapter
/// exposes no trimming flag, so full output always arrives complete.
pub fn detail_for(level: &ContextLevel) -> DetailLevel {
    match level {
        ContextLevel::Overview => DetailLevel::Summary,
        ContextLevel::Endpoint => DetailLevel::Standard,
        ContextLevel::Schema => DetailLevel::Full,
    }
}

/// Name a ladder level the way the CLI spells it.
pub fn level_word(level: &ContextLevel) -> &'static str {
    match level {
        ContextLevel::Overview => "overview",
        ContextLevel::Endpoint => "endpoint",
        ContextLevel::Schema => "schema",
    }
}

/// Parse raw spec bytes as JSON, falling back to YAML.
///
/// The error names the file and keeps the JSON failure as its cause.
pub fn parse_spec_bytes(bytes: &[u8], path: &Path) -> anyhow::Result<Value> {
    match serde_json::from_slice(bytes) {
        Ok(value) => Ok(value),
        Err(json_err) => serde_norway::from_slice(bytes)
            .with_context(|| format!("parse spec file {}: {json_err}", path.display())),
    }
}

/// Find one operation object by method and path template.
///
/// Method comparison ignores case because specs spell methods lower case.
/// The path must equal the template byte for byte. A missing operation
/// reads as none, so callers can report added and removed endpoints.
pub fn find_operation<'a>(spec: &'a Value, method: &str, path: &str) -> Option<&'a Value> {
    let paths = spec.get("paths")?.as_object()?;
    let item = paths.get(path)?.as_object()?;
    for (name, operation) in item {
        if METHOD_NAMES.contains(&name.as_str()) && name.eq_ignore_ascii_case(method) {
            return Some(operation);
        }
    }
    None
}

/// Extract one operation plus its transitive schema closure.
///
/// The result keeps every `$ref` spelling from the source. A missing
/// operation fails and names the method and path it looked for.
pub fn extract_fragments(
    spec: &Value,
    method: &str,
    path: &str,
) -> anyhow::Result<SourceFragments> {
    let operation = find_operation(spec, method, path)
        .ok_or_else(|| anyhow::anyhow!("spec has no operation {method} {path} to extract"))?;
    let mut refs = Vec::new();
    collect_refs(operation, &mut refs);
    let roots: Vec<String> = refs
        .iter()
        .filter_map(|target| component_name_of_ref(target))
        .collect();
    let names = expand_closure(spec, &roots);
    let mut components = BTreeMap::new();
    for name in names {
        if let Some(content) = lookup_component(spec, &name) {
            components.insert(name, content.clone());
        }
    }
    Ok(SourceFragments {
        operation: operation.clone(),
        components,
    })
}

/// Collect every `$ref` string inside a value.
fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref"
                    && let Some(target) = child.as_str()
                {
                    out.push(target.to_string());
                }
                collect_refs(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        _ => {}
    }
}

/// Read the component name out of a local reference target.
///
/// Only references into the named schema maps qualify. Anything else
/// reads as no component.
fn component_name_of_ref(target: &str) -> Option<String> {
    if !target.starts_with('#') {
        return None;
    }
    let fragment = target.split_once('#').map(|(_, rest)| rest).unwrap_or("");
    let mut parts = fragment.split('/');
    if parts.next() != Some("") {
        return None;
    }
    match parts.next() {
        Some("components") => {
            if parts.next() != Some("schemas") {
                return None;
            }
        }
        Some("definitions") => {}
        _ => return None,
    }
    parts
        .next()
        .map(|name| name.replace("~1", "/").replace("~0", "~"))
}

/// Look up one named component in either schema map.
fn lookup_component<'a>(spec: &'a Value, name: &str) -> Option<&'a Value> {
    spec.get("components")
        .and_then(|components| components.get("schemas"))
        .and_then(|schemas| schemas.get(name))
        .or_else(|| {
            spec.get("definitions")
                .and_then(|schemas| schemas.get(name))
        })
}

/// Expand component roots through every transitively referenced component.
///
/// Reference cycles stop after one visit, so recursive schemas end.
fn expand_closure(spec: &Value, roots: &[String]) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut queue: Vec<String> = roots.to_vec();
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(content) = lookup_component(spec, &name) else {
            continue;
        };
        let mut refs = Vec::new();
        collect_refs(content, &mut refs);
        for target in refs {
            if let Some(next) = component_name_of_ref(&target)
                && !seen.contains(&next)
            {
                queue.push(next);
            }
        }
    }
    seen
}

/// Split text into chunks that each fit the byte limit.
///
/// Splitting follows line boundaries, so chunks stay readable. One line
/// longer than the limit fills a chunk alone instead of splitting mid
/// line. Text that fits arrives as a single chunk. The limit floors at
/// one byte, so a zero limit cannot loop forever.
pub fn split_chunks(text: &str, limit_bytes: usize) -> Vec<String> {
    let limit = limit_bytes.max(1);
    if text.len() <= limit {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        if !current.is_empty() && current.len() + line.len() > limit {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(text.to_string());
    }
    chunks
}

/// List the required change ids in report order.
///
/// The required set covers breaking and review severities. Anything else
/// stays out. Order follows the capture, so grouping stays stable.
pub fn required_ids(changes: &[ChangeRecord]) -> Vec<String> {
    changes
        .iter()
        .filter(|record| matches!(record.severity, Severity::Breaking | Severity::Review))
        .map(|record| record.id.clone())
        .collect()
}

/// Partition the required ids into the origin groups the listing prints.
///
/// Component origins sort by component name, operation origins sort by
/// position, and unknown origins sort last. Members keep capture order.
/// The listing command keeps its grouping helpers private to its own
/// module, so this repeats the same origin rule here and both numbers
/// select the same ids. A missing origins map reads as unknown for every
/// change, matching the listing fallback for older captures.
pub fn prepare_groups(
    changes: &[ChangeRecord],
    origins: Option<&OriginsDocument>,
) -> Vec<Vec<String>> {
    let mut members: BTreeMap<(u8, String), Vec<String>> = BTreeMap::new();
    for id in required_ids(changes) {
        let origin = origins
            .and_then(|document| document.origins.get(&id))
            .cloned()
            .unwrap_or(Origin::Unknown);
        let key = match &origin {
            Origin::Component { name } => (0, format!("component {name}")),
            Origin::Operation { position, .. } => (1, format!("operation {position}")),
            Origin::Unknown => (2, "unknown".to_string()),
        };
        members.entry(key).or_default().push(id);
    }
    members.into_values().collect()
}

/// Resolve a prepare selector against the required set.
///
/// The selector accepts `all` for every required id, a 1 based group number
/// over the origin groups the listing prints, or a comma separated list of
/// ids. Group numbers count from one in listing order. Unknown ids, empty
/// lists, and out of range groups all fail with the selector named.
pub fn resolve_group(
    wanted: &str,
    changes: &[ChangeRecord],
    origins: Option<&OriginsDocument>,
) -> anyhow::Result<Vec<String>> {
    let required = required_ids(changes);
    let trimmed = wanted.trim();
    if trimmed == "all" {
        return Ok(required);
    }
    if !trimmed.is_empty() && trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        let number: usize = trimmed
            .parse()
            .with_context(|| format!("parse prepare group {trimmed:?} as a number"))?;
        let groups = prepare_groups(changes, origins);
        let total = groups.len();
        if number < 1 || number > total {
            anyhow::bail!(
                "prepare group {number} is out of range, this attempt has {total} required groups"
            );
        }
        return Ok(groups[number - 1].clone());
    }
    let mut ids = Vec::new();
    for part in trimmed.split(',') {
        let id = part.trim();
        if id.is_empty() {
            continue;
        }
        if !required.contains(&id.to_string()) {
            anyhow::bail!("prepare selection {id:?} names no required change in this attempt");
        }
        if !ids.contains(&id.to_string()) {
            ids.push(id.to_string());
        }
    }
    if ids.is_empty() {
        anyhow::bail!("prepare selection {wanted:?} names no changes");
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SCHEMA_VERSION;
    use crate::vimanam::{ChangeDetails, ChangeKind, EndpointRef};
    use indexmap::IndexMap;
    use serde_json::json;

    /// Build one change record with a fixed endpoint and kind.
    fn record(id: &str, severity: Severity) -> ChangeRecord {
        ChangeRecord {
            id: id.to_string(),
            endpoint: EndpointRef {
                method: "GET".to_string(),
                path: "/widgets".to_string(),
            },
            kind: ChangeKind::ResponseSchemaChanged,
            severity,
            details: ChangeDetails::default(),
        }
    }

    /// Build an origins map from id and origin pairs.
    fn origins(pairs: Vec<(&str, Origin)>) -> OriginsDocument {
        OriginsDocument {
            schema_version: SCHEMA_VERSION,
            origins: pairs
                .into_iter()
                .map(|(id, origin)| (id.to_string(), origin))
                .collect::<IndexMap<String, Origin>>(),
        }
    }

    #[test]
    fn detail_mapping_covers_every_level() {
        assert_eq!(detail_for(&ContextLevel::Overview), DetailLevel::Summary);
        assert_eq!(detail_for(&ContextLevel::Endpoint), DetailLevel::Standard);
        assert_eq!(detail_for(&ContextLevel::Schema), DetailLevel::Full);
        assert_eq!(level_word(&ContextLevel::Overview), "overview");
        assert_eq!(level_word(&ContextLevel::Endpoint), "endpoint");
        assert_eq!(level_word(&ContextLevel::Schema), "schema");
    }

    #[test]
    fn operation_lookup_matches_method_case_insensitively() {
        let spec = json!({
            "paths": {
                "/widgets": {
                    "parameters": [{"name": "x"}],
                    "get": {"operationId": "list"},
                    "post": {"operationId": "create"}
                }
            }
        });
        assert_eq!(
            find_operation(&spec, "POST", "/widgets").unwrap(),
            &json!({"operationId": "create"})
        );
        assert!(find_operation(&spec, "DELETE", "/widgets").is_none());
        assert!(find_operation(&spec, "POST", "/widgets/").is_none());
    }

    #[test]
    fn fragments_close_transitively_and_keep_names() {
        let spec = json!({
            "paths": {
                "/widgets": {
                    "get": {
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "schema": {"$ref": "#/components/schemas/Outer"}
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "components": {
                "schemas": {
                    "Outer": {
                        "type": "object",
                        "properties": {"inner": {"$ref": "#/components/schemas/Inner"}}
                    },
                    "Inner": {"type": "string"},
                    "Unrelated": {"type": "number"}
                }
            }
        });
        let fragments = extract_fragments(&spec, "GET", "/widgets").unwrap();
        assert_eq!(
            fragments.operation,
            spec["paths"]["/widgets"]["get"].clone()
        );
        assert_eq!(
            fragments.components.keys().cloned().collect::<Vec<_>>(),
            vec!["Inner".to_string(), "Outer".to_string()]
        );
        assert_eq!(
            fragments.components["Outer"],
            spec["components"]["schemas"]["Outer"].clone()
        );
    }

    #[test]
    fn fragments_fail_on_a_missing_operation() {
        let spec = json!({"paths": {}});
        let err = extract_fragments(&spec, "GET", "/missing").unwrap_err();
        assert!(err.to_string().contains("GET /missing"));
    }

    #[test]
    fn chunks_split_on_lines_and_keep_short_text_whole() {
        assert_eq!(split_chunks("short", 64), vec!["short".to_string()]);
        let text = "aaa\nbbb\nccc\n";
        let chunks = split_chunks(text, 8);
        assert_eq!(chunks, vec!["aaa\nbbb\n".to_string(), "ccc\n".to_string()]);
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunks_isolate_an_oversized_line() {
        let text = "ok\naverylongline\nok\n";
        let chunks = split_chunks(text, 5);
        assert_eq!(
            chunks,
            vec![
                "ok\n".to_string(),
                "averylongline\n".to_string(),
                "ok\n".to_string()
            ]
        );
    }

    #[test]
    fn group_resolution_follows_origin_groups() {
        let changes = vec![
            record("vc1_0001", Severity::Breaking),
            record("vc1_0002", Severity::Review),
            record("vc1_0003", Severity::Breaking),
            record("vc1_0004", Severity::Review),
            record("vc1_0005", Severity::NonBreaking),
        ];
        let document = origins(vec![
            (
                "vc1_0001",
                Origin::Component {
                    name: "Beta".to_string(),
                },
            ),
            (
                "vc1_0003",
                Origin::Component {
                    name: "Beta".to_string(),
                },
            ),
            (
                "vc1_0002",
                Origin::Component {
                    name: "Alpha".to_string(),
                },
            ),
        ]);
        let origins = Some(&document);
        let required = vec![
            "vc1_0001".to_string(),
            "vc1_0002".to_string(),
            "vc1_0003".to_string(),
            "vc1_0004".to_string(),
        ];
        assert_eq!(resolve_group("all", &changes, origins).unwrap(), required);
        assert_eq!(
            resolve_group("1", &changes, origins).unwrap(),
            vec!["vc1_0002".to_string()]
        );
        assert_eq!(
            resolve_group("2", &changes, origins).unwrap(),
            vec!["vc1_0001".to_string(), "vc1_0003".to_string()]
        );
        assert_eq!(
            resolve_group("3", &changes, origins).unwrap(),
            vec!["vc1_0004".to_string()]
        );
        assert_eq!(
            resolve_group("vc1_0003, vc1_0001,vc1_0003", &changes, origins).unwrap(),
            vec!["vc1_0003".to_string(), "vc1_0001".to_string()]
        );
    }

    #[test]
    fn group_resolution_rejects_bad_selectors() {
        let changes = vec![record("vc1_0001", Severity::Breaking)];
        let document = origins(vec![]);
        let origins = Some(&document);
        assert!(resolve_group("0", &changes, origins).is_err());
        assert!(resolve_group("2", &changes, origins).is_err());
        assert!(resolve_group("vc1_9999", &changes, origins).is_err());
        assert!(resolve_group("vc1_0001", &[], origins).is_err());
        assert!(resolve_group("", &changes, origins).is_err());
        assert!(resolve_group("all", &[], origins).unwrap().is_empty());
    }

    #[test]
    fn group_resolution_without_origins_uses_one_unknown_group() {
        let changes = vec![
            record("vc1_0001", Severity::Breaking),
            record("vc1_0002", Severity::Review),
        ];
        let required = vec!["vc1_0001".to_string(), "vc1_0002".to_string()];
        assert_eq!(resolve_group("all", &changes, None).unwrap(), required);
        assert_eq!(resolve_group("1", &changes, None).unwrap(), required);
        assert!(resolve_group("2", &changes, None).is_err());
    }

    #[test]
    fn out_of_range_names_the_group_scheme() {
        let changes = vec![record("vc1_0001", Severity::Breaking)];
        let err = resolve_group("7", &changes, None).unwrap_err();
        assert!(
            err.to_string().contains("7 is out of range")
                && err.to_string().contains("1 required groups"),
            "unexpected error: {err:#}"
        );
    }
}
