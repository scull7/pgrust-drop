//! Standalone `rinitdb` binary; the multicall `pgdrop` links the library instead.

use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut command_line = std::env::args_os();
    let argv0 = command_line
        .next()
        .unwrap_or_else(|| OsString::from("initdb"));
    let args: Vec<_> = command_line.collect();
    rinitdb::run(
        &argv0,
        &args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}
