//! The `pgdrop` executable: route `argv`, then hand the streams to the applet.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use pgdrop::dispatch::{self, Applet, Dispatch};
use pgdrop::{install, postgres};

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    match dispatch::dispatch(&argv) {
        Dispatch::Applet(Applet::Initdb, applet_args) => {
            rinitdb::run(&applet_args, &mut stdout, &mut stderr)
        }
        Dispatch::Applet(Applet::Psql, applet_args) => {
            rpsql::run(&applet_args, &mut stdout, &mut stderr)
        }
        Dispatch::Applet(Applet::Postgres, applet_args) => {
            postgres::run(&applet_args, &mut stdout, &mut stderr)
        }
        Dispatch::InstallLinks(link_args) => install::run(&link_args, &mut stdout, &mut stderr),
        Dispatch::Start(_) => {
            let _ = writeln!(
                stderr,
                "pgdrop: error: `start` is not implemented yet (Linear NAT-409)"
            );
            ExitCode::FAILURE
        }
        Dispatch::PrintHelp(text) => {
            let _ = stdout.write_all(text.as_bytes());
            ExitCode::SUCCESS
        }
        Dispatch::PrintVersion => {
            let _ = writeln!(stdout, "pgdrop {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Dispatch::Unparsable(text) => {
            let _ = stderr.write_all(text.as_bytes());
            ExitCode::from(2)
        }
    }
}
