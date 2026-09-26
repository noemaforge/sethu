//! Subprocess adapter for the Vimanam contract diff tool.
//!
//! This module is the only code that runs Vimanam. It finds the released
//! binary through PATH, refuses versions below the minimum, runs the JSON
//! diff, and parses the result into typed records. Every spawn sets its
//! working directory and captures both streams. Callers keep `vimanam` on
//! PATH. The gate script adds the install location to PATH before testing.
//!
//! Records keep the ids and severities that Vimanam reported. This module
//! never recomputes either value.

// Later commands will call these helpers. Until then the binary target
// would flag them as unused, so this module allows dead code for now.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Minimum supported Vimanam version.
///
/// Older releases lack exact operation selection. The adapter refuses them
/// with a message that names the version it found.
pub const MINIMUM_VERSION: Version = Version::new(1, 3, 0);

/// Schema version of the Vimanam JSON diff contract.
///
/// The adapter accepts exactly this version and refuses anything else.
pub const SCHEMA_VERSION: u32 = 1;

/// Expected generator name in Vimanam JSON diff output.
///
/// The adapter refuses documents from any other generator.
pub const GENERATOR_NAME: &str = "vimanam";

/// A Vimanam release version.
///
/// Versions compare as plain number triples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// Major release number.
    pub major: u32,
    /// Minor release number.
    pub minor: u32,
    /// Patch release number.
    pub patch: u32,
}

impl Version {
    /// Build a version from its three parts.
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse the version from `vimanam --version` output.
    ///
    /// The output looks like `vimanam 1.3.0`. Parsing takes the last
    /// whitespace word, drops a leading `v`, and reads the leading digits
    /// of each dot separated part. A missing patch defaults to zero.
    pub fn parse(output: &str) -> anyhow::Result<Self> {
        let word = output.split_whitespace().last().unwrap_or_default();
        let word = word.strip_prefix('v').unwrap_or(word);
        let mut parts = word.split('.');
        let major = parse_part(parts.next(), output)?;
        let minor = parse_part(parts.next(), output)?;
        let patch = match parts.next() {
            Some(text) => parse_numeric_prefix(text, output)?,
            None => 0,
        };
        if parts.next().is_some() {
            anyhow::bail!("cannot parse vimanam version from {output:?}");
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    /// Report whether this version meets the minimum.
    pub fn is_supported(self) -> bool {
        self >= MINIMUM_VERSION
    }
}

impl std::fmt::Display for Version {
    /// Format the version as dotted numbers.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Read one mandatory version part.
fn parse_part(part: Option<&str>, output: &str) -> anyhow::Result<u32> {
    match part {
        Some(text) => parse_numeric_prefix(text, output),
        None => anyhow::bail!("cannot parse vimanam version from {output:?}"),
    }
}

/// Read the leading digits of one version part.
fn parse_numeric_prefix(text: &str, output: &str) -> anyhow::Result<u32> {
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        anyhow::bail!("cannot parse vimanam version from {output:?}");
    }
    digits
        .parse()
        .map_err(|_| anyhow::anyhow!("cannot parse vimanam version from {output:?}"))
}

/// Probe the Vimanam binary on PATH and check its version.
///
/// Resolution uses PATH as is. A missing binary, unreadable output, or a
/// version below the minimum all fail. The failure names what was found.
/// Every spawn sets its working directory and captures both streams.
pub fn probe_vimanam(workdir: &Path) -> anyhow::Result<Version> {
    let output = Command::new("vimanam")
        .arg("--version")
        .current_dir(workdir)
        .output()
        .with_context(|| "run `vimanam --version` (needs `vimanam` on PATH)")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`vimanam --version` failed: {}", detail.trim());
    }
    let text =
        String::from_utf8(output.stdout).with_context(|| "read `vimanam --version` output")?;
    let version = Version::parse(&text)
        .with_context(|| format!("parse `vimanam --version` output {text:?}"))?;
    if !version.is_supported() {
        anyhow::bail!("unsupported vimanam version {version}, need {MINIMUM_VERSION} or newer");
    }
    Ok(version)
}

/// Run `vimanam diff` on two specs and parse the JSON report.
///
/// The invocation is `vimanam diff OLD NEW --format json`. Reading uses
/// the full stdout capture, so a long report never breaks on a cut pipe.
/// Exit 0 and exit 3 both carry usable output. Exit 3 means breaking
/// changes were found. It only occurs with fail on breaking enabled, and
/// the JSON on stdout stays complete. Other exits fail and carry the
/// stderr text as the diagnostic. Every spawn sets its working directory
/// and captures both streams.
pub fn run_diff(
    old: &Path,
    new: &Path,
    fail_on_breaking: bool,
    workdir: &Path,
) -> anyhow::Result<DiffDocument> {
    let mut command = Command::new("vimanam");
    command
        .arg("diff")
        .arg(old)
        .arg(new)
        .arg("--format")
        .arg("json");
    if fail_on_breaking {
        command.arg("--fail-on-breaking");
    }
    let output = command.current_dir(workdir).output().with_context(|| {
        format!(
            "run `vimanam diff` on {} and {}",
            old.display(),
            new.display()
        )
    })?;
    match output.status.code() {
        Some(0) | Some(3) => parse_diff_output(&output.stdout),
        _ => {
            let detail = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "`vimanam diff` exited with status {}: {}",
                output.status,
                detail.trim()
            )
        }
    }
}

/// Parse Vimanam JSON diff bytes into a document.
///
/// Parsing refuses unknown schema versions and foreign generators. Ids
/// and severities stay exactly as reported. This function never computes
/// them.
pub fn parse_diff_output(bytes: &[u8]) -> anyhow::Result<DiffDocument> {
    let document: DiffDocument =
        serde_json::from_slice(bytes).with_context(|| "parse `vimanam diff` JSON output")?;
    if document.schema_version != SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported vimanam schema version {}",
            document.schema_version
        );
    }
    if document.generator.name != GENERATOR_NAME {
        anyhow::bail!("unexpected vimanam generator {}", document.generator.name);
    }
    Ok(document)
}

/// Detail level for rendering one operation.
///
/// Each variant maps to the matching Vimanam detail flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailLevel {
    /// Compact service map.
    Summary,
    /// Short endpoint listing.
    Basic,
    /// Endpoint reference with core fields.
    Standard,
    /// Full endpoint reference.
    Full,
}

impl DetailLevel {
    /// Render the level as the Vimanam detail flag value.
    pub fn as_arg(self) -> &'static str {
        match self {
            DetailLevel::Summary => "summary",
            DetailLevel::Basic => "basic",
            DetailLevel::Standard => "standard",
            DetailLevel::Full => "full",
        }
    }
}

/// Build the exact operation selector for one endpoint.
///
/// Vimanam matches the method and the path template exactly and errors
/// when nothing matches.
pub fn operation_selector(method: &str, path: &str) -> String {
    format!("{method} {path}")
}

/// Render one operation from one spec file.
///
/// The invocation is `vimanam SPEC --operation "METHOD /path" --flat`.
/// The caller picks the old or the new spec. The selector matches the
/// method and the path template exactly. An empty method or a path
/// without a leading slash fails before spawning. Any other failure
/// carries the stderr text as the diagnostic. Every spawn sets its
/// working directory and captures both streams.
pub fn render_operation(
    spec: &Path,
    method: &str,
    path: &str,
    detail: DetailLevel,
    workdir: &Path,
) -> anyhow::Result<String> {
    if method.is_empty() {
        anyhow::bail!("cannot render an operation with an empty method");
    }
    if !path.starts_with('/') {
        anyhow::bail!("cannot render operation path {path:?}, it must start with `/`");
    }
    let output = Command::new("vimanam")
        .arg(spec)
        .arg("--operation")
        .arg(operation_selector(method, path))
        .arg("--flat")
        .arg("--detail")
        .arg(detail.as_arg())
        .current_dir(workdir)
        .output()
        .with_context(|| format!("render operation {method} {path} from {}", spec.display()))?;
    if !output.status.success() {
        let detail_text = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`vimanam --operation` failed: {}", detail_text.trim());
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("read rendered operation {method} {path} as text"))
}

/// One Vimanam JSON diff document.
///
/// Field order follows the released output. Changes keep report order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffDocument {
    /// Contract version of this document. The adapter accepts only 1.
    pub schema_version: u32,
    /// Tool that produced this document.
    pub generator: Generator,
    /// Identity of the old spec.
    pub old: SpecSide,
    /// Identity of the new spec.
    pub new: SpecSide,
    /// Change counts by class.
    pub summary: DiffSummary,
    /// Reported changes in report order.
    pub changes: Vec<ChangeRecord>,
}

/// Tool that produced a diff document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generator {
    /// Generator name. The adapter accepts only `vimanam`.
    pub name: String,
    /// Generator version, such as `1.3.0`.
    pub version: String,
}

/// Identity of one input spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecSide {
    /// Spec title from the document info.
    pub title: String,
    /// Spec version from the document info.
    pub version: String,
    /// Lowercase hex SHA-256 of the input file bytes as read from disk.
    pub file_sha256: String,
}

/// Change counts by class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    /// Endpoints present only in the new spec.
    pub endpoints_added: usize,
    /// Endpoints present only in the old spec.
    pub endpoints_removed: usize,
    /// Endpoints present in both specs with changes.
    pub endpoints_changed: usize,
    /// Changes classed as breaking.
    pub breaking: usize,
    /// Changes classed as non breaking.
    pub non_breaking: usize,
    /// Changes classed as needing review.
    pub review: usize,
}

/// One reported change.
///
/// The id and severity are opaque here. The adapter stores them exactly
/// as reported and never derives them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRecord {
    /// Stable content derived id of the change.
    pub id: String,
    /// Endpoint the change belongs to.
    pub endpoint: EndpointRef,
    /// Class of the change.
    pub kind: ChangeKind,
    /// Severity word of the change.
    pub severity: Severity,
    /// Kind specific payload.
    pub details: ChangeDetails,
}

/// Endpoint a change belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRef {
    /// HTTP method in upper case, such as `GET`.
    pub method: String,
    /// Path template from the spec, such as `/widgets`.
    pub path: String,
}

/// Class of a change record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// An endpoint exists only in the new spec.
    EndpointAdded,
    /// An endpoint exists only in the old spec.
    EndpointRemoved,
    /// A parameter exists only in the new spec.
    ParameterAdded,
    /// A parameter exists only in the old spec.
    ParameterRemoved,
    /// A parameter changed its required flag.
    ParameterRequiredChanged,
    /// A parameter moved to another location.
    ParameterLocationChanged,
    /// A parameter schema changed.
    ParameterSchemaChanged,
    /// A response status exists only in the new spec.
    ResponseAdded,
    /// A response status exists only in the old spec.
    ResponseRemoved,
    /// An operation id changed, appeared, or vanished.
    OperationIdChanged,
    /// An endpoint changed its deprecated flag.
    DeprecatedChanged,
    /// A request body schema changed.
    RequestSchemaChanged,
    /// A response body schema changed.
    ResponseSchemaChanged,
}

/// Severity word of a change record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A breaking change.
    Breaking,
    /// A non breaking change.
    NonBreaking,
    /// A change that needs review.
    Review,
}

/// Details of one change record.
///
/// The shape depends on the kind. Only the keys the released contract
/// assigns to that kind are present. Every field stays optional so one
/// struct covers every kind. Serialization skips absent fields, so output
/// keeps exactly the released keys in released order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ChangeDetails {
    /// Response status for response changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Parameter name for parameter changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Parameter location for parameter changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Required flag of an added parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// New required flag after a requirement change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now_required: Option<bool>,
    /// Old location after a location change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_location: Option<String>,
    /// New location after a location change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_location: Option<String>,
    /// Schema delta for body and parameter schema changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_change: Option<SchemaChange>,
    /// Old operation id. Missing means another kind. Null means no old id.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "parse_nullable_id",
        serialize_with = "write_nullable_id"
    )]
    pub old: Option<Option<String>>,
    /// New operation id, encoded like the old one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "parse_nullable_id",
        serialize_with = "write_nullable_id"
    )]
    pub new: Option<Option<String>>,
    /// New deprecated flag after a deprecation change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<bool>,
    /// Old deprecated flag of a removed endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was_deprecated: Option<bool>,
}

/// One schema delta inside a change record.
///
/// The pointer addresses the resolved canonical schema, not necessarily
/// a location in the input file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaChange {
    /// Pointer to the changed schema node.
    pub pointer: String,
    /// What the pointer addresses.
    pub target: ChangeTarget,
    /// Decoded last pointer segment for named members, else null.
    #[serde(default)]
    pub member: Option<String>,
    /// How the node changed.
    pub operation: ChangeOperation,
    /// State of the node before the change.
    pub before: Presence,
    /// State of the node after the change.
    pub after: Presence,
}

/// What a schema change pointer addresses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeTarget {
    /// The type keyword itself.
    Type,
    /// One member of a required list.
    RequiredMember,
    /// One member of an enum list.
    EnumMember,
    /// One named property.
    Property,
    /// The additional properties keyword.
    AdditionalProperties,
    /// The nullable keyword.
    Nullable,
    /// Any other schema keyword.
    Other,
}

/// How a schema node changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOperation {
    /// The node appeared.
    Added,
    /// The node vanished.
    Removed,
    /// The node value changed.
    Changed,
}

/// Decode an operation id side, keeping explicit null apart from absence.
///
/// Missing means another kind. Null means the id itself is absent on that
/// side of the change. Anything else must be a string.
fn parse_nullable_id<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let raw = serde_json::Value::deserialize(deserializer)?;
    match raw {
        serde_json::Value::Null => Ok(Some(None)),
        serde_json::Value::String(text) => Ok(Some(Some(text))),
        _ => Err(D::Error::custom("operation id must be a string or null")),
    }
}

/// Encode an operation id side back to a string or null.
///
/// Skipped fields never reach this function. The wildcard covers the
/// unreachable none case and writes null for it.
fn write_nullable_id<S>(value: &Option<Option<String>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(Some(text)) => serializer.serialize_str(text),
        _ => serializer.serialize_none(),
    }
}

/// Presence of one side of a schema change.
///
/// An absent side carries no value key at all. A present side always
/// carries one, and that value may itself be JSON null. None means absent.
/// A stored null means explicit null, never absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Presence {
    /// Whether this side of the change exists.
    pub present: bool,
    /// The value when the side carries one. Missing means absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

impl<'de> Deserialize<'de> for Presence {
    /// Decode a presence map, keeping explicit null apart from absence.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let raw = serde_json::Value::deserialize(deserializer)?;
        let map = raw
            .as_object()
            .ok_or_else(|| D::Error::custom("presence must be an object"))?;
        let present = map
            .get("present")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| D::Error::custom("presence needs a boolean `present`"))?;
        Ok(Self {
            present,
            value: map.get("value").cloned(),
        })
    }
}

impl Presence {
    /// Fetch the value when the side carries one.
    ///
    /// Absence yields none. An explicit null yields the null itself.
    /// Callers match on the value to tell the two apart.
    pub fn value(&self) -> Option<&serde_json::Value> {
        self.value.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse_accepts_current_release() {
        let version = Version::parse("vimanam 1.3.0").unwrap();
        assert_eq!(version, Version::new(1, 3, 0));
        assert!(version.is_supported());
    }

    #[test]
    fn version_parse_accepts_newer_releases() {
        assert!(Version::parse("vimanam 1.10.0").unwrap().is_supported());
        assert!(Version::parse("vimanam 2.0.0").unwrap().is_supported());
        assert!(Version::parse("vimanam 1.3.1").unwrap().is_supported());
    }

    #[test]
    fn version_parse_reads_older_release_as_unsupported() {
        let version = Version::parse("vimanam 1.2.0").unwrap();
        assert_eq!(version, Version::new(1, 2, 0));
        assert!(!version.is_supported());
    }

    #[test]
    fn version_parse_tolerates_prefix_and_trailing_newline() {
        assert_eq!(
            Version::parse("vimanam v1.3.0\n").unwrap(),
            Version::new(1, 3, 0)
        );
        assert_eq!(
            Version::parse("vimanam 1.3").unwrap(),
            Version::new(1, 3, 0)
        );
    }

    #[test]
    fn version_parse_rejects_garbage() {
        for output in [
            "",
            "vimanam",
            "vimanam x.y.z",
            "vimanam 1..3",
            "vimanam 1.3.0.4",
        ] {
            assert!(
                Version::parse(output).is_err(),
                "output {output:?} should fail"
            );
        }
    }

    #[test]
    fn version_formats_as_dotted_numbers() {
        assert_eq!(Version::new(1, 3, 0).to_string(), "1.3.0");
        assert_eq!(MINIMUM_VERSION.to_string(), "1.3.0");
    }

    #[test]
    fn presence_keeps_explicit_null_apart_from_absence() {
        let raw = serde_json::json!({ "present": true, "value": null });
        let presence: Presence = serde_json::from_value(raw.clone()).unwrap();
        assert!(presence.present);
        assert_eq!(presence.value, Some(serde_json::Value::Null));
        assert_eq!(presence.value(), Some(&serde_json::Value::Null));
        assert_eq!(serde_json::to_value(&presence).unwrap(), raw);

        let raw = serde_json::json!({ "present": false });
        let presence: Presence = serde_json::from_value(raw.clone()).unwrap();
        assert!(!presence.present);
        assert_eq!(presence.value, None);
        assert!(presence.value().is_none());
        assert_eq!(serde_json::to_value(&presence).unwrap(), raw);
    }

    #[test]
    fn presence_round_trips_a_real_value() {
        let raw = serde_json::json!({ "present": true, "value": { "type": "number" } });
        let presence: Presence = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            presence.value(),
            Some(&serde_json::json!({ "type": "number" }))
        );
        assert_eq!(serde_json::to_value(&presence).unwrap(), raw);
    }

    #[test]
    fn operation_selector_joins_method_and_path() {
        assert_eq!(
            operation_selector("POST", "/search/random"),
            "POST /search/random"
        );
    }

    #[test]
    fn detail_level_maps_to_flags() {
        assert_eq!(DetailLevel::Summary.as_arg(), "summary");
        assert_eq!(DetailLevel::Basic.as_arg(), "basic");
        assert_eq!(DetailLevel::Standard.as_arg(), "standard");
        assert_eq!(DetailLevel::Full.as_arg(), "full");
    }
}
