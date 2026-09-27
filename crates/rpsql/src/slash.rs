//! The backslash-command lexer: `src/bin/psql/psqlscanslash.l`.
//!
//! Upstream bolts a second flex lexer onto the same buffer stack as
//! `psqlscan.l` (`psqlscan.h:9` calls it "a compatible add-on lexer"), so
//! this module extends [`Scanner`] rather than owning a buffer of its own. It
//! covers `<xslashcmd>`, `<xslashargstart>`, `<xslasharg>`, `<xslashquote>`,
//! `<xslashdquote>`, `<xslashwholeline>` and `<xslashend>`;
//! `<xslashbackquote>` runs a shell and is an action, so it stops at
//! [`SlashOption::backquote`] for the caller to decide about.

use crate::scan::{QuoteType, Scanner, VariableSource, is_space, is_variable_char};

/// `enum slash_option_type` (`psqlscanslash.h:15`), for the three kinds this
/// port reads. `OT_SQLID` and `OT_SQLIDHACK` only post-process an `OT_NORMAL`
/// argument, and no command ported so far asks for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionType {
    /// `OT_NORMAL`: normal case.
    Normal,
    /// `OT_FILEPIPE`: it's a filename or pipe: a leading `|` takes the rest
    /// of the line (`psqlscanslash.l:164`).
    FilePipe,
    /// `OT_WHOLE_LINE`: just snarf the rest of the line.
    WholeLine,
}

/// One argument, with the quoting mark that produced it.
///
/// `psql_scan_slash_option`'s `quote` out-parameter (`psqlscanslash.l:527`) is
/// `'\0'` for an unquoted word, `'\''`, `'"'`, '`' or `':'`; commands read it
/// to tell `\echo -n` from `\echo '-n'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashOption {
    /// The argument text, after quote removal and variable substitution.
    pub value: String,
    /// The quoting mark, or `None` for an unquoted word.
    pub quote: Option<char>,
}

impl SlashOption {
    /// Was this argument produced by a backquote, i.e. a shell command that
    /// this issue does not run?
    #[must_use]
    pub fn backquote(&self) -> bool {
        self.quote == Some('`')
    }
}

/// End of input inside a quoted argument: `psql_scan_slash_option` logs
/// `unterminated quoted string` and returns NULL (`psqlscanslash.l:628`-`:634`).
///
/// The lexer stays pure, so the caller does the logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnterminatedQuote;

impl Scanner {
    /// `psql_scan_slash_command()` (`psqlscanslash.l:480`): the command name,
    /// which ends at whitespace or a backslash (`:145`).
    pub fn slash_command(&mut self) -> String {
        let rest = self.rest();
        let n = rest
            .iter()
            .take_while(|&&c| !is_space(c) && c != b'\\')
            .count();
        let name = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.skip(n);
        name
    }

    /// `psql_scan_slash_option()` (`psqlscanslash.l:539`) with a NULL
    /// `semicolon`: the next argument, `Ok(None)` at end of command.
    ///
    /// # Errors
    /// [`UnterminatedQuote`] when the input ends inside a quote; the argument
    /// is consumed and thrown away, as upstream does.
    pub fn slash_option(
        &mut self,
        vars: &dyn VariableSource,
        option_type: OptionType,
    ) -> Result<Option<SlashOption>, UnterminatedQuote> {
        if option_type == OptionType::WholeLine {
            return Ok(self.whole_line(Vec::new()));
        }

        // <xslashargstart>: discard whitespace before the argument.
        let skip = self.rest().iter().take_while(|&&c| is_space(c)).count();
        self.skip(skip);

        // "|" is only special at the start of an OT_FILEPIPE argument,
        // where it is kept and takes the rest of the line (`:164`).
        if option_type == OptionType::FilePipe && self.rest().first() == Some(&b'|') {
            self.skip(1);
            return Ok(self.whole_line(vec![b'|']));
        }

        let mut out = Vec::new();
        let mut quote: Option<char> = None;
        loop {
            let rest = self.rest().to_vec();
            let Some(&c) = rest.first() else { break };
            // <xslasharg>{space}|"\\": unquoted space or backslash ends the
            // argument and is not eaten (`psqlscanslash.l:195`).
            if is_space(c) || c == b'\\' {
                break;
            }
            match c {
                b'\'' => {
                    quote = Some('\'');
                    self.skip(1);
                    if !self.read_single_quoted(&mut out) {
                        return Err(UnterminatedQuote);
                    }
                }
                b'"' => {
                    quote = Some('"');
                    self.skip(1);
                    out.push(b'"');
                    if !self.read_double_quoted(&mut out) {
                        return Err(UnterminatedQuote);
                    }
                }
                b'`' => {
                    quote = Some('`');
                    self.skip(1);
                    let n = self.rest().iter().take_while(|&&b| b != b'`').count();
                    out.extend_from_slice(&self.rest()[..n]);
                    if self.rest().len() == n {
                        self.skip(n);
                        return Err(UnterminatedQuote);
                    }
                    self.skip(n + 1);
                }
                b':' => {
                    if self.read_variable(&mut out, vars) {
                        quote = Some(':');
                    }
                }
                _ => {
                    self.skip(1);
                    out.push(c);
                }
            }
        }

        // An unquoted empty argument means end of command (`:664`).
        if out.is_empty() && quote.is_none() {
            return Ok(None);
        }
        Ok(Some(SlashOption {
            value: String::from_utf8_lossy(&out).into_owned(),
            quote,
        }))
    }

    /// `psql_scan_slash_command_end()` (`psqlscanslash.l:678`): swallow a
    /// trailing `\\`, which separates one backslash command from the next,
    /// and nothing else (`<xslashend>`, `:436`).
    pub fn slash_command_end(&mut self) {
        if self.rest().starts_with(b"\\\\") {
            self.skip(2);
        }
    }

    /// `<xslashwholeline>` (`psqlscanslash.l:423`): everything to the end of
    /// the line, with only the whitespace before the first character dropped.
    fn whole_line(&mut self, mut out: Vec<u8>) -> Option<SlashOption> {
        let rest = self.rest().to_vec();
        self.skip(rest.len());
        let mut i = 0;
        while i < rest.len() {
            let run = rest[i..].iter().take_while(|&&c| is_space(c)).count();
            if run > 0 {
                if !out.is_empty() {
                    out.extend_from_slice(&rest[i..i + run]);
                }
                i += run;
            } else {
                out.push(rest[i]);
                i += 1;
            }
        }
        (!out.is_empty()).then(|| SlashOption {
            value: String::from_utf8_lossy(&out).into_owned(),
            quote: None,
        })
    }

    /// `<xslashquote>` (`psqlscanslash.l:321`-`:351`): `''` is a quote, and a
    /// backslash escapes `\n`, `\t`, `\b`, `\r`, `\f`, one to three octal
    /// digits, `x` and one or two hex digits, or any other character.
    /// Returns false if the input ended before the closing quote.
    fn read_single_quoted(&mut self, out: &mut Vec<u8>) -> bool {
        loop {
            let rest = self.rest().to_vec();
            let Some(&c) = rest.first() else { return false };
            match c {
                b'\'' if rest.get(1) == Some(&b'\'') => {
                    self.skip(2);
                    out.push(b'\'');
                }
                b'\'' => {
                    self.skip(1);
                    return true;
                }
                b'\\' if rest.len() > 1 => {
                    let (byte, len) = single_quote_escape(&rest[1..]);
                    self.skip(1 + len);
                    out.push(byte);
                }
                // A lone backslash at the end is `{other}` (`:351`).
                _ => {
                    self.skip(1);
                    out.push(c);
                }
            }
        }
    }

    /// `<xslashdquote>` (`psqlscanslash.l:411`): everything up to the closing
    /// double quote, which stays in the value. Returns false if the input
    /// ended first.
    fn read_double_quoted(&mut self, out: &mut Vec<u8>) -> bool {
        let rest = self.rest().to_vec();
        let n = rest.iter().take_while(|&&c| c != b'"').count();
        out.extend_from_slice(&rest[..n]);
        self.skip(n);
        if rest.len() > n {
            self.skip(1);
            out.push(b'"');
            true
        } else {
            false
        }
    }

    /// The `:`-prefixed rules of `<xslasharg>` (`psqlscanslash.l:230`-`:312`).
    /// Returns whether the rule marks the argument as `:`-quoted, which every
    /// substitution rule does whether or not the variable is set.
    fn read_variable(&mut self, out: &mut Vec<u8>, vars: &dyn VariableSource) -> bool {
        let rest = self.rest().to_vec();

        if let Some(&delim @ (b'\'' | b'"')) = rest.get(1) {
            let len = rest[2..]
                .iter()
                .take_while(|&&b| is_variable_char(b))
                .count();
            if len > 0 && rest.get(2 + len) == Some(&delim) {
                let name = String::from_utf8_lossy(&rest[2..2 + len]).into_owned();
                let quote = if delim == b'\'' {
                    QuoteType::SqlLiteral
                } else {
                    QuoteType::SqlIdent
                };
                // `psqlscan_escape_variable`: the value, or the token as
                // typed when unset (`psqlscan.l:1727`-`:1737`).
                match vars.get_variable(&name, quote) {
                    Some(value) => out.extend_from_slice(value.as_bytes()),
                    None => out.extend_from_slice(&rest[..3 + len]),
                }
                self.skip(3 + len);
                return true;
            }
            // Throw back everything but the colon.
            self.skip(1);
            out.push(b':');
            return false;
        }

        if rest.get(1) == Some(&b'{') && rest.get(2) == Some(&b'?') {
            let len = rest[3..]
                .iter()
                .take_while(|&&b| is_variable_char(b))
                .count();
            if len > 0 && rest.get(3 + len) == Some(&b'}') {
                let name = String::from_utf8_lossy(&rest[3..3 + len]).into_owned();
                let set = vars.get_variable(&name, QuoteType::Plain).is_some();
                self.skip(4 + len);
                out.extend_from_slice(if set { b"TRUE" } else { b"FALSE" });
                return false;
            }
            self.skip(1);
            out.push(b':');
            return false;
        }

        let len = rest[1..]
            .iter()
            .take_while(|&&b| is_variable_char(b))
            .count();
        if len == 0 {
            self.skip(1);
            out.push(b':');
            return false;
        }
        let name = String::from_utf8_lossy(&rest[1..=len]).into_owned();
        self.skip(1 + len);
        match vars.get_variable(&name, QuoteType::Plain) {
            Some(value) => out.extend_from_slice(value.as_bytes()),
            // The text is emitted as typed when the variable is unset.
            None => out.extend_from_slice(&rest[..=len]),
        }
        // `*option_quote = ':'` whether or not a value was found (`:262`).
        true
    }
}

/// The escape after a backslash inside `<xslashquote>`, given the bytes after
/// the backslash (at least one): the byte it stands for and how many bytes of
/// `after` it consumed.
///
/// Flex takes the longest match, so `{xeoctesc}` (`[\\][0-7]{1,3}`) and
/// `{xehexesc}` (`[\\]x[0-9A-Fa-f]{1,2}`) beat `"\\".`; `(char) strtol(…)`
/// keeps the low byte of an octal value above `\377` (`:337`-`:347`).
fn single_quote_escape(after: &[u8]) -> (u8, usize) {
    let octal = after
        .iter()
        .take(3)
        .take_while(|c| (b'0'..=b'7').contains(c))
        .count();
    if octal > 0 {
        let value = after[..octal]
            .iter()
            .fold(0u32, |acc, &d| acc * 8 + u32::from(d - b'0'));
        return (value.to_le_bytes()[0], octal);
    }
    if after[0] == b'x' {
        let hex = after[1..]
            .iter()
            .take(2)
            .take_while(|c| c.is_ascii_hexdigit())
            .count();
        if hex > 0 {
            let digits = std::str::from_utf8(&after[1..=hex]).expect("hex digits are ASCII");
            let value = u8::from_str_radix(digits, 16).expect("at most two hex digits");
            return (value, 1 + hex);
        }
    }
    let byte = match after[0] {
        b'n' => b'\n',
        b't' => b'\t',
        b'b' => 0x08,
        b'r' => b'\r',
        b'f' => 0x0c,
        other => other,
    };
    (byte, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{NoVariables, ScanResult};

    /// Drive the SQL lexer to the backslash, then read the command.
    fn slash(line: &str, vars: &dyn VariableSource) -> (String, Vec<SlashOption>) {
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        let (res, _) = scanner.scan(&mut buf, vars);
        assert_eq!(res, ScanResult::Backslash);
        let cmd = scanner.slash_command();
        let options = all_options(&mut scanner, vars);
        (cmd, options)
    }

    /// Every remaining `OT_NORMAL` argument.
    fn all_options(scanner: &mut Scanner, vars: &dyn VariableSource) -> Vec<SlashOption> {
        let mut all = Vec::new();
        while let Some(option) = scanner.slash_option(vars, OptionType::Normal).unwrap() {
            all.push(option);
        }
        all
    }

    /// Drive the SQL lexer to the backslash and read the command name, leaving
    /// the scanner positioned at its first argument.
    fn at_arguments(line: &str) -> Scanner {
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        assert_eq!(
            scanner.scan(&mut buf, &NoVariables).0,
            ScanResult::Backslash
        );
        scanner.slash_command();
        scanner
    }

    fn values(options: &[SlashOption]) -> Vec<&str> {
        options.iter().map(|o| o.value.as_str()).collect()
    }

    #[test]
    fn a_command_name_ends_at_whitespace() {
        let (cmd, options) = slash("\\echo hello world", &NoVariables);
        assert_eq!(cmd, "echo");
        assert_eq!(values(&options), ["hello", "world"]);
    }

    #[test]
    fn a_command_with_no_arguments_has_none() {
        let (cmd, options) = slash("\\q", &NoVariables);
        assert_eq!(cmd, "q");
        assert!(options.is_empty());
    }

    #[test]
    fn single_quotes_group_and_are_removed() {
        let (_, options) = slash("\\echo 'a b' c", &NoVariables);
        assert_eq!(values(&options), ["a b", "c"]);
        assert_eq!(options[0].quote, Some('\''));
        assert_eq!(options[1].quote, None);
    }

    #[test]
    fn a_doubled_quote_inside_a_quoted_argument_is_one_quote() {
        let (_, options) = slash("\\echo 'it''s'", &NoVariables);
        assert_eq!(values(&options), ["it's"]);
    }

    #[test]
    fn backslash_escapes_inside_single_quotes_are_expanded() {
        // `psqlscanslash.l:331`-`:347`.
        let (_, options) = slash("\\echo 'a\\tb'", &NoVariables);
        assert_eq!(values(&options), ["a\tb"]);
    }

    #[test]
    fn double_quotes_are_kept_in_the_value() {
        // `<xslasharg>{dquote}` echoes the quote (`psqlscanslash.l:223`).
        let (_, options) = slash("\\echo \"a b\"", &NoVariables);
        assert_eq!(values(&options), ["\"a b\""]);
        assert_eq!(options[0].quote, Some('"'));
    }

    #[test]
    fn a_variable_argument_is_substituted_and_marked() {
        struct V;
        impl VariableSource for V {
            fn get_variable(&self, name: &str, _quote: QuoteType) -> Option<String> {
                (name == "x").then(|| "42".to_string())
            }
        }
        let (_, options) = slash("\\echo :x", &V);
        assert_eq!(values(&options), ["42"]);
        assert_eq!(options[0].quote, Some(':'));
    }

    #[test]
    fn an_unset_variable_argument_is_left_as_typed_but_still_marked() {
        // `*option_quote = ':'` is set whether or not the variable is
        // (`psqlscanslash.l:262`), so an unset `:x` is not an unquoted word.
        let (_, options) = slash("\\echo :x", &NoVariables);
        assert_eq!(values(&options), [":x"]);
        assert_eq!(options[0].quote, Some(':'));
    }

    #[test]
    fn unset_quoted_variable_arguments_are_left_as_typed() {
        // `psqlscan_escape_variable` emits the token itself when the callback
        // returns NULL (`psqlscan.l:1733`-`:1737`); this is what
        // `\echo :foo :'foo' :"foo"` prints in an inactive branch.
        let (_, options) = slash("\\echo :'x' :\"x\"", &NoVariables);
        assert_eq!(values(&options), [":'x'", ":\"x\""]);
        assert!(options.iter().all(|o| o.quote == Some(':')));
    }

    #[test]
    fn every_interpolation_form_substitutes_in_an_argument() {
        struct V;
        impl VariableSource for V {
            fn get_variable(&self, name: &str, quote: QuoteType) -> Option<String> {
                (name == "foo").then(|| match quote {
                    QuoteType::Plain => "b'r".to_string(),
                    QuoteType::SqlLiteral => crate::variables::escape_literal("b'r"),
                    QuoteType::SqlIdent => crate::variables::escape_identifier("b'r"),
                    QuoteType::ShellArg => unreachable!("no backquote here"),
                })
            }
        }
        let (_, options) = slash("\\echo :foo :'foo' :\"foo\" :{?foo} :{?bar}", &V);
        assert_eq!(
            values(&options),
            ["b'r", "'b''r'", "\"b'r\"", "TRUE", "FALSE"]
        );
    }

    #[test]
    fn an_incomplete_interpolation_keeps_only_the_colon_special() {
        // The no-backup rules throw back everything but the colon
        // (`psqlscanslash.l:286`-`:312`); what follows is lexed afresh, so
        // the quote opens an ordinary quoted argument.
        let (_, options) = slash("\\echo :'foo x' :{ :{?a", &NoVariables);
        assert_eq!(values(&options), [":foo x", ":{", ":{?a"]);
    }

    #[test]
    fn octal_and_hex_escapes_inside_single_quotes_are_expanded() {
        // `{xeoctesc}` and `{xehexesc}` (`psqlscanslash.l:337`-`:347`).
        let (_, options) = slash("\\echo '\\101\\x42\\x4a3\\q'", &NoVariables);
        assert_eq!(values(&options), ["ABJ3q"]);
    }

    #[test]
    fn an_unterminated_quote_is_an_error_not_an_argument() {
        for line in ["\\echo 'abc", "\\echo \"abc", "\\echo `abc", "\\echo 'a\\"] {
            let mut scanner = at_arguments(line);
            assert_eq!(
                scanner.slash_option(&NoVariables, OptionType::Normal),
                Err(UnterminatedQuote),
                "{line}"
            );
        }
    }

    #[test]
    fn a_whole_line_argument_takes_everything_but_its_leading_space() {
        // `<xslashwholeline>` (`psqlscanslash.l:423`-`:434`), backslashes and
        // all.
        let mut scanner = at_arguments("\\! \t whole  line \\endif ");
        let option = scanner
            .slash_option(&NoVariables, OptionType::WholeLine)
            .unwrap()
            .unwrap();
        assert_eq!(option.value, "whole  line \\endif ");
        assert_eq!(
            scanner.slash_option(&NoVariables, OptionType::Normal),
            Ok(None)
        );
    }

    #[test]
    fn a_filepipe_argument_starting_with_a_bar_takes_the_whole_line() {
        // `psqlscanslash.l:164`-`:175`.
        let mut scanner = at_arguments("\\w |/no/such/file \\else");
        let option = scanner
            .slash_option(&NoVariables, OptionType::FilePipe)
            .unwrap()
            .unwrap();
        assert_eq!(option.value, "|/no/such/file \\else");

        // Anywhere else a bar is ordinary, and without OT_FILEPIPE it is
        // ordinary even at the start.
        let mut scanner = at_arguments("\\w a|b \\else");
        let option = scanner
            .slash_option(&NoVariables, OptionType::FilePipe)
            .unwrap()
            .unwrap();
        assert_eq!(option.value, "a|b");
        let mut scanner = at_arguments("\\echo |x y");
        let option = scanner
            .slash_option(&NoVariables, OptionType::Normal)
            .unwrap()
            .unwrap();
        assert_eq!(option.value, "|x");
    }

    #[test]
    fn arguments_stop_at_the_end_of_a_variables_value() {
        // A divergence (docs/divergences.md): upstream would pop back to the
        // line at the value's end and read `b` too (`psqlscanslash.l:452`).
        struct V;
        impl VariableSource for V {
            fn get_variable(&self, name: &str, _quote: QuoteType) -> Option<String> {
                (name == "x").then(|| "\\echo a".to_string())
            }
        }
        let (cmd, options) = slash(":x b", &V);
        assert_eq!(cmd, "echo");
        assert_eq!(values(&options), ["a"]);
    }

    #[test]
    fn a_quoted_empty_argument_is_an_argument() {
        // Only an *unquoted* empty argument means end of command (`:664`).
        let (_, options) = slash("\\echo '' x", &NoVariables);
        assert_eq!(values(&options), ["", "x"]);
    }

    #[test]
    fn a_double_backslash_separates_two_commands() {
        let mut scanner = Scanner::new();
        scanner.setup(b"\\echo a \\\\ \\echo b", true);
        let mut buf = Vec::new();
        scanner.scan(&mut buf, &NoVariables);
        assert_eq!(scanner.slash_command(), "echo");
        assert_eq!(values(&all_options(&mut scanner, &NoVariables)), ["a"]);
        scanner.slash_command_end();
        let (res, _) = scanner.scan(&mut buf, &NoVariables);
        assert_eq!(res, ScanResult::Backslash);
        assert_eq!(scanner.slash_command(), "echo");
        assert_eq!(values(&all_options(&mut scanner, &NoVariables)), ["b"]);
    }

    #[test]
    fn a_backquoted_argument_is_flagged_rather_than_run() {
        let (_, options) = slash("\\echo `date`", &NoVariables);
        assert!(options[0].backquote());
    }
}
