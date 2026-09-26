//! Synchronous HTTP server that serves validated scenarios.
//!
//! The server binds to loopback only, answers one request at a time,
//! and appends one trace entry per exchange. A request that matches no
//! scenario gets status 599 with a plain text marker, so it can never
//! pass as an ordinary 404. Matched requests are always answered with
//! the declared fixture response, even when the request itself fails
//! validation. The trace records the validation result, so a later
//! checker can tell a clean exchange from a broken one.
//!
//! The server runs without an async runtime on top of blocking
//! connections. Each trace entry is flushed before the next request is
//! accepted. The flush for one exchange lands after its response bytes,
//! so a client can hold a full reply while the entry is still in
//! flight. A driver waits for the expected entries before stopping the
//! process. Stopping then loses nothing.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use indexmap::IndexMap;

use crate::stub::{
    MAX_BODY_BYTES, ParsedBody, TRACE_FILENAME, UNEXPECTED_STATUS, append_trace, body_sha256_hex,
    body_within_limit, find_operation, parse_query, scenario_matches, validate_matched_request,
};

/// Loopback address the stub always binds.
pub fn loopback_ip() -> Ipv4Addr {
    Ipv4Addr::new(127, 0, 0, 1)
}

/// Build the socket address the stub binds for one port.
///
/// The IP part is always loopback. Port zero asks the system for any
/// free port. A later checker reads the bound port back from the
/// readiness line.
pub fn loopback_addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(loopback_ip()), port)
}

/// One parsed incoming request ready for routing.
#[derive(Debug, Clone)]
pub struct Incoming {
    /// Request method as received.
    pub method: String,
    /// Request path without the query string.
    pub path: String,
    /// Decoded query pairs in arrival order.
    pub query: IndexMap<String, String>,
    /// Request headers as name and value pairs.
    pub headers: Vec<(String, String)>,
    /// Raw request body bytes.
    pub body: Vec<u8>,
}

/// One routing decision with everything the trace needs.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Matched scenario id, or none for unexpected requests.
    pub scenario_id: Option<String>,
    /// Whether the request validated against the contract.
    pub request_valid: bool,
    /// Short reason for the validation result.
    pub request_detail: String,
    /// Status code to send back.
    pub status: u16,
    /// Headers to send back.
    pub headers: Vec<(String, String)>,
    /// Body bytes to send back.
    pub body: Vec<u8>,
}

/// One running fixture server.
pub struct StubServer {
    /// Bound synchronous server.
    server: tiny_http::Server,
    /// Validated scenarios in match order.
    scenarios: Vec<crate::stub::Scenario>,
    /// Selected contract used for request validation.
    spec: serde_json::Value,
    /// Lowercase version label shown by the readiness line.
    version_label: String,
    /// Trace file receiving one JSON line per exchange.
    trace_path: std::path::PathBuf,
}

impl StubServer {
    /// Bind loopback and prepare one fixture server.
    ///
    /// Binding is the only step that touches the network. It fails
    /// loudly when the loopback address is unavailable. Scenarios must
    /// already be validated. Nothing is served before the caller runs
    /// one of the serve methods.
    pub fn start(
        port: u16,
        scenarios: Vec<crate::stub::Scenario>,
        spec: serde_json::Value,
        version_label: &str,
        trace_path: std::path::PathBuf,
    ) -> anyhow::Result<Self> {
        let server = tiny_http::Server::http(loopback_addr(port))
            .map_err(|error| anyhow::anyhow!("bind stub to {}:{port}: {error}", loopback_ip()))?;
        Ok(Self {
            server,
            scenarios,
            spec,
            version_label: version_label.to_string(),
            trace_path,
        })
    }

    /// Report the bound socket address.
    pub fn local_addr(&self) -> SocketAddr {
        match self.server.server_addr() {
            tiny_http::ListenAddr::IP(address) => address,
            tiny_http::ListenAddr::Unix(_) => loopback_addr(0),
        }
    }

    /// Render the readiness line printed before serving.
    ///
    /// Later tooling parses the address and the trace path from this
    /// line. It is the only line the server prints to stdout.
    pub fn readiness_line(&self) -> String {
        format!(
            "sethu-stub ready version={} addr={} scenarios={} trace={}",
            self.version_label,
            self.local_addr(),
            self.scenarios.len(),
            self.trace_path.display()
        )
    }

    /// Serve requests until the process ends.
    ///
    /// Each exchange is traced before the next one is accepted. There
    /// is no other shutdown path for the command entry point. The trace
    /// entry for one exchange lands after its response bytes, so a
    /// driver waits for the expected entries before stopping the
    /// process. Stopping then loses nothing.
    pub fn serve_forever(self) -> anyhow::Result<()> {
        let stop = AtomicBool::new(false);
        self.serve_until(&stop)
    }

    /// Serve requests until the flag is set.
    ///
    /// The loop polls with a short timeout so tests can stop it. Every
    /// accepted request is handled exactly like `serve_forever`
    /// handles it.
    pub fn serve_until(&self, stop: &AtomicBool) -> anyhow::Result<()> {
        while !stop.load(Ordering::Relaxed) {
            let request = match self
                .server
                .recv_timeout(std::time::Duration::from_millis(50))
            {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(error) => {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    anyhow::bail!("receive stub request: {error}");
                }
            };
            self.handle(request)?;
        }
        Ok(())
    }

    /// Route one connection and record its trace entry.
    fn handle(&self, mut request: tiny_http::Request) -> anyhow::Result<()> {
        let (incoming, too_large) = read_incoming(&mut request)?;
        let mut outcome = self.route(&incoming);
        if too_large {
            outcome.request_valid = false;
            outcome.request_detail = "request body exceeds the read limit".to_string();
        }
        let written = respond(request, &outcome);
        let entry = crate::stub::TraceEntry {
            scenario_id: outcome.scenario_id.clone(),
            method: incoming.method.clone(),
            path: incoming.path.clone(),
            request_valid: outcome.request_valid,
            request_detail: outcome.request_detail.clone(),
            response_status: outcome.status,
            response_body_sha256: body_sha256_hex(&outcome.body),
            response_written: written,
        };
        append_trace(&self.trace_path, &entry)?;
        Ok(())
    }

    /// Route one parsed request to a scenario or to status 599.
    ///
    /// The first matching scenario wins. Its fixture response is
    /// served with the incoming validation result attached. Anything
    /// without a match becomes an unexpected request with status 599.
    /// Unexpected requests are also reported on stderr, so they stand
    /// out in the server logs.
    pub fn route(&self, incoming: &Incoming) -> Outcome {
        let parsed = ParsedBody::parse(&incoming.body);
        for scenario in &self.scenarios {
            if !scenario_matches(
                scenario,
                &incoming.method,
                &incoming.path,
                &incoming.query,
                &incoming.headers,
                &parsed,
            ) {
                continue;
            }
            let (request_valid, request_detail) =
                validate_matched_request(&self.spec, scenario, &incoming.query, &parsed);
            let body = render_body(&scenario.response.body);
            return Outcome {
                scenario_id: Some(scenario.id.clone()),
                request_valid,
                request_detail,
                status: scenario.response.status,
                headers: scenario.response.headers.iter().map(clone_pair).collect(),
                body,
            };
        }
        let detail = format!("no scenario matches {} {}", incoming.method, incoming.path);
        eprintln!("sethu-stub unexpected request: {detail}");
        Outcome {
            scenario_id: None,
            request_valid: false,
            request_detail: detail,
            status: UNEXPECTED_STATUS,
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
            body: format!(
                "Sethu-Unexpected-Request: {} {}\n",
                incoming.method, incoming.path
            )
            .into_bytes(),
        }
    }

    /// Count the loaded scenarios.
    pub fn scenario_count(&self) -> usize {
        self.scenarios.len()
    }

    /// Default trace path for one working directory.
    pub fn default_trace_path(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join(TRACE_FILENAME)
    }
}

/// Clone one ordered header pair.
fn clone_pair(pair: (&String, &String)) -> (String, String) {
    (pair.0.clone(), pair.1.clone())
}

/// Read one connection into a parsed request.
///
/// Bodies larger than the read limit are replaced with an empty body
/// and reported through the returned flag. The cap applies to the
/// bytes actually read, so chunked bodies without a length get the
/// same treatment. Callers treat an oversized body as invalid.
fn read_incoming(request: &mut tiny_http::Request) -> anyhow::Result<(Incoming, bool)> {
    let method = request.method().as_str().to_string();
    let raw_url = request.url().to_string();
    let (raw_path, raw_query) = match raw_url.find('?') {
        Some(position) => (&raw_url[..position], &raw_url[position + 1..]),
        None => (raw_url.as_str(), ""),
    };
    let headers = request
        .headers()
        .iter()
        .map(|header| {
            (
                header.field.as_str().to_string(),
                header.value.as_str().to_string(),
            )
        })
        .collect::<Vec<_>>();
    let declared = request.body_length().unwrap_or(0);
    let mut body = Vec::new();
    if body_within_limit(declared) {
        request
            .as_reader()
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .with_context(|| "read stub request body")?;
    }
    let too_large = !body_within_limit(declared) || !body_within_limit(body.len());
    let body = if too_large { Vec::new() } else { body };
    let path = crate::stub::percent_decode(raw_path);
    let incoming = Incoming {
        method,
        path,
        query: parse_query(raw_query),
        headers,
        body,
    };
    Ok((incoming, too_large))
}

/// Render a declared response body to bytes.
///
/// A missing body sends zero bytes. Any other value is sent in its
/// canonical JSON form.
fn render_body(body: &Option<serde_json::Value>) -> Vec<u8> {
    match body {
        Some(value) => serde_json::to_vec(value).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Send one routing decision back over the connection.
///
/// Headers that fail ASCII encoding are skipped, since the connection
/// cannot carry them. The flag tells whether the full response was
/// written.
fn respond(request: tiny_http::Request, outcome: &Outcome) -> bool {
    let mut response = tiny_http::Response::from_data(outcome.body.clone())
        .with_status_code(tiny_http::StatusCode(outcome.status));
    let mut has_content_type = false;
    for (name, value) in &outcome.headers {
        if name.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        if let Ok(header) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(header);
        }
    }
    if !has_content_type
        && !outcome.body.is_empty()
        && let Ok(header) = tiny_http::Header::from_bytes("content-type", "application/json")
    {
        response.add_header(header);
    }
    request.respond(response).is_ok()
}

/// Check that one operation still exists in the spec.
///
/// Test helpers use this to confirm a loaded spec covers a scenario
/// before the server starts.
pub fn operation_known(spec: &serde_json::Value, method: &str, path: &str) -> bool {
    find_operation(spec, method, path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a tiny spec with one templated operation.
    fn item_spec() -> serde_json::Value {
        json!({
            "openapi": "3.0.0",
            "info": { "title": "Items", "version": "1" },
            "paths": {
                "/items/{id}": {
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

    /// Build one scenario answering the templated route.
    fn item_scenario() -> crate::stub::Scenario {
        crate::stub::Scenario {
            id: "get-item".to_string(),
            change_ids: vec![],
            request: crate::stub::ScenarioRequest {
                method: "GET".to_string(),
                path: "/items/{id}".to_string(),
                query: IndexMap::new(),
                headers: IndexMap::new(),
                body: None,
            },
            response: crate::stub::ScenarioResponse {
                status: 200,
                headers: IndexMap::new(),
                body: Some(json!({ "id": "42" })),
            },
            unsupported: vec![],
            supports_claim: true,
        }
    }

    /// Build an incoming request for the templated route.
    fn item_incoming() -> Incoming {
        Incoming {
            method: "GET".to_string(),
            path: "/items/42".to_string(),
            query: IndexMap::new(),
            headers: vec![],
            body: Vec::new(),
        }
    }

    #[test]
    fn loopback_address_stays_on_loopback() {
        let address = loopback_addr(8080);
        assert!(address.ip().is_loopback());
        assert_eq!(address.port(), 8080);
        assert_eq!(loopback_ip().to_string(), "127.0.0.1");
    }

    #[test]
    fn templated_scenario_answers_concrete_path() {
        let server = StubServer {
            server: tiny_http::Server::http(loopback_addr(0)).unwrap(),
            scenarios: vec![item_scenario()],
            spec: item_spec(),
            version_label: "test".to_string(),
            trace_path: std::path::PathBuf::from("requests.jsonl"),
        };
        assert!(server.local_addr().ip().is_loopback());
        let outcome = server.route(&item_incoming());
        assert_eq!(outcome.scenario_id.as_deref(), Some("get-item"));
        assert_eq!(outcome.status, 200);
        assert!(outcome.request_valid);
        assert_eq!(outcome.body, b"{\"id\":\"42\"}");
    }

    #[test]
    fn malformed_body_on_bodiless_operation_is_invalid() {
        let server = StubServer {
            server: tiny_http::Server::http(loopback_addr(0)).unwrap(),
            scenarios: vec![item_scenario()],
            spec: item_spec(),
            version_label: "test".to_string(),
            trace_path: std::path::PathBuf::from("requests.jsonl"),
        };
        let mut incoming = item_incoming();
        incoming.body = b"{not json".to_vec();
        let outcome = server.route(&incoming);
        assert_eq!(outcome.scenario_id.as_deref(), Some("get-item"));
        assert_eq!(outcome.status, 200);
        assert!(!outcome.request_valid);
        assert!(
            outcome.request_detail.contains("valid JSON"),
            "{}",
            outcome.request_detail
        );
    }

    #[test]
    fn unknown_path_gets_599_with_marker_body() {
        let server = StubServer {
            server: tiny_http::Server::http(loopback_addr(0)).unwrap(),
            scenarios: vec![item_scenario()],
            spec: item_spec(),
            version_label: "test".to_string(),
            trace_path: std::path::PathBuf::from("requests.jsonl"),
        };
        let incoming = Incoming {
            method: "DELETE".to_string(),
            path: "/elsewhere".to_string(),
            query: IndexMap::new(),
            headers: vec![],
            body: Vec::new(),
        };
        let outcome = server.route(&incoming);
        assert_eq!(outcome.scenario_id, None);
        assert_eq!(outcome.status, UNEXPECTED_STATUS);
        assert!(!outcome.request_valid);
        let text = String::from_utf8(outcome.body).unwrap();
        assert!(text.contains("Sethu-Unexpected-Request"));
        assert!(text.contains("/elsewhere"));
    }

    #[test]
    fn readiness_line_names_version_addr_and_trace() {
        let server = StubServer {
            server: tiny_http::Server::http(loopback_addr(0)).unwrap(),
            scenarios: vec![item_scenario()],
            spec: item_spec(),
            version_label: "old".to_string(),
            trace_path: std::path::PathBuf::from("./requests.jsonl"),
        };
        let line = server.readiness_line();
        assert!(line.starts_with("sethu-stub ready version=old addr=127.0.0.1:"));
        assert!(line.contains("scenarios=1"));
        assert!(line.contains("trace=./requests.jsonl"));
    }

    #[test]
    fn operation_known_follows_template_matching() {
        let spec = item_spec();
        assert!(operation_known(&spec, "GET", "/items/7"));
        assert!(!operation_known(&spec, "POST", "/items/7"));
    }
}
