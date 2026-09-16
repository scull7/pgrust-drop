//! `src/interfaces/libpq/test/libpq_uri_regress.c`, the helper `t/001_uri.pl`
//! drives: parse one conninfo string, print the options that differ from the
//! defaults, and say whether the result is `(local)` or `(inet)`.

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;

use rlibpq::{Env, conndefaults, parse_conninfo, regress_report};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.len() != 2 {
        return ExitCode::from(1);
    }

    // argv[1] is a byte string to C; it is one here too, so a percent-encoded
    // token that decodes to something outside UTF-8 still reaches the parser.
    let conninfo = args[1].as_encoded_bytes();

    let parsed = match parse_conninfo(conninfo) {
        Ok(parsed) => parsed,
        Err(error) => {
            // libpq_uri_regress.c:36 prints the error buffer with no newline of
            // its own, because libpq_append_error already appended one
            // (fe-misc.c:1539).
            let mut message = b"libpq_uri_regress: ".to_vec();
            message.extend_from_slice(&error.message());
            message.push(b'\n');
            let _ = std::io::stderr().lock().write_all(&message);
            return ExitCode::from(1);
        }
    };

    let defaults = conndefaults(&Env::from_process());
    let _ = std::io::stdout()
        .lock()
        .write_all(&regress_report(&parsed, &defaults));
    ExitCode::SUCCESS
}
