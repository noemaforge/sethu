//! Checks for the local fixture server.
//!
//! The tests drive the server library directly and through real
//! sockets. Small synthetic contracts cover matching, validation, and
//! refusal. The pinned specs prove the demo fixtures load and serve.
//! Nothing here touches the network beyond loopback. References
//! resolve from the loaded specs only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::{Value, json};
use sethu::stub::server::{StubServer, loopback_addr};
use sethu::stub::{ParsedBody, Scenario, TraceEntry, body_sha256_hex, load_scenarios, read_trace};

/// Read one pinned spec and its raw bytes by file name.
fn pinned_spec(name: &str) -> (Vec<u8>, Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/immich")
        .join(name);
    let bytes = std::fs::read(&path).unwrap();
    let spec = serde_json::from_slice(&bytes).unwrap();
    (bytes, spec)
}

/// Hash raw spec bytes the way the server pins them.
fn spec_sha(bytes: &[u8]) -> String {
    body_sha256_hex(bytes)
}

/// Write one scenario file into a directory.
fn write_scenario(dir: &Path, name: &str, value: &Value) {
    std::fs::write(dir.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

/// Small contract with one templated read and one guarded write.
fn shop_spec() -> Value {
    json!({
        "openapi": "3.0.0",
        "info": { "title": "Shop", "version": "1" },
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
            },
            "/orders": {
                "post": {
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": { "name": { "type": "string" } },
                                    "required": ["name"]
                                }
                            }
                        }
                    },
                    "responses": {
                        "201": {
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": { "order": { "type": "string" } },
                                        "required": ["order"]
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Scenario answering the templated read.
fn item_scenario() -> Value {
    json!({
        "id": "get-item",
        "change_ids": ["vc1_item"],
        "request": { "method": "GET", "path": "/items/{id}" },
        "response": { "status": 200, "body": { "id": "42" } }
    })
}

/// Scenario answering the guarded write.
fn order_scenario() -> Value {
    json!({
        "id": "create-order",
        "change_ids": ["vc1_order"],
        "request": { "method": "POST", "path": "/orders" },
        "response": { "status": 201, "body": { "order": "o-1" } }
    })
}

/// Load scenarios from files written by the given helper.
fn load_from(dir: &Path, spec: &Value, sha: &str) -> Vec<Scenario> {
    load_scenarios(dir, spec, sha).unwrap()
}

/// One running server on a free loopback port.
struct Running {
    /// Bound port.
    port: u16,
    /// Stop flag for the server thread.
    stop: Arc<AtomicBool>,
    /// Server thread handle.
    thread: Option<std::thread::JoinHandle<()>>,
    /// Trace file path.
    trace: PathBuf,
    /// Temp working directory holding the trace.
    _dir: tempfile::TempDir,
}

impl Running {
    /// Start one server with the given scenarios and spec.
    fn spawn(scenarios: Vec<Scenario>, spec: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let trace = dir.path().join("requests.jsonl");
        let server = StubServer::start(0, scenarios, spec, "test", trace.clone()).unwrap();
        assert!(server.local_addr().ip().is_loopback());
        let port = server.local_addr().port();
        assert!(port != 0);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || server.serve_until(&flag).unwrap());
        Self {
            port,
            stop,
            thread: Some(thread),
            trace,
            _dir: dir,
        }
    }

    /// Stop the server and read its trace back.
    fn stop_and_trace(mut self) -> Vec<TraceEntry> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
        read_trace(&self.trace).unwrap()
    }
}

/// Send one raw HTTP request over loopback and split the reply.
fn round_trip(port: u16, method: &str, target: &str, body: Option<&str>) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(loopback_addr(port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let payload = body.unwrap_or("");
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    split_reply(&reply)
}

/// Send one raw HTTP request with a chunked body and split the reply.
///
/// The payload goes out as a single chunk, followed by the closing
/// zero chunk. Callers use this for bodies without a declared length.
fn round_trip_chunked(port: u16, method: &str, target: &str, payload: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(loopback_addr(port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let head = format!(
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream
        .write_all(format!("{:X}\r\n", payload.len()).as_bytes())
        .unwrap();
    stream.write_all(payload).unwrap();
    stream.write_all(b"\r\n0\r\n\r\n").unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    split_reply(&reply)
}

/// Split one raw HTTP reply into its status and body bytes.
fn split_reply(reply: &[u8]) -> (u16, Vec<u8>) {
    let text = String::from_utf8_lossy(reply).to_string();
    let (head, body) = match text.find("\r\n\r\n") {
        Some(position) => text.split_at(position + 4),
        None => panic!("reply without header terminator: {text:?}"),
    };
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let _ = head;
    (status, body.as_bytes().to_vec())
}

#[test]
fn matched_request_is_served_and_traced() {
    let dir = tempfile::tempdir().unwrap();
    write_scenario(dir.path(), "a-item.json", &item_scenario());
    write_scenario(dir.path(), "b-order.json", &order_scenario());
    let spec = shop_spec();
    let scenarios = load_from(dir.path(), &spec, "no-pin");
    assert_eq!(scenarios.len(), 2);
    assert_eq!(scenarios[0].id, "get-item");
    assert!(scenarios.iter().all(|scenario| scenario.supports_claim));

    let running = Running::spawn(scenarios, spec);
    let (status, body) = round_trip(running.port, "GET", "/items/42", None);
    assert_eq!(status, 200);
    let (created, created_body) = round_trip(
        running.port,
        "POST",
        "/orders",
        Some(r#"{"name":"widget"}"#),
    );
    assert_eq!(created, 201);
    let entries = running.stop_and_trace();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].scenario_id.as_deref(), Some("get-item"));
    assert_eq!(entries[0].method, "GET");
    assert_eq!(entries[0].path, "/items/42");
    assert!(entries[0].request_valid);
    assert_eq!(entries[0].response_status, 200);
    assert_eq!(entries[0].response_body_sha256, body_sha256_hex(&body));
    assert!(entries[0].response_written);
    assert_eq!(body, b"{\"id\":\"42\"}");
    assert_eq!(entries[1].scenario_id.as_deref(), Some("create-order"));
    assert!(entries[1].request_valid);
    assert_eq!(entries[1].response_status, 201);
    assert_eq!(
        entries[1].response_body_sha256,
        body_sha256_hex(&created_body)
    );
    assert!(entries[1].response_written);
}

#[test]
fn unexpected_request_gets_599_and_trace_entry() {
    let dir = tempfile::tempdir().unwrap();
    write_scenario(dir.path(), "a-item.json", &item_scenario());
    let spec = shop_spec();
    let scenarios = load_from(dir.path(), &spec, "no-pin");

    let running = Running::spawn(scenarios, spec);
    let (status, body) = round_trip(running.port, "DELETE", "/elsewhere", None);
    assert_eq!(status, 599);
    let text = String::from_utf8(body.clone()).unwrap();
    assert!(text.contains("Sethu-Unexpected-Request"));
    let entries = running.stop_and_trace();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].scenario_id, None);
    assert!(!entries[0].request_valid);
    assert!(entries[0].request_detail.contains("/elsewhere"));
    assert_eq!(entries[0].response_status, 599);
    assert_eq!(entries[0].response_body_sha256, body_sha256_hex(&body));
    assert!(entries[0].response_written);
}

#[test]
fn invalid_request_body_is_served_but_marked() {
    let dir = tempfile::tempdir().unwrap();
    write_scenario(dir.path(), "b-order.json", &order_scenario());
    let spec = shop_spec();
    let scenarios = load_from(dir.path(), &spec, "no-pin");

    let running = Running::spawn(scenarios, spec);
    let (status, _) = round_trip(running.port, "POST", "/orders", Some(r#"{}"#));
    assert_eq!(status, 201);
    let entries = running.stop_and_trace();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].scenario_id.as_deref(), Some("create-order"));
    assert!(!entries[0].request_valid);
    assert!(entries[0].request_detail.contains("name"));
}

#[test]
fn malformed_body_on_bodiless_operation_is_served_but_invalid() {
    let dir = tempfile::tempdir().unwrap();
    write_scenario(dir.path(), "a-item.json", &item_scenario());
    let spec = shop_spec();
    let scenarios = load_from(dir.path(), &spec, "no-pin");

    let running = Running::spawn(scenarios, spec);
    let (status, _) = round_trip(running.port, "GET", "/items/42", Some("{not json"));
    assert_eq!(status, 200);
    let entries = running.stop_and_trace();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].scenario_id.as_deref(), Some("get-item"));
    assert!(!entries[0].request_valid);
    assert!(
        entries[0].request_detail.contains("valid JSON"),
        "{}",
        entries[0].request_detail
    );
}

#[test]
fn oversized_chunked_body_is_capped_and_marked() {
    let dir = tempfile::tempdir().unwrap();
    write_scenario(dir.path(), "a-item.json", &item_scenario());
    let spec = shop_spec();
    let scenarios = load_from(dir.path(), &spec, "no-pin");

    let running = Running::spawn(scenarios, spec);
    let payload = vec![b'x'; 32 * 1024 * 1024 + 1024];
    let (status, _) = round_trip_chunked(running.port, "GET", "/items/42", &payload);
    assert_eq!(status, 200);
    let entries = running.stop_and_trace();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].scenario_id.as_deref(), Some("get-item"));
    assert!(!entries[0].request_valid);
    assert_eq!(
        entries[0].request_detail,
        "request body exceeds the read limit"
    );
}

#[test]
fn malformed_fixture_is_refused_with_file_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.json");
    std::fs::write(&path, b"{ not json").unwrap();
    let spec = shop_spec();
    let error = load_scenarios(dir.path(), &spec, "no-pin").unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("broken.json"), "{message}");
}

#[test]
fn fixture_with_rejected_response_body_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_scenario(
        dir.path(),
        "bad-body.json",
        &json!({
            "id": "bad-body",
            "request": { "method": "GET", "path": "/items/{id}" },
            "response": { "status": 200, "body": 42 }
        }),
    );
    let spec = shop_spec();
    let error = load_scenarios(dir.path(), &spec, "no-pin").unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("bad-body.json"), "{message}");
    assert!(message.contains("rejects"), "{message}");
}

#[test]
fn fixture_for_unknown_operation_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_scenario(
        dir.path(),
        "ghost.json",
        &json!({
            "id": "ghost",
            "request": { "method": "GET", "path": "/ghost" },
            "response": { "status": 200, "body": {} }
        }),
    );
    let spec = shop_spec();
    let error = load_scenarios(dir.path(), &spec, "no-pin").unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("ghost.json"), "{message}");
    assert!(message.contains("matches no"), "{message}");
}

#[test]
fn fixture_with_wrong_spec_pin_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_scenario(
        dir.path(),
        "pinned.json",
        &json!({
            "id": "get-item",
            "spec_sha256": "0".repeat(64),
            "request": { "method": "GET", "path": "/items/{id}" },
            "response": { "status": 200, "body": { "id": "42" } }
        }),
    );
    let spec = shop_spec();
    let error = load_scenarios(dir.path(), &spec, "actual-sha").unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("pinned.json"), "{message}");
    assert!(message.contains("pins spec"), "{message}");
}

#[test]
fn duplicate_ids_and_reserved_status_are_refused() {
    let spec = shop_spec();
    let first = tempfile::tempdir().unwrap();
    write_scenario(first.path(), "a.json", &item_scenario());
    write_scenario(first.path(), "b.json", &item_scenario());
    let error = load_scenarios(first.path(), &spec, "no-pin").unwrap_err();
    assert!(format!("{error:#}").contains("repeats id"));

    let second = tempfile::tempdir().unwrap();
    write_scenario(
        second.path(),
        "reserved.json",
        &json!({
            "id": "reserved",
            "request": { "method": "GET", "path": "/items/{id}" },
            "response": { "status": 599, "body": {} }
        }),
    );
    let error = load_scenarios(second.path(), &spec, "no-pin").unwrap_err();
    assert!(format!("{error:#}").contains("reserved"));
}

#[test]
fn listener_binds_loopback_only() {
    let server = StubServer::start(
        0,
        vec![],
        shop_spec(),
        "test",
        PathBuf::from("requests.jsonl"),
    )
    .unwrap();
    let addr = server.local_addr();
    assert!(addr.ip().is_loopback());
    let port = addr.port();
    let hex_port = format!("{port:04X}");
    let listeners = std::fs::read_to_string("/proc/net/tcp").unwrap();
    let wanted = format!("0100007F:{hex_port}");
    assert!(
        listeners.contains(&wanted),
        "loopback listener {wanted} missing in /proc/net/tcp"
    );
    let wildcard = format!("00000000:{hex_port}");
    assert!(
        !listeners.contains(&wildcard),
        "wildcard listener {wildcard} must not exist"
    );
    drop(server);
}

#[test]
fn format_only_issues_keep_the_claim() {
    let (bytes, spec) = pinned_spec("new.json");
    let sha = spec_sha(&bytes);
    let dir = tempfile::tempdir().unwrap();
    write_scenario(
        dir.path(),
        "random.json",
        &json!({
            "id": "random-search-new",
            "change_ids": ["vc1_random"],
            "spec_sha256": sha,
            "request": { "method": "POST", "path": "/search/random", "body": {} },
            "response": { "status": 200, "body": [full_asset()] }
        }),
    );
    let scenarios = load_from(dir.path(), &spec, &sha);
    assert_eq!(scenarios.len(), 1);
    assert!(
        scenarios[0].supports_claim,
        "format-only issues stay report-only: {:?}",
        scenarios[0].unsupported
    );

    let running = Running::spawn(scenarios, spec);
    let (status, _) = round_trip(running.port, "POST", "/search/random", Some("{}"));
    assert_eq!(status, 200);
    let entries = running.stop_and_trace();
    assert!(entries[0].request_valid);
}

#[test]
fn other_unsupported_constructs_block_the_claim() {
    let spec = json!({
        "openapi": "3.0.0",
        "info": { "title": "Pets", "version": "1" },
        "paths": {
            "/pets": {
                "get": {
                    "responses": {
                        "200": {
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "oneOf": [{ "type": "string" }],
                                        "discriminator": { "propertyName": "kind" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    write_scenario(
        dir.path(),
        "pets.json",
        &json!({
            "id": "list-pets",
            "request": { "method": "GET", "path": "/pets" },
            "response": { "status": 200, "body": "fluffy" }
        }),
    );
    let scenarios = load_from(dir.path(), &spec, "no-pin");
    assert!(!scenarios[0].supports_claim);

    let running = Running::spawn(scenarios, spec);
    let (status, _) = round_trip(running.port, "GET", "/pets", None);
    assert_eq!(status, 200);
    let entries = running.stop_and_trace();
    assert_eq!(entries[0].scenario_id.as_deref(), Some("list-pets"));
}

#[test]
fn old_contract_fixture_serves_search_response() {
    let (bytes, spec) = pinned_spec("old.json");
    let sha = spec_sha(&bytes);
    let dir = tempfile::tempdir().unwrap();
    write_scenario(
        dir.path(),
        "random.json",
        &json!({
            "id": "random-search-old",
            "change_ids": ["vc1_random"],
            "spec_sha256": sha,
            "request": { "method": "POST", "path": "/search/random", "body": {} },
            "response": { "status": 200, "body": empty_search_response() }
        }),
    );
    let scenarios = load_from(dir.path(), &spec, &sha);
    let running = Running::spawn(scenarios, spec);
    let (status, body) = round_trip(running.port, "POST", "/search/random", Some("{}"));
    assert_eq!(status, 200);
    let seen: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(seen, empty_search_response());
    let entries = running.stop_and_trace();
    assert!(entries[0].request_valid);
    assert_eq!(entries[0].response_body_sha256, body_sha256_hex(&body));
}

#[test]
fn stub_command_serves_and_traces() {
    let sha = {
        let raw = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/immich/old.json"),
        )
        .unwrap();
        spec_sha(&raw)
    };
    let scenarios_dir = tempfile::tempdir().unwrap();
    write_scenario(
        scenarios_dir.path(),
        "random.json",
        &json!({
            "id": "random-search-old",
            "spec_sha256": sha,
            "request": { "method": "POST", "path": "/search/random", "body": {} },
            "response": { "status": 200, "body": empty_search_response() }
        }),
    );
    let run_dir = tempfile::tempdir().unwrap();
    let binary = assert_cmd::cargo::cargo_bin("sethu");
    let mut child = std::process::Command::new(binary)
        .arg("stub")
        .arg("--version")
        .arg("old")
        .arg("--port")
        .arg("0")
        .arg("--scenarios")
        .arg(scenarios_dir.path())
        .current_dir(run_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        let first = lines.next().map(|line| line.unwrap()).unwrap_or_default();
        sender.send(first).unwrap();
    });
    let readiness = receiver
        .recv_timeout(Duration::from_secs(15))
        .expect("stub prints a readiness line");
    assert!(readiness.starts_with("sethu-stub ready version=old addr=127.0.0.1:"));
    let port: u16 = readiness
        .split("addr=127.0.0.1:")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();

    let (missing, _) = round_trip(port, "GET", "/no-such-route", None);
    assert_eq!(missing, 599);
    let (served, served_body) = round_trip(port, "POST", "/search/random", Some("{}"));
    assert_eq!(served, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&served_body).unwrap(),
        empty_search_response()
    );

    child.kill().unwrap();
    child.wait().unwrap();
    let entries = read_trace(&run_dir.path().join("requests.jsonl")).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].scenario_id, None);
    assert_eq!(entries[0].response_status, 599);
    assert_eq!(entries[1].scenario_id.as_deref(), Some("random-search-old"));
    assert!(entries[1].request_valid);
    assert_eq!(entries[1].response_status, 200);
    assert!(entries[1].response_written);
}

/// One valid new-contract asset with a null stack.
fn full_asset() -> Value {
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
        "stack": null
    })
}

/// One valid old-contract search response with empty result sets.
fn empty_search_response() -> Value {
    json!({
        "albums": { "count": 0, "facets": [], "items": [], "total": 0 },
        "assets": {
            "count": 0,
            "facets": [],
            "items": [],
            "nextPage": null,
            "total": 0
        }
    })
}

/// Query pairs used by the header matching check.
fn header_map() -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    map.insert("x-tenant".to_string(), "demo".to_string());
    map
}

#[test]
fn declared_headers_and_query_narrow_the_match() {
    let spec = shop_spec();
    let dir = tempfile::tempdir().unwrap();
    write_scenario(
        dir.path(),
        "narrow.json",
        &json!({
            "id": "narrow-item",
            "request": {
                "method": "GET",
                "path": "/items/{id}",
                "query": { "verbose": "true" },
                "headers": { "x-tenant": "demo" }
            },
            "response": { "status": 200, "body": { "id": "narrow" } }
        }),
    );
    let scenarios = load_from(dir.path(), &spec, "no-pin");
    let scenario = &scenarios[0];
    let headers = header_map().into_iter().collect::<Vec<(String, String)>>();
    let mut query = IndexMap::new();
    query.insert("verbose".to_string(), "true".to_string());
    assert!(sethu::stub::scenario_matches(
        scenario,
        "GET",
        "/items/9",
        &query,
        &headers,
        &ParsedBody::Absent
    ));
    let mut other = IndexMap::new();
    other.insert("verbose".to_string(), "false".to_string());
    assert!(!sethu::stub::scenario_matches(
        scenario,
        "GET",
        "/items/9",
        &other,
        &headers,
        &ParsedBody::Absent
    ));
    assert!(!sethu::stub::scenario_matches(
        scenario,
        "GET",
        "/items/9",
        &query,
        &[],
        &ParsedBody::Absent
    ));
}
