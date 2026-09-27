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
/// failure. It means the harness cannot run on this commit at all. A
/// dependency that cargo cannot resolve or fetch stops the build the
/// same way, before anything compiles.
///
/// Cargo and nextest colour their output when `CARGO_TERM_COLOR=always`
/// reaches them, so escape sequences are removed before any match.
pub fn looks_like_build_failure(stdout: &str, stderr: &str) -> bool {
    let combined = strip_ansi_escapes(&format!("{stdout}\n{stderr}"));
    if tests_started(&combined) {
        return false;
    }
    looks_like_compile_failure(&combined) || looks_like_resolution_failure(&combined)
}

/// Report whether any test binary started running.
///
/// Libtest prints `running N tests` and nextest prints `Starting N
/// tests` once tests begin. Cargo builds every test target before it
/// runs any, so neither line can follow a failed build or a failed
/// resolution. Anything that looks like a cargo error after that point
/// came from a test.
fn tests_started(output: &str) -> bool {
    output.lines().any(|line| {
        let line = line.trim_start();
        let rest = line
            .strip_prefix("running ")
            .or_else(|| line.strip_prefix("Starting "));
        rest.and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|count| count.parse::<u64>().is_ok())
    })
}

/// Report whether rustc rejected the code.
///
/// Three flush-left lines count. `error[E...]` is a coded compiler
/// error, and `error: could not compile` is cargo's summary of one. An
/// uncoded `error: ` line counts only when its own source span (`--> `)
/// is the next non-blank line, as with a syntax error. A warning also
/// prints a span, and a failing run ends with a bare `error: test
/// failed`, so neither line alone may decide.
fn looks_like_compile_failure(output: &str) -> bool {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    while let Some(line) = lines.next() {
        if line.starts_with("error[E") || line.starts_with("error: could not compile ") {
            return true;
        }
        if line.starts_with("error: ")
            && lines
                .clone()
                .next()
                .is_some_and(|next| next.trim_start().starts_with("--> "))
        {
            return true;
        }
    }
    false
}

/// Remove ANSI escape sequences from terminal output.
///
/// A control sequence (`ESC [`, parameter and intermediate bytes, then
/// one final byte) carries colour and style. It ends early, without
/// consuming it, at any character that cannot belong to it, such as a
/// newline. An operating system command (`ESC ]`) runs to a bell or to
/// `ESC \`. A character set designator (`ESC (`, `)`, `*` or `+`) takes
/// one more character. Any other escape takes one following character.
/// An unterminated sequence at the end is dropped.
fn strip_ansi_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                // Parameter and intermediate bytes run 0x20 to 0x3f, and
                // the final byte runs 0x40 to 0x7e. Anything else ends a
                // malformed sequence and stays in the output.
                while let Some(&c) = chars.peek() {
                    if ('\u{20}'..='\u{3f}').contains(&c) {
                        chars.next();
                    } else {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some('(' | ')' | '*' | '+') => {
                chars.next();
            }
            _ => {}
        }
    }
    out
}

/// Cargo's own wording when it cannot resolve or fetch a dependency.
///
/// Each phrase opens an `error: ` line or a line of the `Caused by:`
/// chain beneath one.
const RESOLUTION_FAILURES: [&str; 5] = [
    "no matching package named",
    "failed to select a version for",
    "failed to load source for dependency",
    "failed to download",
    "failed to get `",
];

/// Report whether cargo stopped while resolving or fetching dependencies.
///
/// A test that prints one of cargo's phrases must not read as a build
/// failure. Only a line that starts with `error: ` or sits in the
/// `Caused by:` chain counts. The caller has already ruled out output
/// where tests started. Both matches rely on the caller having removed
/// colour escapes, since a coloured line does not start with these
/// prefixes.
fn looks_like_resolution_failure(output: &str) -> bool {
    let mut in_cause_chain = false;
    for line in output.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "Caused by:" {
            in_cause_chain = true;
            continue;
        }
        let indented = trimmed.len() < line.len();
        let body = if let Some(rest) = trimmed.strip_prefix("error: ") {
            in_cause_chain = false;
            Some(rest)
        } else if in_cause_chain && indented {
            Some(trimmed)
        } else {
            in_cause_chain = false;
            None
        };
        if body.is_some_and(|body| {
            RESOLUTION_FAILURES
                .iter()
                .any(|phrase| body.starts_with(phrase))
        }) {
            return true;
        }
    }
    false
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

    /// Cargo's stderr when an offline build meets a cold registry.
    const OFFLINE_RESOLUTION_STDERR: &str = "error: no matching package named `reqwest` found
location searched: crates.io index
required by package `immich-consumer v0.1.0 (/work/consumer)`
note: offline mode (via `--offline`) can sometimes cause surprising resolution failures
help: if this error is too confusing you may wish to retry without `--offline`
";

    #[test]
    fn offline_resolution_failure_is_a_build_failure() {
        assert!(looks_like_build_failure("", OFFLINE_RESOLUTION_STDERR));
    }

    #[test]
    fn each_resolution_phrase_is_a_build_failure() {
        let outputs = [
            "error: failed to select a version for the requirement `serde = \"=999.0.0\"`\ncandidate versions found which didn't match: 1.0.228\n",
            "error: failed to get `nope` as a dependency of package `consumer v0.1.0 (/work/consumer)`\n\nCaused by:\n  failed to load source for dependency `nope`\n\nCaused by:\n  unable to update /work/nope\n",
            "error: failed to download from `https://static.crates.io/api/v1/crates/serde/1.0.228/download`\n\nCaused by:\n  [6] Couldn't resolve host name\n",
            "error: failed to fetch `https://github.com/rust-lang/crates.io-index`\n\nCaused by:\n  failed to download `serde v1.0.228`\n",
        ];
        for output in outputs {
            assert!(looks_like_build_failure("", output), "missed: {output}");
        }
    }

    #[test]
    fn failing_test_output_is_not_a_build_failure() {
        let stdout = "running 1 test\ntest verify_random_picker ... FAILED\n\nfailures:\n\n---- verify_random_picker stdout ----\nthread 'verify_random_picker' panicked at tests/api.rs:12:5:\nassertion failed: random picker ids\n\nfailures:\n    verify_random_picker\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored\n";
        let stderr = "error: test failed, to rerun pass `--test api`\n";
        assert!(!looks_like_build_failure(stdout, stderr));
    }

    #[test]
    fn cargo_phrases_printed_by_a_running_test_are_not_a_build_failure() {
        let stdout = "running 1 test\ntest verify_random_picker ... FAILED\n\n---- verify_random_picker stdout ----\nerror: no matching package named `reqwest` found\nfailed to download the random picker ids\n";
        assert!(!looks_like_build_failure(stdout, ""));
        let nextest = "    Starting 1 test across 1 binary\n        FAIL [   0.004s] consumer::api verify_random_picker\nerror: failed to download the picker page\n";
        assert!(!looks_like_build_failure("", nextest));
    }

    #[test]
    fn unanchored_cargo_phrase_is_not_a_build_failure() {
        let stderr = "note: failed to download nothing\nwarning: no matching package named `x` in the lockfile comment\n";
        assert!(!looks_like_build_failure("", stderr));
    }

    #[test]
    fn coloured_resolution_failure_is_a_build_failure() {
        let libtest = "\x1b[1m\x1b[91merror\x1b[0m: no matching package named `reqwest` found\nlocation searched: crates.io index\nrequired by package `immich-consumer v0.1.0 (/work/consumer)`\n";
        assert!(looks_like_build_failure("", libtest));
        let nextest = "\x1b[1m\x1b[91merror\x1b[0m: no matching package named `reqwest` found\nlocation searched: crates.io index\n\x1b[31;1merror\x1b[0m: command `cargo metadata --format-version 1 --all-features --filter-platform x86_64-unknown-linux-gnu` exited with code 101\n";
        assert!(looks_like_build_failure("", nextest));
        let chain = "\x1b[1m\x1b[91merror\x1b[0m: failed to get `nope` as a dependency of package `pathdep v0.1.0 (/work/pathdep)`\n\nCaused by:\n  failed to load source for dependency `nope`\n\nCaused by:\n  unable to update /work/nope\n";
        assert!(looks_like_build_failure("", chain));
    }

    #[test]
    fn coloured_nextest_start_guards_against_cargo_phrases() {
        let nextest = "\x1b[32;1m    Starting\x1b[0m \x1b[1m1\x1b[0m test across \x1b[1m1\x1b[0m binary\n    error: failed to download the picker page\n";
        assert!(!looks_like_build_failure("", nextest));
    }

    /// Real `cargo test` stdout from a crate with one warning and one failing test, uncoloured.
    const WARN_FAIL_LIBTEST_PLAIN_STDOUT: &str = concat!(
        "\n",
        "running 1 test\n",
        "test tests::red ... FAILED\n",
        "\n",
        "failures:\n",
        "\n",
        "---- tests::red stdout ----\n",
        "\n",
        "thread 'tests::red' (3609506) panicked at src/lib.rs:3:32:\n",
        "assertion `left == right` failed: random picker ids\n",
        "  left: 1\n",
        " right: 2\n",
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n",
        "\n",
        "\n",
        "failures:\n",
        "    tests::red\n",
        "\n",
        "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        "\n",
    );
    /// Real `cargo test` stderr from the same run.
    const WARN_FAIL_LIBTEST_PLAIN_STDERR: &str = concat!(
        "   Compiling warnfail v0.1.0 (/work/warnfail)\n",
        "warning: unused variable: `unused`\n",
        " --> src/lib.rs:1:25\n",
        "  |\n",
        "1 | pub fn f() -> u32 { let unused = 3; 1 }\n",
        "  |                         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_unused`\n",
        "  |\n",
        "  = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default\n",
        "\n",
        "warning: `warnfail` (lib) generated 1 warning (run `cargo fix --lib -p warnfail` to apply 1 suggestion)\n",
        "warning: `warnfail` (lib test) generated 1 warning (1 duplicate)\n",
        "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.14s\n",
        "     Running unittests src/lib.rs (target/debug/deps/warnfail-8e6f24af83bb4dc1)\n",
        "error: test failed, to rerun pass `--lib`\n",
    );
    /// Real `cargo test` stdout from the same crate under `CARGO_TERM_COLOR=always`.
    const WARN_FAIL_LIBTEST_COLOUR_STDOUT: &str = concat!(
        "\n",
        "running 1 test\n",
        "test tests::red ... FAILED\n",
        "\n",
        "failures:\n",
        "\n",
        "---- tests::red stdout ----\n",
        "\n",
        "thread 'tests::red' (3609592) panicked at src/lib.rs:3:32:\n",
        "assertion `left == right` failed: random picker ids\n",
        "  left: 1\n",
        " right: 2\n",
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n",
        "\n",
        "\n",
        "failures:\n",
        "    tests::red\n",
        "\n",
        "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        "\n",
    );
    /// Real `cargo test` stderr from the coloured run.
    const WARN_FAIL_LIBTEST_COLOUR_STDERR: &str = concat!(
        "\x1b[1m\x1b[33mwarning\x1b[0m\x1b[1m: unused variable: `unused`\x1b[0m\n",
        " \x1b[1m\x1b[94m--> \x1b[0msrc/lib.rs:1:25\n",
        "  \x1b[1m\x1b[94m|\x1b[0m\n",
        "\x1b[1m\x1b[94m1\x1b[0m \x1b[1m\x1b[94m|\x1b[0m pub fn f() -> u32 { let unused = 3; 1 }\n",
        "  \x1b[1m\x1b[94m|\x1b[0m                         \x1b[1m\x1b[33m^^^^^^\x1b[0m \x1b[1m\x1b[33mhelp: if this is intentional, prefix it with an underscore: `_unused`\x1b[0m\n",
        "  \x1b[1m\x1b[94m|\x1b[0m\n",
        "  \x1b[1m\x1b[94m= \x1b[0m\x1b[1mnote\x1b[0m: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default\n",
        "\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m: `warnfail` (lib) generated 1 warning (run `cargo fix --lib -p warnfail` to apply 1 suggestion)\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m: `warnfail` (lib test) generated 1 warning (1 duplicate)\n",
        "\x1b[1m\x1b[92m    Finished\x1b[0m `test` profile [unoptimized + debuginfo] target(s) in 0.00s\n",
        "\x1b[1m\x1b[92m     Running\x1b[0m unittests src/lib.rs (target/debug/deps/warnfail-8e6f24af83bb4dc1)\n",
        "\x1b[1m\x1b[91merror\x1b[0m: test failed, to rerun pass `--lib`\n",
    );
    /// Real `cargo nextest run` stderr from the same crate, uncoloured. Stdout was empty.
    const WARN_FAIL_NEXTEST_PLAIN_STDERR: &str = concat!(
        "warning: unused variable: `unused`\n",
        " --> src/lib.rs:1:25\n",
        "  |\n",
        "1 | pub fn f() -> u32 { let unused = 3; 1 }\n",
        "  |                         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_unused`\n",
        "  |\n",
        "  = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default\n",
        "\n",
        "warning: `warnfail` (lib) generated 1 warning (run `cargo fix --lib -p warnfail` to apply 1 suggestion)\n",
        "warning: `warnfail` (lib test) generated 1 warning (1 duplicate)\n",
        "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.00s\n",
        "────────────\n",
        " Nextest run ID 593e9b36-070f-4f3e-9746-e66faa9881c4 with nextest profile: default\n",
        "    Starting 1 test across 1 binary\n",
        "        FAIL [   0.004s] (1/1) warnfail tests::red\n",
        "  stdout ───\n",
        "\n",
        "    running 1 test\n",
        "    test tests::red ... FAILED\n",
        "\n",
        "    failures:\n",
        "\n",
        "    failures:\n",
        "        tests::red\n",
        "\n",
        "    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        "\n",
        "  stderr ───\n",
        "\n",
        "    thread 'tests::red' (3609579) panicked at src/lib.rs:3:32:\n",
        "    assertion `left == right` failed: random picker ids\n",
        "      left: 1\n",
        "     right: 2\n",
        "    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n",
        "\n",
        "────────────\n",
        "     Summary [   0.005s] 1 test run: 0 passed, 1 failed, 0 skipped\n",
        "        FAIL [   0.004s] (1/1) warnfail tests::red\n",
        "error: test run failed\n",
    );
    /// Real `cargo nextest run` stderr from the coloured run. Stdout was empty.
    const WARN_FAIL_NEXTEST_COLOUR_STDERR: &str = concat!(
        "\x1b[1m\x1b[33mwarning\x1b[0m\x1b[1m: unused variable: `unused`\x1b[0m\n",
        " \x1b[1m\x1b[94m--> \x1b[0msrc/lib.rs:1:25\n",
        "  \x1b[1m\x1b[94m|\x1b[0m\n",
        "\x1b[1m\x1b[94m1\x1b[0m \x1b[1m\x1b[94m|\x1b[0m pub fn f() -> u32 { let unused = 3; 1 }\n",
        "  \x1b[1m\x1b[94m|\x1b[0m                         \x1b[1m\x1b[33m^^^^^^\x1b[0m \x1b[1m\x1b[33mhelp: if this is intentional, prefix it with an underscore: `_unused`\x1b[0m\n",
        "  \x1b[1m\x1b[94m|\x1b[0m\n",
        "  \x1b[1m\x1b[94m= \x1b[0m\x1b[1mnote\x1b[0m: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default\n",
        "\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m: `warnfail` (lib) generated 1 warning (run `cargo fix --lib -p warnfail` to apply 1 suggestion)\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m: `warnfail` (lib test) generated 1 warning (1 duplicate)\n",
        "\x1b[1m\x1b[92m    Finished\x1b[0m `test` profile [unoptimized + debuginfo] target(s) in 0.00s\n",
        "────────────\n",
        "\x1b[32;1m Nextest run\x1b[0m ID \x1b[1m50389bcb-d9bd-4579-9da2-4541399da5a3\x1b[0m with nextest profile: \x1b[1mdefault\x1b[0m\n",
        "\x1b[32;1m    Starting\x1b[0m \x1b[1m1\x1b[0m test across \x1b[1m1\x1b[0m binary\n",
        "\x1b[31;1m        FAIL\x1b[0m [   0.004s] (1/1) \x1b[35;1mwarnfail\x1b[0m \x1b[36mtests\x1b[0m\x1b[36m::\x1b[0m\x1b[34;1mred\x1b[0m\n",
        "\x1b[31;1m \x1b[0m \x1b[31;1mstdout\x1b[0m \x1b[31;1m───\x1b[0m\n",
        "\n",
        "    running 1 test\n",
        "    test tests::red ... FAILED\n",
        "\n",
        "    failures:\n",
        "\n",
        "    failures:\n",
        "        tests::red\n",
        "\n",
        "    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        "    \x1b[0m\n",
        "\x1b[31;1m \x1b[0m \x1b[31;1mstderr\x1b[0m \x1b[31;1m───\x1b[0m\n",
        "\n",
        "    \x1b[0m\x1b[31;1mthread 'tests::red' (3609661) panicked at src/lib.rs:3:32:\x1b[0m\n",
        "    \x1b[31;1massertion `left == right` failed: random picker ids\x1b[0m\n",
        "      left: 1\n",
        "     right: 2\n",
        "    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\x1b[0m\n",
        "\n",
        "────────────\n",
        "\x1b[31;1m     Summary\x1b[0m [   0.004s] \x1b[1m1\x1b[0m test run: \x1b[1m0\x1b[0m \x1b[32;1mpassed\x1b[0m, \x1b[1m1\x1b[0m \x1b[31;1mfailed\x1b[0m, \x1b[1m0\x1b[0m \x1b[33;1mskipped\x1b[0m\n",
        "\x1b[31;1m        FAIL\x1b[0m [   0.004s] (1/1) \x1b[35;1mwarnfail\x1b[0m \x1b[36mtests\x1b[0m\x1b[36m::\x1b[0m\x1b[34;1mred\x1b[0m\n",
        "\x1b[31;1merror\x1b[0m: test run failed\n",
    );
    /// Real coloured `cargo test` stderr for a coded compile error. Stdout was empty.
    const CODED_ERROR_LIBTEST_COLOUR_STDERR: &str = concat!(
        "\x1b[1m\x1b[92m   Compiling\x1b[0m code v0.1.0 (/work/code)\n",
        "\x1b[1m\x1b[91merror[E0425]\x1b[0m\x1b[1m: cannot find value `missing` in this scope\x1b[0m\n",
        " \x1b[1m\x1b[94m--> \x1b[0msrc/lib.rs:1:37\n",
        "  \x1b[1m\x1b[94m|\x1b[0m\n",
        "\x1b[1m\x1b[94m1\x1b[0m \x1b[1m\x1b[94m|\x1b[0m pub fn f() -> u32 { let unused = 3; missing }\n",
        "  \x1b[1m\x1b[94m|\x1b[0m                                     \x1b[1m\x1b[91m^^^^^^^\x1b[0m \x1b[1m\x1b[91mnot found in this scope\x1b[0m\n",
        "\n",
        "\x1b[1mFor more information about this error, try `rustc --explain E0425`.\x1b[0m\n",
        "\x1b[1m\x1b[91merror\x1b[0m: could not compile `code` (lib) due to 1 previous error\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m: build failed, waiting for other jobs to finish...\n",
        "\x1b[1m\x1b[91merror\x1b[0m: could not compile `code` (lib test) due to 1 previous error\n",
    );
    /// Real uncoloured `cargo nextest run` stderr for a coded compile error.
    const CODED_ERROR_NEXTEST_PLAIN_STDERR: &str = concat!(
        "   Compiling code v0.1.0 (/work/code)\n",
        "error[E0425]: cannot find value `missing` in this scope\n",
        " --> src/lib.rs:1:37\n",
        "  |\n",
        "1 | pub fn f() -> u32 { let unused = 3; missing }\n",
        "  |                                     ^^^^^^^ not found in this scope\n",
        "\n",
        "For more information about this error, try `rustc --explain E0425`.\n",
        "error: could not compile `code` (lib) due to 1 previous error\n",
        "warning: build failed, waiting for other jobs to finish...\n",
        "error: could not compile `code` (lib test) due to 1 previous error\n",
        "error: command `cargo '--color=never' test --no-run --message-format json-render-diagnostics` exited with code 101\n",
    );
    /// Real uncoloured `cargo test` stderr for an uncoded syntax error next to a warning.
    const SYNTAX_ERROR_LIBTEST_PLAIN_STDERR: &str = concat!(
        "   Compiling syntax v0.1.0 (/work/syntax)\n",
        "error: expected expression, found `}`\n",
        " --> src/lib.rs:2:26\n",
        "  |\n",
        "2 | pub fn g() -> u32 { 1 +  }\n",
        "  |                          ^ expected expression\n",
        "\n",
        "warning: unused variable: `unused`\n",
        " --> src/lib.rs:1:25\n",
        "  |\n",
        "1 | pub fn f() -> u32 { let unused = 3; 1 }\n",
        "  |                         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_unused`\n",
        "  |\n",
        "  = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default\n",
        "\n",
        "warning: `syntax` (lib test) generated 1 warning (1 duplicate)\n",
        "error: could not compile `syntax` (lib test) due to 1 previous error; 1 warning emitted\n",
        "warning: build failed, waiting for other jobs to finish...\n",
        "warning: `syntax` (lib) generated 1 warning\n",
        "error: could not compile `syntax` (lib) due to 1 previous error; 1 warning emitted\n",
    );
    /// Real coloured `cargo nextest run` stderr for the same syntax error.
    const SYNTAX_ERROR_NEXTEST_COLOUR_STDERR: &str = concat!(
        "\x1b[1m\x1b[92m   Compiling\x1b[0m syntax v0.1.0 (/work/syntax)\n",
        "\x1b[1m\x1b[91merror\x1b[0m\x1b[1m: expected expression, found `}`\x1b[0m\n",
        " \x1b[1m\x1b[94m--> \x1b[0msrc/lib.rs:2:26\n",
        "  \x1b[1m\x1b[94m|\x1b[0m\n",
        "\x1b[1m\x1b[94m2\x1b[0m \x1b[1m\x1b[94m|\x1b[0m pub fn g() -> u32 { 1 +  }\n",
        "  \x1b[1m\x1b[94m|\x1b[0m                          \x1b[1m\x1b[91m^\x1b[0m \x1b[1m\x1b[91mexpected expression\x1b[0m\n",
        "\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m\x1b[1m: unused variable: `unused`\x1b[0m\n",
        " \x1b[1m\x1b[94m--> \x1b[0msrc/lib.rs:1:25\n",
        "  \x1b[1m\x1b[94m|\x1b[0m\n",
        "\x1b[1m\x1b[94m1\x1b[0m \x1b[1m\x1b[94m|\x1b[0m pub fn f() -> u32 { let unused = 3; 1 }\n",
        "  \x1b[1m\x1b[94m|\x1b[0m                         \x1b[1m\x1b[33m^^^^^^\x1b[0m \x1b[1m\x1b[33mhelp: if this is intentional, prefix it with an underscore: `_unused`\x1b[0m\n",
        "  \x1b[1m\x1b[94m|\x1b[0m\n",
        "  \x1b[1m\x1b[94m= \x1b[0m\x1b[1mnote\x1b[0m: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default\n",
        "\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m: `syntax` (lib) generated 1 warning\n",
        "\x1b[1m\x1b[91merror\x1b[0m: could not compile `syntax` (lib) due to 1 previous error; 1 warning emitted\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m: build failed, waiting for other jobs to finish...\n",
        "\x1b[1m\x1b[33mwarning\x1b[0m: `syntax` (lib test) generated 1 warning (1 duplicate)\n",
        "\x1b[1m\x1b[91merror\x1b[0m: could not compile `syntax` (lib test) due to 1 previous error; 1 warning emitted\n",
        "\x1b[31;1merror\x1b[0m: command `\x1b[1mcargo '--color=always' test --no-run --message-format json-render-diagnostics\x1b[0m` exited with code \x1b[1m101\x1b[0m\n",
    );
    #[test]
    fn warning_then_failing_test_is_not_a_build_failure() {
        let runs = [
            (
                WARN_FAIL_LIBTEST_PLAIN_STDOUT,
                WARN_FAIL_LIBTEST_PLAIN_STDERR,
            ),
            (
                WARN_FAIL_LIBTEST_COLOUR_STDOUT,
                WARN_FAIL_LIBTEST_COLOUR_STDERR,
            ),
            ("", WARN_FAIL_NEXTEST_PLAIN_STDERR),
            ("", WARN_FAIL_NEXTEST_COLOUR_STDERR),
        ];
        for (stdout, stderr) in runs {
            assert!(!looks_like_build_failure(stdout, stderr), "{stderr}");
        }
    }

    #[test]
    fn warning_then_cargo_error_without_tests_is_not_a_build_failure() {
        // Stdout can be lost or empty, so the span rule itself must reject
        // a warning's span followed by an unrelated cargo error.
        assert!(!looks_like_build_failure(
            "",
            WARN_FAIL_LIBTEST_PLAIN_STDERR
        ));
        assert!(!looks_like_build_failure(
            "",
            WARN_FAIL_LIBTEST_COLOUR_STDERR
        ));
    }

    #[test]
    fn real_compile_errors_are_build_failures() {
        let outputs = [
            CODED_ERROR_LIBTEST_COLOUR_STDERR,
            CODED_ERROR_NEXTEST_PLAIN_STDERR,
            SYNTAX_ERROR_LIBTEST_PLAIN_STDERR,
            SYNTAX_ERROR_NEXTEST_COLOUR_STDERR,
        ];
        for output in outputs {
            assert!(looks_like_build_failure("", output), "missed: {output}");
        }
    }

    #[test]
    fn each_compile_error_shape_alone_is_a_build_failure() {
        let coded = "error[E0425]: cannot find value `missing` in this scope\n";
        assert!(looks_like_build_failure("", coded));
        let summary = "error: could not compile `code` (lib) due to 1 previous error\n";
        assert!(looks_like_build_failure("", summary));
        let spanned = "error: expected expression, found `}`\n\n --> src/lib.rs:2:26\n  |\n";
        assert!(looks_like_build_failure("", spanned));
    }

    #[test]
    fn compile_error_shapes_need_their_own_line() {
        let span_elsewhere = "error: expected expression\nnote: unrelated\n --> src/lib.rs:2:26\n";
        assert!(!looks_like_build_failure("", span_elsewhere));
        let indented = "    error[E0425]: cannot find value\n    error: could not compile `x`\n";
        assert!(!looks_like_build_failure("", indented));
    }

    #[test]
    fn compile_error_printed_by_a_running_test_is_not_a_build_failure() {
        let stdout = "running 1 test\ntest ui ... FAILED\n\n---- ui stdout ----\nerror[E0425]: cannot find value `x`\n --> tests/ui/x.rs:1:1\nerror: could not compile `ui`\n";
        assert!(!looks_like_build_failure(stdout, ""));
    }

    #[test]
    fn malformed_control_sequence_stops_at_a_newline() {
        assert_eq!(strip_ansi_escapes("a\x1b[\nerror: b"), "a\nerror: b");
        assert_eq!(strip_ansi_escapes("a\x1b[31\u{7}b"), "a\u{7}b");
        let guard =
            "    Starting 1 test across 1 binary\n    error: failed to download the picker page\n";
        assert!(!looks_like_build_failure("", guard));
        let stray = format!("warning: build note\x1b[\n{guard}");
        assert!(!looks_like_build_failure("", &stray));
    }

    #[test]
    fn character_set_designators_are_removed() {
        assert_eq!(strip_ansi_escapes("\x1b(Bx\x1b)0y"), "xy");
        let guard = "\x1b(B    Starting 1 test across 1 binary\n    error: failed to download the picker page\n";
        assert!(!looks_like_build_failure("", guard));
    }

    #[test]
    fn escape_sequences_are_removed() {
        assert_eq!(
            strip_ansi_escapes(
                "\x1b[1m\x1b[91merror\x1b[0m: x\x1b]8;;https://a\x07link\x1b]8;;\x1b\\ y\x1bc!\x1b["
            ),
            "error: xlink y!"
        );
    }

    #[test]
    fn bare_phrase_outside_an_error_line_is_not_a_build_failure() {
        assert!(!looks_like_build_failure(
            "",
            "failed to download the picker ids\n"
        ));
        assert!(!looks_like_build_failure(
            "",
            "  failed to download the picker ids\n"
        ));
    }

    #[test]
    fn flush_left_line_after_caused_by_is_not_a_build_failure() {
        assert!(!looks_like_build_failure(
            "",
            "Caused by:\nfailed to download x\n"
        ));
    }

    #[test]
    fn phrase_inside_an_error_line_is_not_a_build_failure() {
        assert!(!looks_like_build_failure(
            "",
            "error: test harness saw no matching package named x\n"
        ));
    }

    #[test]
    fn cause_chain_ends_at_a_flush_left_line() {
        let stderr = "error: something else\n\nCaused by:\n  unrelated\nsummary line\n  failed to download x\n";
        assert!(!looks_like_build_failure("", stderr));
        let stderr = "Caused by:\n  unrelated\nerror: another thing\n  failed to download x\n";
        assert!(!looks_like_build_failure("", stderr));
    }

    #[test]
    fn each_chain_phrase_alone_is_a_build_failure() {
        assert!(looks_like_build_failure(
            "",
            "error: failed to get `nope` as a dependency of package `consumer v0.1.0`\n"
        ));
        assert!(looks_like_build_failure(
            "",
            "error: unable to resolve the workspace\n\nCaused by:\n  failed to load source for dependency `nope`\n"
        ));
    }

    #[test]
    fn offline_resolution_failure_on_the_red_stage_is_invalid_red() {
        let check = regression_check();
        let stderr = OFFLINE_RESOLUTION_STDERR.to_string();
        let run = TestRun {
            command: "cargo nextest run".to_string(),
            parser: ParserKind::NextestJunit,
            exit_code: Some(101),
            signal: None,
            timed_out: false,
            build_failed: looks_like_build_failure("", &stderr),
            cases: vec![],
            stdout: String::new(),
            stderr,
        };
        assert!(run.build_failed);
        let verdict = evaluate(&check, true, &run, &[], &scenario_map());
        let StageVerdict::InvalidRed(reason) = &verdict else {
            panic!("expected an invalid red, got {verdict:?}");
        };
        assert!(reason.contains("did not build"), "reason: {reason}");
        assert!(!reason.contains("did not run"), "reason: {reason}");
        assert!(!stage_met(&verdict, true));
        let green = evaluate(&check, false, &run, &[], &scenario_map());
        assert!(!stage_met(&green, false));
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
