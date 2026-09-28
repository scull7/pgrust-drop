//! The `pgdrop` executable: route `argv`, then hand the streams to the applet.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::process::ExitCode;

use pgdrop::dispatch::{self, Applet, Dispatch};
use pgdrop::{install, launch, postgres, stop};

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    match dispatch::dispatch(&argv) {
        Dispatch::Applet(Applet::Initdb, applet_args) => {
            let argv0 = argv
                .first()
                .map_or(OsStr::new("initdb"), OsString::as_os_str);
            rinitdb::run(argv0, &applet_args, &mut stdout, &mut stderr)
        }
        Dispatch::Applet(Applet::Psql, applet_args) => {
            rpsql::run(&applet_args, &mut stdout, &mut stderr)
        }
        Dispatch::Applet(Applet::Postgres, applet_args) => {
            // Unlocked: the server writes to both streams itself.
            drop((stdout, stderr));
            let argv0 = argv
                .first()
                .map_or(OsStr::new("postgres"), OsString::as_os_str);
            postgres::run(argv0, &applet_args, &mut std::io::stdout())
        }
        Dispatch::InstallLinks(link_args) => install::run(&link_args, &mut stdout, &mut stderr),
        Dispatch::Start(start_args) => launch::run(&start_args, &mut stdout, &mut stderr),
        Dispatch::Stop(stop_args) => stop::run(&stop_args, &mut stderr),
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
