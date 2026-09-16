//! Standalone `rpsql` binary; the multicall `pgdrop` links the library instead.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    rpsql::run(
        &args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}
