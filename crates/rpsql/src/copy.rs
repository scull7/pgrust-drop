//! `\copy` and the COPY data transfer: `src/bin/psql/copy.c`.
//!
//! [`parse_slash_copy`] (`copy.c:89`) is a pure calculation over the
//! command's whole line, on top of ports of `stringutils.c`'s `strtokx` and
//! `strip_quotes`. [`do_copy`] (`copy.c:268`) is the action: it opens the
//! file, builds a `COPY … FROM STDIN` / `COPY … TO STDOUT`, and runs it
//! through `SendQuery` with the file as `pset.copyStream`.
//! [`handle_copy_out`] and [`handle_copy_in`] (`copy.c:434`, `:513`) move the
//! data; `ExecQueryAndProcessResults` calls them for every COPY, `\copy` or
//! not ([`crate::common`]).
//!
//! `PROGRAM` is parsed but not run: it opens a shell with `popen`, which
//! lands with `\g |` and `\o |`, the other two commands that pipe to one.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, IsTerminal as _, Write};
use std::os::unix::ffi::OsStrExt;

use rlibpq::{ExecStatus, QueryResult};

use crate::common::{CommandSource, CopyIo, CopyStream, Executor, send_query_with};
use crate::logging;
use crate::settings::PsqlSettings;
use crate::variables::VariableSpace;

/// `COPYBUFSIZ` (`copy.c:510`): the read chunk for COPY IN.
const COPYBUFSIZ: usize = 8192;

/// What `%m` expands to for `err`: `strerror(errno)` without the
/// ` (os error N)` that `std::io::Error`'s `Display` appends. The same
/// function as `rinitdb::strerror`, which an MIT crate may not reach through
/// another tool's crate.
#[must_use]
pub fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match err.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .unwrap_or(&text)
            .to_owned(),
        None => text,
    }
}

/// `strtokx()` (`stringutils.c:52`) over one string: the C function keeps
/// its position in statics, this keeps it in a value.
///
/// C terminates each token by writing a NUL just past it, and when the byte
/// there was one of the call's whitespace characters it is overwritten, so
/// the next call starts one byte later ([`Tokens::advance`]). That matters
/// only when the whitespace set changes between calls, which `\copy`'s last
/// call does: the one whitespace byte after the file name is eaten, and the
/// COPY options are the rest of the line after it.
///
/// Bytes are stepped one at a time where upstream steps by
/// `PQmblenBounded` over the client encoding. The two agree for UTF-8, in
/// which no byte of a multibyte character is ASCII; see
/// `docs/divergences.md` for the client-encoding row.
struct Tokens<'a> {
    s: &'a [u8],
    pos: usize,
}

/// One `strtokx` call's character classes.
struct Classes<'a> {
    whitespace: &'a [u8],
    delim: &'a [u8],
    quote: &'a [u8],
    escape: Option<u8>,
    e_strings: bool,
}

/// `strtokx(…, " \t\n\r", ".,()", "\"", 0, false, false, …)`: an identifier,
/// possibly double-quoted, or one of `.,()`.
const IDENT: Classes<'static> = Classes {
    whitespace: WHITESPACE,
    delim: b".,()",
    quote: b"\"",
    escape: None,
    e_strings: false,
};

/// `parse_slash_copy`'s whitespace (`copy.c:93`).
const WHITESPACE: &[u8] = b" \t\n\r";

impl<'a> Tokens<'a> {
    fn new(s: &'a [u8]) -> Self {
        Self { s, pos: 0 }
    }

    /// Move past a token that ends at `p`, eating the byte there if it is
    /// whitespace (`stringutils.c:112`-`:124`).
    fn advance(&mut self, p: usize, whitespace: &[u8]) {
        self.pos = if p < self.s.len() && whitespace.contains(&self.s[p]) {
            p + 1
        } else {
            p
        };
    }

    /// The next token, or `None` at the end of the string. `del_quotes` is
    /// always false in `copy.c`, so it is not a parameter.
    fn next(&mut self, c: &Classes<'_>) -> Option<Vec<u8>> {
        let s = self.s;
        let start = self.pos
            + s[self.pos..]
                .iter()
                .take_while(|b| c.whitespace.contains(b))
                .count();
        if start >= s.len() {
            self.pos = s.len();
            return None;
        }

        // A delimiter is a token of its own (`stringutils.c:103`).
        if c.delim.contains(&s[start]) {
            self.advance(start + 1, c.whitespace);
            return Some(vec![s[start]]);
        }

        // E'…' switches to single quotes with a backslash escape (`:131`).
        let mut p = start;
        let mut quote = c.quote;
        let mut escape = c.escape;
        if c.e_strings && matches!(s[p], b'E' | b'e') && s.get(p + 1) == Some(&b'\'') {
            quote = b"'";
            escape = Some(b'\\');
            p += 1;
        }

        // A quoted token runs to its closing quote (`:141`-`:181`).
        if quote.contains(&s[p]) {
            let thisquote = s[p];
            p += 1;
            while p < s.len() {
                let ch = s[p];
                // An escaped byte, or a doubled quote, is data.
                if (Some(ch) == escape && p + 1 < s.len())
                    || (ch == thisquote && s.get(p + 1) == Some(&thisquote))
                {
                    p += 2;
                } else if ch == thisquote {
                    p += 1;
                    break;
                } else {
                    p += 1;
                }
            }
            let token = s[start..p].to_vec();
            self.advance(p, c.whitespace);
            return Some(token);
        }

        // Otherwise up to the next whitespace, delimiter or quote (`:188`).
        let end = s[start..]
            .iter()
            .position(|b| c.whitespace.contains(b) || c.delim.contains(b) || quote.contains(b))
            .map_or(s.len(), |n| start + n);
        let token = s[start..end].to_vec();
        self.advance(end, c.whitespace);
        Some(token)
    }

    /// `strtokx(NULL, "", NULL, NULL, …)`: the rest of the string.
    fn rest(&mut self) -> Option<Vec<u8>> {
        let rest = &self.s[self.pos..];
        self.pos = self.s.len();
        (!rest.is_empty()).then(|| rest.to_vec())
    }
}

/// `strip_quotes()` (`stringutils.c:240`): drop a leading and a trailing
/// `quote`, undouble embedded ones, and let `escape` protect the byte after
/// it.
#[must_use]
pub fn strip_quotes(source: &[u8], quote: u8, escape: Option<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(source.len());
    let mut src = usize::from(source.first() == Some(&quote));
    while src < source.len() {
        let c = source[src];
        if c == quote && src + 1 == source.len() {
            break;
        } else if (c == quote && source.get(src + 1) == Some(&quote))
            || (Some(c) == escape && src + 1 < source.len())
        {
            src += 1;
        }
        out.push(source[src]);
        src += 1;
    }
    out
}

/// `expand_tilde()` (`common.c:2697`): `~` and `~/…` become `home` and
/// `home/…`. `~user` is left alone, as upstream leaves it when `getpwnam`
/// knows no such user: looking the user up is not reachable from the
/// standard library.
#[must_use]
pub fn expand_tilde(filename: &[u8], home: Option<&[u8]>) -> Vec<u8> {
    if filename.first() != Some(&b'~') {
        return filename.to_vec();
    }
    let user_end = filename
        .iter()
        .position(|&b| b == b'/')
        .unwrap_or(filename.len());
    match home {
        Some(home) if user_end == 1 && !home.is_empty() => {
            let mut out = home.to_vec();
            out.extend_from_slice(&filename[user_end..]);
            out
        }
        _ => filename.to_vec(),
    }
}

/// `{ 'filename' | PROGRAM 'command' | STDIN | STDOUT | PSTDIN | PSTDOUT }`
/// (`copy.c:196`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyFile {
    /// `stdin` / `stdout`: the command source and `pset.queryFout`
    /// (`file == NULL`, `psql_inout == false`).
    Stdio,
    /// `pstdin` / `pstdout`: psql's own stdin and stdout
    /// (`psql_inout == true`).
    PsqlStdio,
    /// A file name, quotes stripped and `~` expanded.
    Path(Vec<u8>),
    /// `PROGRAM 'command'`, quotes stripped.
    Program(Vec<u8>),
}

/// `struct copy_options` (`copy.c:53`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyOptions {
    /// `before_tofrom`: the COPY string before `FROM`/`TO`.
    pub before_tofrom: Vec<u8>,
    /// `after_tofrom`: the COPY string after the file name.
    pub after_tofrom: Option<Vec<u8>>,
    /// Where the data comes from or goes to.
    pub file: CopyFile,
    /// `from`: `FROM` rather than `TO`.
    pub from: bool,
}

impl CopyOptions {
    /// The command `do_copy` sends (`copy.c:358`-`:366`).
    #[must_use]
    pub fn query(&self) -> Vec<u8> {
        let mut query = b"COPY ".to_vec();
        query.extend_from_slice(&self.before_tofrom);
        query.extend_from_slice(if self.from {
            b" FROM STDIN "
        } else {
            b" TO STDOUT "
        });
        if let Some(after) = &self.after_tofrom {
            query.extend_from_slice(after);
        }
        query
    }
}

/// `parse_slash_copy()` (`copy.c:89`): `\copy`'s whole line, as
/// `OT_WHOLE_LINE` hands it over, into its parts.
///
/// `std_strings` is `standard_strings()`, which decides whether a backslash
/// escapes a quote inside `\copy (query)` (`copy.c:94`); `home` is what `~`
/// expands to.
///
/// # Errors
/// The message upstream logs with `pg_log_error`, as bytes, since it quotes
/// the offending token.
// One statement per statement of upstream's, in its order; split up, the
// correspondence with `copy.c:89`-`:259` would be lost for no branch saved.
#[allow(clippy::too_many_lines)]
pub fn parse_slash_copy(
    args: Option<&[u8]>,
    std_strings: bool,
    home: Option<&[u8]>,
) -> Result<CopyOptions, Vec<u8>> {
    let Some(args) = args else {
        return Err(b"\\copy: arguments required".to_vec());
    };
    let parse_error = |token: Option<&[u8]>| -> Vec<u8> {
        match token {
            Some(token) => {
                let mut message = b"\\copy: parse error at \"".to_vec();
                message.extend_from_slice(token);
                message.push(b'"');
                message
            }
            None => b"\\copy: parse error at end of line".to_vec(),
        }
    };
    let nonstd_backslash = (!std_strings).then_some(b'\\');
    let mut tokens = Tokens::new(args);
    let mut before = Vec::new();
    let next = |tokens: &mut Tokens<'_>, classes: &Classes<'_>| {
        tokens.next(classes).ok_or_else(|| parse_error(None))
    };

    let mut token = next(&mut tokens, &IDENT)?;

    // The pre-7.3 `BINARY` before the table name (`copy.c:112`).
    if token.eq_ignore_ascii_case(b"binary") {
        before.extend_from_slice(&token);
        token = next(&mut tokens, &IDENT)?;
    }

    // `COPY (query)` (`copy.c:122`).
    if token[0] == b'(' {
        let query_classes = Classes {
            whitespace: WHITESPACE,
            delim: b"()",
            quote: b"\"'",
            escape: nonstd_backslash,
            e_strings: true,
        };
        let mut parens = 1;
        while parens > 0 {
            before.push(b' ');
            before.extend_from_slice(&token);
            token = next(&mut tokens, &query_classes)?;
            if token[0] == b'(' {
                parens += 1;
            } else if token[0] == b')' {
                parens -= 1;
            }
        }
    }

    before.push(b' ');
    before.extend_from_slice(&token);
    token = next(&mut tokens, &IDENT)?;

    // `schema . table` (`copy.c:152`).
    if token[0] == b'.' {
        before.extend_from_slice(&token);
        token = next(&mut tokens, &IDENT)?;
        before.extend_from_slice(&token);
        token = next(&mut tokens, &IDENT)?;
    }

    // A parenthesized column list (`copy.c:167`).
    if token[0] == b'(' {
        let columns = Classes {
            whitespace: WHITESPACE,
            delim: b"()",
            quote: b"\"",
            escape: None,
            e_strings: false,
        };
        loop {
            before.push(b' ');
            before.extend_from_slice(&token);
            token = next(&mut tokens, &columns)?;
            if token[0] == b')' {
                break;
            }
        }
        before.push(b' ');
        before.extend_from_slice(&token);
        token = next(&mut tokens, &IDENT)?;
    }

    let from = if token.eq_ignore_ascii_case(b"from") {
        true
    } else if token.eq_ignore_ascii_case(b"to") {
        false
    } else {
        return Err(parse_error(Some(&token)));
    };

    let target = Classes {
        whitespace: WHITESPACE,
        delim: b";",
        quote: b"'",
        escape: None,
        e_strings: false,
    };
    token = next(&mut tokens, &target)?;
    let file = if token.eq_ignore_ascii_case(b"program") {
        token = next(&mut tokens, &target)?;
        // "The shell command must be quoted" (`copy.c:211`).
        if token.len() < 2 || token[0] != b'\'' || token[token.len() - 1] != b'\'' {
            return Err(parse_error(Some(&token)));
        }
        CopyFile::Program(strip_quotes(&token, b'\'', None))
    } else if token.eq_ignore_ascii_case(b"stdin") || token.eq_ignore_ascii_case(b"stdout") {
        CopyFile::Stdio
    } else if token.eq_ignore_ascii_case(b"pstdin") || token.eq_ignore_ascii_case(b"pstdout") {
        CopyFile::PsqlStdio
    } else {
        CopyFile::Path(expand_tilde(&strip_quotes(&token, b'\'', None), home))
    };

    Ok(CopyOptions {
        before_tofrom: before,
        after_tofrom: tokens.rest(),
        file,
        from,
    })
}

/// `do_copy()` (`copy.c:268`): run `\copy`'s whole line.
///
/// The file is opened here and handed to `SendQuery` as `pset.copyStream`;
/// `\copy … from stdin` and `… to stdout` use the command source and stdout,
/// as upstream does by pointing `copyStream` at them.
pub fn do_copy(
    args: Option<&[u8]>,
    executor: &mut dyn Executor,
    pset: &mut PsqlSettings,
    vars: &mut VariableSpace,
    source: &mut CommandSource<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let home = std::env::var_os("HOME");
    let options = match parse_slash_copy(
        args,
        executor.standard_strings(),
        home.as_deref().map(OsStrExt::as_bytes),
    ) {
        Ok(options) => options,
        Err(message) => {
            logging::error(pset, message, stderr);
            return false;
        }
    };

    // The streams `copystream` may point at, opened in the arm that needs
    // one and borrowed by the query for as long as it runs.
    let mut stdin_lock: std::io::StdinLock<'static>;
    let mut file_reader: BufReader<File>;
    let mut file_writer: Option<BufWriter<File>> = None;
    let stream = match &options.file {
        CopyFile::Program(_) => {
            logging::error(
                pset,
                "\\copy … PROGRAM is not implemented yet (Linear NAT-403)",
                stderr,
            );
            return false;
        }
        // `copy.c:298`-`:301`, `:317`-`:320`: stdin is the command source
        // and stdout is `pset.queryFout`; pstdin is psql's own stdin, which
        // is the command source too when that is stdin, and pstdout is
        // stdout, which `pset.queryFout` is until `\o` exists.
        CopyFile::Stdio => CopyStream::Default,
        CopyFile::PsqlStdio if !options.from || source.is_stdin => CopyStream::Default,
        CopyFile::PsqlStdio => {
            let stdin = std::io::stdin();
            let is_tty = stdin.is_terminal();
            stdin_lock = stdin.lock();
            CopyStream::Read {
                reader: &mut stdin_lock,
                is_tty,
            }
        }
        CopyFile::Path(path) => {
            let Some(file) = open_copy_file(path, options.from, pset, stderr) else {
                return false;
            };
            if options.from {
                file_reader = BufReader::new(file);
                CopyStream::Read {
                    reader: &mut file_reader,
                    is_tty: false,
                }
            } else {
                CopyStream::Write(file_writer.insert(BufWriter::new(file)))
            }
        }
    };

    // `copy.c:369`: run it like a user command, with `copystream` as the
    // data's source or sink.
    let ok = {
        let mut io = CopyIo {
            source,
            stream,
            copy_from_stdin: Some(usize::from(options.from)),
        };
        send_query_with(
            executor,
            &options.query(),
            pset,
            vars,
            &mut io,
            stdout,
            stderr,
        )
    };

    // `copy.c:399`: `fclose`, whose flush can still fail.
    if let (Some(mut writer), CopyFile::Path(path)) = (file_writer, &options.file)
        && let Err(err) = writer.flush()
    {
        logging::error(pset, file_message(path, &strerror(&err)), stderr);
        return false;
    }
    ok
}

/// `pg_log_error("%s: …", options->file)`: the file name as typed, bytes and
/// all, then `rest`.
fn file_message(path: &[u8], rest: &str) -> Vec<u8> {
    let mut message = path.to_vec();
    message.extend_from_slice(b": ");
    message.extend_from_slice(rest.as_bytes());
    message
}

/// `copy.c:282`-`:355` for a file: open it for reading or writing, and
/// refuse a directory.
fn open_copy_file(
    path: &[u8],
    from: bool,
    pset: &PsqlSettings,
    stderr: &mut dyn Write,
) -> Option<File> {
    let os_path = OsStr::from_bytes(path);
    let opened = if from {
        File::open(os_path)
    } else {
        File::create(os_path)
    };
    let file = match opened {
        Ok(file) => file,
        Err(err) => {
            logging::error(pset, file_message(path, &strerror(&err)), stderr);
            return None;
        }
    };
    match file.metadata() {
        Err(err) => {
            let mut message = b"could not stat file \"".to_vec();
            message.extend_from_slice(path);
            message.extend_from_slice(b"\": ");
            message.extend_from_slice(strerror(&err).as_bytes());
            logging::error(pset, message, stderr);
            None
        }
        Ok(meta) if meta.is_dir() => {
            logging::error(
                pset,
                file_message(path, "cannot copy from/to a directory"),
                stderr,
            );
            None
        }
        Ok(_) => Some(file),
    }
}

/// `pg_log_info("%s", PQerrorMessage(conn))` after a COPY whose command did
/// not end in `PGRES_COMMAND_OK` (`copy.c:486`, `:739`).
fn log_copy_result(result: Option<&QueryResult>, pset: &PsqlSettings, stderr: &mut dyn Write) {
    let message = result.map_or_else(Vec::new, |r| crate::common::result_error_message(r, pset));
    logging::info(pset, message, stderr);
}

/// `handleCopyOut()` (`copy.c:434`): write a COPY OUT's rows to `copystream`
/// (or drop them, for `None`), then collect the COPY command's result.
pub fn handle_copy_out(
    executor: &mut dyn Executor,
    mut copystream: Option<&mut dyn Write>,
    pset: &PsqlSettings,
    stderr: &mut dyn Write,
) -> (bool, Option<QueryResult>) {
    let mut ok = true;
    let mut transfer_failed = None;
    loop {
        match executor.get_copy_data() {
            Ok(Some(buf)) => {
                if ok
                    && let Some(stream) = copystream.as_deref_mut()
                    && let Err(err) = stream.write_all(&buf)
                {
                    logging::error(
                        pset,
                        format!("could not write COPY data: {}", strerror(&err)),
                        stderr,
                    );
                    // Complain only once, and keep reading the server's data.
                    ok = false;
                }
            }
            Ok(None) => break,
            Err(err) => {
                transfer_failed = Some(err);
                break;
            }
        }
    }

    if ok
        && let Some(stream) = copystream
        && let Err(err) = stream.flush()
    {
        logging::error(
            pset,
            format!("could not write COPY data: {}", strerror(&err)),
            stderr,
        );
        ok = false;
    }

    // `copy.c:465`: `PQgetCopyData`'s -2.
    if let Some(err) = transfer_failed {
        let mut message = b"COPY data transfer failed: ".to_vec();
        message.extend_from_slice(err.as_bytes());
        logging::error(pset, message, stderr);
        ok = false;
    }

    finish_copy(executor, ok, pset, stderr)
}

/// The COPY command's result, `copy.c:483`-`:488` and `:737`-`:741`.
fn finish_copy(
    executor: &mut dyn Executor,
    mut ok: bool,
    pset: &PsqlSettings,
    stderr: &mut dyn Write,
) -> (bool, Option<QueryResult>) {
    match executor.get_result() {
        Ok(result) => {
            if result.as_ref().map(QueryResult::status) != Some(ExecStatus::CommandOk) {
                log_copy_result(result.as_ref(), pset, stderr);
                ok = false;
            }
            (ok, result)
        }
        Err(err) => {
            logging::info(pset, err.as_bytes(), stderr);
            (false, None)
        }
    }
}

/// `handleCopyIn()` (`copy.c:513`): send `copystream`'s data to a COPY IN,
/// or with no `executor` read and drop what a refused `COPY … FROM STDIN`
/// would have taken, so that it is not run as commands.
///
/// `is_cmd_source` is `copystream == pset.cur_cmd_source`: only then does a
/// `\.` line end the data (`copy.c:634`) and each line move `pset.lineno` on
/// (`:652`). `is_tty` is `isatty(fileno(copystream))`.
///
/// Upstream prompts on a terminal — the `Enter data to be copied…` banner and
/// `PROMPT3` (`copy.c:544`, `:555`, `:599`). Those belong with interactive
/// mode (NAT-405) and are not printed; the data is read the same way.
pub fn handle_copy_in(
    mut executor: Option<&mut dyn Executor>,
    copystream: &mut dyn BufRead,
    is_cmd_source: bool,
    is_tty: bool,
    binary: bool,
    pset: &mut PsqlSettings,
    stderr: &mut dyn Write,
) -> (bool, Option<QueryResult>) {
    // `copy.c:522`: on a terminal, don't bother the user just to discard.
    if is_tty && executor.is_none() {
        return (true, None);
    }

    let mut ok = true;
    if binary {
        let mut buf = [0u8; COPYBUFSIZ];
        loop {
            let n = match copystream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                // `copy.c:685`: `ferror`.
                Err(_) => {
                    ok = false;
                    break;
                }
            };
            if let Some(conn) = executor.as_deref_mut()
                && conn.put_copy_data(&buf[..n]).is_err()
            {
                ok = false;
                break;
            }
        }
    } else {
        // Line by line, so as never to read past the `\.` that ends data
        // inlined in a script (`copy.c:588`). Upstream reads at most one
        // 8 kB buffer at a time and sends it; here a whole line is read and
        // the buffer sent once it holds 8 kB. The bytes the server receives
        // are the same, in differently sized CopyData messages.
        let mut buf: Vec<u8> = Vec::with_capacity(COPYBUFSIZ);
        let mut copydone = false;
        while !copydone {
            let mut line = Vec::new();
            match copystream.read_until(b'\n', &mut line) {
                Ok(0) => copydone = true,
                Ok(_) => {
                    if line.last() == Some(&b'\n') {
                        if is_cmd_source && (line == b"\\.\n" || line == b"\\.\r\n") {
                            // The EOF marker ends the data and is not sent,
                            // or CSV would take it for a row (`copy.c:641`).
                            copydone = true;
                            line.clear();
                        }
                        if is_cmd_source {
                            pset.lineno += 1;
                            pset.stmt_lineno += 1;
                        }
                    }
                    buf.extend_from_slice(&line);
                }
                Err(_) => {
                    ok = false;
                    copydone = true;
                }
            }
            if buf.len() >= COPYBUFSIZ - 5 || (copydone && !buf.is_empty()) {
                if let Some(conn) = executor.as_deref_mut()
                    && conn.put_copy_data(&buf).is_err()
                {
                    ok = false;
                    break;
                }
                buf.clear();
            }
        }
    }

    let Some(conn) = executor else {
        return (ok, None);
    };

    // `copy.c:694`: end the transfer, failing it if the read did.
    let failure: &[u8] = b"aborted because of read failure";
    if conn.put_copy_end((!ok).then_some(failure)).is_err() {
        ok = false;
    }

    // `copy.c:728`: never leave the connection in COPY IN.
    loop {
        match conn.get_result() {
            Ok(Some(result)) if result.status() == ExecStatus::CopyIn => {
                ok = false;
                let _ = conn.put_copy_end(Some(b"trying to exit copy mode"));
            }
            Ok(result) => {
                if result.as_ref().map(QueryResult::status) != Some(ExecStatus::CommandOk) {
                    log_copy_result(result.as_ref(), pset, stderr);
                    ok = false;
                }
                return (ok, result);
            }
            Err(err) => {
                logging::info(pset, err.as_bytes(), stderr);
                return (false, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::common::ErrorMessage;
    use crate::settings::SendMode;
    use rlibpq::{Backend, QueryRunner, ResultError, TransactionStatus};

    fn parse(args: &str) -> CopyOptions {
        parse_slash_copy(Some(args.as_bytes()), true, Some(b"/home/u")).unwrap()
    }

    fn parse_err(args: Option<&str>) -> String {
        String::from_utf8(parse_slash_copy(args.map(str::as_bytes), true, None).unwrap_err())
            .unwrap()
    }

    fn query(args: &str) -> String {
        String::from_utf8(parse(args).query()).unwrap()
    }

    #[test]
    fn the_query_is_built_the_way_do_copy_builds_it() {
        // `copy.c:141`: every piece of `before_tofrom` is appended after a
        // space, including the first, so `COPY` is followed by two
        // (`psql.out:5921`'s `\copy no_such_table from stdin` sends
        // `COPY  no_such_table FROM STDIN `).
        assert_eq!(
            query("no_such_table from stdin"),
            "COPY  no_such_table FROM STDIN "
        );
        assert_eq!(query("t to stdout"), "COPY  t TO STDOUT ");
        assert_eq!(
            query("s.\"T x\" (a, \"B\") to 'f' with (format csv)"),
            // The column list's only delimiters are `()` (`copy.c:174`),
            // so `a,` is one token.
            "COPY  s.\"T x\" ( a, \"B\" ) TO STDOUT with (format csv)"
        );
        assert_eq!(query("binary t from stdin"), "COPY binary t FROM STDIN ");
    }

    #[test]
    fn a_query_keeps_its_quoted_parentheses() {
        // `copy.c:122`: parentheses inside quotes do not count.
        assert_eq!(
            query("(select ')(' from t where \"a(\" = 1) to stdout"),
            "COPY  ( select ')(' from t where \"a(\" = 1 ) TO STDOUT "
        );
        // An E'' string's backslash escapes its quote (`stringutils.c:131`).
        assert_eq!(
            query(r"(select E'\')' as x) to stdout"),
            r"COPY  ( select E'\')' as x ) TO STDOUT "
        );
    }

    #[test]
    fn without_standard_strings_a_backslash_escapes_a_quote_in_a_query() {
        // `copy.c:94`: `nonstd_backslash`.
        let options = parse_slash_copy(Some(br"(select '\')') to stdout"), false, None).unwrap();
        assert_eq!(options.before_tofrom, br" ( select '\')' )");
        // With standard strings the same text ends the quote early and the
        // parenthesis count goes wrong.
        assert!(parse_slash_copy(Some(br"(select '\')') to stdout"), true, None).is_err());
    }

    #[test]
    fn one_whitespace_byte_after_the_file_name_is_eaten() {
        // `stringutils.c:112`-`:124`: the NUL that ends the file name
        // overwrites the whitespace after it; the options are the rest.
        let options = parse("t from 'f'  with csv");
        assert_eq!(options.after_tofrom.as_deref(), Some(&b" with csv"[..]));
        // A `;` is a delimiter there, and so starts the options.
        let options = parse("t from 'f';");
        assert_eq!(options.file, CopyFile::Path(b"f".to_vec()));
        assert_eq!(options.after_tofrom.as_deref(), Some(&b";"[..]));
    }

    #[test]
    fn the_file_forms_are_told_apart() {
        assert_eq!(parse("t from stdin").file, CopyFile::Stdio);
        assert_eq!(parse("t to STDOUT").file, CopyFile::Stdio);
        assert_eq!(parse("t from pstdin").file, CopyFile::PsqlStdio);
        assert_eq!(parse("t to PSTDOUT").file, CopyFile::PsqlStdio);
        assert_eq!(
            parse("t from 'it''s'").file,
            CopyFile::Path(b"it's".to_vec())
        );
        assert_eq!(
            parse("t from plain").file,
            CopyFile::Path(b"plain".to_vec())
        );
        assert_eq!(
            parse("t to program 'gzip > ''x'''").file,
            CopyFile::Program(b"gzip > 'x'".to_vec())
        );
        assert!(parse("t FROM x").from);
        assert!(!parse("t To x").from);
        // Not canonicalized (`copy.c:283`; docs/divergences.md).
        assert_eq!(
            parse("t from './a//b/'").file,
            CopyFile::Path(b"./a//b/".to_vec())
        );
    }

    #[test]
    fn a_program_is_refused_before_anything_runs() {
        // `PROGRAM` lands with `\g |`; until then it is refused, not run
        // (docs/divergences.md), and no query reaches the server.
        let mut server = CopyServer::default();
        let mut input: &[u8] = b"";
        let mut source = CommandSource::file(&mut input);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert!(!do_copy(
            Some(b"t to program 'cat'"),
            &mut server,
            &mut PsqlSettings::default(),
            &mut VariableSpace::new(),
            &mut source,
            &mut out,
            &mut err,
        ));
        assert!(out.is_empty());
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "psql: error: \\copy \u{2026} PROGRAM is not implemented yet (Linear NAT-403)\n"
        );
    }

    #[test]
    fn a_tilde_is_expanded_to_home_but_not_for_another_user() {
        // `common.c:2697`.
        assert_eq!(
            parse("t from ~/d/f").file,
            CopyFile::Path(b"/home/u/d/f".to_vec())
        );
        assert_eq!(
            parse("t from '~'").file,
            CopyFile::Path(b"/home/u".to_vec())
        );
        assert_eq!(expand_tilde(b"~bob/f", Some(b"/home/u")), b"~bob/f");
        assert_eq!(expand_tilde(b"~/f", None), b"~/f");
        assert_eq!(expand_tilde(b"a~/f", Some(b"/home/u")), b"a~/f");
    }

    #[test]
    fn a_parse_error_names_the_token_or_the_end_of_the_line() {
        // `copy.c:98`, `:253`, `:255`.
        assert_eq!(parse_err(None), "\\copy: arguments required");
        assert_eq!(parse_err(Some("t foo")), "\\copy: parse error at \"foo\"");
        assert_eq!(
            parse_err(Some("t from")),
            "\\copy: parse error at end of line"
        );
        assert_eq!(
            parse_err(Some("t (a, b")),
            "\\copy: parse error at end of line"
        );
        // `copy.c:216`: a program must be quoted.
        assert_eq!(
            parse_err(Some("t to program gzip")),
            "\\copy: parse error at \"gzip\""
        );
    }

    #[test]
    fn strip_quotes_undoubles_and_honours_the_escape() {
        // `stringutils.c:240`.
        assert_eq!(strip_quotes(b"'a''b'", b'\'', None), b"a'b");
        assert_eq!(strip_quotes(b"'a\\'b'", b'\'', Some(b'\\')), b"a'b");
        assert_eq!(strip_quotes(b"plain", b'\'', None), b"plain");
        assert_eq!(strip_quotes(b"'", b'\'', None), b"");
    }

    #[test]
    fn strerror_drops_the_os_error_suffix_rust_adds() {
        let err = std::io::Error::from_raw_os_error(2);
        assert_eq!(strerror(&err), "No such file or directory");
        assert_eq!(strerror(&std::io::Error::other("x")), "x");
    }

    /// A server end of one COPY: rows to hand out, what was sent, and the
    /// results `PQgetResult` returns afterwards.
    #[derive(Default)]
    struct CopyServer {
        rows: VecDeque<Vec<u8>>,
        fail_transfer: bool,
        received: Vec<Vec<u8>>,
        ended: Vec<Option<Vec<u8>>>,
        refuse_put: bool,
        after: VecDeque<QueryResult>,
    }

    impl Executor for CopyServer {
        fn exec(
            &mut self,
            _query: &[u8],
            _mode: &SendMode,
        ) -> Result<Vec<QueryResult>, ErrorMessage> {
            unreachable!("the COPY is already running")
        }
        fn get_result(&mut self) -> Result<Option<QueryResult>, ErrorMessage> {
            Ok(self.after.pop_front())
        }
        fn get_copy_data(&mut self) -> Result<Option<Vec<u8>>, ErrorMessage> {
            match self.rows.pop_front() {
                Some(row) => Ok(Some(row)),
                None if self.fail_transfer => Err(ErrorMessage::new(b"lost sync\n".to_vec())),
                None => Ok(None),
            }
        }
        fn put_copy_data(&mut self, data: &[u8]) -> Result<(), ErrorMessage> {
            if self.refuse_put {
                return Err(ErrorMessage::new(b"no COPY in progress".to_vec()));
            }
            self.received.push(data.to_vec());
            Ok(())
        }
        fn put_copy_end(&mut self, error: Option<&[u8]>) -> Result<(), ErrorMessage> {
            self.ended.push(error.map(<[u8]>::to_vec));
            Ok(())
        }
        fn connected(&self) -> bool {
            true
        }
        fn abandon(&mut self) {}
    }

    fn command_complete(tag: &str) -> QueryResult {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::CommandComplete(tag.as_bytes().to_vec()))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results().remove(0)
    }

    fn server_error(message: &str) -> QueryResult {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::ErrorResponse(ResultError::new(vec![
                (b'S', b"ERROR".to_vec()),
                (b'C', b"22P02".to_vec()),
                (b'M', message.as_bytes().to_vec()),
            ])))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results().remove(0)
    }

    fn in_file(pset: PsqlSettings) -> PsqlSettings {
        PsqlSettings {
            inputfile: Some("<stdin>".into()),
            lineno: 3,
            ..pset
        }
    }

    #[test]
    fn copy_out_writes_every_row_then_takes_the_command_result() {
        let mut server = CopyServer {
            rows: VecDeque::from([b"1\ta\n".to_vec(), b"2\tb\n".to_vec()]),
            after: VecDeque::from([command_complete("COPY 2")]),
            ..CopyServer::default()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (ok, result) = handle_copy_out(
            &mut server,
            Some(&mut out),
            &PsqlSettings::default(),
            &mut err,
        );
        assert!(ok);
        assert_eq!(out, b"1\ta\n2\tb\n");
        assert_eq!(result.unwrap().command_status(), b"COPY 2");
        assert!(err.is_empty());
    }

    #[test]
    fn a_write_failure_is_reported_once_and_the_data_is_still_drained() {
        // `copy.c:449`: "complain only once, keep reading data from server".
        let mut server = CopyServer {
            rows: VecDeque::from([b"1\n".to_vec(), b"2\n".to_vec()]),
            after: VecDeque::from([command_complete("COPY 2")]),
            ..CopyServer::default()
        };
        let mut err = Vec::new();
        let mut full = std::io::Cursor::new([0u8; 0]);
        let (ok, _) = handle_copy_out(
            &mut server,
            Some(&mut full),
            &PsqlSettings::default(),
            &mut err,
        );
        assert!(!ok);
        assert!(server.rows.is_empty(), "every row was read");
        let err = String::from_utf8(err).unwrap();
        assert_eq!(
            err.matches("could not write COPY data: ").count(),
            1,
            "{err}"
        );
    }

    #[test]
    fn a_failed_transfer_says_so_with_libpqs_message() {
        // `copy.c:465`.
        let mut server = CopyServer {
            fail_transfer: true,
            after: VecDeque::from([server_error("boom")]),
            ..CopyServer::default()
        };
        let mut err = Vec::new();
        let (ok, result) = handle_copy_out(
            &mut server,
            Some(&mut Vec::new()),
            &in_file(PsqlSettings::default()),
            &mut err,
        );
        assert!(!ok);
        assert_eq!(result.unwrap().status(), ExecStatus::FatalError);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "psql:<stdin>:3: error: COPY data transfer failed: lost sync\n\
             psql:<stdin>:3: ERROR:  boom\n"
        );
    }

    #[test]
    fn copy_in_from_the_command_source_stops_at_the_eof_marker_and_counts_lines() {
        // `copy.c:634`-`:656`.
        let mut server = CopyServer {
            after: VecDeque::from([command_complete("COPY 2")]),
            ..CopyServer::default()
        };
        let mut source: &[u8] = b"1\ta\n2\tb\r\n\\.\nselect 1;\n";
        let mut pset = PsqlSettings {
            lineno: 10,
            ..PsqlSettings::default()
        };
        let (ok, result) = handle_copy_in(
            Some(&mut server),
            &mut source,
            true,
            false,
            false,
            &mut pset,
            &mut Vec::new(),
        );
        assert!(ok);
        assert_eq!(server.received.concat(), b"1\ta\n2\tb\r\n");
        assert_eq!(server.ended, [None]);
        assert_eq!(result.unwrap().command_status(), b"COPY 2");
        assert_eq!(source, b"select 1;\n", "the script goes on after \\.");
        assert_eq!(pset.lineno, 13);
    }

    #[test]
    fn a_crlf_eof_marker_ends_the_data_too() {
        let mut server = CopyServer {
            after: VecDeque::from([command_complete("COPY 1")]),
            ..CopyServer::default()
        };
        let mut source: &[u8] = b"1\r\n\\.\r\nrest\n";
        let (ok, _) = handle_copy_in(
            Some(&mut server),
            &mut source,
            true,
            false,
            false,
            &mut PsqlSettings::default(),
            &mut Vec::new(),
        );
        assert!(ok);
        assert_eq!(server.received.concat(), b"1\r\n");
        assert_eq!(source, b"rest\n");
    }

    #[test]
    fn from_a_file_the_eof_marker_is_data_for_the_server_to_judge() {
        // `copy.c:630`: "we let it decide whether it's an EOF or not".
        let mut server = CopyServer {
            after: VecDeque::from([command_complete("COPY 1")]),
            ..CopyServer::default()
        };
        let mut file: &[u8] = b"1\n\\.\n2";
        let mut pset = PsqlSettings::default();
        let (ok, _) = handle_copy_in(
            Some(&mut server),
            &mut file,
            false,
            false,
            false,
            &mut pset,
            &mut Vec::new(),
        );
        assert!(ok);
        assert_eq!(server.received.concat(), b"1\n\\.\n2");
        assert_eq!(pset.lineno, 0, "only the command source counts lines");
    }

    #[test]
    fn a_long_input_goes_out_in_chunks_of_about_eight_kilobytes() {
        let mut server = CopyServer {
            after: VecDeque::from([command_complete("COPY 3000")]),
            ..CopyServer::default()
        };
        let data: Vec<u8> = (0..3000)
            .flat_map(|i| format!("{i:08}\n").into_bytes())
            .collect();
        let (ok, _) = handle_copy_in(
            Some(&mut server),
            &mut &data[..],
            false,
            false,
            false,
            &mut PsqlSettings::default(),
            &mut Vec::new(),
        );
        assert!(ok);
        assert_eq!(server.received.concat(), data);
        assert!(server.received.len() > 1);
        assert!(server.received.iter().all(|c| c.len() < COPYBUFSIZ + 9));
    }

    #[test]
    fn binary_data_is_sent_as_read_with_no_eof_marker() {
        let mut server = CopyServer {
            after: VecDeque::from([command_complete("COPY 1")]),
            ..CopyServer::default()
        };
        let data = b"PGCOPY\n\xff\r\n\0\\.\n".to_vec();
        let (ok, _) = handle_copy_in(
            Some(&mut server),
            &mut &data[..],
            true,
            false,
            true,
            &mut PsqlSettings::default(),
            &mut Vec::new(),
        );
        assert!(ok);
        assert_eq!(server.received.concat(), data);
    }

    #[test]
    fn without_a_connection_the_data_is_read_and_dropped() {
        // `copy.c:499`: a COPY the server refused still owns its data.
        let mut source: &[u8] = b"foo\n\\echo no\n\\.\n\\echo yes\n";
        let mut pset = PsqlSettings::default();
        let (ok, result) = handle_copy_in(
            None,
            &mut source,
            true,
            false,
            false,
            &mut pset,
            &mut Vec::new(),
        );
        assert!(ok);
        assert!(result.is_none());
        assert_eq!(source, b"\\echo yes\n");
        assert_eq!(pset.lineno, 3);
    }

    #[test]
    fn on_a_terminal_nothing_is_read_just_to_be_dropped() {
        // `copy.c:522`.
        let mut source: &[u8] = b"typed\n";
        let (ok, _) = handle_copy_in(
            None,
            &mut source,
            true,
            true,
            false,
            &mut PsqlSettings::default(),
            &mut Vec::new(),
        );
        assert!(ok);
        assert_eq!(source, b"typed\n");
    }

    #[test]
    fn on_a_terminal_the_data_is_read_without_a_prompt() {
        // `copy.c:544`, `:599` print a banner and `PROMPT3` here; this port
        // takes no stdout at all, so neither can appear (docs/divergences.md).
        let mut server = CopyServer {
            after: VecDeque::from([command_complete("COPY 1")]),
            ..CopyServer::default()
        };
        let (ok, _) = handle_copy_in(
            Some(&mut server),
            &mut &b"1\n\\.\n"[..],
            true,
            true,
            false,
            &mut PsqlSettings::default(),
            &mut Vec::new(),
        );
        assert!(ok);
        assert_eq!(server.received.concat(), b"1\n");
    }

    #[test]
    fn a_read_failure_fails_the_copy_with_upstreams_message() {
        // `copy.c:685`, `:696`.
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from_raw_os_error(5))
            }
        }
        let mut server = CopyServer {
            after: VecDeque::from([server_error(
                "COPY from stdin failed: aborted because of read failure",
            )]),
            ..CopyServer::default()
        };
        let mut err = Vec::new();
        let (ok, _) = handle_copy_in(
            Some(&mut server),
            &mut BufReader::new(Broken),
            false,
            false,
            false,
            &mut PsqlSettings::default(),
            &mut err,
        );
        assert!(!ok);
        assert_eq!(
            server.ended,
            [Some(b"aborted because of read failure".to_vec())]
        );
        assert!(
            String::from_utf8(err)
                .unwrap()
                .contains("aborted because of read failure")
        );
    }

    #[test]
    fn a_server_that_already_ended_the_copy_is_answered_with_its_error() {
        // The server rejected a row mid-COPY: `PQputCopyData` is refused, the
        // COPY is ended as failed, and the server's error is the result.
        let mut server = CopyServer {
            refuse_put: true,
            after: VecDeque::from([server_error("invalid input syntax for type integer: \"x\"")]),
            ..CopyServer::default()
        };
        let mut err = Vec::new();
        let (ok, result) = handle_copy_in(
            Some(&mut server),
            &mut &b"x\n"[..],
            false,
            false,
            false,
            &mut PsqlSettings::default(),
            &mut err,
        );
        assert!(!ok);
        assert_eq!(result.unwrap().status(), ExecStatus::FatalError);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "psql: ERROR:  invalid input syntax for type integer: \"x\"\n"
        );
    }

    #[test]
    fn a_copy_still_taking_data_is_ended_until_it_stops() {
        // `copy.c:728`: never return in COPY IN.
        let mut server = CopyServer {
            after: VecDeque::from([
                QueryResult::new(ExecStatus::CopyIn),
                command_complete("COPY 0"),
            ]),
            ..CopyServer::default()
        };
        let (ok, result) = handle_copy_in(
            Some(&mut server),
            &mut &b""[..],
            false,
            false,
            false,
            &mut PsqlSettings::default(),
            &mut Vec::new(),
        );
        assert!(!ok);
        assert_eq!(
            server.ended,
            [None, Some(b"trying to exit copy mode".to_vec())]
        );
        assert_eq!(result.unwrap().command_status(), b"COPY 0");
    }
}
