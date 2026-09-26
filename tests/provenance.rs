//! Integration tests for shared origins.
//!
//! Tests run the provenance computation against the checked in specs and the
//! checked in diff output. The pinned pair proves the random search operation
//! origin and the image component fan out. Synthetic specs prove that shared
//! pointers alone never group unrelated changes.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sethu::provenance::{self, Origin};
use sethu::state::{
    SCHEMA_VERSION, atomic, capture::CaptureRecord, layout, pair, write_state_file,
};
use sethu::vimanam::{
    ChangeDetails, ChangeKind, ChangeOperation, ChangeRecord, ChangeTarget, EndpointRef, Presence,
    SchemaChange, Severity,
};

/// Locate a checked in fixture by path under the crate root.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Load the pinned old and new specs as parsed JSON.
fn load_specs() -> (Value, Value) {
    let old_bytes = std::fs::read(fixture("immich/old.json")).unwrap();
    let new_bytes = std::fs::read(fixture("immich/new.json")).unwrap();
    let old: Value = serde_json::from_slice(&old_bytes).unwrap();
    let new: Value = serde_json::from_slice(&new_bytes).unwrap();
    (old, new)
}

/// Load the checked in diff output and return its change records.
fn load_changes() -> Vec<ChangeRecord> {
    let bytes = std::fs::read(fixture("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json")).unwrap();
    sethu::vimanam::parse_diff_output(&bytes).unwrap().changes
}

/// Read the schema pointer of one record, if it carries a schema change.
fn record_pointer(record: &ChangeRecord) -> Option<&str> {
    record
        .details
        .schema_change
        .as_ref()
        .map(|change| change.pointer.as_str())
}

/// Name the body position of one record for grouping checks.
fn record_position_label(record: &ChangeRecord) -> String {
    let side = match record.kind {
        ChangeKind::RequestSchemaChanged => "request".to_string(),
        _ => record.details.status.clone().unwrap_or_default(),
    };
    format!("{} {} {side}", record.endpoint.method, record.endpoint.path)
}

/// Random search response records carry an operation origin.
///
/// The old spelling names the shared response component and the new spelling
/// names the asset component. Both neighbours still return the old shape.
#[test]
fn random_search_responses_carry_an_operation_origin() {
    let (old, new) = load_specs();
    let changes = load_changes();
    let origins = provenance::compute_origins(&old, &new, &changes);

    let targets: Vec<&ChangeRecord> = changes
        .iter()
        .filter(|record| {
            record.endpoint.method == "POST"
                && record.endpoint.path == "/search/random"
                && record.kind == ChangeKind::ResponseSchemaChanged
        })
        .collect();
    assert_eq!(targets.len(), 4);

    for record in targets {
        match origins.origins.get(&record.id).unwrap() {
            Origin::Operation {
                position,
                old,
                new,
                still_references,
            } => {
                assert_eq!(position, "POST /search/random 200");
                assert!(old.contains("SearchResponseDto"), "old spelling: {old}");
                assert!(new.contains("AssetResponseDto"), "new spelling: {new}");
                assert_eq!(
                    *still_references,
                    vec![
                        "POST /search/metadata".to_string(),
                        "POST /search/smart".to_string()
                    ]
                );
            }
            other => panic!("record {} has unexpected origin {other:?}", record.id),
        }
    }
}

/// Image records share one component origin across operations.
///
/// The required image records span three operations and four body positions.
/// Every one names the same image component.
#[test]
fn image_records_share_one_component_origin() {
    let (old, new) = load_specs();
    let changes = load_changes();
    let origins = provenance::compute_origins(&old, &new, &changes);

    let targets: Vec<&ChangeRecord> = changes
        .iter()
        .filter(|record| {
            matches!(record.severity, Severity::Breaking | Severity::Review)
                && record_pointer(record)
                    .is_some_and(|pointer| pointer.starts_with("/properties/image/"))
        })
        .collect();
    assert_eq!(targets.len(), 22);

    let mut operations = std::collections::BTreeSet::new();
    let mut positions = std::collections::BTreeSet::new();
    for record in &targets {
        match origins.origins.get(&record.id).unwrap() {
            Origin::Component { name } => assert_eq!(name, "SystemConfigImageDto"),
            other => panic!("record {} has unexpected origin {other:?}", record.id),
        }
        operations.insert(format!(
            "{} {}",
            record.endpoint.method, record.endpoint.path
        ));
        positions.insert(record_position_label(record));
    }
    assert_eq!(operations.len(), 3);
    assert_eq!(positions.len(), 4);
}

/// The removed page parameter traces to its request component.
///
/// The request reference itself never moved, so the record keeps a component
/// origin even though its operation also owns response records with an
/// operation origin.
#[test]
fn removed_page_parameter_traces_to_its_request_component() {
    let (old, new) = load_specs();
    let changes = load_changes();
    let origins = provenance::compute_origins(&old, &new, &changes);

    let record = changes
        .iter()
        .find(|record| {
            record.endpoint.method == "POST"
                && record.endpoint.path == "/search/random"
                && record.kind == ChangeKind::RequestSchemaChanged
        })
        .unwrap();
    assert_eq!(
        origins.origins.get(&record.id).unwrap(),
        &Origin::Component {
            name: "RandomSearchDto".to_string()
        }
    );
}

/// An added endpoint has no origin to establish.
#[test]
fn added_endpoint_stays_unknown() {
    let (old, new) = load_specs();
    let changes = load_changes();
    let origins = provenance::compute_origins(&old, &new, &changes);

    let record = changes
        .iter()
        .find(|record| record.kind == ChangeKind::EndpointAdded)
        .unwrap();
    assert_eq!(origins.origins.get(&record.id).unwrap(), &Origin::Unknown);
}

/// Origins cover every change without touching the records.
///
/// The map holds one entry per change id in report order. The input records
/// keep their ids and severities, since origins live beside them.
#[test]
fn origins_cover_every_change_id_in_order() {
    let (old, new) = load_specs();
    let changes = load_changes();
    let before: Vec<(String, Severity)> = changes
        .iter()
        .map(|record| (record.id.clone(), record.severity.clone()))
        .collect();

    let origins = provenance::compute_origins(&old, &new, &changes);

    let ids: Vec<&String> = origins.origins.keys().collect();
    let expected: Vec<&String> = changes.iter().map(|record| &record.id).collect();
    assert_eq!(ids, expected);
    let after: Vec<(String, Severity)> = changes
        .iter()
        .map(|record| (record.id.clone(), record.severity.clone()))
        .collect();
    assert_eq!(after, before);
}

/// Origins survive a write and read back through a capture directory.
///
/// The test builds a pair and capture tree with the state helpers, copies the
/// pinned specs and the checked in diff output into it, then computes and
/// stores origins through the capture level functions.
#[test]
fn origins_write_and_read_back_through_a_capture() {
    let dir = tempfile::tempdir().unwrap();
    let root = layout::state_root(dir.path());
    let old_bytes = std::fs::read(fixture("immich/old.json")).unwrap();
    let new_bytes = std::fs::read(fixture("immich/new.json")).unwrap();
    let old_hash = layout::sha256_hex(&old_bytes);
    let new_hash = layout::sha256_hex(&new_bytes);
    let (pair_dir, _) = pair::ensure_pair(
        &root,
        Path::new("old.json"),
        Path::new("new.json"),
        &old_bytes,
        &new_bytes,
        &old_hash,
        &new_hash,
    )
    .unwrap();

    let capture_id = "provenance-capture";
    let capture_dir = layout::capture_dir(&pair_dir, capture_id);
    std::fs::create_dir_all(&capture_dir).unwrap();
    let record = CaptureRecord {
        schema_version: SCHEMA_VERSION,
        capture_id: capture_id.to_string(),
        generator_name: "vimanam".to_string(),
        generator_version: "1.3.0".to_string(),
        invocation: vec![
            "vimanam".to_string(),
            "diff".to_string(),
            "old.json".to_string(),
            "new.json".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ],
        vimanam_schema_version: 1,
        old_spec_hash: old_hash,
        new_spec_hash: new_hash,
    };
    write_state_file(&layout::capture_file(&capture_dir), &record).unwrap();
    let changes_bytes =
        std::fs::read(fixture("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json")).unwrap();
    atomic::write_atomic(&layout::changes_file(&capture_dir), &changes_bytes).unwrap();

    let computed = provenance::origins_for_capture(&capture_dir).unwrap();
    provenance::write_origins(&capture_dir, &computed).unwrap();
    assert!(layout::origins_file(&capture_dir).is_file());

    let stored = provenance::read_origins(&capture_dir).unwrap();
    stored.validate().unwrap();
    assert_eq!(stored, computed);

    let (old, new) = load_specs();
    let changes = load_changes();
    assert_eq!(computed, provenance::compute_origins(&old, &new, &changes));
    assert_eq!(computed.origins.len(), changes.len());
}

/// Build one GET operation returning the named component.
fn operation_for(component: &str) -> Value {
    serde_json::json!({
        "responses": {
            "200": {
                "content": {
                    "application/json": {
                        "schema": {"$ref": format!("#/components/schemas/{component}")}
                    }
                }
            }
        }
    })
}

/// Build a spec from path to component pairs and named components.
fn spec_from(paths: &[(&str, &str)], components: &[(&str, Value)]) -> Value {
    let mut path_map = serde_json::Map::new();
    for (path, component) in paths {
        let mut item = serde_json::Map::new();
        item.insert("get".to_string(), operation_for(component));
        path_map.insert((*path).to_string(), Value::Object(item));
    }
    let mut component_map = serde_json::Map::new();
    for (name, schema) in components {
        component_map.insert((*name).to_string(), schema.clone());
    }
    serde_json::json!({
        "openapi": "3.0.0",
        "info": {"title": "Demo", "version": "1"},
        "paths": Value::Object(path_map),
        "components": {"schemas": Value::Object(component_map)}
    })
}

/// Build one response schema change record at the given pointer.
fn response_record(id: &str, path: &str, pointer: &str) -> ChangeRecord {
    ChangeRecord {
        id: id.to_string(),
        endpoint: EndpointRef {
            method: "GET".to_string(),
            path: path.to_string(),
        },
        kind: ChangeKind::ResponseSchemaChanged,
        severity: Severity::Breaking,
        details: ChangeDetails {
            status: Some("200".to_string()),
            schema_change: Some(SchemaChange {
                pointer: pointer.to_string(),
                target: ChangeTarget::Property,
                member: Some("id".to_string()),
                operation: ChangeOperation::Changed,
                before: Presence {
                    present: true,
                    value: Some(Value::String("string".to_string())),
                },
                after: Presence {
                    present: true,
                    value: Some(Value::String("integer".to_string())),
                },
            }),
            ..Default::default()
        },
    }
}

/// Unrelated components with one shared pointer stay separate.
///
/// Both components changed under the same pointer, but each record names its
/// own component. Pointer equality alone never merges them.
#[test]
fn unrelated_components_with_a_shared_pointer_stay_separate() {
    let old_shape = serde_json::json!({"type": "object", "properties": {"id": {"type": "string"}}});
    let new_shape = serde_json::json!({
        "type": "object",
        "properties": {"id": {"type": "string"}, "note": {"type": "string"}}
    });
    let old = spec_from(
        &[("/a", "WidgetA"), ("/b", "WidgetB")],
        &[("WidgetA", old_shape.clone()), ("WidgetB", old_shape)],
    );
    let new = spec_from(
        &[("/a", "WidgetA"), ("/b", "WidgetB")],
        &[("WidgetA", new_shape.clone()), ("WidgetB", new_shape)],
    );
    let changes = vec![
        response_record("change-a", "/a", "/properties/id/type"),
        response_record("change-b", "/b", "/properties/id/type"),
    ];

    let origins = provenance::compute_origins(&old, &new, &changes);

    assert_eq!(
        origins.origins.get("change-a").unwrap(),
        &Origin::Component {
            name: "WidgetA".to_string()
        }
    );
    assert_eq!(
        origins.origins.get("change-b").unwrap(),
        &Origin::Component {
            name: "WidgetB".to_string()
        }
    );
}

/// A shared unchanged component never counts as a shared origin.
///
/// Both operations return the same component and both records point at the
/// same node, yet the component never changed. Both origins stay unknown.
#[test]
fn shared_unchanged_component_with_a_shared_pointer_stays_unknown() {
    let shape = serde_json::json!({"type": "object", "properties": {"id": {"type": "string"}}});
    let old = spec_from(
        &[("/a", "Shared"), ("/b", "Shared")],
        &[("Shared", shape.clone())],
    );
    let new = spec_from(&[("/a", "Shared"), ("/b", "Shared")], &[("Shared", shape)]);
    let changes = vec![
        response_record("change-a", "/a", "/properties/id/type"),
        response_record("change-b", "/b", "/properties/id/type"),
    ];

    let origins = provenance::compute_origins(&old, &new, &changes);

    assert_eq!(origins.origins.get("change-a").unwrap(), &Origin::Unknown);
    assert_eq!(origins.origins.get("change-b").unwrap(), &Origin::Unknown);
}
