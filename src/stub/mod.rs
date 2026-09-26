//! Fixed versioned scenarios for the local fixture server.
//!
//! A scenario directory holds one JSON file per scenario. Each file
//! declares the request it answers and the response it serves. The
//! server matches incoming requests against those declarations, checks
//! both sides against one version of the contract, and records every
//! exchange in a request trace. The pages below describe the file
//! format, the matching rules, the validation boundary, and the trace.
//!
//! ## Scenario file format
//!
//! Each file is one JSON object with these fields:
//!
//! ```json
//! {
//!   "id": "random-search-new",
//!   "change_ids": ["vc1_ddca3206"],
//!   "spec_sha256": "9f2c…",
//!   "request": {
//!     "method": "POST",
//!     "path": "/search/random",
//!     "query": { "page": "2" },
//!     "headers": { "x-tenant": "demo" },
//!     "body": { "size": 10 }
//!   },
//!   "response": {
//!     "status": 200,
//!     "headers": { "x-fixture": "random-search-new" },
//!     "body": [{ "id": "550e8400-…" }]
//!   }
//! }
//! ```
//!
//! Only `id`, `request.method`, and `request.path` are required. The
//! `query`, `headers`, and `body` members narrow the match. The
//! `change_ids` list links the scenario to contract change records. The
//! `spec_sha256` value pins the exact spec text the fixture was written
//! against. A mismatch refuses the fixture before anything is served.
//! `response.status` defaults to 200. Status 599 is reserved for
//! unexpected requests and is refused in a fixture.
//!
//! ## Matching
//!
//! A request matches a scenario when all of these hold. The method
//! matches ignoring case. The path matches the declared template
//! segment by segment, where a `{name}` segment accepts any single
//! non-empty segment. Every declared query pair and header pair is
//! present with an equal value. Header names compare ignoring case. A
//! declared body requires an equal JSON value after parsing. Key order
//! and number spelling do not matter. A body that is not valid JSON
//! never validates. Undeclared query pairs, headers, and bodies impose
//! nothing on matching. Files load in sorted filename order and the
//! first match wins.
//!
//! ## Validation boundary
//!
//! Request and response bodies validate against the selected spec
//! through the schema converter. The converter reports unsupported
//! constructs instead of passing them quietly. A scenario that reaches
//! any such construct outside the narrow exception below cannot back a
//! verification claim. Its `supports_claim` flag reads false. The
//! server still serves it. The trace records the exchange without the
//! flag, so a later checker recomputes claim support from the fixture
//! instead of trusting served output.
//!
//! Format names outside the validator built-in set stay report-only.
//! The validator never fails an instance on an unknown format, so such
//! a name cannot weaken a claim. The demo contract carries one example,
//! an integer width on exchange metadata, and treating it as blocking
//! would keep the main scenario from ever supporting a claim. Every
//! other unsupported kind blocks claims.
//!
//! ## Trace and readiness
//!
//! Each exchange appends one JSON object to `requests.jsonl` in the
//! server working directory. The entry carries the scenario id (null
//! for unexpected requests), the request validation result, the
//! response status, the hex hash of the exact response bytes, and
//! whether the response was fully written. The server prints one
//! readiness line on stdout before accepting connections:
//!
//! ```text
//! sethu-stub ready version=old addr=127.0.0.1:8080 scenarios=2 trace=./requests.jsonl
//! ```
//!
//! Later tooling parses the address and the trace path from that line.
//! The server binds to loopback only and flushes the trace after every
//! request, so killing the process loses at most the in-flight entry.

/// OpenAPI to JSON Schema conversion for request and response validation.
pub mod schema;
/// Synchronous HTTP server that serves validated scenarios.
pub mod server;

use anyhow::Context;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::stub::schema::{Direction, SpecIndex, UnsupportedIssue, UnsupportedKind};

/// File name of the request trace inside the server working directory.
pub const TRACE_FILENAME: &str = "requests.jsonl";

/// Status reserved for requests no scenario declares.
pub const UNEXPECTED_STATUS: u16 = 599;

/// Largest request body the server reads before calling it invalid.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// One declared request inside a scenario file.
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioRequest {
    /// HTTP method, such as `POST`. Matching ignores case.
    pub method: String,
    /// Path template, such as `/search/random` or `/items/{id}`.
    pub path: String,
    /// Query pairs that must all be present with equal values.
    #[serde(default)]
    pub query: IndexMap<String, String>,
    /// Header pairs that must all be present with equal values.
    #[serde(default)]
    pub headers: IndexMap<String, String>,
    /// Required JSON body. Absent means any body is accepted.
    pub body: Option<serde_json::Value>,
}

/// One declared response inside a scenario file.
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioResponse {
    /// HTTP status to serve. Defaults to 200.
    #[serde(default = "default_status")]
    pub status: u16,
    /// Headers to serve with the response.
    #[serde(default)]
    pub headers: IndexMap<String, String>,
    /// JSON body to serve. Absent means an empty body.
    pub body: Option<serde_json::Value>,
}

/// Default response status for a scenario that names none.
fn default_status() -> u16 {
    200
}

/// One raw scenario file as read from disk.
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioFile {
    /// Stable scenario identifier used by the trace.
    pub id: String,
    /// Contract change records this scenario exercises.
    #[serde(default)]
    pub change_ids: Vec<String>,
    /// Hex hash of the exact spec text the fixture was written against.
    pub spec_sha256: Option<String>,
    /// The request this scenario answers.
    pub request: ScenarioRequest,
    /// The response this scenario serves.
    pub response: ScenarioResponse,
}

/// One validated scenario ready to serve.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// Stable scenario identifier used by the trace.
    pub id: String,
    /// Contract change records this scenario exercises.
    pub change_ids: Vec<String>,
    /// Declared request with the method uppercased.
    pub request: ScenarioRequest,
    /// Declared response.
    pub response: ScenarioResponse,
    /// Unsupported constructs reached through either direction.
    pub unsupported: Vec<UnsupportedIssue>,
    /// Whether the scenario can back a verification claim.
    pub supports_claim: bool,
}

/// One entry of the request trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    /// Matched scenario id, or null for unexpected requests.
    pub scenario_id: Option<String>,
    /// Request method as received.
    pub method: String,
    /// Request path without the query string.
    pub path: String,
    /// Whether the request validated against the contract.
    pub request_valid: bool,
    /// Short reason for the validation result.
    pub request_detail: String,
    /// Status code sent back.
    pub response_status: u16,
    /// Hex hash of the exact response body bytes.
    pub response_body_sha256: String,
    /// Whether the full response reached the connection.
    pub response_written: bool,
}

/// Load the embedded spec text for one contract version.
///
/// The bytes are compiled into the binary from the pinned fixtures, so
/// a local stub never reads a spec over the network. The label is the
/// lowercase version name used by the readiness line.
pub fn embedded_spec(version: &crate::cli::SpecVersion) -> (&'static [u8], &'static str) {
    match version {
        crate::cli::SpecVersion::Old => (
            include_bytes!("../../tests/fixtures/immich/old.json").as_slice(),
            "old",
        ),
        crate::cli::SpecVersion::New => (
            include_bytes!("../../tests/fixtures/immich/new.json").as_slice(),
            "new",
        ),
    }
}

/// Hash exact bytes with SHA-256 and render the hex digest.
pub fn body_sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Report whether one issue list still allows a verification claim.
///
/// Only format names outside the validator built-in set are excused.
/// The validator cannot fail an instance on such a name, so it carries
/// no signal about the fixture. Every other unsupported kind blocks
/// claims.
pub fn claim_supported(issues: &[UnsupportedIssue]) -> bool {
    issues
        .iter()
        .all(|issue| issue.kind == UnsupportedKind::Format)
}

/// Report whether a path template matches a concrete request path.
///
/// Both sides split on `/`. A template segment in braces accepts any
/// single non-empty segment. All other segments must match exactly.
/// A trailing slash is ignored on either side.
pub fn template_matches(template: &str, path: &str) -> bool {
    let template_segments: Vec<&str> = template
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let path_segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if template_segments.len() != path_segments.len() {
        return template.is_empty() && path.is_empty();
    }
    if template_segments.is_empty() {
        return true;
    }
    template_segments
        .iter()
        .zip(path_segments.iter())
        .all(|(wanted, seen)| {
            !seen.is_empty() && (wanted.starts_with('{') && wanted.ends_with('}') || wanted == seen)
        })
}

/// Find one operation object by method and path template.
///
/// Matching is template-aware in both directions, so a scenario that
/// declares `/items/42` still finds the `/items/{id}` operation. The
/// method compares ignoring case.
pub fn find_operation<'a>(
    spec: &'a serde_json::Value,
    method: &str,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let paths = spec.get("paths")?.as_object()?;
    for (template, item) in paths {
        if !template_matches(template, path) {
            continue;
        }
        let operations = item.as_object()?;
        for (name, operation) in operations {
            if name.eq_ignore_ascii_case(method) {
                return Some(operation);
            }
        }
    }
    None
}

/// Read the request body schema of one operation, if it declares one.
///
/// A JSON media type wins when several are present. Otherwise the
/// first declared media type supplies the schema. Absent content
/// means the operation takes no declared body.
fn request_body_schema(operation: &serde_json::Value) -> Option<serde_json::Value> {
    let content = operation.get("requestBody")?.get("content")?.as_object()?;
    let entry = content
        .get("application/json")
        .or_else(|| content.values().next())?;
    entry.get("schema").cloned()
}

/// Read the response body schema for one status, if it declares one.
///
/// An exact status entry wins, then the `default` entry. Absent
/// content means the status carries no declared body.
fn response_body_schema(operation: &serde_json::Value, status: u16) -> Option<serde_json::Value> {
    let responses = operation.get("responses")?.as_object()?;
    let entry = responses
        .get(&status.to_string())
        .or_else(|| responses.get("default"))?;
    let content = entry.get("content")?.as_object()?;
    let media = content
        .get("application/json")
        .or_else(|| content.values().next())?;
    media.get("schema").cloned()
}

/// Find one path item object by concrete request path.
///
/// Matching is template-aware, so `/items/42` finds `/items/{id}`.
/// Path-item-level parameters apply to every operation under the item.
fn find_path_item<'a>(spec: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let paths = spec.get("paths")?.as_object()?;
    paths
        .iter()
        .find(|(template, _)| template_matches(template, path))
        .map(|(_, item)| item)
}

/// List the required query parameters of one operation.
///
/// Both operation-level and path-item-level parameter lists count. A
/// parameter counts only when it sits in `query` and sets
/// `required: true`.
fn required_query_params(
    operation: &serde_json::Value,
    path_item: Option<&serde_json::Value>,
) -> Vec<String> {
    let mut names = Vec::new();
    collect_required_query_params(operation.get("parameters"), &mut names);
    if let Some(item) = path_item {
        collect_required_query_params(item.get("parameters"), &mut names);
    }
    names
}

/// Push required query names from one parameter list.
///
/// A missing or non-array list adds nothing. Duplicates stay in place,
/// so callers see every declaration.
fn collect_required_query_params(list: Option<&serde_json::Value>, names: &mut Vec<String>) {
    let Some(list) = list.and_then(|value| value.as_array()) else {
        return;
    };
    for parameter in list {
        let is_query = parameter.get("in").and_then(|value| value.as_str()) == Some("query");
        let is_required = parameter.get("required").and_then(|value| value.as_bool()) == Some(true);
        if is_query
            && is_required
            && let Some(name) = parameter.get("name").and_then(|value| value.as_str())
        {
            names.push(name.to_string());
        }
    }
}

/// Validate one instance against one schema value in one direction.
///
/// Conversion runs first, then the validator checks the instance. The
/// returned flag tells whether validation passed. The detail names the
/// first failure or confirms the pass. The issues list carries every
/// unsupported construct the conversion met.
pub fn validate_instance(
    schema_value: &serde_json::Value,
    index: &SpecIndex,
    direction: Direction,
    instance: &serde_json::Value,
) -> (bool, String, Vec<UnsupportedIssue>) {
    let converted = match schema::convert_schema(schema_value, index, direction) {
        Ok(converted) => converted,
        Err(error) => {
            return (
                false,
                format!("schema conversion failed: {error:#}"),
                Vec::new(),
            );
        }
    };
    let issues = converted.unsupported.clone();
    let document = converted.document();
    let validator = match jsonschema::validator_for(&document) {
        Ok(validator) => validator,
        Err(error) => {
            return (false, format!("validator build failed: {error}"), issues);
        }
    };
    if validator.is_valid(instance) {
        return (true, "body matches the contract schema".to_string(), issues);
    }
    let mut errors = validator.iter_errors(instance);
    match errors.next() {
        Some(first) => (false, format!("body violates the schema: {first}"), issues),
        None => (false, "body violates the schema".to_string(), issues),
    }
}

/// Decode one query string into an ordered map.
///
/// Pairs split on `&` and then on the first `=`. A missing value reads
/// as an empty string. Both names and values are percent-decoded. The
/// first occurrence of a repeated name wins, so matching stays
/// deterministic.
pub fn parse_query(raw: &str) -> IndexMap<String, String> {
    let mut pairs = IndexMap::new();
    for part in raw.split('&') {
        if part.is_empty() {
            continue;
        }
        let (name, value) = match part.find('=') {
            Some(position) => (&part[..position], &part[position + 1..]),
            None => (part, ""),
        };
        let name = percent_decode(name);
        if !pairs.contains_key(&name) {
            pairs.insert(name, percent_decode(value));
        }
    }
    pairs
}

/// Decode percent escapes and `+` as space.
///
/// Query names, values, and paths pass through here so encoded
/// characters compare by value rather than by spelling.
pub fn percent_decode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut position = 0;
    while position < bytes.len() {
        match bytes[position] {
            b'%' if position + 2 < bytes.len() + 1 => {
                let pair = &raw[position + 1..position + 3];
                match u8::from_str_radix(pair, 16) {
                    Ok(byte) => {
                        out.push(byte as char);
                        position += 3;
                    }
                    Err(_) => {
                        out.push('%');
                        position += 1;
                    }
                }
            }
            b'+' => {
                out.push(' ');
                position += 1;
            }
            _ => {
                out.push(bytes[position] as char);
                position += 1;
            }
        }
    }
    out
}

/// Check that a method token is a non-empty HTTP token.
fn valid_method(method: &str) -> bool {
    !method.is_empty()
        && method
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Load and validate every scenario in one directory.
///
/// Files load in sorted filename order and only `.json` files count.
/// Each file is parsed, checked structurally, pinned against the given
/// spec hash, matched to one spec operation, and validated in both
/// directions. The first problem refuses the whole directory with an
/// error naming the file. Nothing is served before this returns.
pub fn load_scenarios(
    dir: &std::path::Path,
    spec: &serde_json::Value,
    spec_sha: &str,
) -> anyhow::Result<Vec<Scenario>> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("read scenario directory {}", dir.display()))?;
    let mut names: Vec<std::ffi::OsString> = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("list {}", dir.display()))?;
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    let index =
        SpecIndex::from_spec(spec).with_context(|| format!("index spec for {}", dir.display()))?;
    let mut scenarios = Vec::with_capacity(names.len());
    let mut seen: Vec<String> = Vec::new();
    for name in names {
        let path = dir.join(&name);
        let scenario = load_one(&path, spec, spec_sha, &index)?;
        if seen.contains(&scenario.id) {
            anyhow::bail!(
                "scenario file {} repeats id {:?} from an earlier file",
                path.display(),
                scenario.id
            );
        }
        seen.push(scenario.id.clone());
        scenarios.push(scenario);
    }
    Ok(scenarios)
}

/// Load and validate one scenario file.
fn load_one(
    path: &std::path::Path,
    spec: &serde_json::Value,
    spec_sha: &str,
    index: &SpecIndex,
) -> anyhow::Result<Scenario> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read scenario file {}", path.display()))?;
    let file: ScenarioFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse scenario file {}", path.display()))?;
    if file.id.trim().is_empty() {
        anyhow::bail!("scenario file {} has an empty id", path.display());
    }
    if !valid_method(&file.request.method) {
        anyhow::bail!(
            "scenario file {} names an invalid method {:?}",
            path.display(),
            file.request.method
        );
    }
    if !file.request.path.starts_with('/') {
        anyhow::bail!(
            "scenario file {} has a path {:?} that does not start with `/`",
            path.display(),
            file.request.path
        );
    }
    if file.response.status < 100 || file.response.status > 599 {
        anyhow::bail!(
            "scenario file {} has status {} outside 100 to 599",
            path.display(),
            file.response.status
        );
    }
    if file.response.status == UNEXPECTED_STATUS {
        anyhow::bail!(
            "scenario file {} uses status 599, which is reserved for unexpected requests",
            path.display()
        );
    }
    if let Some(pinned) = &file.spec_sha256
        && pinned != spec_sha
    {
        anyhow::bail!(
            "scenario file {} pins spec {pinned:?} but the selected spec hashes {spec_sha:?}",
            path.display()
        );
    }
    let method = file.request.method.to_ascii_uppercase();
    let operation = find_operation(spec, &method, &file.request.path).ok_or_else(|| {
        anyhow::anyhow!(
            "scenario file {} matches no {} {} operation in the selected spec",
            path.display(),
            method,
            file.request.path
        )
    })?;
    let mut unsupported = Vec::new();
    if let Some(schema_value) = request_body_schema(operation) {
        match schema::convert_schema(&schema_value, index, Direction::Request) {
            Ok(converted) => unsupported.extend(converted.unsupported),
            Err(error) => {
                anyhow::bail!(
                    "scenario file {} has a request schema that fails conversion: {error:#}",
                    path.display()
                );
            }
        }
        if let Some(declared) = &file.request.body {
            let (valid, detail, _) =
                validate_instance(&schema_value, index, Direction::Request, declared);
            if !valid {
                anyhow::bail!(
                    "scenario file {} declares a request body the spec rejects: {detail}",
                    path.display()
                );
            }
        }
    } else if file.request.body.is_some() {
        anyhow::bail!(
            "scenario file {} declares a request body but the operation takes none",
            path.display()
        );
    }
    match response_body_schema(operation, file.response.status) {
        Some(schema_value) => {
            let Some(declared) = &file.response.body else {
                anyhow::bail!(
                    "scenario file {} needs a response body for status {}",
                    path.display(),
                    file.response.status
                );
            };
            let (valid, detail, issues) =
                validate_instance(&schema_value, index, Direction::Response, declared);
            unsupported.extend(issues);
            if !valid {
                anyhow::bail!(
                    "scenario file {} declares a response body the spec rejects: {detail}",
                    path.display()
                );
            }
        }
        None => {
            if let Some(declared) = &file.response.body
                && !declared.is_null()
            {
                anyhow::bail!(
                    "scenario file {} declares a response body but status {} defines none",
                    path.display(),
                    file.response.status
                );
            }
        }
    }
    let supports_claim = claim_supported(&unsupported);
    Ok(Scenario {
        id: file.id.clone(),
        change_ids: file.change_ids.clone(),
        request: ScenarioRequest {
            method,
            path: file.request.path.clone(),
            query: file.request.query.clone(),
            headers: file.request.headers.clone(),
            body: file.request.body.clone(),
        },
        response: file.response.clone(),
        unsupported,
        supports_claim,
    })
}

/// One request body with malformed bytes kept distinct from an empty body.
///
/// An empty body means no payload arrived. Well-formed JSON parses to
/// a value. Any other non-empty input is malformed, which validation
/// rejects instead of treating as empty.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedBody {
    /// Only whitespace arrived, or nothing at all.
    Absent,
    /// Well-formed JSON input.
    Value(serde_json::Value),
    /// Non-empty input that is not valid JSON.
    Malformed,
}

impl ParsedBody {
    /// Parse raw body bytes into the three states.
    pub fn parse(body: &[u8]) -> Self {
        if body.iter().all(|byte| byte.is_ascii_whitespace()) {
            return Self::Absent;
        }
        match serde_json::from_slice(body) {
            Ok(value) => Self::Value(value),
            Err(_) => Self::Malformed,
        }
    }

    /// Report whether the input was non-empty and not valid JSON.
    pub fn is_malformed(&self) -> bool {
        matches!(self, Self::Malformed)
    }

    /// Borrow the parsed value for well-formed JSON input.
    ///
    /// Absent and malformed bodies yield none. Matching treats an
    /// absent body as empty, while validation rejects malformed input.
    pub fn value(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Value(value) => Some(value),
            Self::Absent | Self::Malformed => None,
        }
    }
}

/// Report whether one scenario matches an incoming request.
///
/// The method, path template, declared query pairs, declared headers,
/// and declared body must all agree. A malformed body still matches a
/// scenario with no declared body. Validation then rejects it. See the
/// module pages for the exact rules.
pub fn scenario_matches(
    scenario: &Scenario,
    method: &str,
    path: &str,
    query: &IndexMap<String, String>,
    headers: &[(String, String)],
    body: &ParsedBody,
) -> bool {
    if !scenario.request.method.eq_ignore_ascii_case(method) {
        return false;
    }
    if !template_matches(&scenario.request.path, path) {
        return false;
    }
    for (name, wanted) in &scenario.request.query {
        if query.get(name) != Some(wanted) {
            return false;
        }
    }
    for (name, wanted) in &scenario.request.headers {
        let found = headers
            .iter()
            .any(|(seen, value)| seen.eq_ignore_ascii_case(name) && value == wanted);
        if !found {
            return false;
        }
    }
    match &scenario.request.body {
        Some(wanted) => body.value() == Some(wanted),
        None => true,
    }
}

/// Validate one matched request against the contract.
///
/// Required query parameters must be present and the body must fit the
/// request schema. A malformed body always fails, even when the
/// operation takes no body. The flag tells whether the request is
/// valid. The text names the first problem or confirms the pass.
pub fn validate_matched_request(
    spec: &serde_json::Value,
    scenario: &Scenario,
    query: &IndexMap<String, String>,
    body: &ParsedBody,
) -> (bool, String) {
    let Some(operation) = find_operation(spec, &scenario.request.method, &scenario.request.path)
    else {
        return (false, "operation left the selected spec".to_string());
    };
    if body.is_malformed() {
        return (false, "request body is not valid JSON".to_string());
    }
    let path_item = find_path_item(spec, &scenario.request.path);
    for name in required_query_params(operation, path_item) {
        if !query.contains_key(&name) {
            return (
                false,
                format!("required query parameter {name:?} is missing"),
            );
        }
    }
    match request_body_schema(operation) {
        Some(schema_value) => {
            let Some(seen) = body.value() else {
                let required = operation
                    .get("requestBody")
                    .and_then(|value| value.get("required"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                if required {
                    return (false, "request needs a body but none arrived".to_string());
                }
                return (true, "no body arrived and none is required".to_string());
            };
            let index = match SpecIndex::from_spec(spec) {
                Ok(index) => index,
                Err(error) => {
                    return (false, format!("spec index failed: {error:#}"));
                }
            };
            let (valid, detail, _) =
                validate_instance(&schema_value, &index, Direction::Request, seen);
            (valid, detail)
        }
        None => match body.value() {
            Some(seen) if !body_is_empty(seen) => (
                false,
                "request carries a body but the operation takes none".to_string(),
            ),
            _ => (true, "no declared body to check".to_string()),
        },
    }
}

/// Report whether a JSON body counts as empty.
///
/// An empty object, an empty array, an empty string, and null carry no
/// payload. Anything else counts as a body, including `false` and `0`.
fn body_is_empty(body: &serde_json::Value) -> bool {
    match body {
        serde_json::Value::Null => true,
        serde_json::Value::String(text) => text.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Check that a body fits inside the read limit.
pub fn body_within_limit(length: usize) -> bool {
    length <= MAX_BODY_BYTES
}

/// Append one trace entry as a single JSON line.
///
/// The file is created with its parent when missing. Each call opens,
/// writes, flushes, and closes, so killing the server loses at most
/// the in-flight entry.
pub fn append_trace(path: &std::path::Path, entry: &TraceEntry) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create trace directory {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open trace file {}", path.display()))?;
    let mut line = serde_json::to_string(entry)
        .with_context(|| format!("render trace entry for {}", path.display()))?;
    line.push('\n');
    file.write_all(line.as_bytes())
        .with_context(|| format!("write trace file {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flush trace file {}", path.display()))?;
    Ok(())
}

/// Read one trace file back into entries, skipping blank lines.
pub fn read_trace(path: &std::path::Path) -> anyhow::Result<Vec<TraceEntry>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read trace {}", path.display()))?;
    let mut entries = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: TraceEntry = serde_json::from_str(line)
            .with_context(|| format!("parse trace {} line {}", path.display(), number + 1))?;
        entries.push(entry);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a small spec with one templated operation.
    fn item_spec() -> serde_json::Value {
        json!({
            "openapi": "3.0.0",
            "info": { "title": "Items", "version": "1" },
            "paths": {
                "/items/{id}": {
                    "get": {
                        "parameters": [
                            { "name": "verbose", "in": "query", "required": true }
                        ],
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "schema": { "type": "object" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn template_matching_treats_braces_as_wildcards() {
        assert!(template_matches("/items/{id}", "/items/42"));
        assert!(template_matches("/items/{id}", "/items/42/"));
        assert!(!template_matches("/items/{id}", "/items/"));
        assert!(!template_matches("/items/{id}", "/items/42/extra"));
        assert!(!template_matches("/items/{id}", "/other/42"));
        assert!(!template_matches("/search/random", "/search/smart"));
        assert!(template_matches("/search/random", "/search/random"));
    }

    #[test]
    fn operation_lookup_matches_concrete_paths() {
        let spec = item_spec();
        assert!(find_operation(&spec, "get", "/items/42").is_some());
        assert!(find_operation(&spec, "GET", "/items/42").is_some());
        assert!(find_operation(&spec, "post", "/items/42").is_none());
        assert!(find_operation(&spec, "get", "/missing").is_none());
    }

    #[test]
    fn required_query_params_cover_operation_level() {
        let spec = item_spec();
        let operation = find_operation(&spec, "GET", "/items/7").unwrap();
        assert_eq!(
            required_query_params(operation, None),
            vec!["verbose".to_string()]
        );
    }

    /// Build a spec with the required query at path-item level.
    fn path_level_spec() -> serde_json::Value {
        json!({
            "openapi": "3.0.0",
            "info": { "title": "Items", "version": "1" },
            "paths": {
                "/items/{id}": {
                    "parameters": [
                        { "name": "verbose", "in": "query", "required": true }
                    ],
                    "get": {
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "schema": { "type": "object" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    /// Build a scenario answering the templated read with no declared body.
    fn bodiless_item_scenario() -> Scenario {
        Scenario {
            id: "get-item".to_string(),
            change_ids: vec![],
            request: ScenarioRequest {
                method: "GET".to_string(),
                path: "/items/{id}".to_string(),
                query: IndexMap::new(),
                headers: IndexMap::new(),
                body: None,
            },
            response: ScenarioResponse {
                status: 200,
                headers: IndexMap::new(),
                body: Some(json!({ "id": "42" })),
            },
            unsupported: vec![],
            supports_claim: true,
        }
    }

    #[test]
    fn required_query_params_cover_path_item_level() {
        let spec = path_level_spec();
        let operation = find_operation(&spec, "GET", "/items/7").unwrap();
        let path_item = find_path_item(&spec, "/items/7").unwrap();
        assert_eq!(
            required_query_params(operation, Some(path_item)),
            vec!["verbose".to_string()]
        );
        assert!(required_query_params(operation, None).is_empty());
    }

    #[test]
    fn omitted_path_item_level_query_fails_validation() {
        let spec = path_level_spec();
        let scenario = bodiless_item_scenario();
        let query = IndexMap::new();
        let (valid, detail) =
            validate_matched_request(&spec, &scenario, &query, &ParsedBody::Absent);
        assert!(!valid);
        assert!(detail.contains("verbose"), "{detail}");
    }

    #[test]
    fn body_parsing_keeps_malformed_distinct_from_absent() {
        assert_eq!(ParsedBody::parse(b""), ParsedBody::Absent);
        assert_eq!(ParsedBody::parse(b"  \n\t"), ParsedBody::Absent);
        assert_eq!(ParsedBody::parse(b"{}"), ParsedBody::Value(json!({})));
        assert_eq!(ParsedBody::parse(b"{not json"), ParsedBody::Malformed);
        assert!(ParsedBody::parse(b"{not json").is_malformed());
        assert!(!ParsedBody::parse(b"").is_malformed());
        assert_eq!(ParsedBody::parse(b"{}").value(), Some(&json!({})));
        assert_eq!(ParsedBody::parse(b"").value(), None);
        assert_eq!(ParsedBody::parse(b"{not json").value(), None);
    }

    #[test]
    fn format_only_issues_keep_the_claim() {
        let format = UnsupportedIssue {
            location: "#/properties/size".to_string(),
            kind: UnsupportedKind::Format,
            detail: "format".to_string(),
        };
        assert!(claim_supported(&[format]));
        let blocking = UnsupportedIssue {
            location: "#".to_string(),
            kind: UnsupportedKind::Discriminator,
            detail: "discriminator".to_string(),
        };
        assert!(!claim_supported(&[blocking]));
        assert!(claim_supported(&[]));
    }

    #[test]
    fn query_parsing_decodes_and_keeps_first() {
        let parsed = parse_query("a=1&b=&a=2&flag");
        assert_eq!(parsed.get("a").unwrap(), "1");
        assert_eq!(parsed.get("b").unwrap(), "");
        assert_eq!(parsed.get("flag").unwrap(), "");
        assert_eq!(parse_query("name=a+b&city=a%20b")["name"], "a b");
    }

    #[test]
    fn empty_bodies_cover_null_and_blank_shapes() {
        assert!(body_is_empty(&json!(null)));
        assert!(body_is_empty(&json!("")));
        assert!(body_is_empty(&json!([])));
        assert!(body_is_empty(&json!({})));
        assert!(!body_is_empty(&json!(false)));
        assert!(!body_is_empty(&json!(0)));
        assert!(!body_is_empty(&json!({ "a": 1 })));
    }
}
