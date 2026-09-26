//! Conversion checks for OpenAPI schemas.
//!
//! Unit tests cover every conversion rule in both directions with small
//! synthetic schemas. Pinned-spec tests load the checked in Immich
//! contract and prove the demo item schema converts, accepts a null
//! stack, and records the applied idiom. Nothing here touches the
//! network. References resolve from the loaded specs only.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sethu::stub::schema::{
    Application, ConvertedSchema, Direction, Rule, SpecIndex, UnsupportedKind, convert_named,
    convert_schema,
};

/// Both validation directions, for tests that assert each one.
const DIRECTIONS: [Direction; 2] = [Direction::Request, Direction::Response];

/// Locate a checked in fixture by path under the crate root.
fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Read the new pinned Immich spec as data.
fn new_spec() -> Value {
    let bytes = std::fs::read(fixture_path("immich/new.json")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Build an index from a `components.schemas` map.
fn index_with(schemas: Value) -> SpecIndex {
    let spec = json!({ "components": { "schemas": schemas } });
    SpecIndex::from_spec(&spec).unwrap()
}

/// Build an empty index for reference-free schemas.
fn empty_index() -> SpecIndex {
    SpecIndex::from_spec(&json!({})).unwrap()
}

/// Convert one schema value against the given index.
fn convert_against(schema: &Value, index: &SpecIndex, direction: Direction) -> ConvertedSchema {
    convert_schema(schema, index, direction).unwrap()
}

/// Convert one reference-free schema value.
fn convert(schema: &Value, direction: Direction) -> ConvertedSchema {
    convert_against(schema, &empty_index(), direction)
}

/// Check an instance against one converted document.
fn is_valid(document: &Value, instance: &Value) -> bool {
    jsonschema::validator_for(document)
        .unwrap()
        .is_valid(instance)
}

/// Find applications firing one rule.
fn with_rule(applications: &[Application], rule: Rule) -> Vec<&Application> {
    applications
        .iter()
        .filter(|item| item.rule == rule)
        .collect()
}

/// Build a valid asset instance with the given stack value.
fn valid_asset(stack: Value) -> Value {
    json!({
        "checksum": "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        "deviceAssetId": "device-asset-1",
        "deviceId": "device-1",
        "duration": "0:00:01.000000",
        "fileCreatedAt": "2024-09-27T10:00:00.000Z",
        "fileModifiedAt": "2024-09-27T10:00:00.000Z",
        "hasMetadata": true,
        "id": "550e8400-e29b-41d4-a716-446655440000",
        "isArchived": false,
        "isFavorite": false,
        "isOffline": false,
        "isTrashed": false,
        "localDateTime": "2024-09-27T10:00:00.000Z",
        "originalFileName": "photo.jpg",
        "originalPath": "/photos/photo.jpg",
        "ownerId": "550e8400-e29b-41d4-a716-446655440001",
        "thumbhash": "3OcRJwh4d3h6eIeIh3h2e3h4gQ",
        "type": "IMAGE",
        "updatedAt": "2024-09-27T10:00:00.000Z",
        "stack": stack,
    })
}

#[test]
fn nullable_type_widens_for_requests() {
    let schema = json!({ "type": "string", "minLength": 2, "nullable": true });
    let converted = convert(&schema, Direction::Request);
    assert_eq!(converted.schema["type"], json!(["string", "null"]));
    assert!(
        !converted
            .schema
            .as_object()
            .unwrap()
            .contains_key("nullable")
    );
    let found = with_rule(&converted.applications, Rule::NullableType);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].location, "#");
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(is_valid(&document, &Value::Null));
    assert!(is_valid(&document, &json!("ok")));
    assert!(!is_valid(&document, &json!("x")));
    assert!(!is_valid(&document, &json!(7)));
}

#[test]
fn nullable_type_widens_for_responses() {
    let schema = json!({ "type": "string", "minLength": 2, "nullable": true });
    let converted = convert(&schema, Direction::Response);
    assert_eq!(converted.schema["type"], json!(["string", "null"]));
    let found = with_rule(&converted.applications, Rule::NullableType);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].location, "#");
    let document = converted.document();
    assert!(is_valid(&document, &Value::Null));
    assert!(is_valid(&document, &json!("ok")));
    assert!(!is_valid(&document, &json!("x")));
}

#[test]
fn nullable_reference_idiom_for_requests() {
    let index = index_with(json!({
        "Target": {
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"],
        }
    }));
    let schema = json!({ "allOf": [{ "$ref": "#/components/schemas/Target" }], "nullable": true });
    let converted = convert_against(&schema, &index, Direction::Request);
    assert_eq!(
        converted.schema,
        json!({ "anyOf": [{ "$ref": "#/definitions/Target" }, { "type": "null" }] })
    );
    let idiom = with_rule(&converted.applications, Rule::NullableReference);
    assert_eq!(idiom.len(), 1);
    assert_eq!(idiom[0].location, "#");
    let resolved = with_rule(&converted.applications, Rule::LocalRef);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].location, "#/anyOf/0");
    assert!(converted.definitions.contains_key("Target"));
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(is_valid(&document, &Value::Null));
    assert!(is_valid(&document, &json!({ "id": "a" })));
    assert!(!is_valid(&document, &json!({})));
    assert!(!is_valid(&document, &json!({ "id": 1 })));
}

#[test]
fn nullable_reference_idiom_for_responses() {
    let index = index_with(json!({
        "Target": {
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"],
        }
    }));
    let schema = json!({ "allOf": [{ "$ref": "#/components/schemas/Target" }], "nullable": true });
    let converted = convert_against(&schema, &index, Direction::Response);
    assert_eq!(
        converted.schema,
        json!({ "anyOf": [{ "$ref": "#/definitions/Target" }, { "type": "null" }] })
    );
    let idiom = with_rule(&converted.applications, Rule::NullableReference);
    assert_eq!(idiom.len(), 1);
    let document = converted.document();
    assert!(is_valid(&document, &Value::Null));
    assert!(is_valid(&document, &json!({ "id": "a" })));
    assert!(!is_valid(&document, &json!({})));
}

#[test]
fn readonly_leaves_required_for_requests() {
    let schema = json!({
        "type": "object",
        "properties": { "res": { "type": "string", "readOnly": true } },
        "required": ["res"],
    });
    let converted = convert(&schema, Direction::Request);
    assert_eq!(converted.schema["required"], json!([]));
    let found = with_rule(&converted.applications, Rule::ReadOnly);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].location, "#");
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(is_valid(&document, &json!({})));
    assert!(is_valid(&document, &json!({ "res": "pong" })));
}

#[test]
fn readonly_stays_required_for_responses() {
    let schema = json!({
        "type": "object",
        "properties": { "res": { "type": "string", "readOnly": true } },
        "required": ["res"],
    });
    let converted = convert(&schema, Direction::Response);
    assert_eq!(converted.schema["required"], json!(["res"]));
    assert!(with_rule(&converted.applications, Rule::ReadOnly).is_empty());
    let document = converted.document();
    assert!(!is_valid(&document, &json!({})));
    assert!(is_valid(&document, &json!({ "res": "pong" })));
}

#[test]
fn writeonly_leaves_required_for_responses() {
    let schema = json!({
        "type": "object",
        "properties": { "secret": { "type": "string", "writeOnly": true } },
        "required": ["secret"],
    });
    let converted = convert(&schema, Direction::Response);
    assert_eq!(converted.schema["required"], json!([]));
    let found = with_rule(&converted.applications, Rule::WriteOnly);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].location, "#");
    let document = converted.document();
    assert!(is_valid(&document, &json!({})));
}

#[test]
fn writeonly_stays_required_for_requests() {
    let schema = json!({
        "type": "object",
        "properties": { "secret": { "type": "string", "writeOnly": true } },
        "required": ["secret"],
    });
    let converted = convert(&schema, Direction::Request);
    assert_eq!(converted.schema["required"], json!(["secret"]));
    assert!(with_rule(&converted.applications, Rule::WriteOnly).is_empty());
    let document = converted.document();
    assert!(!is_valid(&document, &json!({})));
    assert!(is_valid(&document, &json!({ "secret": "s" })));
}

#[test]
fn exclusive_minimum_converts_for_requests() {
    let schema = json!({ "type": "integer", "minimum": 5, "exclusiveMinimum": true });
    let converted = convert(&schema, Direction::Request);
    assert_eq!(converted.schema["exclusiveMinimum"], json!(5));
    assert!(
        !converted
            .schema
            .as_object()
            .unwrap()
            .contains_key("minimum")
    );
    let found = with_rule(&converted.applications, Rule::ExclusiveMinimum);
    assert_eq!(found.len(), 1);
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(!is_valid(&document, &json!(5)));
    assert!(is_valid(&document, &json!(6)));
}

#[test]
fn exclusive_minimum_converts_for_responses() {
    let schema = json!({ "type": "integer", "minimum": 5, "exclusiveMinimum": true });
    let converted = convert(&schema, Direction::Response);
    assert_eq!(converted.schema["exclusiveMinimum"], json!(5));
    assert!(
        !converted
            .schema
            .as_object()
            .unwrap()
            .contains_key("minimum")
    );
    assert_eq!(
        with_rule(&converted.applications, Rule::ExclusiveMinimum).len(),
        1
    );
    let document = converted.document();
    assert!(!is_valid(&document, &json!(5)));
    assert!(is_valid(&document, &json!(6)));
}

#[test]
fn exclusive_maximum_converts_for_requests() {
    let schema = json!({ "type": "integer", "maximum": 10, "exclusiveMaximum": true });
    let converted = convert(&schema, Direction::Request);
    assert_eq!(converted.schema["exclusiveMaximum"], json!(10));
    assert!(
        !converted
            .schema
            .as_object()
            .unwrap()
            .contains_key("maximum")
    );
    assert_eq!(
        with_rule(&converted.applications, Rule::ExclusiveMaximum).len(),
        1
    );
    let document = converted.document();
    assert!(!is_valid(&document, &json!(10)));
    assert!(is_valid(&document, &json!(9)));
}

#[test]
fn exclusive_maximum_converts_for_responses() {
    let schema = json!({ "type": "integer", "maximum": 10, "exclusiveMaximum": true });
    let converted = convert(&schema, Direction::Response);
    assert_eq!(converted.schema["exclusiveMaximum"], json!(10));
    assert_eq!(
        with_rule(&converted.applications, Rule::ExclusiveMaximum).len(),
        1
    );
    let document = converted.document();
    assert!(!is_valid(&document, &json!(10)));
    assert!(is_valid(&document, &json!(9)));
}

#[test]
fn exclusive_false_flag_drops() {
    let schema = json!({ "type": "integer", "minimum": 5, "exclusiveMinimum": false });
    let converted = convert(&schema, Direction::Request);
    assert_eq!(converted.schema["minimum"], json!(5));
    assert!(
        !converted
            .schema
            .as_object()
            .unwrap()
            .contains_key("exclusiveMinimum")
    );
    assert!(converted.applications.is_empty());
    let document = converted.document();
    assert!(is_valid(&document, &json!(5)));
    assert!(!is_valid(&document, &json!(4)));
}

#[test]
fn local_reference_resolves_for_requests() {
    let index = index_with(json!({
        "Target": { "type": "string", "minLength": 2 }
    }));
    let schema = json!({ "$ref": "#/components/schemas/Target" });
    let converted = convert_against(&schema, &index, Direction::Request);
    assert_eq!(converted.schema, json!({ "$ref": "#/definitions/Target" }));
    assert_eq!(
        converted.definitions.get("Target").unwrap(),
        &json!({ "type": "string", "minLength": 2 })
    );
    let found = with_rule(&converted.applications, Rule::LocalRef);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].location, "#");
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(is_valid(&document, &json!("ok")));
    assert!(!is_valid(&document, &json!("x")));
}

#[test]
fn local_reference_resolves_for_responses() {
    let index = index_with(json!({
        "Target": { "type": "string", "minLength": 2 }
    }));
    let schema = json!({ "$ref": "#/components/schemas/Target" });
    let converted = convert_against(&schema, &index, Direction::Response);
    assert_eq!(converted.schema, json!({ "$ref": "#/definitions/Target" }));
    assert!(converted.definitions.contains_key("Target"));
    assert_eq!(with_rule(&converted.applications, Rule::LocalRef).len(), 1);
    let document = converted.document();
    assert!(is_valid(&document, &json!("ok")));
    assert!(!is_valid(&document, &json!("x")));
}

#[test]
fn allof_passes_through_for_requests() {
    let schema = json!({ "allOf": [{ "type": "string" }, { "minLength": 2 }] });
    let converted = convert(&schema, Direction::Request);
    assert_eq!(converted.schema, schema);
    assert!(converted.applications.is_empty());
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(is_valid(&document, &json!("ok")));
    assert!(!is_valid(&document, &json!("x")));
}

#[test]
fn allof_passes_through_for_responses() {
    let schema = json!({ "allOf": [{ "type": "string" }, { "minLength": 2 }] });
    let converted = convert(&schema, Direction::Response);
    assert_eq!(converted.schema, schema);
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(is_valid(&document, &json!("ok")));
    assert!(!is_valid(&document, &json!("x")));
}

#[test]
fn choice_keywords_pass_through_for_requests() {
    let schema = json!({
        "oneOf": [{ "type": "string" }, { "type": "integer" }],
        "anyOf": [{ "maxLength": 3 }],
    });
    let converted = convert(&schema, Direction::Request);
    assert_eq!(converted.schema, schema);
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(is_valid(&document, &json!("ok")));
    assert!(!is_valid(&document, &json!("toolong")));
}

#[test]
fn choice_keywords_pass_through_for_responses() {
    let schema = json!({
        "oneOf": [{ "type": "string" }, { "type": "integer" }],
        "anyOf": [{ "maxLength": 3 }],
    });
    let converted = convert(&schema, Direction::Response);
    assert_eq!(converted.schema, schema);
    assert!(converted.supports_claim());
    let document = converted.document();
    assert!(is_valid(&document, &json!(4)));
    assert!(!is_valid(&document, &json!("toolong")));
}

#[test]
fn property_named_type_converts() {
    for direction in DIRECTIONS {
        let index = index_with(json!({ "Kind": { "type": "string" } }));
        let schema = json!({
            "type": "object",
            "properties": { "type": { "$ref": "#/components/schemas/Kind" } },
            "required": ["type"],
        });
        let converted = convert_against(&schema, &index, direction);
        assert_eq!(
            converted.schema["properties"]["type"],
            json!({ "$ref": "#/definitions/Kind" })
        );
        assert!(converted.supports_claim());
        let document = converted.document();
        assert!(is_valid(&document, &json!({ "type": "x" })));
        assert!(!is_valid(&document, &json!({})));
    }
}

#[test]
fn discriminator_is_reported() {
    for direction in DIRECTIONS {
        let schema = json!({
            "oneOf": [{ "$ref": "#/components/schemas/A" }, { "type": "string" }],
            "discriminator": { "propertyName": "kind" },
        });
        let index = index_with(json!({ "A": { "type": "string" } }));
        let converted = convert_against(&schema, &index, direction);
        let issues: Vec<_> = converted
            .unsupported
            .iter()
            .filter(|issue| issue.kind == UnsupportedKind::Discriminator)
            .collect();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].location, "#");
        assert!(!converted.supports_claim());
    }
}

#[test]
fn nullable_without_type_is_reported() {
    for direction in DIRECTIONS {
        let schema = json!({ "description": "open value", "nullable": true });
        let converted = convert(&schema, direction);
        let issues: Vec<_> = converted
            .unsupported
            .iter()
            .filter(|issue| issue.kind == UnsupportedKind::NullableWithoutType)
            .collect();
        assert_eq!(issues.len(), 1);
        assert!(
            !converted
                .schema
                .as_object()
                .unwrap()
                .contains_key("nullable")
        );
        assert!(!converted.supports_claim());
    }
}

#[test]
fn external_reference_is_reported() {
    for direction in DIRECTIONS {
        let schema = json!({ "$ref": "https://example.com/schemas.json#/Target" });
        let converted = convert(&schema, direction);
        let issues: Vec<_> = converted
            .unsupported
            .iter()
            .filter(|issue| issue.kind == UnsupportedKind::ExternalRef)
            .collect();
        assert_eq!(issues.len(), 1);
        assert_eq!(
            converted.schema["$ref"],
            json!("https://example.com/schemas.json#/Target")
        );
        assert!(!converted.supports_claim());
    }
}

#[test]
fn unknown_format_is_reported() {
    for direction in DIRECTIONS {
        let schema = json!({ "type": "string", "format": "binary" });
        let converted = convert(&schema, direction);
        let issues: Vec<_> = converted
            .unsupported
            .iter()
            .filter(|issue| issue.kind == UnsupportedKind::Format)
            .collect();
        assert_eq!(issues.len(), 1);
        assert_eq!(converted.schema["format"], json!("binary"));
        assert!(!converted.supports_claim());
    }
}

#[test]
fn builtin_formats_pass_quietly() {
    for direction in DIRECTIONS {
        let schema = json!({ "type": "string", "format": "date-time" });
        let converted = convert(&schema, direction);
        assert!(converted.unsupported.is_empty());
        assert_eq!(converted.schema["format"], json!("date-time"));
    }
}

#[test]
fn xml_is_reported() {
    for direction in DIRECTIONS {
        let schema = json!({ "type": "object", "xml": { "name": "Pet" } });
        let converted = convert(&schema, direction);
        let issues: Vec<_> = converted
            .unsupported
            .iter()
            .filter(|issue| issue.kind == UnsupportedKind::Xml)
            .collect();
        assert_eq!(issues.len(), 1);
        assert!(!converted.supports_claim());
    }
}

#[test]
fn unknown_component_errors() {
    let index = empty_index();
    assert!(convert_named(&index, "Missing", Direction::Request).is_err());
    let schema = json!({ "$ref": "#/components/schemas/Missing" });
    assert!(convert_schema(&schema, &index, Direction::Request).is_err());
    let odd = json!({ "$ref": "#/paths/~1search" });
    assert!(convert_schema(&odd, &index, Direction::Request).is_err());
}

#[test]
fn asset_response_dto_accepts_null_stack() {
    let spec = new_spec();
    let index = SpecIndex::from_spec(&spec).unwrap();
    for direction in DIRECTIONS {
        let converted = convert_named(&index, "AssetResponseDto", direction).unwrap();
        let stack = &converted.schema["properties"]["stack"];
        assert_eq!(
            stack["anyOf"][1],
            json!({ "type": "null" }),
            "stack keeps a null branch"
        );
        assert_eq!(
            stack["anyOf"][0]["$ref"],
            json!("#/definitions/AssetStackResponseDto")
        );
        let idiom: Vec<_> = converted
            .applications
            .iter()
            .filter(|item| {
                item.rule == Rule::NullableReference && item.location == "#/properties/stack"
            })
            .collect();
        assert_eq!(idiom.len(), 1, "stack records the applied idiom");
        let document = converted.document();
        assert!(is_valid(&document, &valid_asset(Value::Null)));
        assert!(is_valid(
            &document,
            &valid_asset(json!({
                "assetCount": 2,
                "id": "stack-1",
                "primaryAssetId": "550e8400-e29b-41d4-a716-446655440000",
            }))
        ));
        assert!(!is_valid(&document, &valid_asset(json!("nope"))));
        // The closure reaches one format the validator cannot check, an
        // integer width on the exchange metadata size. It is reported
        // above and never fails validation with formats off.
        assert_eq!(converted.unsupported.len(), 1);
        assert_eq!(converted.unsupported[0].kind, UnsupportedKind::Format);
        assert!(converted.unsupported[0].location.contains("fileSizeInByte"));
        assert!(!converted.supports_claim());
    }
}

#[test]
fn idiom_rule_covers_further_components() {
    let spec = new_spec();
    let index = SpecIndex::from_spec(&spec).unwrap();
    for direction in DIRECTIONS {
        let face = convert_named(&index, "AssetFaceResponseDto", direction).unwrap();
        let person: Vec<_> = face
            .applications
            .iter()
            .filter(|item| {
                item.rule == Rule::NullableReference && item.location == "#/properties/person"
            })
            .collect();
        assert_eq!(person.len(), 1);
        let face_instance = json!({
            "boundingBoxX1": 1,
            "boundingBoxX2": 4,
            "boundingBoxY1": 2,
            "boundingBoxY2": 5,
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "imageHeight": 100,
            "imageWidth": 100,
            "person": null,
            "sourceType": "exif",
        });
        assert!(is_valid(&face.document(), &face_instance));

        let admin = convert_named(&index, "UserAdminResponseDto", direction).unwrap();
        let license: Vec<_> = admin
            .applications
            .iter()
            .filter(|item| {
                item.rule == Rule::NullableReference && item.location == "#/properties/license"
            })
            .collect();
        assert_eq!(license.len(), 1);
    }
}

#[test]
fn conversion_is_deterministic() {
    let spec = new_spec();
    let index = SpecIndex::from_spec(&spec).unwrap();
    let first = convert_named(&index, "AssetResponseDto", Direction::Response).unwrap();
    let second = convert_named(&index, "AssetResponseDto", Direction::Response).unwrap();
    assert_eq!(first, second);
    let round_trip: ConvertedSchema =
        serde_json::from_value(serde_json::to_value(&first).unwrap()).unwrap();
    assert_eq!(first, round_trip);
}
