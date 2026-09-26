//! Merge of owned modes into the project mode file.
//!
//! The project file keeps every unrelated mode untouched. Owned modes arrive
//! from the embedded pack. A mode whose slug matches an owned slug takes the
//! embedded content. A matching slug outside the owned set signals a user
//! mode with a colliding name. The merge reports the collision and keeps the
//! user content. Serialization normalizes formatting. Callers skip the write
//! when the parsed values already match, so formatting edits survive.

use anyhow::Context;
use serde_norway::Value;

/// Key holding the mode list at the top of the mode file.
const MODES_KEY: &str = "customModes";

/// Key holding the unique identifier inside one mode entry.
const SLUG_KEY: &str = "slug";

/// Result of merging owned modes into one project mode file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModesMerge {
    /// Full merged file content with one trailing newline.
    pub content: String,
    /// Owned slugs appended to the file, in pack order.
    pub added: Vec<String>,
    /// Owned slugs replaced with pack content, in pack order.
    pub updated: Vec<String>,
    /// Colliding slugs left untouched, in pack order.
    pub collisions: Vec<String>,
    /// False when the parsed merge matches the parsed input.
    pub changed: bool,
}

/// List the mode slugs in a mode file, in file order.
///
/// An empty document reads as no modes. Anything else must parse as a
/// mapping whose mode list holds mappings with string slugs.
pub fn entry_slugs(text: &str) -> anyhow::Result<Vec<String>> {
    parse_modes(text, "mode text").and_then(|modes| {
        modes
            .iter()
            .map(|mode| slug_of(mode, "mode text"))
            .collect()
    })
}

/// Merge owned pack modes into an existing project mode file.
///
/// `existing` is None when the project file is absent. `owned` names the
/// slugs a previous install recorded as owned. A pack slug that matches an
/// existing slug takes the pack content only when `owned` holds it.
/// Otherwise the merge records a collision and keeps the existing entry.
/// Unrelated entries and unrelated top level keys ride along unchanged.
pub fn merge_modes(
    existing: Option<&str>,
    pack: &str,
    owned: &[String],
) -> anyhow::Result<ModesMerge> {
    let pack_modes = parse_modes(pack, "embedded mode file")?;
    for mode in &pack_modes {
        slug_of(mode, "embedded mode file")?;
    }
    let existing_value = match existing {
        None => Value::Mapping(serde_norway::Mapping::new()),
        Some(text) => parse_document(text, "project mode file")?,
    };
    let mut merged_map = match &existing_value {
        Value::Mapping(map) => map.clone(),
        Value::Null => serde_norway::Mapping::new(),
        _ => anyhow::bail!("project mode file must hold a mapping"),
    };
    let mut merged_modes = match merged_map.get(Value::from(MODES_KEY)) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Sequence(items)) => items.clone(),
        Some(_) => anyhow::bail!("project mode file key {MODES_KEY} must hold a list"),
    };
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut collisions = Vec::new();
    for mode in &pack_modes {
        let slug = slug_of(mode, "embedded mode file")?;
        match position_by_slug(&merged_modes, &slug) {
            None => {
                merged_modes.push(mode.clone());
                added.push(slug);
            }
            Some(index) => {
                if owned.iter().any(|name| name == &slug) {
                    if merged_modes[index] != *mode {
                        merged_modes[index] = mode.clone();
                        updated.push(slug);
                    }
                } else {
                    collisions.push(slug);
                }
            }
        }
    }
    merged_map.insert(Value::from(MODES_KEY), Value::Sequence(merged_modes));
    let content = normalize_text(&serde_norway::to_string(&Value::Mapping(merged_map))?);
    let changed = match existing {
        None => true,
        Some(_) => parse_document(&content, "merged mode file")? != existing_value,
    };
    Ok(ModesMerge {
        content,
        added,
        updated,
        collisions,
        changed,
    })
}

/// Parse a mode file into its mode list.
///
/// Empty input reads as an empty list. A missing mode key reads the same.
/// The error names the given label, never a caller path.
fn parse_modes(text: &str, label: &str) -> anyhow::Result<Vec<Value>> {
    let value = parse_document(text, label)?;
    let map = match &value {
        Value::Mapping(map) => map,
        Value::Null => return Ok(Vec::new()),
        _ => anyhow::bail!("{label} must hold a mapping"),
    };
    match map.get(Value::from(MODES_KEY)) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Sequence(items)) => Ok(items.to_vec()),
        Some(_) => anyhow::bail!("{label} key {MODES_KEY} must hold a list"),
    }
}

/// Parse one YAML document, reading empty input as null.
///
/// The error names the given label so callers add their own file context.
fn parse_document(text: &str, label: &str) -> anyhow::Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_norway::from_str(text).with_context(|| format!("parse {label} as YAML"))
}

/// Read the slug of one mode entry.
///
/// The entry must be a mapping with a string slug. The error names the
/// given label.
fn slug_of(mode: &Value, label: &str) -> anyhow::Result<String> {
    mode.get(SLUG_KEY)
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("{label} holds a mode without a string {SLUG_KEY}"))
}

/// Find the first entry with a slug in a mode list.
fn position_by_slug(modes: &[Value], slug: &str) -> Option<usize> {
    modes.iter().position(|mode| {
        mode.get(SLUG_KEY)
            .and_then(Value::as_str)
            .is_some_and(|found| found == slug)
    })
}

/// End file text with exactly one newline.
///
/// Serialization already ends with a newline in the common case. This
/// helper keeps that shape when it changes.
fn normalize_text(text: &str) -> String {
    let trimmed = text.trim_end_matches('\n');
    format!("{trimmed}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack text with one owned mode for merge tests.
    fn pack_text() -> String {
        "customModes:\n  - slug: sethu-migrator\n    name: Sethu Migrator\n    roleDefinition: Migrate with care.\n    groups:\n      - read\n      - execute\n".to_string()
    }

    #[test]
    fn slugs_read_in_file_order() {
        let text = "customModes:\n  - slug: beta\n  - slug: alpha\n";
        assert_eq!(entry_slugs(text).unwrap(), vec!["beta", "alpha"]);
        assert!(entry_slugs("").unwrap().is_empty());
    }

    #[test]
    fn slugs_reject_a_mode_without_slug() {
        let text = "customModes:\n  - name: No Slug\n";
        assert!(entry_slugs(text).is_err());
    }

    #[test]
    fn fresh_merge_writes_pack_modes() {
        let merge = merge_modes(None, &pack_text(), &[]).unwrap();
        assert!(merge.changed);
        assert_eq!(merge.added, vec!["sethu-migrator"]);
        assert!(merge.updated.is_empty());
        assert!(merge.collisions.is_empty());
        assert_eq!(entry_slugs(&merge.content).unwrap(), vec!["sethu-migrator"]);
    }

    #[test]
    fn repeat_merge_reports_no_change() {
        let first = merge_modes(None, &pack_text(), &[]).unwrap();
        let owned = vec!["sethu-migrator".to_string()];
        let second = merge_modes(Some(&first.content), &pack_text(), &owned).unwrap();
        assert!(!second.changed);
        assert!(second.added.is_empty());
        assert!(second.updated.is_empty());
        assert!(second.collisions.is_empty());
        assert_eq!(second.content, first.content);
    }

    #[test]
    fn unrelated_modes_and_keys_survive() {
        let existing = "title: Mine\ncustomModes:\n  - slug: reviewer\n    name: Reviewer\n    roleDefinition: Review only.\n    groups:\n      - read\n";
        let merge = merge_modes(Some(existing), &pack_text(), &[]).unwrap();
        assert!(merge.changed);
        assert_eq!(merge.added, vec!["sethu-migrator"]);
        let value: Value = serde_norway::from_str(&merge.content).unwrap();
        assert_eq!(value.get("title").and_then(Value::as_str), Some("Mine"));
        assert_eq!(
            entry_slugs(&merge.content).unwrap(),
            vec!["reviewer", "sethu-migrator"]
        );
    }

    #[test]
    fn owned_slug_takes_pack_content() {
        let existing = "customModes:\n  - slug: sethu-migrator\n    name: Old Name\n    roleDefinition: Old.\n    groups:\n      - read\n";
        let owned = vec!["sethu-migrator".to_string()];
        let merge = merge_modes(Some(existing), &pack_text(), &owned).unwrap();
        assert!(merge.changed);
        assert_eq!(merge.updated, vec!["sethu-migrator"]);
        let value: Value = serde_norway::from_str(&merge.content).unwrap();
        let modes = value.get(MODES_KEY).and_then(Value::as_sequence).unwrap();
        assert_eq!(modes.len(), 1);
        assert_eq!(
            modes[0].get("name").and_then(Value::as_str),
            Some("Sethu Migrator")
        );
    }

    #[test]
    fn unowned_slug_reports_a_collision_and_stays() {
        let existing = "customModes:\n  - slug: sethu-migrator\n    name: Mine\n    roleDefinition: Mine.\n    groups:\n      - read\n";
        let merge = merge_modes(Some(existing), &pack_text(), &[]).unwrap();
        assert_eq!(merge.collisions, vec!["sethu-migrator"]);
        assert!(merge.added.is_empty());
        assert!(merge.updated.is_empty());
        let value: Value = serde_norway::from_str(&merge.content).unwrap();
        let modes = value.get(MODES_KEY).and_then(Value::as_sequence).unwrap();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].get("name").and_then(Value::as_str), Some("Mine"));
    }

    #[test]
    fn invalid_project_yaml_fails_loudly() {
        let err = merge_modes(Some("customModes: [oops\n"), &pack_text(), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("project mode file"));
    }

    #[test]
    fn non_mapping_project_file_fails() {
        let err = merge_modes(Some("- just\n- a\n- list\n"), &pack_text(), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("mapping"));
    }

    #[test]
    fn formatting_only_edits_count_as_unchanged() {
        let first = merge_modes(None, &pack_text(), &[]).unwrap();
        let reformatted = format!("\n\n{}  \n", first.content.trim());
        let owned = vec!["sethu-migrator".to_string()];
        let second = merge_modes(Some(&reformatted), &pack_text(), &owned).unwrap();
        assert!(!second.changed);
    }
}
