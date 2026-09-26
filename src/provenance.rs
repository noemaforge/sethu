//! Shared origin for captured changes.
//!
//! The diff tool reports pointers into the resolved schema. Those pointers
//! show what changed but not where the change came from. This module links
//! each change back to a named component or to the operation itself. It reads
//! the raw specs and the stored change list and writes one origin per change
//! id. Stored change records stay untouched.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{SCHEMA_VERSION, check_schema_version, layout, pair};
use crate::vimanam::{ChangeKind, ChangeRecord};

/// HTTP methods that can carry an operation object.
///
/// Path items also hold helpers like `parameters` and `summary`. Those keys
/// never name an operation, so enumeration skips them.
const METHOD_NAMES: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// One shared origin for a single change.
///
/// Component origins name the changed component that holds the record
/// pointer. Operation origins describe a reference that the operation itself
/// switched at one body position. Unknown covers every other record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    /// The change comes from a named component shared across operations.
    Component {
        /// Name of the changed component.
        name: String,
    },
    /// The operation changed its own schema reference at one position.
    Operation {
        /// Operation and body position, such as `POST /search/random 200`.
        position: String,
        /// Reference spelling on the old side. A bare `$ref` keeps its
        /// value. An array of a component reads `array of` plus that
        /// reference. Any other shape reads `inline`. A missing schema
        /// reads `absent`.
        old: String,
        /// Reference spelling on the new side, encoded like the old one.
        new: String,
        /// Operations that still reference the old schema, in sorted order.
        still_references: Vec<String>,
    },
    /// Neither a component nor an operation origin could be established.
    Unknown,
}

/// Origin map stored as `origins.json` inside a capture directory.
///
/// The map holds one entry per change id in change report order. It never
/// rewrites the change records themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginsDocument {
    /// Schema version of this file. Writers always stamp the shared version.
    pub schema_version: u32,
    /// Origin per change id in report order. The map keeps write order stable.
    pub origins: IndexMap<String, Origin>,
}

impl OriginsDocument {
    /// Refuse unknown schema versions before callers trust the payload.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_schema_version(self.schema_version, "origins")
    }
}

/// Compute one origin per change from two raw specs.
///
/// Component comparison uses order insensitive JSON, so key order never marks
/// a component changed. Operation comparison uses the unresolved body
/// schemas, so a switched `$ref` shows up even when every component stayed
/// the same. Records that match neither rule get an unknown origin. The
/// input slice keeps its order and its values.
pub fn compute_origins(
    old_spec: &Value,
    new_spec: &Value,
    changes: &[ChangeRecord],
) -> OriginsDocument {
    let changed = changed_components(old_spec, new_spec);
    let moved = moved_positions(old_spec, new_spec);

    let mut origins = IndexMap::new();
    for record in changes {
        let origin = match record_position(record) {
            None => Origin::Unknown,
            Some((method, path, pos)) => {
                let key = (format!("{method} {path}"), pos);
                if let Some((old, new, names)) = moved.get(&key) {
                    Origin::Operation {
                        position: format!("{} {}", key.0, key.1),
                        old: old.clone(),
                        new: new.clone(),
                        still_references: still_referencing(new_spec, names, &method, &path),
                    }
                } else {
                    match component_origin(old_spec, new_spec, &changed, record, &method, &path) {
                        Some(name) => Origin::Component { name },
                        None => Origin::Unknown,
                    }
                }
            }
        };
        origins.insert(record.id.clone(), origin);
    }
    OriginsDocument {
        schema_version: SCHEMA_VERSION,
        origins,
    }
}

/// Read capture inputs and the stored change list, then compute origins.
///
/// The specs come from the pair `inputs` directory and the records come from
/// the capture `changes.json`. Both paths resolve through the state helpers,
/// so callers pass only the capture directory.
pub fn origins_for_capture(capture_dir: &Path) -> anyhow::Result<OriginsDocument> {
    let pair_dir = capture_dir
        .parent()
        .and_then(|captures| captures.parent())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "capture directory {} has no pair directory",
                capture_dir.display()
            )
        })?;
    let old_path = pair::inputs_old_path(pair_dir);
    let new_path = pair::inputs_new_path(pair_dir);
    let old_spec = parse_spec_bytes(&pair::read_spec_bytes(&old_path)?, &old_path)?;
    let new_spec = parse_spec_bytes(&pair::read_spec_bytes(&new_path)?, &new_path)?;
    let changes_path = layout::changes_file(capture_dir);
    let changes_bytes = std::fs::read(&changes_path)
        .with_context(|| format!("read stored change list {}", changes_path.display()))?;
    let document = crate::vimanam::parse_diff_output(&changes_bytes)?;
    Ok(compute_origins(&old_spec, &new_spec, &document.changes))
}

/// Write an origins document into a capture directory.
///
/// Encoding uses pretty JSON with a trailing newline. Storage reuses the
/// atomic writer, so readers see the old file or the new file.
pub fn write_origins(capture_dir: &Path, origins: &OriginsDocument) -> anyhow::Result<()> {
    crate::state::write_state_file(&layout::origins_file(capture_dir), origins)
}

/// Load and validate an origins document from a capture directory.
///
/// The error names the file for both read failures and parse failures.
pub fn read_origins(capture_dir: &Path) -> anyhow::Result<OriginsDocument> {
    let origins: OriginsDocument =
        crate::state::read_state_file(&layout::origins_file(capture_dir))?;
    origins.validate()?;
    Ok(origins)
}

/// Parse raw spec bytes as JSON, falling back to YAML.
///
/// The error names the file and keeps the JSON failure as its cause.
fn parse_spec_bytes(bytes: &[u8], path: &Path) -> anyhow::Result<Value> {
    match serde_json::from_slice(bytes) {
        Ok(value) => Ok(value),
        Err(json_err) => serde_norway::from_slice(bytes)
            .with_context(|| format!("parse spec file {}: {json_err}", path.display())),
    }
}

/// Compare two values ignoring object key order.
///
/// Arrays keep their order. Numbers compare as parsed values.
fn value_equal(first: &Value, second: &Value) -> bool {
    match (first, second) {
        (Value::Object(first_map), Value::Object(second_map)) => {
            first_map.len() == second_map.len()
                && first_map.iter().all(|(key, value)| {
                    second_map
                        .get(key)
                        .is_some_and(|other| value_equal(value, other))
                })
        }
        (Value::Array(first_items), Value::Array(second_items)) => {
            first_items.len() == second_items.len()
                && first_items
                    .iter()
                    .zip(second_items.iter())
                    .all(|(value, other)| value_equal(value, other))
        }
        _ => first == second,
    }
}

/// Fetch the named component map of a spec.
///
/// OpenAPI 3 keeps schemas under `components` while Swagger 2 uses
/// `definitions`. Specs without either contribute no components.
fn components_root(spec: &Value) -> Option<&serde_json::Map<String, Value>> {
    spec.get("components")
        .and_then(|components| components.get("schemas"))
        .or_else(|| spec.get("definitions"))
        .and_then(Value::as_object)
}

/// Find components whose canonical JSON differs between two specs.
///
/// A component that exists on only one side counts as changed. Key order
/// never counts, since comparison ignores it.
fn changed_components(old_spec: &Value, new_spec: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for root in [components_root(old_spec), components_root(new_spec)]
        .into_iter()
        .flatten()
    {
        names.extend(root.keys().cloned());
    }
    names
        .into_iter()
        .filter(|name| {
            let old = components_root(old_spec).and_then(|map| map.get(name));
            let new = components_root(new_spec).and_then(|map| map.get(name));
            match (old, new) {
                (Some(first), Some(second)) => !value_equal(first, second),
                (None, None) => false,
                _ => true,
            }
        })
        .collect()
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
/// Only references into the named component maps qualify. Anything else
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

/// Resolve a local pointer fragment against a spec document.
///
/// The fragment follows the `#` of a reference. Segments unescape in pointer
/// order, so `~1` becomes `/` before `~0` becomes `~`.
fn resolve_local_pointer<'a>(spec: &'a Value, fragment: &str) -> Option<&'a Value> {
    let trimmed = fragment.strip_prefix('/').unwrap_or(fragment);
    if trimmed.is_empty() {
        return Some(spec);
    }
    let mut current = spec;
    for raw in trimmed.split('/') {
        let key = raw.replace("~1", "/").replace("~0", "~");
        if let Some(map) = current.as_object() {
            current = map.get(key.as_str())?;
        } else {
            let items = current.as_array()?;
            current = items.get(key.parse::<usize>().ok()?)?;
        }
    }
    Some(current)
}

/// Expand component roots through every transitively referenced component.
///
/// Unknown names still join the set, so a reference to a removed component
/// keeps its meaning on the side that still holds it.
fn expand_closure(spec: &Value, roots: &[String]) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut queue: Vec<String> = roots.to_vec();
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(content) = components_root(spec).and_then(|map| map.get(&name)) else {
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

/// Compute every component one operation references, directly or indirectly.
fn operation_closure(spec: &Value, operation: &Value) -> BTreeSet<String> {
    let mut refs = Vec::new();
    collect_refs(operation, &mut refs);
    let roots: Vec<String> = refs
        .iter()
        .filter_map(|target| component_name_of_ref(target))
        .collect();
    expand_closure(spec, &roots)
}

/// List every operation as method, path template, and object.
///
/// Methods read upper case to match the change records. The list sorts by
/// method and path, so repeated runs agree.
fn iter_operations(spec: &Value) -> Vec<(String, String, &Value)> {
    let mut operations = Vec::new();
    let Some(paths) = spec.get("paths").and_then(Value::as_object) else {
        return operations;
    };
    for (path, item) in paths {
        let Some(methods) = item.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            if METHOD_NAMES.contains(&method.as_str()) {
                operations.push((method.to_uppercase(), path.clone(), operation));
            }
        }
    }
    operations.sort_by(|first, second| (&first.0, &first.1).cmp(&(&second.0, &second.1)));
    operations
}

/// Find one operation object by method and path template.
///
/// Method comparison ignores case, since specs spell methods lower case.
fn find_operation<'a>(spec: &'a Value, method: &str, path: &str) -> Option<&'a Value> {
    iter_operations(spec)
        .into_iter()
        .find(|(found_method, found_path, _)| {
            found_method.eq_ignore_ascii_case(method) && found_path == path
        })
        .map(|(_, _, operation)| operation)
}

/// Read the schema of one media map.
///
/// The JSON media type wins when present. Otherwise the first sorted media
/// type with a schema wins, which keeps multi media specs deterministic.
fn content_schema(content: &Value) -> Option<&Value> {
    let media = content.as_object()?;
    if let Some(schema) = media
        .get("application/json")
        .and_then(|entry| entry.get("schema"))
    {
        return Some(schema);
    }
    let mut names: Vec<&String> = media.keys().collect();
    names.sort();
    for name in names {
        if let Some(schema) = media.get(name).and_then(|entry| entry.get("schema")) {
            return Some(schema);
        }
    }
    None
}

/// List the body schemas of one operation by position.
///
/// Positions read `request` for the request body and the status text for each
/// response. Only positions with a schema appear.
fn position_schemas(operation: &Value) -> Vec<(String, &Value)> {
    let mut positions = Vec::new();
    if let Some(schema) = operation
        .get("requestBody")
        .and_then(|body| body.get("content"))
        .and_then(content_schema)
    {
        positions.push(("request".to_string(), schema));
    }
    if let Some(responses) = operation.get("responses").and_then(Value::as_object) {
        let mut statuses: Vec<&String> = responses.keys().collect();
        statuses.sort();
        for status in statuses {
            if let Some(schema) = responses
                .get(status)
                .and_then(|response| response.get("content"))
                .and_then(content_schema)
            {
                positions.push((status.clone(), schema));
            }
        }
    }
    positions
}

/// Render the reference spelling of one unresolved body schema.
///
/// A bare `$ref` keeps its value. An array over a component reads `array of`
/// plus that reference. Any other shape reads `inline`.
fn describe_reference(schema: &Value) -> String {
    if let Some(target) = schema.get("$ref").and_then(Value::as_str) {
        return target.to_string();
    }
    let is_array = schema.get("type").and_then(Value::as_str) == Some("array");
    if is_array
        && let Some(target) = schema
            .get("items")
            .and_then(|items| items.get("$ref"))
            .and_then(Value::as_str)
    {
        return format!("array of {target}");
    }
    "inline".to_string()
}

/// Render the spelling of an optional body schema.
///
/// A missing schema reads `absent`, which marks a position that one side
/// does not define at all.
fn spelling_of(schema: Option<&Value>) -> String {
    match schema {
        Some(value) => describe_reference(value),
        None => "absent".to_string(),
    }
}

/// List the components one unresolved body schema names directly.
///
/// Only local component references qualify. The list sorts and dedupes.
fn referenced_components(schema: Option<&Value>) -> Vec<String> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    collect_refs(schema, &mut refs);
    let mut names: Vec<String> = refs
        .iter()
        .filter_map(|target| component_name_of_ref(target))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Compare every shared body position between two specs.
///
/// The result maps each selector and position pair to both spellings plus the
/// components that the old schema names. Those names feed the still
/// referencing lookup for operation origins.
fn moved_positions(
    old_spec: &Value,
    new_spec: &Value,
) -> BTreeMap<(String, String), (String, String, Vec<String>)> {
    let mut moved = BTreeMap::new();
    let mut selectors = BTreeSet::new();
    for spec in [old_spec, new_spec] {
        for (method, path, _) in iter_operations(spec) {
            selectors.insert((method, path));
        }
    }
    for (method, path) in selectors {
        let old_positions = find_operation(old_spec, &method, &path)
            .map(position_schemas)
            .unwrap_or_default();
        let new_positions = find_operation(new_spec, &method, &path)
            .map(position_schemas)
            .unwrap_or_default();
        let mut names = BTreeSet::new();
        names.extend(old_positions.iter().map(|(name, _)| name.clone()));
        names.extend(new_positions.iter().map(|(name, _)| name.clone()));
        for name in names {
            let old_schema = find_position_schema(&old_positions, &name);
            let new_schema = find_position_schema(&new_positions, &name);
            let same = match (old_schema, new_schema) {
                (None, None) => true,
                (Some(first), Some(second)) => value_equal(first, second),
                _ => false,
            };
            if !same {
                let key = format!("{method} {path}");
                moved.insert(
                    (key, name),
                    (
                        spelling_of(old_schema),
                        spelling_of(new_schema),
                        referenced_components(old_schema),
                    ),
                );
            }
        }
    }
    moved
}

/// Find one position schema inside a position list.
fn find_position_schema<'a>(
    positions: &'a [(String, &'a Value)],
    wanted: &str,
) -> Option<&'a Value> {
    positions
        .iter()
        .find(|(name, _)| name == wanted)
        .map(|(_, schema)| *schema)
}

/// Map one change record to its operation and body position.
///
/// Request schema changes map to the request body. Response schema changes
/// map to their status. Every other kind has no body position.
fn record_position(record: &ChangeRecord) -> Option<(String, String, String)> {
    let method = record.endpoint.method.clone();
    let path = record.endpoint.path.clone();
    match record.kind {
        ChangeKind::RequestSchemaChanged => Some((method, path, "request".to_string())),
        ChangeKind::ResponseSchemaChanged => {
            let status = record.details.status.clone()?;
            Some((method, path, status))
        }
        _ => None,
    }
}

/// List operations that still reference the named components.
///
/// The scan runs over the new spec and skips the changed operation itself.
/// Selectors read `METHOD path` and sort.
fn still_referencing(new_spec: &Value, names: &[String], method: &str, path: &str) -> Vec<String> {
    if names.is_empty() {
        return Vec::new();
    }
    let mut holders = Vec::new();
    for (found_method, found_path, operation) in iter_operations(new_spec) {
        if found_method == method && found_path == path {
            continue;
        }
        let closure = operation_closure(new_spec, operation);
        if names.iter().any(|name| closure.contains(name)) {
            holders.push(format!("{found_method} {found_path}"));
        }
    }
    holders.sort();
    holders
}

/// Escape one JSON pointer segment.
///
/// Tildes escape first, so an existing `~1` sequence survives intact.
fn escape_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Record every component position inside one resolved body schema.
///
/// Each entry pairs a pointer prefix with the component whose content sits
/// there. Reference cycles stop after one visit, so recursive schemas end.
fn resolve_under(
    spec: &Value,
    node: &Value,
    prefix: String,
    out: &mut Vec<(String, String)>,
    visiting: &mut Vec<String>,
) {
    if let Some(target) = node.get("$ref").and_then(Value::as_str) {
        if !target.starts_with('#') {
            return;
        }
        let Some(name) = component_name_of_ref(target) else {
            return;
        };
        out.push((prefix.clone(), name.clone()));
        if visiting.contains(&name) {
            return;
        }
        let fragment = target.split_once('#').map(|(_, rest)| rest).unwrap_or("");
        let Some(resolved) = resolve_local_pointer(spec, fragment) else {
            return;
        };
        visiting.push(name);
        resolve_under(spec, resolved, prefix, out, visiting);
        visiting.pop();
        return;
    }
    if let Some(map) = node.as_object() {
        for (key, child) in map {
            let mut child_prefix = prefix.clone();
            child_prefix.push('/');
            child_prefix.push_str(&escape_segment(key));
            resolve_under(spec, child, child_prefix, out, visiting);
        }
        return;
    }
    if let Some(items) = node.as_array() {
        for (index, child) in items.iter().enumerate() {
            resolve_under(spec, child, format!("{prefix}/{index}"), out, visiting);
        }
    }
}

/// List component positions for one body schema of one spec.
fn component_positions(spec: &Value, schema: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    resolve_under(spec, schema, String::new(), &mut out, &mut Vec::new());
    out
}

/// Read the body position name of one record.
///
/// Request schema changes use the request body. Response schema changes use
/// their status. Every other kind has no body position.
fn record_body_position(record: &ChangeRecord) -> Option<String> {
    match record.kind {
        ChangeKind::RequestSchemaChanged => Some("request".to_string()),
        ChangeKind::ResponseSchemaChanged => record.details.status.clone(),
        _ => None,
    }
}

/// Attribute one record to the deepest changed component holding its pointer.
///
/// The check runs on both spec sides, since removed nodes resolve on the old
/// side and added nodes resolve on the new side. Pointer equality across
/// unrelated components never groups them. Each record keeps its own answer.
fn component_origin(
    old_spec: &Value,
    new_spec: &Value,
    changed: &BTreeSet<String>,
    record: &ChangeRecord,
    method: &str,
    path: &str,
) -> Option<String> {
    let pointer = record.details.schema_change.as_ref()?.pointer.clone();
    let position = record_body_position(record)?;
    let mut best: Option<(usize, String)> = None;
    for spec in [old_spec, new_spec] {
        let Some(operation) = find_operation(spec, method, path) else {
            continue;
        };
        let positions = position_schemas(operation);
        let Some(schema) = find_position_schema(&positions, &position) else {
            continue;
        };
        for (prefix, name) in component_positions(spec, schema) {
            if !changed.contains(&name) {
                continue;
            }
            let inside = pointer == prefix
                || (pointer.starts_with(prefix.as_str())
                    && pointer[prefix.len()..].starts_with('/'));
            if inside
                && best
                    .as_ref()
                    .is_none_or(|(length, _)| prefix.len() > *length)
            {
                best = Some((prefix.len(), name));
            }
        }
    }
    best.map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_never_marks_a_component_changed() {
        let first = json!({"b": 1, "a": {"y": [1, 2], "x": true}});
        let second = json!({"a": {"x": true, "y": [1, 2]}, "b": 1});
        assert!(value_equal(&first, &second));
    }

    #[test]
    fn array_order_still_counts() {
        assert!(!value_equal(&json!([1, 2]), &json!([2, 1])));
    }

    #[test]
    fn reference_spellings_cover_every_shape() {
        assert_eq!(
            describe_reference(&json!({"$ref": "#/components/schemas/Widget"})),
            "#/components/schemas/Widget"
        );
        assert_eq!(
            describe_reference(
                &json!({"type": "array", "items": {"$ref": "#/components/schemas/Widget"}})
            ),
            "array of #/components/schemas/Widget"
        );
        assert_eq!(describe_reference(&json!({"type": "object"})), "inline");
        assert_eq!(spelling_of(None), "absent");
    }

    #[test]
    fn ref_names_decode_definitions_and_escapes() {
        assert_eq!(
            component_name_of_ref("#/components/schemas/Widget"),
            Some("Widget".to_string())
        );
        assert_eq!(
            component_name_of_ref("#/definitions/Widget"),
            Some("Widget".to_string())
        );
        assert_eq!(
            component_name_of_ref("#/components/schemas/A~1B"),
            Some("A/B".to_string())
        );
        assert_eq!(
            component_name_of_ref("https://example.com/x.json#/Widget"),
            None
        );
        assert_eq!(component_name_of_ref("#/paths/~1a/get"), None);
    }

    #[test]
    fn operation_listing_skips_non_method_keys() {
        let spec = json!({
            "paths": {
                "/a": {
                    "parameters": [{"name": "x"}],
                    "summary": "helper",
                    "get": {"responses": {}}
                }
            }
        });
        let found = iter_operations(&spec);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "GET");
        assert_eq!(found[0].1, "/a");
    }
}
