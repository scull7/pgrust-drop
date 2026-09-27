//! psql's diagnostics: `src/common/logging.c` as `psql` configures it.
//!
//! Every `pg_log_error`, `pg_log_warning` and `pg_log_info` in psql goes
//! through `pg_log_generic_v` (`logging.c:219`), which prefixes the message
//! according to two pieces of state: the `PG_LOG_FLAG_TERSE` flag that
//! `pg_logging_config` sets, and the locus that psql's `log_locus_callback`
//! (`startup.c:99`) reports — the file being read and the line within it.
//! So one message reads `psql: error: …` at startup, `…` alone under `-c`,
//! and `psql:<stdin>:2: error: …` under `-f -`.
//!
//! [`render`] is that prefixing as a pure function. The state lives on
//! [`PsqlSettings`] ([`PsqlSettings::log_terse`], [`PsqlSettings::inputfile`],
//! [`PsqlSettings::lineno`]), and [`log`] reads it the way the callback does.
//!
//! `PG_COLOR`'s SGR escapes (`logging.c:254`) are not emitted: no color is
//! the default, and nothing in this port sets `PG_COLOR`.

use std::io::Write;

use crate::settings::PsqlSettings;

/// `enum pg_log_level` (`logging.h:16`), the three levels psql logs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `PG_LOG_INFO`: no label. psql reports server errors at this level,
    /// because the server's text already says `ERROR:` (`common.c:457`).
    Info,
    /// `PG_LOG_WARNING`: `warning: `.
    Warning,
    /// `PG_LOG_ERROR`: `error: `.
    Error,
}

/// Calculation: `pg_log_generic_v` (`logging.c:219`) for `PG_LOG_PRIMARY`,
/// the bytes one message puts on stderr.
///
/// `locus` is what `log_locus_callback` reports: `Some((filename, lineno))`
/// while a file is being read, where a `lineno` of 0 prints no line
/// (`logging.c:261`). One trailing newline is stripped from `message` and
/// one is written, so libpq's newline-terminated `PQerrorMessage` text is
/// not doubled (`logging.c:330`).
#[must_use]
pub fn render(
    progname: &str,
    terse: bool,
    locus: Option<(&str, u64)>,
    level: Level,
    message: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 32);
    // `logging.c:252`-`:267`.
    if !terse || locus.is_some() {
        if !terse {
            out.extend_from_slice(progname.as_bytes());
            out.push(b':');
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
    // `logging.c:269`-`:309`.
    if !terse {
        match level {
            Level::Error => out.extend_from_slice(b"error: "),
            Level::Warning => out.extend_from_slice(b"warning: "),
            Level::Info => {}
        }
    }
    out.extend_from_slice(message.strip_suffix(b"\n").unwrap_or(message));
    out.push(b'\n');
    out
}

/// Action: log `message` at `level` with the prefix `pset`'s logging state
/// calls for, the way `pg_log_error("%s", …)` and its siblings do.
pub fn log(pset: &PsqlSettings, level: Level, message: &[u8], stderr: &mut dyn Write) {
    // `log_locus_callback()` (`startup.c:99`): the locus exists only while a
    // file is being read.
    let locus = pset.inputfile.as_deref().map(|f| (f, pset.lineno));
    let _ = stderr.write_all(&render(
        &pset.progname,
        pset.log_terse,
        locus,
        level,
        message,
    ));
}

/// `pg_log_error(…)`.
pub fn error(pset: &PsqlSettings, message: impl AsRef<[u8]>, stderr: &mut dyn Write) {
    log(pset, Level::Error, message.as_ref(), stderr);
}

/// `pg_log_warning(…)`.
pub fn warning(pset: &PsqlSettings, message: impl AsRef<[u8]>, stderr: &mut dyn Write) {
    log(pset, Level::Warning, message.as_ref(), stderr);
}

/// `pg_log_info(…)`.
pub fn info(pset: &PsqlSettings, message: impl AsRef<[u8]>, stderr: &mut dyn Write) {
    log(pset, Level::Info, message.as_ref(), stderr);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(terse: bool, locus: Option<(&str, u64)>, level: Level, message: &str) -> String {
        String::from_utf8(render("psql", terse, locus, level, message.as_bytes())).unwrap()
    }

    #[test]
    fn before_any_configuration_every_message_carries_the_program_name() {
        // `log_flags` is 0 until `pg_logging_config` runs: the connection
        // failures at startup read `psql: error: …`.
        assert_eq!(
            text(false, None, Level::Error, "boom"),
            "psql: error: boom\n"
        );
        assert_eq!(
            text(false, None, Level::Warning, "hm"),
            "psql: warning: hm\n"
        );
        assert_eq!(text(false, None, Level::Info, "note"), "psql: note\n");
    }

    #[test]
    fn terse_without_a_file_is_the_bare_message() {
        // `-c` and stdin without `-f` (`startup.c:384`, `command.c:4970`):
        // what `psql.out` shows, e.g. `invalid command \lo`.
        assert_eq!(
            text(true, None, Level::Error, "invalid command \\lo"),
            "invalid command \\lo\n"
        );
        assert_eq!(
            text(true, None, Level::Info, "ERROR:  boom\n"),
            "ERROR:  boom\n"
        );
    }

    #[test]
    fn a_file_adds_its_name_and_line() {
        // `001_basic.pl:175`-`:178`: `psql:<stdin>:1: ERROR:  …` then
        // `psql:<stdin>:2: error: ERROR:  …`.
        assert_eq!(
            text(false, Some(("<stdin>", 1)), Level::Info, "ERROR:  x\n"),
            "psql:<stdin>:1: ERROR:  x\n"
        );
        assert_eq!(
            text(false, Some(("<stdin>", 2)), Level::Error, "ERROR:  x\n"),
            "psql:<stdin>:2: error: ERROR:  x\n"
        );
    }

    #[test]
    fn line_zero_prints_no_line() {
        // `logging.c:261`.
        assert_eq!(
            text(false, Some(("a.sql", 0)), Level::Error, "x"),
            "psql:a.sql: error: x\n"
        );
    }

    #[test]
    fn terse_with_a_file_keeps_the_locus_and_drops_the_rest() {
        // Unreachable from psql's own calls, which clear the flag whenever a
        // file is set, but it is what `logging.c:252`-`:267` does.
        assert_eq!(text(true, Some(("f", 3)), Level::Error, "x"), "f:3: x\n");
    }

    #[test]
    fn only_one_trailing_newline_is_stripped() {
        // `logging.c:330`: libpq's messages end in `\n`, and a message that
        // is several lines keeps its inner ones.
        assert_eq!(text(true, None, Level::Info, "a\nb\n\n"), "a\nb\n\n");
        assert_eq!(text(true, None, Level::Info, "a\nb"), "a\nb\n");
    }

    #[test]
    fn a_message_keeps_its_bytes() {
        let out = render("psql", true, None, Level::Info, b"\xff\xfe\n");
        assert_eq!(out, b"\xff\xfe\n");
    }

    #[test]
    fn log_reads_the_locus_from_the_settings() {
        let pset = PsqlSettings {
            inputfile: Some("<stdin>".into()),
            lineno: 7,
            ..PsqlSettings::default()
        };
        let mut err = Vec::new();
        error(&pset, "x", &mut err);
        assert_eq!(err, b"psql:<stdin>:7: error: x\n");
    }
}
