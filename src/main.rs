//! Entry point for the sethu binary.
//!
//! Parsing and dispatch live in the library. This wrapper forwards the
//! exit code that the library run returns.

use std::process::ExitCode;

fn main() -> ExitCode {
    sethu::run()
}
