//! `pg_log_error` and friends: `src/common/logging.c`, as psql configures it.
//!
//! psql installs a locus callback that names the input file and line
//! (`startup.c:99`) and switches `PG_LOG_FLAG_TERSE` on and off as it moves
//! between `-c`, `-f` and piped input (`startup.c:384`, `:397`, `:462`,
//! `command.c:4970`). Terse output drops the `psql: ` and `error: ` labels,
//! which is why a piped script's errors read `\endif: no matching \if` while
//! the same script under `-f` reads `psql:x.sql:3: error: \endif: …`.
//!
//! [`render`] is that decision as a pure function; [`log`] is the one-line
//! action that writes it.

use std::io::Write;

use crate::settings::PsqlSettings;

/// `enum pg_log_level` plus `enum pg_log_part` (`logging.h:16`-`:80`), for
/// the calls psql makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `pg_log_error`
    Error,
    /// `pg_log_warning`
    Warning,
    /// `pg_log_info`: no label, even when not terse.
    Info,
    /// `pg_log_error_hint`
    Hint,
}

/// `pg_log_generic_v()` (`logging.c:219`): the bytes one call writes.
///
/// `locus` is what psql's `log_locus_callback` returns: the input file and
/// line, or `None` when there is no input file. One trailing newline of
/// `message` is stripped before the call's own is added, "for
/// PQerrorMessage()" (`logging.c:330`).
#[must_use]
pub fn render(level: Level, terse: bool, locus: Option<(&str, u64)>, message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 32);
    // `logging.c:252`-`:265`.
    if !terse || locus.is_some() {
        if !terse {
            out.extend_from_slice(b"psql:");
        }
        if let Some((filename, lineno)) = locus {
            out.extend_from_slice(filename.as_bytes());
            out.push(b':');
            if lineno > 0 {
                out.extend_from_slice(lineno.to_string().as_bytes());
                out.push(b':');
            }
        }
        out.push(b' ');
    }
    // `logging.c:269`-`:305`.
    if !terse {
        out.extend_from_slice(match level {
            Level::Error => b"error: ".as_slice(),
            Level::Warning => b"warning: ",
            Level::Hint => b"hint: ",
            Level::Info => b"",
        });
    }
    out.extend_from_slice(message.strip_suffix(b"\n").unwrap_or(message));
    out.push(b'\n');
    out
}

/// Log `message` at `level` the way psql's current configuration would.
pub fn log(stderr: &mut dyn Write, pset: &PsqlSettings, level: Level, message: impl AsRef<[u8]>) {
    let locus = pset.inputfile.as_deref().map(|f| (f, pset.lineno));
    let _ = stderr.write_all(&render(level, pset.log_terse, locus, message.as_ref()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terse_without_a_file_is_the_message_alone() {
        // What pg_regress sees: psql reading a pipe (`startup.c:462`).
        assert_eq!(
            render(Level::Error, true, None, b"\\endif: no matching \\if"),
            b"\\endif: no matching \\if\n"
        );
        assert_eq!(
            render(Level::Warning, true, None, b"w"),
            b"w\n",
            "no label either"
        );
    }

    #[test]
    fn an_input_file_prefixes_its_name_and_line() {
        assert_eq!(
            render(Level::Error, false, Some(("x.sql", 3)), b"boom"),
            b"psql:x.sql:3: error: boom\n"
        );
        // Line 0 is left out (`logging.c:261`).
        assert_eq!(
            render(Level::Warning, false, Some(("<stdin>", 0)), b"w"),
            b"psql:<stdin>: warning: w\n"
        );
    }

    #[test]
    fn a_file_is_named_even_when_terse() {
        assert_eq!(
            render(Level::Error, true, Some(("x.sql", 3)), b"boom"),
            b"x.sql:3: boom\n"
        );
    }

    #[test]
    fn not_terse_and_no_file_is_the_program_name() {
        assert_eq!(
            render(Level::Error, false, None, b"boom"),
            b"psql: error: boom\n"
        );
        assert_eq!(render(Level::Hint, false, None, b"h"), b"psql: hint: h\n");
    }

    #[test]
    fn info_has_no_label_and_one_newline_is_stripped() {
        // `pg_log_info("%s", PQerrorMessage(...))`: the server's text already
        // ends in a newline, and exactly one is kept.
        assert_eq!(
            render(Level::Info, false, Some(("x.sql", 2)), b"ERROR:  oops\n"),
            b"psql:x.sql:2: ERROR:  oops\n"
        );
        assert_eq!(render(Level::Info, true, None, b"a\n\n"), b"a\n\n");
    }
}
