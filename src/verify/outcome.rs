//! Structured test results plus stub-trace correlation.
//!
//! A stage result never trusts a bare exit code. The runner parses the
//! per-test report (JUnit XML from the nextest profile, or libtest text
//! when nextest is absent) and matches it against the stub trace for
//! the same check. A green stage needs the test green plus every
//! expected exchange received, valid, and answered with the scenario
//! response, plus no unexpected requests. A red stage needs the test
//! failed with the declared diagnostic plus the same clean exchange.
//! Anything else is an invalid red for a regression check, or a plain
//! failure for a guard.

use std::collections::HashMap;

use anyhow::Context;

use crate::stub::TraceEntry;
use crate::verify::manifest::{CheckSpec, ExpectedDiagnostic, Role};

/// Which report parser produced a test result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserKind {
    /// JUnit XML from the nextest profile.
    NextestJunit,
    /// Plain text from the libtest fallback.
    LibtestText,
}

/// Render a parser kind for run artefacts.
pub fn parser_name(parser: ParserKind) -> &'static str {
    match parser {
        ParserKind::NextestJunit => "nextest-junit",
        ParserKind::LibtestText => "libtest-text",
    }
}

/// One parsed test case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    /// Test case name as reported.
    pub name: String,
    /// Class or binary name as reported, when present.
    pub classname: String,
    /// Whether the case passed.
    pub passed: bool,
    /// Failure message plus captured output.
    pub output: String,
}

/// Full test process result for one check in one stage.
#[derive(Debug, Clone)]
pub struct TestRun {
    /// Exact command line that ran, joined with spaces.
    pub command: String,
    /// Which parser read the report.
    pub parser: ParserKind,
    /// Process exit code, when the process exited.
    pub exit_code: Option<i32>,
    /// Signal that killed the process, when one did.
    pub signal: Option<i32>,
    /// Whether the run hit the deadline and was killed.
    pub timed_out: bool,
    /// Whether output shows the harness failed to build.
    pub build_failed: bool,
    /// Parsed per-test cases.
    pub cases: Vec<CaseResult>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Verdict for one check in one stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageVerdict {
    /// The test passed with the full expected exchange.
    Pass,
    /// The test failed with the declared diagnostic after the exchange.
    ExpectedRed,
    /// A regression check met none of the two states above.
    InvalidRed(String),
    /// A guard check did not pass.
    Failed(String),
}

/// Whether a stage verdict counts as meeting its expectation.
pub fn stage_met(verdict: &StageVerdict, want_red: bool) -> bool {
    matches!(
        (verdict, want_red),
        (StageVerdict::Pass, false) | (StageVerdict::ExpectedRed, true)
    )
}

/// One expected exchange with its scenario response attached.
#[derive(Debug, Clone)]
struct ResolvedExchange<'a> {
    /// Scenario ID from the manifest entry.
    pub scenario: &'a str,
    /// Uppercase method from the manifest entry.
    pub method: String,
    /// Path template from the manifest entry.
    pub path: &'a str,
    /// Declared response status of the scenario.
    pub status: u16,
    /// Hex hash of the canonical scenario response bytes.
    pub body_hash: String,
}

/// Evaluate one check in one stage from its test result and trace.
///
/// `want_red` is true only for a regression check on original sources
/// against new fixtures. Guards always pass `want_red` as false. The
/// scenario map carries every loaded scenario by ID for the stage.
pub fn evaluate(
    check: &CheckSpec,
    want_red: bool,
    run: &TestRun,
    trace: &[TraceEntry],
    scenarios: &HashMap<String, crate::stub::Scenario>,
) -> StageVerdict {
    let failure = |reason: String| match check.role {
        Role::Regression => StageVerdict::InvalidRed(reason),
        Role::Guard => StageVerdict::Failed(reason),
    };
    if run.timed_out {
        return failure(format!(
            "test process for {:?} hit the deadline and was killed",
            check.test
        ));
    }
    if let Some(signal) = run.signal {
        return failure(format!(
            "test process for {:?} died on signal {signal}",
            check.test
        ));
    }
    if run.build_failed {
        return failure(format!(
            "harness for {:?} did not build; the harness must build against both commits, so this stage cannot show a regression",
            check.test
        ));
    }
    let matched = matching_cases(&run.cases, &check.test);
    if matched.is_empty() {
        return failure(format!(
            "named test {:?} did not run ({} case{} parsed)",
            check.test,
            run.cases.len(),
            if run.cases.len() == 1 { "" } else { "s" }
        ));
    }
    let green = matched.iter().all(|case| case.passed);
    let combined = combined_output(run, &matched);
    let resolved = match resolve_exchanges(check, scenarios) {
        Ok(resolved) => resolved,
        Err(error) => return failure(format!("{error:#}")),
    };
    let exchange = check_exchange(&resolved, trace);
    if want_red {
        if green {
            return failure(format!(
                "test {:?} passed on original sources against new fixtures, but the red stage needs a failure",
                check.test
            ));
        }
        let diagnostic = check
            .expected_diagnostic
            .as_ref()
            .map_or_else(|| "<missing>".to_string(), describe_diagnostic);
        let Some(wanted) = check.expected_diagnostic.as_ref() else {
            return failure(format!(
                "regression check {:?} declares no expected diagnostic",
                check.name
            ));
        };
        if !diagnostic_matches(wanted, &combined) {
            return failure(format!(
                "test {:?} failed without the expected diagnostic {diagnostic}; failure output did not match",
                check.test
            ));
        }
        if let Err(reason) = exchange {
            return failure(format!("red stage exchange broken: {reason}"));
        }
        return StageVerdict::ExpectedRed;
    }
    if !green {
        return failure(format!(
            "test {:?} failed where a pass was required; output: {}",
            check.test,
            snippet(&combined)
        ));
    }
    if let Err(reason) = exchange {
        return failure(format!("green stage exchange broken: {reason}"));
    }
    StageVerdict::Pass
}

/// Find the parsed cases that belong to one named check.
///
/// A case matches on an exact name or on a trailing `::name`
/// suffix, which covers namespaced nextest and libtest reports.
fn matching_cases<'a>(cases: &'a [CaseResult], test: &str) -> Vec<&'a CaseResult> {
    cases
        .iter()
        .filter(|case| case.name == test || case.name.ends_with(&format!("::{test}")))
        .collect()
}

/// Join the failure output of matched cases with the process streams.
///
/// The diagnostic may live in the JUnit message, in the captured
/// output, or in the libtest failure section on stdout, so every
/// source joins here before matching.
fn combined_output(run: &TestRun, matched: &[&CaseResult]) -> String {
    let mut parts = Vec::new();
    for case in matched {
        if !case.output.trim().is_empty() {
            parts.push(case.output.clone());
        }
    }
    parts.push(run.stdout.clone());
    parts.push(run.stderr.clone());
    parts.join("\n")
}

/// Shorten long output for failure reasons.
///
/// Cutting on character boundaries keeps multibyte output intact.
fn snippet(text: &str) -> String {
    const LIMIT: usize = 500;
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LIMIT {
        return flat;
    }
    let mut short: String = flat.chars().take(LIMIT).collect();
    short.push('…');
    short
}

/// Render one expected diagnostic for messages.
fn describe_diagnostic(diagnostic: &ExpectedDiagnostic) -> String {
    match diagnostic {
        ExpectedDiagnostic::Substring(text) => format!("{text:?}"),
        ExpectedDiagnostic::Pattern { regex } => format!("regex {regex:?}"),
    }
}

/// Report whether failure output carries one expected diagnostic.
pub fn diagnostic_matches(diagnostic: &ExpectedDiagnostic, output: &str) -> bool {
    match diagnostic {
        ExpectedDiagnostic::Substring(text) => output.contains(text),
        ExpectedDiagnostic::Pattern { regex } => match regex::Regex::new(regex) {
            Ok(pattern) => pattern.is_match(output),
            Err(_) => false,
        },
    }
}

/// Attach scenario responses to the manifest exchanges.
///
/// An unknown scenario ID fails the stage, since the expectation can
/// never be met.
fn resolve_exchanges<'a>(
    check: &'a CheckSpec,
    scenarios: &HashMap<String, crate::stub::Scenario>,
) -> anyhow::Result<Vec<ResolvedExchange<'a>>> {
    let mut resolved = Vec::with_capacity(check.expected_exchange.len());
    for exchange in &check.expected_exchange {
        let Some(scenario) = scenarios.get(&exchange.scenario) else {
            anyhow::bail!(
                "check {:?} expects unknown scenario {:?}",
                check.name,
                exchange.scenario
            );
        };
        let body = match &scenario.response.body {
            Some(body) => serde_json::to_vec(body)
                .with_context(|| format!("render scenario {:?} response", exchange.scenario))?,
            None => Vec::new(),
        };
        resolved.push(ResolvedExchange {
            scenario: &exchange.scenario,
            method: exchange.method.to_ascii_uppercase(),
            path: &exchange.path,
            status: scenario.response.status,
            body_hash: crate::stub::body_sha256_hex(&body),
        });
    }
    Ok(resolved)
}

/// Check every expected exchange against one stub trace.
///
/// Each expected scenario needs at least one entry that arrived,
/// validated, and was answered with the declared response. Every
/// other entry (unknown scenario, invalid request, unwritten reply)
/// fails the stage, since the trace must belong to this check alone.
fn check_exchange(resolved: &[ResolvedExchange<'_>], trace: &[TraceEntry]) -> anyhow::Result<()> {
    for wanted in resolved {
        let hit = trace.iter().any(|entry| {
            entry.scenario_id.as_deref() == Some(wanted.scenario)
                && entry.method.eq_ignore_ascii_case(&wanted.method)
                && crate::stub::template_matches(wanted.path, &entry.path)
                && entry.request_valid
                && entry.response_written
                && entry.response_status == wanted.status
                && entry.response_body_sha256 == wanted.body_hash
        });
        if !hit {
            anyhow::bail!(
                "expected exchange {:?} {} {} was not received, validated, and answered",
                wanted.scenario,
                wanted.method,
                wanted.path
            );
        }
    }
    for entry in trace {
        match entry.scenario_id.as_deref() {
            None => {
                anyhow::bail!(
                    "unexpected request {} {} reached no scenario (status {})",
                    entry.method,
                    entry.path,
                    entry.response_status
                );
            }
            Some(seen) => {
                if !resolved.iter().any(|wanted| wanted.scenario == seen) {
                    anyhow::bail!(
                        "request {} {} hit scenario {seen:?}, which the check does not expect",
                        entry.method,
                        entry.path
                    );
                }
                if !entry.request_valid {
                    anyhow::bail!(
                        "request {} {} for scenario {seen:?} failed validation: {}",
                        entry.method,
                        entry.path,
                        entry.request_detail
                    );
                }
                if !entry.response_written {
                    anyhow::bail!("response for scenario {seen:?} was not fully written",);
                }
            }
        }
    }
    Ok(())
}

/// Parse nextest JUnit XML into per-test cases.
///
/// Each `testcase` element yields one case. A nested `failure` or
/// `error` element marks it failed, with the message attribute plus
/// element text and `system-out` joined as the output.
pub fn parse_junit(xml: &str) -> anyhow::Result<Vec<CaseResult>> {
    use quick_xml::XmlVersion;
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut cases = Vec::new();
    let mut current: Option<CaseResult> = None;
    let mut in_failure = false;
    let mut pending_message = String::new();
    let mut pending_text = String::new();
    let mut in_system_out = false;
    let mut system_out = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => match element.name().into_inner() {
                "testcase" => {
                    current = Some(CaseResult {
                        name: junit_attr(&element, "name"),
                        classname: junit_attr(&element, "classname"),
                        passed: true,
                        output: String::new(),
                    });
                    pending_message.clear();
                    pending_text.clear();
                    system_out.clear();
                }
                "failure" | "error" => {
                    in_failure = true;
                    pending_message = junit_attr(&element, "message");
                    pending_text.clear();
                }
                "system-out" => {
                    in_system_out = true;
                }
                _ => {}
            },
            Ok(Event::Empty(element)) => match element.name().into_inner() {
                "testcase" => {
                    cases.push(CaseResult {
                        name: junit_attr(&element, "name"),
                        classname: junit_attr(&element, "classname"),
                        passed: true,
                        output: String::new(),
                    });
                }
                "failure" | "error" => {
                    if let Some(case) = current.as_mut() {
                        case.passed = false;
                        case.output = junit_attr(&element, "message");
                    }
                }
                _ => {}
            },
            Ok(Event::Text(text)) => {
                let decoded = text.xml_content(XmlVersion::Explicit1_0).into_owned();
                if in_failure {
                    pending_text.push_str(&decoded);
                } else if in_system_out {
                    system_out.push_str(&decoded);
                }
            }
            Ok(Event::CData(text)) => {
                let decoded = text.into_inner().into_owned();
                if in_failure {
                    pending_text.push_str(&decoded);
                } else if in_system_out {
                    system_out.push_str(&decoded);
                }
            }
            Ok(Event::End(element)) => match element.name().into_inner() {
                "failure" | "error" => {
                    in_failure = false;
                    if let Some(case) = current.as_mut() {
                        case.passed = false;
                        case.output = if pending_text.trim().is_empty() {
                            pending_message.clone()
                        } else if pending_message.trim().is_empty() {
                            pending_text.clone()
                        } else {
                            format!("{}\n{}", pending_message, pending_text)
                        };
                    }
                }
                "system-out" => {
                    in_system_out = false;
                }
                "testcase" => {
                    if let Some(mut case) = current.take() {
                        if !system_out.trim().is_empty() {
                            if case.output.trim().is_empty() {
                                case.output = system_out.clone();
                            } else {
                                case.output.push('\n');
                                case.output.push_str(&system_out);
                            }
                        }
                        cases.push(case);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => {
                anyhow::bail!("parse JUnit report: {error}");
            }
            _ => {}
        }
    }
    Ok(cases)
}

/// Read one string attribute from a JUnit element.
///
/// A missing or undecodable attribute reads as empty, so a weird
/// report yields a weird case rather than a parser crash.
fn junit_attr(element: &quick_xml::events::BytesStart<'_>, name: &str) -> String {
    use quick_xml::XmlVersion;
    for attribute in element.attributes().flatten() {
        if attribute.key.into_inner() == name {
            return attribute
                .normalized_value(XmlVersion::Explicit1_0)
                .map(|value| value.into_owned())
                .unwrap_or_default();
        }
    }
    String::new()
}

/// Parse libtest text output into per-test cases.
///
/// Lines shaped like `test <path> ... ok` or `... FAILED` yield
/// cases. A `---- <path> stdout ----` section attaches its output
/// to the matching case. Lines before any case are ignored.
pub fn parse_libtest(text: &str) -> Vec<CaseResult> {
    let mut cases: Vec<CaseResult> = Vec::new();
    let mut outputs: HashMap<String, String> = HashMap::new();
    let mut section: Option<String> = None;
    let mut current_text = String::new();
    for line in text.lines() {
        if let Some(name) = section_header(line) {
            if let Some(open) = section.take() {
                outputs.insert(open, std::mem::take(&mut current_text));
            }
            section = Some(name);
            continue;
        }
        if section.is_some() {
            if line.starts_with("---- ") && line.ends_with(" ----") {
                continue;
            }
            if line.trim().is_empty() && current_text.trim().is_empty() {
                continue;
            }
            current_text.push_str(line);
            current_text.push('\n');
            continue;
        }
        if let Some((name, passed)) = result_line(line) {
            let index = cases.iter().position(|case| case.name == name);
            match index {
                Some(position) => {
                    cases[position].passed = cases[position].passed && passed;
                }
                None => cases.push(CaseResult {
                    name,
                    classname: String::new(),
                    passed,
                    output: String::new(),
                }),
            }
        }
    }
    if let Some(open) = section.take() {
        outputs.insert(open, current_text);
    }
    for case in &mut cases {
        if let Some(text) = outputs.remove(&case.name) {
            case.output = text;
        }
    }
    cases
}

/// Read a libtest section header, if the line opens one.
///
/// Headers look like `---- verify_random_picker stdout ----`.
fn section_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("---- ")?;
    let inner = rest.strip_suffix(" ----")?;
    let name = inner.strip_suffix(" stdout")?;
    if name.trim().is_empty() || name.contains(' ') {
        return None;
    }
    Some(name.to_string())
}

/// Read a libtest result line, if the line reports one.
///
/// Lines look like `test verify_random_picker ... ok`.
fn result_line(line: &str) -> Option<(String, bool)> {
    let rest = line.strip_prefix("test ")?;
    let (name, tail) = rest.rsplit_once(" ... ")?;
    let name = name.trim();
    if name.is_empty() || name.contains(' ') {
        return None;
    }
    let status = tail.split_whitespace().next()?;
    match status {
        "ok" => Some((name.to_string(), true)),
        "FAILED" | "failed" => Some((name.to_string(), false)),
        _ => None,
    }
}

/// Report whether output shows the harness failed to build.
///
/// A nonzero cargo exit with a compile error never counts as a test
/// failure. It means the harness cannot run on this commit at all.
pub fn looks_like_build_failure(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}");
    combined.contains("could not compile")
        || combined.contains("error: could not compile")
        || combined.contains("error[")
        || (combined.contains("error:") && combined.contains("--> "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::manifest::{ExpectedExchange, Role};

    /// One passing case with the given name.
    fn passing(name: &str) -> CaseResult {
        CaseResult {
            name: name.to_string(),
            classname: String::new(),
            passed: true,
            output: String::new(),
        }
    }

    /// One failing case with the given output.
    fn failing(name: &str, output: &str) -> CaseResult {
        CaseResult {
            name: name.to_string(),
            classname: String::new(),
            passed: false,
            output: output.to_string(),
        }
    }

    /// One green test run around the given cases.
    fn green_run(cases: Vec<CaseResult>) -> TestRun {
        TestRun {
            command: "cargo test".to_string(),
            parser: ParserKind::LibtestText,
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            build_failed: false,
            cases,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// One regression check with one expected exchange.
    fn regression_check() -> CheckSpec {
        CheckSpec {
            name: "random-picker".to_string(),
            role: Role::Regression,
            change_ids: vec!["vc1_x".to_string()],
            test: "verify_random_picker".to_string(),
            expected_diagnostic: Some(ExpectedDiagnostic::Substring(
                "random picker ids".to_string(),
            )),
            expected_exchange: vec![ExpectedExchange {
                scenario: "random-search".to_string(),
                method: "POST".to_string(),
                path: "/search/random".to_string(),
            }],
        }
    }

    /// One scenario map entry with a fixed response.
    fn scenario_map() -> HashMap<String, crate::stub::Scenario> {
        let scenario = crate::stub::Scenario {
            id: "random-search".to_string(),
            change_ids: vec![],
            request: crate::stub::ScenarioRequest {
                method: "POST".to_string(),
                path: "/search/random".to_string(),
                query: indexmap::IndexMap::new(),
                headers: indexmap::IndexMap::new(),
                body: None,
            },
            response: crate::stub::ScenarioResponse {
                status: 200,
                headers: indexmap::IndexMap::new(),
                body: Some(serde_json::json!({"items": []})),
            },
            unsupported: vec![],
            supports_claim: true,
        };
        HashMap::from([("random-search".to_string(), scenario)])
    }

    /// One trace entry matching the single expected exchange.
    fn good_trace() -> Vec<TraceEntry> {
        let body = serde_json::to_vec(&serde_json::json!({"items": []})).unwrap();
        vec![TraceEntry {
            scenario_id: Some("random-search".to_string()),
            method: "POST".to_string(),
            path: "/search/random".to_string(),
            request_valid: true,
            request_detail: "body matches the contract schema".to_string(),
            response_status: 200,
            response_body_sha256: crate::stub::body_sha256_hex(&body),
            response_written: true,
        }]
    }

    #[test]
    fn green_test_with_clean_trace_passes() {
        let check = regression_check();
        let run = green_run(vec![passing("verify_random_picker")]);
        let verdict = evaluate(&check, false, &run, &good_trace(), &scenario_map());
        assert_eq!(verdict, StageVerdict::Pass);
    }

    #[test]
    fn failing_test_with_diagnostic_and_exchange_is_expected_red() {
        let check = regression_check();
        let mut run = green_run(vec![failing(
            "verify_random_picker",
            "assertion failed: random picker ids",
        )]);
        run.exit_code = Some(101);
        let verdict = evaluate(&check, true, &run, &good_trace(), &scenario_map());
        assert_eq!(verdict, StageVerdict::ExpectedRed);
    }

    #[test]
    fn wrong_failure_message_is_invalid_red() {
        let check = regression_check();
        let mut run = green_run(vec![failing("verify_random_picker", "connection refused")]);
        run.exit_code = Some(101);
        let verdict = evaluate(&check, true, &run, &good_trace(), &scenario_map());
        assert!(matches!(verdict, StageVerdict::InvalidRed(_)));
    }

    #[test]
    fn connection_failure_with_empty_trace_is_invalid_red() {
        let check = regression_check();
        let mut run = green_run(vec![failing(
            "verify_random_picker",
            "request failed: connection refused",
        )]);
        run.exit_code = Some(101);
        let verdict = evaluate(&check, true, &run, &[], &scenario_map());
        assert!(matches!(verdict, StageVerdict::InvalidRed(_)));
    }

    #[test]
    fn unexpected_request_breaks_a_pass() {
        let check = regression_check();
        let run = green_run(vec![passing("verify_random_picker")]);
        let mut trace = good_trace();
        trace.push(TraceEntry {
            scenario_id: None,
            method: "GET".to_string(),
            path: "/elsewhere".to_string(),
            request_valid: false,
            request_detail: "no scenario matches".to_string(),
            response_status: 599,
            response_body_sha256: crate::stub::body_sha256_hex(b"missing"),
            response_written: true,
        });
        let verdict = evaluate(&check, false, &run, &trace, &scenario_map());
        assert!(matches!(verdict, StageVerdict::InvalidRed(_)));
    }

    #[test]
    fn guard_failure_reports_failed_not_invalid_red() {
        let mut check = regression_check();
        check.role = Role::Guard;
        check.expected_diagnostic = None;
        let mut run = green_run(vec![failing("verify_random_picker", "random picker ids")]);
        run.exit_code = Some(101);
        let verdict = evaluate(&check, false, &run, &good_trace(), &scenario_map());
        assert!(matches!(verdict, StageVerdict::Failed(_)));
    }

    #[test]
    fn junit_parser_reads_failure_message_and_output() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
<testsuite name="nextest" tests="2">
<testcase classname="harness" name="verify_random_picker">
<failure message="random picker ids">assertion failed</failure>
<system-out>request trace</system-out>
</testcase>
<testcase classname="harness" name="verify_smart_guard"/>
</testsuite>
</testsuites>"#;
        let cases = parse_junit(xml).unwrap();
        assert_eq!(cases.len(), 2);
        assert!(!cases[0].passed);
        assert!(cases[0].output.contains("random picker ids"));
        assert!(cases[0].output.contains("request trace"));
        assert!(cases[1].passed);
    }

    #[test]
    fn libtest_parser_reads_results_and_sections() {
        let text = "running 2 tests\ntest verify_random_picker ... FAILED\ntest verify_smart_guard ... ok\nfailures:\n---- verify_random_picker stdout ----\nrandom picker ids missing\n";
        let cases = parse_libtest(text);
        assert_eq!(cases.len(), 2);
        let failed = cases
            .iter()
            .find(|case| case.name == "verify_random_picker")
            .unwrap();
        assert!(!failed.passed);
        assert!(failed.output.contains("random picker ids"));
    }

    #[test]
    fn regex_diagnostic_matches_patterns() {
        let diagnostic = ExpectedDiagnostic::Pattern {
            regex: "random picker ids: .*".to_string(),
        };
        assert!(diagnostic_matches(
            &diagnostic,
            "random picker ids: left != right"
        ));
        assert!(!diagnostic_matches(&diagnostic, "unrelated failure"));
    }
}
