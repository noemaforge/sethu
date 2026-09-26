//! Fixture and live checks for the Vimanam adapter.
//!
//! Fixture tests read the checked in JSON captures and need no binary.
//! Live tests run the released binary and skip with a named reason when
//! `vimanam` is absent from PATH. The gate provides the binary on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use sethu::vimanam::{
    ChangeKind, DetailLevel, DiffDocument, parse_diff_output, probe_vimanam, render_operation,
    run_diff,
};

/// Locate a checked in fixture by path under the crate root.
fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Read a fixture file as raw bytes.
fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).unwrap()
}

/// Parse a fixture file into both its raw value and its typed document.
fn parse_fixture(name: &str) -> (serde_json::Value, DiffDocument) {
    let bytes = fixture_bytes(name);
    let raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let document = parse_diff_output(&bytes).unwrap();
    (raw, document)
}

/// Check that every record survives a typed rewrite unchanged.
///
/// The comparison covers the full record value, and then the id and the
/// severity strings on their own. The adapter never recomputes either.
fn assert_records_round_trip(raw: &serde_json::Value, document: &DiffDocument) {
    let raw_changes = raw["changes"].as_array().unwrap();
    assert_eq!(raw_changes.len(), document.changes.len());
    for (raw_change, change) in raw_changes.iter().zip(document.changes.iter()) {
        let rewritten = serde_json::to_value(change).unwrap();
        assert_eq!(&rewritten, raw_change);
        assert_eq!(
            rewritten["id"].as_str().unwrap(),
            raw_change["id"].as_str().unwrap()
        );
        assert_eq!(
            rewritten["severity"].as_str().unwrap(),
            raw_change["severity"].as_str().unwrap()
        );
    }
}

#[test]
fn parses_immich_capture_from_current_release() {
    let (raw, document) = parse_fixture("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json");
    assert_eq!(document.schema_version, 1);
    assert_eq!(document.generator.name, "vimanam");
    assert_eq!(document.generator.version, "1.3.0");
    assert_eq!(document.changes.len(), 46);
    assert_eq!(document.summary.breaking, 26);
    assert_eq!(document.summary.non_breaking, 19);
    assert_eq!(document.summary.review, 1);
    assert_records_round_trip(&raw, &document);
}

#[test]
fn parses_self_pair_capture_from_current_release() {
    let (raw, document) = parse_fixture("vimanam-1.3.0/vimanam-pair.json");
    assert_eq!(document.generator.version, "1.3.0");
    assert_eq!(document.changes.len(), 13);
    assert_records_round_trip(&raw, &document);
}

#[test]
fn parses_older_release_capture_with_same_records() {
    let (raw_old, old_document) = parse_fixture("vimanam-1.2.0/immich-v1.116.2-v1.117.0.json");
    assert_eq!(old_document.generator.version, "1.2.0");
    assert_eq!(old_document.changes.len(), 46);
    assert_records_round_trip(&raw_old, &old_document);

    let (_, new_document) = parse_fixture("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json");
    let old_ids: Vec<&str> = old_document
        .changes
        .iter()
        .map(|change| change.id.as_str())
        .collect();
    let new_ids: Vec<&str> = new_document
        .changes
        .iter()
        .map(|change| change.id.as_str())
        .collect();
    assert_eq!(old_ids, new_ids);
}

#[test]
fn parses_operation_id_change_with_null_side() {
    let (raw, document) = parse_fixture("vimanam-1.3.0/operation-id-change.json");
    assert_eq!(document.generator.version, "1.3.0");
    assert_eq!(document.changes.len(), 1);
    let change = &document.changes[0];
    assert_eq!(change.kind, ChangeKind::OperationIdChanged);
    assert_eq!(change.details.old, Some(None));
    assert_eq!(change.details.new, Some(Some("listWidgets".to_string())));
    assert_records_round_trip(&raw, &document);
}

#[test]
fn refuses_unknown_schema_version() {
    let bytes = fixture_bytes("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json");
    let mut raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    raw["schema_version"] = serde_json::json!(99);
    let bytes = serde_json::to_vec(&raw).unwrap();
    assert!(parse_diff_output(&bytes).is_err());
}

#[test]
fn refuses_foreign_generator() {
    let bytes = fixture_bytes("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json");
    let mut raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    raw["generator"]["name"] = serde_json::json!("other");
    let bytes = serde_json::to_vec(&raw).unwrap();
    let error = parse_diff_output(&bytes).unwrap_err();
    assert!(error.to_string().contains("other"));
}

/// Locate the live binary or skip the calling test with a named reason.
///
/// Resolution respects PATH as is. Tests that need the real binary call
/// this first and return early when it answers none.
fn require_vimanam(test_name: &str) -> Option<tempfile::TempDir> {
    let probe = Command::new("vimanam")
        .arg("--version")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();
    match probe {
        Ok(output) if output.status.success() => Some(tempfile::tempdir().unwrap()),
        _ => {
            eprintln!(
                "SKIP {test_name}: `vimanam` is not on PATH, live check needs the released binary"
            );
            None
        }
    }
}

#[test]
fn live_probe_reports_supported_release() {
    let Some(workdir) = require_vimanam("live_probe_reports_supported_release") else {
        return;
    };
    let version = probe_vimanam(workdir.path()).unwrap();
    assert!(version.is_supported());
    assert_eq!(version.to_string(), "1.3.0");
}

#[test]
fn live_diff_matches_checked_in_fixture() {
    let Some(workdir) = require_vimanam("live_diff_matches_checked_in_fixture") else {
        return;
    };
    let old = fixture_path("immich/old.json");
    let new = fixture_path("immich/new.json");
    let document = run_diff(&old, &new, false, workdir.path()).unwrap();
    let (_, fixture) = parse_fixture("vimanam-1.3.0/immich-v1.116.2-v1.117.0.json");
    let live_ids: Vec<&str> = document
        .changes
        .iter()
        .map(|change| change.id.as_str())
        .collect();
    let fixture_ids: Vec<&str> = fixture
        .changes
        .iter()
        .map(|change| change.id.as_str())
        .collect();
    assert_eq!(live_ids, fixture_ids);
}

#[test]
fn live_exit_three_parses_as_output() {
    let Some(workdir) = require_vimanam("live_exit_three_parses_as_output") else {
        return;
    };
    let old = fixture_path("immich/old.json");
    let new = fixture_path("immich/new.json");
    let plain = run_diff(&old, &new, false, workdir.path()).unwrap();
    let breaking = run_diff(&old, &new, true, workdir.path()).unwrap();
    assert_eq!(breaking.changes.len(), 46);
    let plain_ids: Vec<&str> = plain
        .changes
        .iter()
        .map(|change| change.id.as_str())
        .collect();
    let breaking_ids: Vec<&str> = breaking
        .changes
        .iter()
        .map(|change| change.id.as_str())
        .collect();
    assert_eq!(plain_ids, breaking_ids);
}

#[test]
fn live_render_operation_returns_endpoint_text() {
    let Some(workdir) = require_vimanam("live_render_operation_returns_endpoint_text") else {
        return;
    };
    let old = fixture_path("immich/old.json");
    let text = render_operation(
        &old,
        "POST",
        "/search/random",
        DetailLevel::Standard,
        workdir.path(),
    )
    .unwrap();
    assert!(text.contains("POST /search/random"));
    let missing = render_operation(
        &old,
        "GET",
        "/no/such/path",
        DetailLevel::Standard,
        workdir.path(),
    );
    assert!(missing.is_err());
}
