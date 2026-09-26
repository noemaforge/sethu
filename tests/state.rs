//! Tests for state primitives: atomic writes, envelopes, and lookups.

use std::path::{Path, PathBuf};

use sethu::state;
use sethu::state::atomic;
use sethu::state::layout;
use state::{
    CaptureFile, ChangeRef, ChangesFile, Installation, LedgerEntry, LedgerFile, MigrationManifest,
    OriginsFile, PairFile,
};

fn temp_root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn full_hash(prefix12: &str, fill: char) -> String {
    let mut hash = prefix12.to_string();
    while hash.len() < 64 {
        hash.push(fill);
    }
    hash
}

fn state_root_for(temp: &tempfile::TempDir) -> PathBuf {
    layout::state_root(temp.path())
}

fn write_pair(root: &Path, old_full: &str, new_full: &str) -> PathBuf {
    let dir = layout::pair_dir(root, old_full, new_full).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let file = PairFile::new(old_full, new_full);
    state::write_state_file(&layout::pair_file(&dir), &file).unwrap();
    dir
}

fn write_manifest(root: &Path, attempt: &str, old_full: &str, new_full: &str) -> PathBuf {
    let dir = layout::migration_dir(root, attempt);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = MigrationManifest::new(attempt, old_full, new_full);
    state::write_state_file(&layout::manifest_path(&dir), &manifest).unwrap();
    dir
}

fn write_capture(pair: &Path, capture_id: &str) -> PathBuf {
    let dir = layout::capture_dir(pair, capture_id);
    std::fs::create_dir_all(&dir).unwrap();
    let capture = CaptureFile::new(capture_id);
    state::write_state_file(&layout::capture_file(&dir), &capture).unwrap();
    dir
}

fn leave_stale_temp(dir: &Path, partial: &[u8]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let temp = dir.join(format!(
        "{}stale-1{}",
        atomic::PENDING_PREFIX,
        atomic::PENDING_SUFFIX
    ));
    std::fs::write(&temp, partial).unwrap();
    assert!(atomic::is_pending_temp(&temp));
    temp
}

#[test]
fn atomic_write_creates_file_with_exact_bytes() {
    let root = temp_root();
    let path = root.path().join("sub").join("state.json");
    atomic::write_atomic(&path, b"hello").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    assert!(!atomic::is_pending_temp(&path));
}

#[test]
fn atomic_write_replaces_content() {
    let root = temp_root();
    let path = root.path().join("state.json");
    atomic::write_atomic(&path, b"old").unwrap();
    atomic::write_atomic(&path, b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
}

#[test]
fn simulated_crash_keeps_previous_content() {
    let root = temp_root();
    let path = root.path().join("ledger.json");
    atomic::write_atomic(&path, b"old").unwrap();
    leave_stale_temp(root.path(), b"par");
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
}

#[test]
fn simulated_crash_before_first_write_leaves_no_target() {
    let root = temp_root();
    let path = root.path().join("new.json");
    leave_stale_temp(root.path(), b"par");
    assert!(!path.exists());
    atomic::write_atomic(&path, b"fresh").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"fresh");
}

#[test]
fn write_after_crash_parses_cleanly() {
    let root = temp_root();
    let path = root.path().join("installation.json");
    leave_stale_temp(root.path(), b"{broken");
    let value = Installation::new("0.1.0");
    state::write_state_file(&path, &value).unwrap();
    let back: Installation = state::read_state_file(&path).unwrap();
    back.validate().unwrap();
    assert_eq!(back, value);
}

#[test]
fn installation_envelope_stamps_supported_version() {
    let root = temp_root();
    let path = root.path().join("installation.json");
    let value = Installation::new("0.1.0");
    state::write_state_file(&path, &value).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"schema_version\""));
    let back: Installation = state::read_state_file(&path).unwrap();
    back.validate().unwrap();
    assert_eq!(back.schema_version, state::SCHEMA_VERSION);
    assert_eq!(back.sethu_version, "0.1.0");
}

#[test]
fn unknown_schema_version_fails_validation() {
    let root = temp_root();
    let path = root.path().join("installation.json");
    atomic::write_atomic(&path, br#"{"schema_version":999,"sethu_version":"x"}"#).unwrap();
    let back: Installation = state::read_state_file(&path).unwrap();
    assert!(back.validate().is_err());
}

#[test]
fn ledger_keeps_insertion_order() {
    let root = temp_root();
    let path = root.path().join("ledger.json");
    let mut entries = indexmap::IndexMap::new();
    entries.insert("zeta".to_string(), LedgerEntry::new("unresolved", None));
    entries.insert("alpha".to_string(), LedgerEntry::new("fixed", Some("ok")));
    let ledger = LedgerFile::new(entries);
    state::write_state_file(&path, &ledger).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    let first = raw.find("zeta").unwrap();
    let second = raw.find("alpha").unwrap();
    assert!(first < second);
    let back: LedgerFile = state::read_state_file(&path).unwrap();
    back.validate().unwrap();
    let keys: Vec<&str> = back.entries.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["zeta", "alpha"]);
    assert_eq!(back, ledger);
}

#[test]
fn changes_and_origins_stamp_supported_version() {
    let root = temp_root();
    let changes_path = root.path().join("changes.json");
    let origins_path = root.path().join("origins.json");
    let changes = ChangesFile::new(vec![
        ChangeRef::new("chg-1", "breaking"),
        ChangeRef::new("chg-2", "cosmetic"),
    ]);
    state::write_state_file(&changes_path, &changes).unwrap();
    let mut origins_map = indexmap::IndexMap::new();
    origins_map.insert("chg-1".to_string(), "diff".to_string());
    origins_map.insert("chg-2".to_string(), "trace".to_string());
    let origins = OriginsFile::new(origins_map);
    state::write_state_file(&origins_path, &origins).unwrap();
    let back_changes: ChangesFile = state::read_state_file(&changes_path).unwrap();
    back_changes.validate().unwrap();
    assert_eq!(back_changes, changes);
    let back_origins: OriginsFile = state::read_state_file(&origins_path).unwrap();
    back_origins.validate().unwrap();
    assert_eq!(back_origins, origins);
}

#[test]
fn pair_lookup_returns_none_when_missing() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("0123456789ab", 'a');
    let new = full_hash("cdef01234567", 'c');
    let hit = layout::find_pair(&state_root, &old, &new).unwrap();
    assert!(hit.is_none());
}

#[test]
fn pair_lookup_finds_exact_match_with_expected_layout() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("0123456789ab", 'a');
    let new = full_hash("cdef01234567", 'c');
    let dir = write_pair(&state_root, &old, &new);
    assert_eq!(
        dir.file_name().unwrap().to_str().unwrap(),
        "0123456789ab-cdef01234567"
    );
    let hit = layout::find_pair(&state_root, &old, &new).unwrap().unwrap();
    assert_eq!(hit, dir);
    assert_eq!(layout::pair_file(&dir), dir.join("pair.json"));
    assert_eq!(layout::pair_inputs_dir(&dir), dir.join("inputs"));
    assert_eq!(
        layout::capture_file(&layout::capture_dir(&dir, "cap-1")),
        dir.join("captures").join("cap-1").join("capture.json")
    );
    assert_eq!(
        layout::installation_path(&state_root),
        state_root.join("installation.json")
    );
}

#[test]
fn pair_lookup_rejects_same_prefix_with_different_full_hash() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old_a = full_hash("0123456789ab", 'a');
    let new_a = full_hash("cdef01234567", 'c');
    write_pair(&state_root, &old_a, &new_a);
    let old_b = full_hash("0123456789ab", 'b');
    let new_b = full_hash("cdef01234567", 'd');
    assert_eq!(
        layout::pair_dir_name(&old_a, &new_a).unwrap(),
        layout::pair_dir_name(&old_b, &new_b).unwrap()
    );
    let hit = layout::find_pair(&state_root, &old_b, &new_b);
    assert!(hit.is_err());
    assert!(
        layout::check_pair_identity(
            &layout::pair_dir(&state_root, &old_a, &new_a).unwrap(),
            &old_b,
            &new_b
        )
        .is_err()
    );
}

#[test]
fn hash_prefix_validation_rejects_bad_input() {
    let digest = layout::sha256_hex(b"old-spec");
    assert_eq!(digest.len(), 64);
    let prefix = layout::hash_prefix(&digest).unwrap();
    assert_eq!(prefix.len(), 12);
    assert_eq!(prefix, digest[..12].to_ascii_lowercase());
    assert!(layout::hash_prefix("abc").is_err());
    assert!(layout::hash_prefix(&"z".repeat(64)).is_err());
    assert!(layout::hash_prefix(&"A".repeat(64)).is_ok());
}

#[test]
fn migration_lookup_needs_unique_prefix() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("111111111111", '1');
    let new = full_hash("222222222222", '2');
    write_manifest(&state_root, "attempt-alpha-001", &old, &new);
    write_manifest(&state_root, "attempt-alpha-002", &old, &new);
    assert!(layout::find_migration(&state_root, "attempt-alpha").is_err());
    let one = layout::find_migration(&state_root, "attempt-alpha-001")
        .unwrap()
        .unwrap();
    assert_eq!(one.file_name().unwrap(), "attempt-alpha-001");
    assert_eq!(layout::manifest_path(&one), one.join("manifest.json"));
    assert_eq!(layout::ledger_path(&one), one.join("ledger.json"));
    assert!(
        layout::find_migration(&state_root, "attempt-missing")
            .unwrap()
            .is_none()
    );
}

#[test]
fn migration_lookup_rejects_manifest_mismatch() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("111111111111", '1');
    let new = full_hash("222222222222", '2');
    write_manifest(&state_root, "attempt-solo", &old, &new);
    let dir = layout::migration_dir(&state_root, "attempt-other");
    std::fs::create_dir_all(&dir).unwrap();
    let wrong = MigrationManifest::new("attempt-solo", &old, &new);
    state::write_state_file(&layout::manifest_path(&dir), &wrong).unwrap();
    assert!(layout::find_migration(&state_root, "attempt-other").is_err());
}

#[test]
fn migration_lookup_ignores_stale_temps() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("111111111111", '1');
    let new = full_hash("222222222222", '2');
    let dir = write_manifest(&state_root, "attempt-clean", &old, &new);
    leave_stale_temp(&layout::migrations_dir(&state_root), b"par");
    let hit = layout::find_migration(&state_root, "attempt-clean")
        .unwrap()
        .unwrap();
    assert_eq!(hit, dir);
}

#[test]
fn capture_lookup_checks_full_id() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("0123456789ab", 'a');
    let new = full_hash("cdef01234567", 'c');
    let pair = write_pair(&state_root, &old, &new);
    let first = write_capture(&pair, "capture-aaa-1");
    write_capture(&pair, "capture-aaa-2");
    assert!(layout::find_capture(&pair, "capture-aaa").is_err());
    let hit = layout::find_capture(&pair, "capture-aaa-1")
        .unwrap()
        .unwrap();
    assert_eq!(hit, first);
    layout::check_capture_identity(&hit, "capture-aaa-1").unwrap();
    assert!(layout::check_capture_identity(&hit, "capture-aaa-2").is_err());
    assert_eq!(layout::changes_file(&hit), hit.join("changes.json"));
    assert_eq!(layout::origins_file(&hit), hit.join("origins.json"));
}

#[test]
fn migration_subpaths_cover_expected_tree() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let dir = layout::migration_dir(&state_root, "attempt-tree");
    assert_eq!(layout::traces_dir(&dir), dir.join("traces"));
    assert_eq!(layout::context_dir(&dir), dir.join("context"));
    assert_eq!(layout::harness_dir(&dir), dir.join("harness"));
    assert_eq!(layout::runs_dir(&dir), dir.join("runs"));
    assert_eq!(
        layout::run_dir(&dir, "run-1"),
        dir.join("runs").join("run-1")
    );
    assert_eq!(layout::reports_dir(&dir), dir.join("reports"));
}

#[test]
fn pair_lookup_rejects_unknown_schema_version() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("0123456789ab", 'a');
    let new = full_hash("cdef01234567", 'c');
    let dir = write_pair(&state_root, &old, &new);
    let raw = format!(
        "{{\"schema_version\":999,\"old_spec_hash\":\"{old}\",\"new_spec_hash\":\"{new}\"}}"
    );
    atomic::write_atomic(&layout::pair_file(&dir), raw.as_bytes()).unwrap();
    assert!(layout::find_pair(&state_root, &old, &new).is_err());
}

#[test]
fn capture_lookup_rejects_unknown_schema_version() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("0123456789ab", 'a');
    let new = full_hash("cdef01234567", 'c');
    let pair = write_pair(&state_root, &old, &new);
    let capture = write_capture(&pair, "capture-versioned");
    let raw = "{\"schema_version\":999,\"capture_id\":\"capture-versioned\"}";
    atomic::write_atomic(&layout::capture_file(&capture), raw.as_bytes()).unwrap();
    assert!(layout::find_capture(&pair, "capture-versioned").is_err());
}

#[test]
fn migration_lookup_rejects_unknown_schema_version() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("111111111111", '1');
    let new = full_hash("222222222222", '2');
    let dir = write_manifest(&state_root, "attempt-versioned", &old, &new);
    let raw = format!(
        "{{\"schema_version\":999,\"attempt_id\":\"attempt-versioned\",\"old_spec_hash\":\"{old}\",\"new_spec_hash\":\"{new}\"}}"
    );
    atomic::write_atomic(&layout::manifest_path(&dir), raw.as_bytes()).unwrap();
    assert!(layout::find_migration(&state_root, "attempt-versioned").is_err());
}

#[test]
fn migration_lookup_rejects_empty_input() {
    let root = temp_root();
    let state_root = state_root_for(&root);
    let old = full_hash("111111111111", '1');
    let new = full_hash("222222222222", '2');
    write_manifest(&state_root, "attempt-solo-empty", &old, &new);
    assert!(layout::find_migration(&state_root, "").is_err());
}
