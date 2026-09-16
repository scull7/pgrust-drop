//! The SQL statement splitter: `src/fe_utils/psqlscan.l` as a hand-written
//! state machine.
//!
//! Upstream is a flex lexer whose every rule body is `ECHO`; the only thing it
//! computes is *where a statement ends*. The flex start conditions become
//! [`StartState`], the `yylex()` return values become [`LexRes`], and the rule
//! table becomes one `match` per state in [`Scanner::step`]. Flex's
//! longest-match-then-first-rule tie-break is reproduced by ordering each
//! state's arms the way the `%%` section orders its rules and by measuring the
//! candidates whose lengths can differ (`psqlscan.l:96`-`:101` states the rule).
//!
//! Everything here is a pure calculation over bytes: the only outside world it
//! touches is the [`VariableSource`] callback, which stands in for upstream's
//! `PsqlScanCallbacks.get_variable` (`psqlscan.h:64`).

use crate::variables::{escape_identifier, escape_literal};

/// `yylex()`'s return values (`psqlscan.l:57`-`:59`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexRes {
    /// `LEXRES_EOL`: end of input.
    Eol,
    /// `LEXRES_SEMI`: command-terminating semicolon found.
    Semi,
    /// `LEXRES_BACKSLASH`: backslash command start.
    Backslash,
}

/// Termination states for [`Scanner::scan`] (`psqlscan.h:33`-`:38`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanResult {
    /// `PSCAN_SEMICOLON`: found command-ending semicolon.
    Semicolon,
    /// `PSCAN_BACKSLASH`: found backslash command.
    Backslash,
    /// `PSCAN_INCOMPLETE`: end of line, SQL statement incomplete.
    Incomplete,
    /// `PSCAN_EOL`: end of line, SQL possibly complete.
    Eol,
}

/// Prompt type returned by `psql_scan()` (`psqlscan.h:41`-`:51`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptStatus {
    /// `PROMPT_READY`
    #[default]
    Ready,
    /// `PROMPT_CONTINUE`
    Continue,
    /// `PROMPT_COMMENT`
    Comment,
    /// `PROMPT_SINGLEQUOTE`
    SingleQuote,
    /// `PROMPT_DOUBLEQUOTE`
    DoubleQuote,
    /// `PROMPT_DOLLARQUOTE`
    DollarQuote,
    /// `PROMPT_PAREN`
    Paren,
    /// `PROMPT_COPY`
    Copy,
}

/// Quoting request types for the `get_variable()` callback
/// (`psqlscan.h:54`-`:60`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteType {
    /// `PQUOTE_PLAIN`: just return the actual value.
    Plain,
    /// `PQUOTE_SQL_LITERAL`: add quotes to make a valid SQL literal.
    SqlLiteral,
    /// `PQUOTE_SQL_IDENT`: quote if needed to make a SQL identifier.
    SqlIdent,
    /// `PQUOTE_SHELL_ARG`: quote if needed to be safe in a shell command.
    ShellArg,
}

/// Upstream's `PsqlScanCallbacks.get_variable` as a trait.
///
/// `None` means the variable is unset, in which case the lexer copies the text
/// through unchanged (`psqlscan.l:750`).
pub trait VariableSource {
    /// Value of `name`, already quoted as `quote` asks.
    fn get_variable(&self, name: &str, quote: QuoteType) -> Option<String>;
}

/// A [`VariableSource`] in which nothing is ever set.
///
/// Upstream allows a NULL `get_variable` pointer (`psqlscan.h:66`); this is
/// that, without the pointer.
pub struct NoVariables;

impl VariableSource for NoVariables {
    fn get_variable(&self, _name: &str, _quote: QuoteType) -> Option<String> {
        None
    }
}

/// The flex exclusive start conditions (`psqlscan.l:124`-`:133`).
///
/// `xeu` is deliberately absent, exactly as upstream says it is: "we
/// intentionally don't mimic the backend's `<xeu>` state" (`psqlscan.l:115`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartState {
    /// Flex's `INITIAL`.
    #[default]
    Initial,
    /// `<xb>` bit string literal.
    Xb,
    /// `<xc>` extended C-style comments.
    Xc,
    /// `<xd>` delimited identifiers (double-quoted identifiers).
    Xd,
    /// `<xh>` hexadecimal byte string.
    Xh,
    /// `<xq>` standard quoted strings.
    Xq,
    /// `<xqs>` quote stop (detect continued strings).
    Xqs,
    /// `<xe>` extended quoted strings (backslash escapes).
    Xe,
    /// `<xdolq>` `$foo$` quoted strings.
    Xdolq,
    /// `<xui>` quoted identifier with Unicode escapes.
    Xui,
    /// `<xus>` quoted string with Unicode escapes.
    Xus,
}

// --- character classes, from the definitions section (`psqlscan.l:151`-`:360`)

/// `space  [ \t\n\r\f\v]`
const fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b)
}

/// `non_newline_space  [ \t\f\v]`
const fn is_non_newline_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | 0x0c | 0x0b)
}

/// `newline  [\n\r]`
const fn is_newline(c: u8) -> bool {
    matches!(c, b'\n' | b'\r')
}

/// `ident_start  [A-Za-z\200-\377_]`
const fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

/// `ident_cont  [A-Za-z\200-\377_0-9\$]`
const fn is_ident_cont(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit() || c == b'$'
}

/// `dolq_start  [A-Za-z\200-\377_]`
const fn is_dolq_start(c: u8) -> bool {
    is_ident_start(c)
}

/// `dolq_cont  [A-Za-z\200-\377_0-9]`
const fn is_dolq_cont(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

/// `variable_char  [A-Za-z\200-\377_0-9]` — psql-specific.
pub(crate) const fn is_variable_char(c: u8) -> bool {
    is_dolq_cont(c)
}

/// `self  [,()\[\].;\:\+\-\*\/\%\^\<\>\=]`
const fn is_self(c: u8) -> bool {
    matches!(
        c,
        b',' | b'('
            | b')'
            | b'['
            | b']'
            | b'.'
            | b';'
            | b':'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'^'
            | b'<'
            | b'>'
            | b'='
    )
}

/// `op_chars  [\~\!\@\#\^\&\|\`\?\+\-\*\/\%\<\>\=]`
const fn is_op_char(c: u8) -> bool {
    matches!(
        c,
        b'~' | b'!'
            | b'@'
            | b'#'
            | b'^'
            | b'&'
            | b'|'
            | b'`'
            | b'?'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'<'
            | b'>'
            | b'='
    )
}

/// The eight characters that let a trailing `+`/`-` stay part of an operator
/// (`psqlscan.l:834`-`:841`).
const fn qualifies_trailing_sign(c: u8) -> bool {
    matches!(
        c,
        b'~' | b'!' | b'@' | b'#' | b'^' | b'&' | b'|' | b'`' | b'?' | b'%'
    )
}

/// One input buffer: the outer line, or the value of a variable being expanded.
///
/// Upstream keeps these on `state->buffer_stack` as flex buffers
/// (`psqlscan_int.h:66`); the data and the read position are all we need.
#[derive(Debug, Clone)]
struct Buf {
    data: Vec<u8>,
    pos: usize,
    /// Name of the variable providing the data, for recursion detection.
    varname: Option<String>,
}

/// State that lives across successive input lines until [`Scanner::reset`]
/// (`psqlscan_int.h:110`-`:129`).
#[derive(Debug, Clone, Default)]
pub struct ScanState {
    /// `start_state`: yylex's starting/finishing state.
    pub start_state: StartState,
    /// `state_before_str_stop`: start condition before the end quote.
    state_before_str_stop: StartState,
    /// `paren_depth`: depth of nesting in parentheses.
    paren_depth: i32,
    /// `xcdepth`: depth of nesting in slash-star comments.
    xcdepth: i32,
    /// `dolqstart`: current `$foo$` quote start string.
    dolqstart: Option<Vec<u8>>,
    /// `begin_depth`: depth of begin/end pairs.
    begin_depth: i32,
    /// `copy_stdin_count`: number of COPY FROM STDIN commands.
    copy_stdin_count: i32,
    /// `init_idents_count`: identifiers since the start of the statement.
    init_idents_count: usize,
    /// `init_idents[8]`: records the first few identifiers.
    init_idents: [u8; 8],
    /// `std_strings`: are string literals standard?
    pub std_strings: bool,
}

/// The lexer, holding both the cross-line [`ScanState`] and the buffers being
/// read (`PsqlScanStateData`, `psqlscan_int.h:78`).
#[derive(Debug, Clone)]
pub struct Scanner {
    state: ScanState,
    /// Index 0 is the outer line; the rest are variable expansions.
    stack: Vec<Buf>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    /// `psql_scan_create()` (`psqlscan.l:1108`). `std_strings` starts true,
    /// which is what `standard_conforming_strings` defaults to.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: ScanState {
                std_strings: true,
                ..ScanState::default()
            },
            stack: Vec::new(),
        }
    }

    /// `psql_scan_setup()` (`psqlscan.l:1166`): hand the lexer a new line.
    pub fn setup(&mut self, line: &[u8], std_strings: bool) {
        self.state.std_strings = std_strings;
        self.stack.clear();
        self.stack.push(Buf {
            data: line.to_vec(),
            pos: 0,
            varname: None,
        });
    }

    /// `psql_scan_finish()` (`psqlscan.l:1355`): release the input buffers.
    pub fn finish(&mut self) {
        self.stack.clear();
    }

    /// `psql_scan_reset()` (`psqlscan.l:1381`): forget everything but
    /// `std_strings`, which `psql_scan_setup` sets afresh each line.
    pub fn reset(&mut self) {
        let std_strings = self.state.std_strings;
        self.state = ScanState {
            std_strings,
            ..ScanState::default()
        };
        self.stack.clear();
    }

    /// `psql_scan_count_copy_from_stdin()` (`psqlscan.l:1422`): read and reset
    /// the counter, as upstream does.
    pub fn count_copy_from_stdin(&mut self) -> i32 {
        std::mem::take(&mut self.state.copy_stdin_count)
    }

    /// `psql_scan_in_quote()` (`psqlscan.l:1443`).
    #[must_use]
    pub fn in_quote(&self) -> bool {
        !matches!(self.state.start_state, StartState::Initial)
    }

    /// The start condition the lexer will resume in; `PSCAN_*` decoding needs
    /// it and the tests read it.
    #[must_use]
    pub fn start_state(&self) -> StartState {
        self.state.start_state
    }

    /// Depth of unclosed parentheses, for the tests that pin `PROMPT_PAREN`.
    #[must_use]
    pub fn paren_depth(&self) -> i32 {
        self.state.paren_depth
    }

    /// `psql_scan()` (`psqlscan.l:1228`): lex until a statement boundary,
    /// appending everything consumed to `query_buf`.
    pub fn scan(
        &mut self,
        query_buf: &mut Vec<u8>,
        vars: &dyn VariableSource,
    ) -> (ScanResult, PromptStatus) {
        let lexresult = loop {
            if let Some(res) = self.step(query_buf, vars) {
                break res;
            }
        };

        match lexresult {
            LexRes::Semi => (ScanResult::Semicolon, PromptStatus::Ready),
            LexRes::Backslash => (ScanResult::Backslash, PromptStatus::Ready),
            LexRes::Eol => match self.state.start_state {
                // `xqs` is treated like INITIAL (`psqlscan.l:1259`).
                StartState::Initial | StartState::Xqs => {
                    if self.state.paren_depth > 0 {
                        (ScanResult::Incomplete, PromptStatus::Paren)
                    } else if self.state.begin_depth > 0 {
                        (ScanResult::Incomplete, PromptStatus::Continue)
                    } else if query_buf.is_empty() {
                        // never bother to send an empty buffer
                        (ScanResult::Incomplete, PromptStatus::Ready)
                    } else {
                        (ScanResult::Eol, PromptStatus::Continue)
                    }
                }
                StartState::Xb
                | StartState::Xh
                | StartState::Xe
                | StartState::Xq
                | StartState::Xus => (ScanResult::Incomplete, PromptStatus::SingleQuote),
                StartState::Xc => (ScanResult::Incomplete, PromptStatus::Comment),
                StartState::Xd | StartState::Xui => {
                    (ScanResult::Incomplete, PromptStatus::DoubleQuote)
                }
                StartState::Xdolq => (ScanResult::Incomplete, PromptStatus::DollarQuote),
            },
        }
    }

    /// The remaining bytes of the buffer on top of the stack.
    pub(crate) fn rest(&self) -> &[u8] {
        let buf = self.stack.last().expect("scan without setup");
        &buf.data[buf.pos..]
    }

    /// Consume `n` bytes from the top buffer and echo them to `out`.
    pub(crate) fn echo(&mut self, n: usize, out: &mut Vec<u8>) {
        let buf = self.stack.last_mut().expect("scan without setup");
        out.extend_from_slice(&buf.data[buf.pos..buf.pos + n]);
        buf.pos += n;
    }

    /// Consume `n` bytes without echoing them.
    pub(crate) fn skip(&mut self, n: usize) {
        let buf = self.stack.last_mut().expect("scan without setup");
        buf.pos += n;
    }

    /// One `yylex()` rule firing. `None` means "keep lexing".
    #[allow(clippy::too_many_lines)]
    fn step(&mut self, out: &mut Vec<u8>, vars: &dyn VariableSource) -> Option<LexRes> {
        if self.rest().is_empty() {
            // The `<<EOF>>` rule (`psqlscan.l:945`).
            if self.stack.len() > 1 {
                self.stack.pop();
                return None;
            }
            return Some(LexRes::Eol);
        }

        match self.state.start_state {
            StartState::Xc => self.step_xc(out),
            StartState::Xb => self.step_xb_xh(out, StartState::Xb),
            StartState::Xh => self.step_xb_xh(out, StartState::Xh),
            StartState::Xq | StartState::Xus => self.step_xq(out),
            StartState::Xe => self.step_xe(out),
            StartState::Xqs => self.step_xqs(out),
            StartState::Xdolq => self.step_xdolq(out),
            StartState::Xd | StartState::Xui => self.step_xd(out),
            StartState::Initial => return self.step_initial(out, vars),
        }
        None
    }

    /// `<xc>` (`psqlscan.l:426`-`:451`).
    fn step_xc(&mut self, out: &mut Vec<u8>) {
        let rest = self.rest();
        if rest.starts_with(b"/*") {
            self.state.xcdepth += 1;
            self.echo(2, out); // yyless(2)
        } else if let Some(n) = match_xcstop(rest) {
            if self.state.xcdepth <= 0 {
                self.state.start_state = StartState::Initial;
            } else {
                self.state.xcdepth -= 1;
            }
            self.echo(n, out);
        } else {
            // {xcinside} [^*/]+, then the single-char {op_chars} and \*+ rules.
            let inside = rest.iter().take_while(|&&c| c != b'*' && c != b'/').count();
            if inside > 0 {
                self.echo(inside, out);
            } else {
                let stars = rest.iter().take_while(|&&c| c == b'*').count();
                self.echo(stars.max(1), out);
            }
        }
    }

    /// `<xb>`/`<xh>` (`psqlscan.l:456`-`:461`) plus the shared end-quote rule.
    fn step_xb_xh(&mut self, out: &mut Vec<u8>, here: StartState) {
        let rest = self.rest();
        if rest[0] == b'\'' {
            self.state.state_before_str_stop = here;
            self.state.start_state = StartState::Xqs;
            self.echo(1, out);
        } else {
            let n = rest.iter().take_while(|&&c| c != b'\'').count();
            self.echo(n, out);
        }
    }

    /// `<xq>` and `<xus>` (`psqlscan.l:520`-`:526`).
    fn step_xq(&mut self, out: &mut Vec<u8>) {
        let rest = self.rest();
        if rest.starts_with(b"''") {
            self.echo(2, out);
        } else if rest[0] == b'\'' {
            self.state.state_before_str_stop = self.state.start_state;
            self.state.start_state = StartState::Xqs;
            self.echo(1, out);
        } else {
            let n = rest.iter().take_while(|&&c| c != b'\'').count();
            self.echo(n, out);
        }
    }

    /// `<xe>` (`psqlscan.l:520`-`:552`).
    fn step_xe(&mut self, out: &mut Vec<u8>) {
        let rest = self.rest();
        if rest.starts_with(b"''") {
            self.echo(2, out);
        } else if rest[0] == b'\'' {
            self.state.state_before_str_stop = StartState::Xe;
            self.state.start_state = StartState::Xqs;
            self.echo(1, out);
        } else if rest[0] == b'\\' {
            // The backslash rules all start with `\`; flex takes the longest.
            let n = match_xe_escape(rest);
            self.echo(n, out);
        } else {
            // {xeinside} [^\\']+
            let n = rest
                .iter()
                .take_while(|&&c| c != b'\\' && c != b'\'')
                .count();
            self.echo(n, out);
        }
    }

    /// `<xqs>` (`psqlscan.l:504`-`:519`): look ahead for a string continuation.
    fn step_xqs(&mut self, out: &mut Vec<u8>) {
        if let Some(n) = match_quotecontinue(self.rest()) {
            self.state.start_state = self.state.state_before_str_stop;
            self.echo(n, out);
        } else {
            // yyless(0): throw everything back and re-lex it as INITIAL.
            self.state.start_state = StartState::Initial;
        }
    }

    /// `<xdolq>` (`psqlscan.l:566`-`:592`).
    fn step_xdolq(&mut self, out: &mut Vec<u8>) {
        let rest = self.rest();
        if let Some(n) = match_dolqdelim(rest) {
            if self.state.dolqstart.as_deref() == Some(&rest[..n]) {
                self.state.dolqstart = None;
                self.state.start_state = StartState::Initial;
                self.echo(n, out);
            } else {
                // Put back the final `$` for rescanning.
                self.echo(n - 1, out);
            }
        } else if rest[0] == b'$' {
            // {dolqfailed}, or the `.` rule for a lone `$`.
            let n = 1 + rest[1..].iter().take_while(|&&c| is_dolq_cont(c)).count();
            self.echo(n, out);
        } else {
            // {dolqinside} [^$]+
            let n = rest.iter().take_while(|&&c| c != b'$').count();
            self.echo(n, out);
        }
    }

    /// `<xd>` and `<xui>` (`psqlscan.l:601`-`:616`).
    fn step_xd(&mut self, out: &mut Vec<u8>) {
        let rest = self.rest();
        if rest.starts_with(b"\"\"") {
            self.echo(2, out);
        } else if rest[0] == b'"' {
            self.state.start_state = StartState::Initial;
            self.echo(1, out);
        } else {
            let n = rest.iter().take_while(|&&c| c != b'"').count();
            self.echo(n, out);
        }
    }

    /// The `INITIAL` rules, in the order the `%%` section lists them.
    #[allow(clippy::too_many_lines)]
    fn step_initial(&mut self, out: &mut Vec<u8>, vars: &dyn VariableSource) -> Option<LexRes> {
        let rest = self.rest();
        let c = rest[0];

        // {whitespace} — suppressed until some non-whitespace has been
        // collected (`psqlscan.l:405`).
        if is_space(c) {
            let n = rest.iter().take_while(|&&b| is_space(b)).count();
            if out.is_empty() {
                self.skip(n);
            } else {
                self.echo(n, out);
            }
            return None;
        }
        if rest.starts_with(b"--") {
            let n = rest.iter().take_while(|&&b| !is_newline(b)).count();
            if out.is_empty() {
                self.skip(n);
            } else {
                self.echo(n, out);
            }
            return None;
        }

        // {xcstart}: `/*` plus {op_chars}*, put back past the slash-star.
        if rest.starts_with(b"/*") {
            self.state.xcdepth = 0;
            self.state.start_state = StartState::Xc;
            self.echo(2, out);
            return None;
        }

        // {xbstart} / {xhstart} / {xnstart} / {xqstart} / {xestart} /
        // {xusstart} / {xuistart} (`psqlscan.l:455`-`:499`).
        if rest.len() >= 2 && rest[1] == b'\'' {
            match c {
                b'b' | b'B' => {
                    self.state.start_state = StartState::Xb;
                    self.echo(2, out);
                    return None;
                }
                b'x' | b'X' => {
                    self.state.start_state = StartState::Xh;
                    self.echo(2, out);
                    return None;
                }
                b'n' | b'N' => {
                    // yyless(1): eat only the `n` this time.
                    self.echo(1, out);
                    return None;
                }
                b'e' | b'E' => {
                    self.state.start_state = StartState::Xe;
                    self.echo(2, out);
                    return None;
                }
                _ => {}
            }
        }
        if matches!(c, b'u' | b'U') && rest.len() >= 2 && rest[1] == b'&' {
            match rest.get(2) {
                Some(b'\'') => {
                    self.state.start_state = StartState::Xus;
                    self.echo(3, out);
                }
                Some(b'"') => {
                    self.state.start_state = StartState::Xui;
                    self.echo(3, out);
                }
                // {xufailed}: throw back all but the initial u/U.
                _ => self.echo(1, out),
            }
            return None;
        }
        if c == b'\'' {
            self.state.start_state = if self.state.std_strings {
                StartState::Xq
            } else {
                StartState::Xe
            };
            self.echo(1, out);
            return None;
        }
        if c == b'"' {
            self.state.start_state = StartState::Xd;
            self.echo(1, out);
            return None;
        }

        // {dolqdelim} / {dolqfailed} (`psqlscan.l:556`-`:565`).
        if c == b'$' {
            if let Some(n) = match_dolqdelim(rest) {
                self.state.dolqstart = Some(rest[..n].to_vec());
                self.state.start_state = StartState::Xdolq;
                self.echo(n, out);
                return None;
            }
            if rest.len() > 1 && is_dolq_start(rest[1]) {
                // {dolqfailed}: throw back all but the initial `$`.
                self.echo(1, out);
                return None;
            }
            // {param} / {param_junk}: `$` digits, then maybe an identifier.
            let digits = rest[1..]
                .iter()
                .take_while(|&&b| b.is_ascii_digit())
                .count();
            if digits > 0 {
                let junk = rest[1 + digits..]
                    .iter()
                    .take_while(|&&b| is_ident_cont(b))
                    .count();
                self.echo(1 + digits + junk, out);
                return None;
            }
            // {other}
            self.echo(1, out);
            return None;
        }

        // The psql-specific rules, which must precede {self}
        // (`psqlscan.l:684`-`:723`).
        if c == b'(' {
            self.state.paren_depth += 1;
            self.echo(1, out);
            return None;
        }
        if c == b')' {
            if self.state.paren_depth > 0 {
                self.state.paren_depth -= 1;
            }
            self.echo(1, out);
            return None;
        }
        if c == b';' {
            self.echo(1, out);
            if self.state.paren_depth == 0 && self.state.begin_depth == 0 {
                if self.is_copy_from_stdin() {
                    self.state.copy_stdin_count += 1;
                }
                self.state.init_idents_count = 0;
                return Some(LexRes::Semi);
            }
            return None;
        }
        if c == b'\\' {
            // `"\\"[;:]`: force a semicolon or colon into the query buffer.
            if let Some(&next) = rest.get(1)
                && (next == b';' || next == b':')
            {
                {
                    self.skip(2);
                    out.push(next);
                    if next == b';' && self.state.paren_depth == 0 && self.state.begin_depth == 0 {
                        if self.is_copy_from_stdin() {
                            self.state.copy_stdin_count += 1;
                        }
                        self.state.init_idents_count = 0;
                    }
                    return None;
                }
            }
            // The `"\\"` rule consumes the backslash without echoing it
            // (`psqlscan.l:715`); the slash lexer resumes at the command name.
            self.skip(1);
            return Some(LexRes::Backslash);
        }
        if c == b':' {
            self.step_colon(out, vars);
            return None;
        }

        // {typecast}, {dot_dot}, {colon_equals} and the operator-like tokens
        // are all subsumed by {operator}/{self} below, since every rule body
        // here is ECHO and the tie-breaks pick the same text.
        if is_op_char(c) {
            let n = self.operator_length();
            self.echo(n, out);
            return None;
        }

        // {numericfail} `1..10` throws back the `..` and lexes as an integer
        // (`psqlscan.l:911`).
        if c.is_ascii_digit() || (c == b'.' && rest.get(1).is_some_and(u8::is_ascii_digit)) {
            let n = match_number(rest);
            self.echo(n, out);
            return None;
        }

        if is_ident_start(c) {
            let n = 1 + rest[1..].iter().take_while(|&&b| is_ident_cont(b)).count();
            let ident = rest[..n].to_vec();
            self.track_identifier(&ident);
            self.echo(n, out);
            return None;
        }

        if is_self(c) {
            self.echo(1, out);
            return None;
        }

        // {other}
        self.echo(1, out);
        None
    }

    /// The four `:`-prefixed psql variable rules and their no-backup
    /// companions (`psqlscan.l:725`-`:800`).
    fn step_colon(&mut self, out: &mut Vec<u8>, vars: &dyn VariableSource) {
        let rest = self.rest().to_vec();

        // {colon_equals} `:=` and {typecast} `::` are operators, not variables.
        if rest.get(1) == Some(&b'=') || rest.get(1) == Some(&b':') {
            let n = self.operator_length();
            self.echo(n, out);
            return;
        }

        // `:'name'` and `:"name"`
        if let Some(&delim @ (b'\'' | b'"')) = rest.get(1) {
            let name_len = rest[2..]
                .iter()
                .take_while(|&&b| is_variable_char(b))
                .count();
            if name_len > 0 && rest.get(2 + name_len) == Some(&delim) {
                let name = String::from_utf8_lossy(&rest[2..2 + name_len]).into_owned();
                let quote = if delim == b'\'' {
                    QuoteType::SqlLiteral
                } else {
                    QuoteType::SqlIdent
                };
                let value = vars.get_variable(&name, quote).unwrap_or_else(|| {
                    // `psqlscan_escape_variable` emits the *name*, quoted, when
                    // the variable is unset (`psqlscan.l:1592`).
                    if delim == b'\'' {
                        escape_literal("")
                    } else {
                        escape_identifier(&name)
                    }
                });
                self.skip(3 + name_len);
                out.extend_from_slice(value.as_bytes());
                return;
            }
            // No-backup rule: throw back everything but the colon.
            self.echo(1, out);
            return;
        }

        // `:{?name}`
        if rest.get(1) == Some(&b'{') && rest.get(2) == Some(&b'?') {
            let name_len = rest[3..]
                .iter()
                .take_while(|&&b| is_variable_char(b))
                .count();
            if name_len > 0 && rest.get(3 + name_len) == Some(&b'}') {
                let name = String::from_utf8_lossy(&rest[3..3 + name_len]).into_owned();
                let set = vars.get_variable(&name, QuoteType::Plain).is_some();
                self.skip(4 + name_len);
                out.extend_from_slice(if set { b"TRUE" } else { b"FALSE" });
                return;
            }
            self.echo(1, out);
            return;
        }

        // `:name`
        let name_len = rest[1..]
            .iter()
            .take_while(|&&b| is_variable_char(b))
            .count();
        if name_len == 0 {
            // Falls through to {self}.
            self.echo(1, out);
            return;
        }
        let name = String::from_utf8_lossy(&rest[1..=name_len]).into_owned();
        match vars.get_variable(&name, QuoteType::Plain) {
            Some(value) => {
                if self.var_is_current_source(&name) {
                    // Recursive expansion — copy the string as is
                    // (`psqlscan.l:760`).
                    self.echo(1 + name_len, out);
                } else {
                    self.skip(1 + name_len);
                    self.stack.push(Buf {
                        data: value.into_bytes(),
                        pos: 0,
                        varname: Some(name),
                    });
                }
            }
            None => self.echo(1 + name_len, out),
        }
    }

    /// `psqlscan_var_is_current_source()` (`psqlscan.l:1517`).
    fn var_is_current_source(&self, name: &str) -> bool {
        self.stack
            .iter()
            .any(|b| b.varname.as_deref() == Some(name))
    }

    /// The `{operator}` rule (`psqlscan.l:809`-`:878`): how many bytes of the
    /// op_chars run actually belong to this token.
    fn operator_length(&self) -> usize {
        let rest = self.rest();
        let yyleng = rest.iter().take_while(|&&b| is_op_char(b)).count();
        let mut nchars = yyleng;

        // Embedded slash-star or dash-dash stops the operator there.
        let slashstar = find(&rest[..yyleng], b"/*");
        let dashdash = find(&rest[..yyleng], b"--");
        let stop = match (slashstar, dashdash) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if let Some(stop) = stop {
            nchars = stop;
        }

        // A trailing `+`/`-` is only kept when the operator holds a character
        // that is not in the SQL operator set.
        if nchars > 1 && matches!(rest[nchars - 1], b'+' | b'-') {
            let qualifies = rest[..nchars - 1]
                .iter()
                .rev()
                .any(|&b| qualifies_trailing_sign(b));
            if !qualifies {
                while nchars > 1 && matches!(rest[nchars - 1], b'+' | b'-') {
                    nchars -= 1;
                }
            }
        }

        // An operator that shrank to nothing is a single self char: that is
        // what `/*` and `--` at offset 0 do, and those matched earlier rules.
        nchars.max(1)
    }

    /// `psqlscan_track_identifier()` (`psqlscan.l:1059`).
    fn track_identifier(&mut self, identifier: &[u8]) {
        if self.state.paren_depth != 0 {
            return;
        }
        if self.state.init_idents_count == 0 {
            self.state.init_idents = [0; 8];
        }
        self.record_initial_keyword(identifier);

        if is_create_routine(self.state.init_idents) {
            if eq_ignore_case(identifier, b"begin") {
                self.state.begin_depth += 1;
            } else if eq_ignore_case(identifier, b"case") {
                if self.state.begin_depth >= 1 {
                    self.state.begin_depth += 1;
                }
            } else if eq_ignore_case(identifier, b"end") && self.state.begin_depth > 0 {
                self.state.begin_depth -= 1;
            }
        }
    }

    /// `psqlscan_record_initial_keyword()` (`psqlscan.l:970`).
    fn record_initial_keyword(&mut self, identifier: &[u8]) {
        let n = self.state.init_idents_count;
        if n < self.state.init_idents.len() {
            let first = identifier[0];
            if ["create", "function", "procedure", "or", "replace"]
                .iter()
                .any(|kw| eq_ignore_case(identifier, kw.as_bytes()))
            {
                self.state.init_idents[n] = first.to_ascii_lowercase();
            } else if ["copy", "from", "stdin", "stdout"]
                .iter()
                .any(|kw| eq_ignore_case(identifier, kw.as_bytes()))
            {
                self.state.init_idents[n] = first.to_ascii_uppercase();
            }
            self.state.init_idents_count = n + 1;
        }
    }

    /// `psqlscan_is_copy_from_stdin()` (`psqlscan.l:1017`).
    fn is_copy_from_stdin(&self) -> bool {
        let idents = &self.state.init_idents;
        if idents[0] != b'C' {
            return false;
        }
        for i in 1..idents.len() - 1 {
            if idents[i] != b'F' {
                continue;
            }
            return idents[i + 1] == b'S';
        }
        false
    }
}

/// `psqlscan_is_create_routine()` (`psqlscan.l:1005`).
fn is_create_routine(idents: [u8; 8]) -> bool {
    idents[0] == b'c'
        && (idents[1] == b'f'
            || idents[1] == b'p'
            || (idents[1] == b'o' && idents[2] == b'r' && (idents[3] == b'f' || idents[3] == b'p')))
}

fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Is `c` a digit in `radix` (2, 8 or 16)?
fn is_digit_in_radix(c: u8, radix: u32) -> bool {
    char::from(c).is_digit(radix)
}

/// `{xcstop}  \*+\/`
fn match_xcstop(rest: &[u8]) -> Option<usize> {
    let stars = rest.iter().take_while(|&&c| c == b'*').count();
    if stars > 0 && rest.get(stars) == Some(&b'/') {
        Some(stars + 1)
    } else {
        None
    }
}

/// `{dolqdelim}  \$({dolq_start}{dolq_cont}*)?\$`
fn match_dolqdelim(rest: &[u8]) -> Option<usize> {
    if rest.first() != Some(&b'$') {
        return None;
    }
    if rest.get(1) == Some(&b'$') {
        return Some(2);
    }
    let start = *rest.get(1)?;
    if !is_dolq_start(start) {
        return None;
    }
    let tag = 1 + rest[2..].iter().take_while(|&&c| is_dolq_cont(c)).count();
    if rest.get(1 + tag) == Some(&b'$') {
        Some(2 + tag)
    } else {
        None
    }
}

/// The `<xe>` backslash rules, longest match first (`psqlscan.l:528`-`:548`).
fn match_xe_escape(rest: &[u8]) -> usize {
    // {xeunicode}  [\\](u[0-9A-Fa-f]{4}|U[0-9A-Fa-f]{8})
    // {xeunicodefail}  [\\](u[0-9A-Fa-f]{0,3}|U[0-9A-Fa-f]{0,7})
    if let Some(&tag @ (b'u' | b'U')) = rest.get(1) {
        let want = if tag == b'u' { 4 } else { 8 };
        let have = rest[2..]
            .iter()
            .take_while(|&&c| c.is_ascii_hexdigit())
            .count()
            .min(want);
        return 2 + have;
    }
    // {xehexesc}  [\\]x[0-9A-Fa-f]{1,2}
    if rest.get(1) == Some(&b'x') {
        let have = rest[2..]
            .iter()
            .take_while(|&&c| c.is_ascii_hexdigit())
            .count()
            .min(2);
        if have > 0 {
            return 2 + have;
        }
    }
    // {xeoctesc}  [\\][0-7]{1,3}
    if let Some(&d) = rest.get(1) {
        if (b'0'..=b'7').contains(&d) {
            let have = rest[1..]
                .iter()
                .take_while(|&&c| (b'0'..=b'7').contains(&c))
                .count()
                .min(3);
            return 1 + have;
        }
        // {xeescape}  [\\][^0-7]
        return 2;
    }
    // The `.` rule, "only needed for \ just before EOF".
    1
}

/// `{quotecontinue}  {whitespace_with_newline}{quote}` (`psqlscan.l:175`).
fn match_quotecontinue(rest: &[u8]) -> Option<usize> {
    let mut i = 0;
    // {non_newline_whitespace}* — spaces or `--` comments, no newline yet.
    loop {
        if i < rest.len() && is_non_newline_space(rest[i]) {
            i += 1;
        } else if rest[i..].starts_with(b"--") {
            i += rest[i..].iter().take_while(|&&c| !is_newline(c)).count();
        } else {
            break;
        }
    }
    // {newline}
    if i >= rest.len() || !is_newline(rest[i]) {
        return None;
    }
    i += 1;
    // {special_whitespace}*  ({space}+|{comment}{newline})*
    loop {
        if i < rest.len() && is_space(rest[i]) {
            i += 1;
        } else if rest[i..].starts_with(b"--") {
            let comment = rest[i..].iter().take_while(|&&c| !is_newline(c)).count();
            if rest.get(i + comment).is_some_and(|&c| is_newline(c)) {
                i += comment + 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    if rest.get(i) == Some(&b'\'') {
        Some(i + 1)
    } else {
        None
    }
}

/// The numeric rules (`psqlscan.l:880`-`:931`): how many bytes this number
/// token covers, including the `{numericfail}` throw-back of a trailing `..`.
fn match_number(rest: &[u8]) -> usize {
    let digits = |from: usize| {
        rest[from..]
            .iter()
            .take_while(|&&c| c.is_ascii_digit() || c == b'_')
            .count()
    };

    // {hexinteger} / {octinteger} / {bininteger} and their fail rules.
    if rest[0] == b'0'
        && let Some(&tag) = rest.get(1)
    {
        {
            let radix = match tag {
                b'x' | b'X' => Some(16),
                b'o' | b'O' => Some(8),
                b'b' | b'B' => Some(2),
                _ => None,
            };
            if let Some(radix) = radix {
                let n = rest[2..]
                    .iter()
                    .take_while(|&&c| c == b'_' || is_digit_in_radix(c, radix))
                    .count();
                // {hexfail} etc. still consume `0x` (plus an optional `_`).
                let n = if n == 0 {
                    usize::from(rest.get(2) == Some(&b'_'))
                } else {
                    n
                };
                let junk = rest[2 + n..]
                    .iter()
                    .take_while(|&&c| is_ident_cont(c))
                    .count();
                return 2 + n + junk;
            }
        }
    }

    let mut i = digits(0);
    let int_end = i;

    // {numericfail}: `1..10` throws back the `..`.
    if rest[i..].starts_with(b"..") {
        return int_end;
    }
    if rest.get(i) == Some(&b'.') {
        i += 1 + digits(i + 1);
    }
    // {real} / {realfail}
    if let Some(&e) = rest.get(i)
        && matches!(e, b'e' | b'E')
    {
        {
            let mut j = i + 1;
            if matches!(rest.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            let exp = digits(j);
            // {realfail} consumes the sign even with no digits after it.
            i = if exp > 0 { j + exp } else { j };
        }
    }
    // {integer_junk} / {numeric_junk} / {real_junk}
    i + rest[i..].iter().take_while(|&&c| is_ident_cont(c)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one line through the lexer, returning every scan result and the
    /// query buffer at the point each one fired.
    fn scan_line(line: &str) -> Vec<(ScanResult, String)> {
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut out = Vec::new();
        let mut results = Vec::new();
        loop {
            let (res, _) = scanner.scan(&mut out, &NoVariables);
            results.push((res, String::from_utf8_lossy(&out).into_owned()));
            match res {
                ScanResult::Semicolon => out.clear(),
                ScanResult::Backslash | ScanResult::Eol | ScanResult::Incomplete => break,
            }
        }
        results
    }

    fn first(line: &str) -> (ScanResult, String) {
        scan_line(line).remove(0)
    }

    #[test]
    fn semicolon_ends_a_statement() {
        assert_eq!(
            first("select 1;"),
            (ScanResult::Semicolon, "select 1;".to_string())
        );
    }

    #[test]
    fn leading_whitespace_is_suppressed_until_there_is_data() {
        // `psqlscan.l:405`: whitespace is suppressed until some
        // non-whitespace data has been collected.
        assert_eq!(
            first("   select 1;"),
            (ScanResult::Semicolon, "select 1;".to_string())
        );
    }

    #[test]
    fn semicolon_inside_parens_does_not_end_the_statement() {
        let (res, _) = first("select (1;");
        assert_eq!(res, ScanResult::Incomplete);
        let mut scanner = Scanner::new();
        scanner.setup(b"select (1;", true);
        let mut out = Vec::new();
        let (_, prompt) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(prompt, PromptStatus::Paren);
        assert_eq!(scanner.paren_depth(), 1);
    }

    #[test]
    fn semicolon_inside_a_quoted_string_does_not_end_the_statement() {
        let (res, _) = first("select 'a;b'");
        assert_eq!(res, ScanResult::Eol);
        let (res, _) = first("select 'a;b");
        assert_eq!(res, ScanResult::Incomplete);
    }

    #[test]
    fn dollar_quoting_is_opaque() {
        // The comment at `psqlscan.l:227`: no processing of the quoted text.
        let (res, buf) = first("select $$a;b$$;");
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(buf, "select $$a;b$$;");

        // `$delim$...$junk$delim$` from the comment at `psqlscan.l:576`.
        let (res, buf) = first("select $d$x$junk$d$;");
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(buf, "select $d$x$junk$d$;");
    }

    #[test]
    fn dollar_quote_left_open_is_incomplete() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select $$a;b", true);
        let mut out = Vec::new();
        let (res, prompt) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Incomplete);
        assert_eq!(prompt, PromptStatus::DollarQuote);
        assert_eq!(scanner.start_state(), StartState::Xdolq);
    }

    #[test]
    fn nested_slash_star_comments_need_matching_stops() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select /* a /* b */ ;", true);
        let mut out = Vec::new();
        let (res, prompt) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Incomplete);
        assert_eq!(prompt, PromptStatus::Comment);

        let (res, _) = first("select /* a /* b */ */ 1;");
        assert_eq!(res, ScanResult::Semicolon);
    }

    #[test]
    fn dash_dash_comment_runs_to_end_of_line() {
        // `psqlscan.l:146`: with no newline after --, absorb to end of input.
        let (res, buf) = first("select 1 -- ;");
        assert_eq!(res, ScanResult::Eol);
        assert_eq!(buf, "select 1 -- ;");
    }

    #[test]
    fn plus_slash_star_lexes_as_operator_then_comment() {
        // The comment at `psqlscan.l:263`: plus-slash-star is a `+` operator
        // and a comment start, not a three-character operator.
        let mut scanner = Scanner::new();
        scanner.setup(b"select 1 +/* c */ 2;", true);
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(String::from_utf8_lossy(&out), "select 1 +/* c */ 2;");
    }

    #[test]
    fn equals_dash_dash_lexes_as_equals_then_comment() {
        // The trailing `-` is dropped from `=--` (`psqlscan.l:824`), leaving
        // `=` and a comment that swallows the semicolon.
        let mut scanner = Scanner::new();
        scanner.setup(b"select 1 =-- ;", true);
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Eol);
    }

    #[test]
    fn trailing_sign_survives_a_non_sql_operator_character() {
        // `?-` is a legal operator name (`psqlscan.l:838`), so the `-` stays.
        let mut scanner = Scanner::new();
        scanner.setup(b"select a ?- b;", true);
        let mut out = Vec::new();
        let (res, buf) = (scanner.scan(&mut out, &NoVariables).0, out.clone());
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(String::from_utf8_lossy(&buf), "select a ?- b;");
    }

    #[test]
    fn dot_dot_throws_back_from_an_integer() {
        // `1..10` lexes as 1, dot_dot, 10 (`psqlscan.l:322`).
        assert_eq!(match_number(b"1..10"), 1);
        assert_eq!(match_number(b"1.10"), 4);
        assert_eq!(match_number(b"0x1234"), 6);
        assert_eq!(match_number(b"1e-3"), 4);
    }

    #[test]
    fn a_backslash_stops_the_scan() {
        let mut scanner = Scanner::new();
        scanner.setup(b"\\q", true);
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Backslash);
    }

    #[test]
    fn backslash_semicolon_forces_a_semicolon_into_the_buffer() {
        // `"\\"[;:]` emits the second character only (`psqlscan.l:700`).
        let mut scanner = Scanner::new();
        scanner.setup(b"select 1\\;", true);
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Eol);
        assert_eq!(String::from_utf8_lossy(&out), "select 1;");
    }

    #[test]
    fn string_continuation_needs_a_newline() {
        // SQL requires at least one newline between concatenated literals
        // (`psqlscan.l:167`).
        assert!(match_quotecontinue(b"\n  '").is_some());
        assert!(match_quotecontinue(b"   '").is_none());
        assert!(match_quotecontinue(b" -- c\n'").is_some());

        let mut scanner = Scanner::new();
        scanner.setup(b"select 'a'\n'b';", true);
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(String::from_utf8_lossy(&out), "select 'a'\n'b';");
    }

    #[test]
    fn create_function_body_semicolons_do_not_end_the_statement() {
        // The BEGIN..END heuristic (`psqlscan.l:1040`).
        let mut scanner = Scanner::new();
        scanner.setup(
            b"CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END $$ LANGUAGE plpgsql;",
            true,
        );
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(
            String::from_utf8_lossy(&out),
            "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END $$ LANGUAGE plpgsql;"
        );
    }

    #[test]
    fn copy_from_stdin_is_counted() {
        let mut scanner = Scanner::new();
        scanner.setup(b"copy t from stdin;", true);
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(scanner.count_copy_from_stdin(), 1);

        let mut scanner = Scanner::new();
        scanner.setup(b"copy t to stdout;", true);
        let mut out = Vec::new();
        scanner.scan(&mut out, &NoVariables);
        assert_eq!(scanner.count_copy_from_stdin(), 0);
    }

    struct OneVar(&'static str, &'static str);

    impl VariableSource for OneVar {
        fn get_variable(&self, name: &str, quote: QuoteType) -> Option<String> {
            if name != self.0 {
                return None;
            }
            Some(match quote {
                QuoteType::Plain | QuoteType::ShellArg => self.1.to_string(),
                QuoteType::SqlLiteral => escape_literal(self.1),
                QuoteType::SqlIdent => escape_identifier(self.1),
            })
        }
    }

    #[test]
    fn a_set_variable_is_substituted() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select :n;", true);
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &OneVar("n", "42"));
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(String::from_utf8_lossy(&out), "select 42;");
    }

    #[test]
    fn an_unset_variable_is_copied_through() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select :n;", true);
        let mut out = Vec::new();
        scanner.scan(&mut out, &NoVariables);
        assert_eq!(String::from_utf8_lossy(&out), "select :n;");
    }

    #[test]
    fn quoted_variable_forms_are_escaped() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select :'n', :\"n\";", true);
        let mut out = Vec::new();
        scanner.scan(&mut out, &OneVar("n", "a'b"));
        assert_eq!(String::from_utf8_lossy(&out), "select 'a''b', \"a'b\";");
    }

    #[test]
    fn variable_existence_test_renders_true_or_false() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select :{?n}, :{?z};", true);
        let mut out = Vec::new();
        scanner.scan(&mut out, &OneVar("n", "1"));
        assert_eq!(String::from_utf8_lossy(&out), "select TRUE, FALSE;");
    }

    #[test]
    fn a_variable_inside_a_literal_is_not_substituted() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select ':n';", true);
        let mut out = Vec::new();
        scanner.scan(&mut out, &OneVar("n", "42"));
        assert_eq!(String::from_utf8_lossy(&out), "select ':n';");
    }

    #[test]
    fn recursive_expansion_is_refused() {
        struct SelfRef;
        impl VariableSource for SelfRef {
            fn get_variable(&self, name: &str, _quote: QuoteType) -> Option<String> {
                (name == "a").then(|| ":a".to_string())
            }
        }
        let mut scanner = Scanner::new();
        scanner.setup(b"select :a;", true);
        let mut out = Vec::new();
        let (res, _) = scanner.scan(&mut out, &SelfRef);
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(String::from_utf8_lossy(&out), "select :a;");
    }

    #[test]
    fn colon_equals_is_not_a_variable() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select a := :n;", true);
        let mut out = Vec::new();
        scanner.scan(&mut out, &OneVar("n", "1"));
        assert_eq!(String::from_utf8_lossy(&out), "select a := 1;");
    }

    #[test]
    fn non_standard_strings_enter_the_escape_state() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select 'a\\';", false);
        let mut out = Vec::new();
        let (res, prompt) = scanner.scan(&mut out, &NoVariables);
        // With standard_conforming_strings off the backslash escapes the
        // quote, so the literal is still open.
        assert_eq!(res, ScanResult::Incomplete);
        assert_eq!(prompt, PromptStatus::SingleQuote);
    }

    #[test]
    fn delimited_identifiers_hide_semicolons() {
        let (res, buf) = first("select \"a;b\";");
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(buf, "select \"a;b\";");
        let mut scanner = Scanner::new();
        scanner.setup(b"select \"a;b", true);
        let mut out = Vec::new();
        let (res, prompt) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Incomplete);
        assert_eq!(prompt, PromptStatus::DoubleQuote);
    }

    #[test]
    fn unicode_escape_starts_are_recognized() {
        let (res, buf) = first("select u&'\\0041';");
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(buf, "select u&'\\0041';");
        // {xufailed}: `u&` with neither quote throws back all but the `u`.
        let (res, buf) = first("select u&x;");
        assert_eq!(res, ScanResult::Semicolon);
        assert_eq!(buf, "select u&x;");
    }

    #[test]
    fn bit_and_hex_string_literals_have_their_own_states() {
        let mut scanner = Scanner::new();
        scanner.setup(b"select b'101", true);
        let mut out = Vec::new();
        let (res, prompt) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Incomplete);
        assert_eq!(prompt, PromptStatus::SingleQuote);
        assert_eq!(scanner.start_state(), StartState::Xb);

        let mut scanner = Scanner::new();
        scanner.setup(b"select x'ff", true);
        let mut out = Vec::new();
        scanner.scan(&mut out, &NoVariables);
        assert_eq!(scanner.start_state(), StartState::Xh);
    }

    #[test]
    fn national_character_start_eats_only_the_n() {
        // {xnstart} does yyless(1) (`psqlscan.l:474`), so the quote is lexed
        // by the normal rule and the state is xq, not something n-specific.
        let mut scanner = Scanner::new();
        scanner.setup(b"select n'abc", true);
        let mut out = Vec::new();
        scanner.scan(&mut out, &NoVariables);
        assert_eq!(scanner.start_state(), StartState::Xq);
    }

    #[test]
    fn multiple_statements_on_one_line_scan_one_at_a_time() {
        let results = scan_line("select 1; select 2;");
        assert_eq!(results[0].0, ScanResult::Semicolon);
        assert_eq!(results[0].1, "select 1;");
        assert_eq!(results[1].0, ScanResult::Semicolon);
        assert_eq!(results[1].1, "select 2;");
    }

    #[test]
    fn an_empty_buffer_is_never_sent() {
        // `psqlscan.l:1276`: never bother to send an empty buffer.
        let mut scanner = Scanner::new();
        scanner.setup(b"   ", true);
        let mut out = Vec::new();
        let (res, prompt) = scanner.scan(&mut out, &NoVariables);
        assert_eq!(res, ScanResult::Incomplete);
        assert_eq!(prompt, PromptStatus::Ready);
    }
}
