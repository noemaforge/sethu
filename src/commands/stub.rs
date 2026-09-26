//! Implementation of `sethu stub`.

use std::process::ExitCode;

use anyhow::Context;

use crate::cli::StubArgs;
use crate::commands::Status;
use crate::stub::{self, server::StubServer};

/// Build-time status of this command.
pub const STATUS: Status = Status::Available;

/// Entry point for `sethu stub`.
///
/// Every fixture is validated before the server binds, so an invalid
/// fixture never reaches a listening socket. The readiness line goes
/// to stdout and every other message goes to stderr. The server runs
/// until the process ends.
pub fn run(args: &StubArgs) -> anyhow::Result<ExitCode> {
    let Some(dir) = &args.scenarios else {
        anyhow::bail!("no scenario directory given (pass --scenarios DIR)");
    };
    let (bytes, label) = stub::embedded_spec(&args.version);
    let spec: serde_json::Value =
        serde_json::from_slice(bytes).with_context(|| "parse embedded contract")?;
    let spec_sha = stub::body_sha256_hex(bytes);
    let scenarios = stub::load_scenarios(dir, &spec, &spec_sha)?;
    for scenario in &scenarios {
        if scenario.supports_claim {
            continue;
        }
        eprintln!(
            "warning: scenario {:?} cannot back a verification claim ({} unsupported construct{})",
            scenario.id,
            scenario.unsupported.len(),
            if scenario.unsupported.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        for issue in &scenario.unsupported {
            eprintln!("  {}: {:?}", issue.location, issue.kind);
        }
    }
    let workdir =
        std::env::current_dir().with_context(|| "read working directory for stub trace")?;
    let trace_path = StubServer::default_trace_path(&workdir);
    let server = StubServer::start(args.port, scenarios, spec, label, trace_path)?;
    println!("{}", server.readiness_line());
    use std::io::Write;
    std::io::stdout()
        .flush()
        .with_context(|| "flush stub readiness line")?;
    server.serve_forever()?;
    Ok(ExitCode::SUCCESS)
}
