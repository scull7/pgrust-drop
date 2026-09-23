//! The backslash-command lexer: `src/bin/psql/psqlscanslash.l`.
//!
//! Upstream bolts a second flex lexer onto the same buffer stack as
//! `psqlscan.l` (`psqlscan.h:9` calls it "a compatible add-on lexer"), so
//! this module extends [`Scanner`] rather than owning a buffer of its own. It
//! covers `<xslashcmd>`, `<xslashargstart>`, `<xslasharg>`, `<xslashquote>`,
//! `<xslashdquote>` and `<xslashend>`; `<xslashbackquote>` runs a shell and is
//! an action, so it stops at [`SlashOption::backquote`] for the caller to
//! decide about.

use crate::scan::{QuoteType, Scanner, VariableSource, is_variable_char};

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

impl Scanner {
    /// `psql_scan_slash_command()` (`psqlscanslash.l:480`): the command name,
    /// which ends at whitespace or a backslash.
    pub fn slash_command(&mut self) -> String {
        let rest = self.rest();
        let n = rest
            .iter()
            .take_while(|&&c| !c.is_ascii_whitespace() && c != b'\\')
            .count();
        let name = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.skip(n);
        name
    }

    /// `psql_scan_slash_option(OT_NORMAL)` (`psqlscanslash.l:539`): the next
    /// argument, or `None` at end of command.
    pub fn slash_option(&mut self, vars: &dyn VariableSource) -> Option<SlashOption> {
        // <xslashargstart>: discard whitespace before the argument.
        let skip = self
            .rest()
            .iter()
            .take_while(|c| c.is_ascii_whitespace())
            .count();
        self.skip(skip);

        let rest = self.rest();
        if rest.is_empty() || rest[0] == b'\\' {
            return None;
        }

        let mut out = Vec::new();
        let mut quote: Option<char> = None;
        loop {
            let rest = self.rest().to_vec();
            if rest.is_empty() {
                break;
            }
            let c = rest[0];
            // <xslasharg>{space}|"\\": unquoted space or backslash ends the
            // argument and is not eaten (`psqlscanslash.l:195`).
            if c.is_ascii_whitespace() || c == b'\\' {
                break;
            }
            match c {
                b'\'' => {
                    quote = Some('\'');
                    self.skip(1);
                    self.read_single_quoted(&mut out);
                }
                b'"' => {
                    quote = Some('"');
                    self.skip(1);
                    out.push(b'"');
                    self.read_double_quoted(&mut out);
                }
                b'`' => {
                    quote = Some('`');
                    self.skip(1);
                    let n = self.rest().iter().take_while(|&&b| b != b'`').count();
                    out.extend_from_slice(&self.rest()[..n]);
                    self.skip(n + usize::from(self.rest().len() > n));
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

        Some(SlashOption {
            value: String::from_utf8_lossy(&out).into_owned(),
            quote,
        })
    }

    /// Every remaining argument, which is how `HandleSlashCmds` eats the tail
    /// of a command line (`command.c:278`).
    pub fn slash_options(&mut self, vars: &dyn VariableSource) -> Vec<SlashOption> {
        let mut all = Vec::new();
        while let Some(option) = self.slash_option(vars) {
            all.push(option);
        }
        all
    }

    /// `psql_scan_slash_command_end()` (`psqlscanslash.l:678`): swallow a
    /// trailing `\\`, which separates one backslash command from the next.
    pub fn slash_command_end(&mut self) {
        let skip = self
            .rest()
            .iter()
            .take_while(|c| c.is_ascii_whitespace())
            .count();
        self.skip(skip);
        if self.rest().starts_with(b"\\\\") {
            self.skip(2);
        }
    }

    /// `<xslashquote>` (`psqlscanslash.l:321`): `''` is a quote, and the
    /// backslash escapes `\n`, `\t`, `\b`, `\r`, `\f`, `\digits`, `\xhex`.
    fn read_single_quoted(&mut self, out: &mut Vec<u8>) {
        loop {
            let rest = self.rest().to_vec();
            let Some(&c) = rest.first() else { return };
            match c {
                b'\'' if rest.get(1) == Some(&b'\'') => {
                    self.skip(2);
                    out.push(b'\'');
                }
                b'\'' => {
                    self.skip(1);
                    return;
                }
                b'\\' => {
                    let Some(&escape) = rest.get(1) else {
                        self.skip(1);
                        return;
                    };
                    self.skip(2);
                    match escape {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'r' => out.push(b'\r'),
                        b'f' => out.push(0x0c),
                        other => out.push(other),
                    }
                }
                _ => {
                    self.skip(1);
                    out.push(c);
                }
            }
        }
    }

    /// `<xslashdquote>` (`psqlscanslash.l:411`): everything up to the closing
    /// double quote, which stays in the value.
    fn read_double_quoted(&mut self, out: &mut Vec<u8>) {
        let rest = self.rest().to_vec();
        let n = rest.iter().take_while(|&&c| c != b'"').count();
        out.extend_from_slice(&rest[..n]);
        self.skip(n);
        if rest.len() > n {
            self.skip(1);
            out.push(b'"');
        }
    }

    /// The `:`-prefixed rules of `<xslasharg>` (`psqlscanslash.l:230`-`:312`).
    /// Returns whether a substitution actually happened.
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
                let value = vars.get_variable(&name, quote).unwrap_or_else(|| {
                    if delim == b'\'' {
                        crate::variables::escape_literal("")
                    } else {
                        crate::variables::escape_identifier(&name)
                    }
                });
                self.skip(3 + len);
                out.extend_from_slice(value.as_bytes());
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
        if let Some(value) = vars.get_variable(&name, QuoteType::Plain) {
            out.extend_from_slice(value.as_bytes());
            true
        } else {
            // The value is emitted as typed when the variable is unset.
            out.push(b':');
            out.extend_from_slice(name.as_bytes());
            false
        }
    }
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
        let options = scanner.slash_options(vars);
        (cmd, options)
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
    fn an_unset_variable_argument_is_left_as_typed() {
        let (_, options) = slash("\\echo :x", &NoVariables);
        assert_eq!(values(&options), [":x"]);
        assert_eq!(options[0].quote, None);
    }

    #[test]
    fn a_double_backslash_separates_two_commands() {
        let mut scanner = Scanner::new();
        scanner.setup(b"\\echo a \\\\ \\echo b", true);
        let mut buf = Vec::new();
        scanner.scan(&mut buf, &NoVariables);
        assert_eq!(scanner.slash_command(), "echo");
        assert_eq!(values(&scanner.slash_options(&NoVariables)), ["a"]);
        scanner.slash_command_end();
        let (res, _) = scanner.scan(&mut buf, &NoVariables);
        assert_eq!(res, ScanResult::Backslash);
        assert_eq!(scanner.slash_command(), "echo");
        assert_eq!(values(&scanner.slash_options(&NoVariables)), ["b"]);
    }

    #[test]
    fn a_backquoted_argument_is_flagged_rather_than_run() {
        let (_, options) = slash("\\echo `date`", &NoVariables);
        assert!(options[0].backquote());
    }
}
