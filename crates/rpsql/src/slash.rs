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

/// `dequote_downcase_identifier()` (`psqlscanslash.l:783`): strip the
/// double quotes from an identifier-ish argument, `""` inside quotes being
/// one literal quote, and with `downcase` fold the letters outside quotes to
/// lower case. `FOO"BAR"BAZ` becomes `fooBARbaz`.
///
/// Upstream folds with `pg_tolower`, stepping over multibyte characters, and
/// so under the C locale touches only ASCII `A`-`Z`. This port's arguments
/// are UTF-8, where every byte of a multibyte character is non-ASCII, so an
/// ASCII fold byte by byte is the same thing.
#[must_use]
pub fn dequote_downcase_identifier(s: &[u8], downcase: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut inquotes = false;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == b'"' {
            if inquotes && s.get(i + 1) == Some(&b'"') {
                // Keep the first quote, remove the second.
                out.push(b'"');
                i += 2;
                continue;
            }
            inquotes = !inquotes;
        } else if downcase && !inquotes {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
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
        self.slash_option_with(vars, false, false)
    }

    /// `psql_scan_slash_option(OT_FILEPIPE, NULL, semicolon)`
    /// (`psqlscanslash.l:165`): a `|` at the start of the argument makes it
    /// the whole rest of the line, `|` included, as `\g` and `\o` take a
    /// pipe; anything else is an ordinary argument. With `semicolon`, the
    /// argument's unquoted trailing semicolons are stripped, and for a pipe
    /// its trailing semicolons and whitespace (`:604`, `:635`).
    pub fn slash_option_filepipe(
        &mut self,
        vars: &dyn VariableSource,
        semicolon: bool,
    ) -> Option<SlashOption> {
        self.slash_option_with(vars, true, semicolon)
    }

    fn slash_option_with(
        &mut self,
        vars: &dyn VariableSource,
        filepipe: bool,
        semicolon: bool,
    ) -> Option<SlashOption> {
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
        if filepipe && rest[0] == b'|' {
            // "treat like whole-string case" (`psqlscanslash.l:167`): the
            // `|` is echoed, so every later space is kept too.
            let mut line = rest.to_vec();
            self.skip(line.len());
            if semicolon {
                while line
                    .last()
                    .is_some_and(|&c| c == b';' || c.is_ascii_whitespace() || c == 0x0b)
                {
                    line.pop();
                }
            }
            return Some(SlashOption {
                value: String::from_utf8_lossy(&line).into_owned(),
                quote: None,
            });
        }

        let mut out = Vec::new();
        let mut quote: Option<char> = None;
        // `unquoted_option_chars`: how many bytes at the end of the argument
        // were not quoted, which bounds the semicolons that may be stripped.
        let mut unquoted = 0usize;
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
                    unquoted = 0;
                    self.skip(1);
                    self.read_single_quoted(&mut out);
                }
                b'"' => {
                    quote = Some('"');
                    unquoted = 0;
                    self.skip(1);
                    out.push(b'"');
                    self.read_double_quoted(&mut out);
                }
                b'`' => {
                    quote = Some('`');
                    unquoted = 0;
                    self.skip(1);
                    let n = self.rest().iter().take_while(|&&b| b != b'`').count();
                    out.extend_from_slice(&self.rest()[..n]);
                    self.skip(n + usize::from(self.rest().len() > n));
                }
                b':' => match self.read_variable(&mut out, vars) {
                    VariableRule::Substituted => {
                        quote = Some(':');
                        unquoted = 0;
                    }
                    VariableRule::Unset => unquoted = 0,
                    VariableRule::Tested => {}
                    VariableRule::Colon => unquoted += 1,
                },
                _ => {
                    self.skip(1);
                    out.push(c);
                    unquoted += 1;
                }
            }
        }

        // Strip any unquoted trailing semicolons if requested
        // (`psqlscanslash.l:604`).
        if semicolon {
            while unquoted > 0 && out.last() == Some(&b';') {
                out.pop();
                unquoted -= 1;
            }
        }
        // "An unquoted empty argument isn't possible unless we are at end of
        // command. Return NULL instead." (`psqlscanslash.l:661`).
        if out.is_empty() && quote.is_none() {
            return None;
        }

        Some(SlashOption {
            value: String::from_utf8_lossy(&out).into_owned(),
            quote,
        })
    }

    /// `psql_scan_slash_option(OT_WHOLE_LINE, NULL, false)`
    /// (`psqlscanslash.l:423`, `:574`): the rest of the line, leading
    /// whitespace dropped and nothing else touched — no quotes, no
    /// variables, and no end at a backslash. `None` when nothing is left
    /// (`:661`).
    pub fn slash_option_whole_line(&mut self) -> Option<Vec<u8>> {
        // `{space}` is `[ \t\n\r\f\v]` (`psqlscanslash.l:104`).
        let skip = self
            .rest()
            .iter()
            .take_while(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b))
            .count();
        self.skip(skip);
        let line = self.rest().to_vec();
        self.skip(line.len());
        (!line.is_empty()).then_some(line)
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

    /// The `:`-prefixed rules of `<xslasharg>` (`psqlscanslash.l:230`-`:312`),
    /// and which of them matched.
    fn read_variable(&mut self, out: &mut Vec<u8>, vars: &dyn VariableSource) -> VariableRule {
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
                return VariableRule::Substituted;
            }
            // Throw back everything but the colon.
            self.skip(1);
            out.push(b':');
            return VariableRule::Colon;
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
                return VariableRule::Tested;
            }
            self.skip(1);
            out.push(b':');
            return VariableRule::Colon;
        }

        let len = rest[1..]
            .iter()
            .take_while(|&&b| is_variable_char(b))
            .count();
        if len == 0 {
            self.skip(1);
            out.push(b':');
            return VariableRule::Colon;
        }
        let name = String::from_utf8_lossy(&rest[1..=len]).into_owned();
        self.skip(1 + len);
        if let Some(value) = vars.get_variable(&name, QuoteType::Plain) {
            out.extend_from_slice(value.as_bytes());
            VariableRule::Substituted
        } else {
            // The value is emitted as typed when the variable is unset.
            out.push(b':');
            out.extend_from_slice(name.as_bytes());
            VariableRule::Unset
        }
    }
}

/// Which `:` rule of `<xslasharg>` matched, for the argument's quote mark
/// and its count of unquoted trailing bytes.
enum VariableRule {
    /// `:name` set, or `:'name'` / `:"name"`: the quote mark becomes `:`.
    Substituted,
    /// `:name` unset: echoed as typed, but counted as quoted.
    Unset,
    /// `:{?name}`, which touches neither.
    Tested,
    /// A lone `:`, thrown back as an ordinary character.
    Colon,
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

    #[test]
    fn dequote_downcase_identifier_strips_quotes_and_folds_what_they_do_not_cover() {
        let d =
            |s: &str| String::from_utf8(dequote_downcase_identifier(s.as_bytes(), true)).unwrap();
        // `psqlscanslash.l:776`'s own example.
        assert_eq!(d("FOO\"BAR\"BAZ"), "fooBARbaz");
        // `psql_crosstab.sql:38`: a doubled quote inside quotes is one quote.
        assert_eq!(d("\"\"\"month\"\" name\""), "\"month\" name");
        assert_eq!(d("\"22\""), "22");
        assert_eq!(d("B"), "b");
        assert_eq!(d("\"B\""), "B");
        // Only ASCII folds; a multibyte character passes through.
        assert_eq!(d("ÄB"), "Äb");
        assert_eq!(
            dequote_downcase_identifier(b"\"A\"B", false),
            b"AB".to_vec()
        );
    }

    /// The command name and its `OT_FILEPIPE` argument.
    fn filepipe(line: &str, semicolon: bool) -> Option<SlashOption> {
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        assert_eq!(
            scanner.scan(&mut buf, &NoVariables).0,
            ScanResult::Backslash
        );
        scanner.slash_command();
        scanner.slash_option_filepipe(&NoVariables, semicolon)
    }

    #[test]
    fn a_pipe_takes_the_rest_of_the_line_as_typed() {
        // `psqlscanslash.l:165`: `|` at the start makes it `OT_WHOLE_LINE`,
        // quotes, variables, backslashes and all.
        let pipe = filepipe("\\g | cat 'a b' :x \\\\ \\echo  ", false).unwrap();
        assert_eq!(pipe.value, "| cat 'a b' :x \\\\ \\echo  ");
        assert_eq!(pipe.quote, None);
        // `\o` strips its trailing semicolons and whitespace (`:635`).
        assert_eq!(filepipe("\\o |cat ; ;  ", true).unwrap().value, "|cat");
        // A `|` later in the argument is an ordinary character.
        assert_eq!(filepipe("\\g a|b c", false).unwrap().value, "a|b");
    }

    #[test]
    fn only_unquoted_trailing_semicolons_are_stripped() {
        // `psqlscanslash.l:604`: `unquoted_option_chars` bounds the strip.
        assert_eq!(filepipe("\\o f;;", true).unwrap().value, "f");
        assert_eq!(filepipe("\\o 'f;';", true).unwrap().value, "f;");
        assert_eq!(filepipe("\\g f;", false).unwrap().value, "f;");
        // Nothing left and nothing quoted is no argument at all (`:661`).
        assert_eq!(filepipe("\\o ;", true), None);
        assert_eq!(filepipe("\\o ''", true).unwrap().value, "");
        assert_eq!(filepipe("\\o", true), None);
    }

    fn whole_line(line: &str) -> Option<Vec<u8>> {
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        assert_eq!(
            scanner.scan(&mut buf, &NoVariables).0,
            ScanResult::Backslash
        );
        scanner.slash_command();
        scanner.slash_option_whole_line()
    }

    #[test]
    fn a_whole_line_argument_is_the_rest_of_the_line_as_typed() {
        // `psqlscanslash.l:423`: leading whitespace goes, everything else —
        // quotes, `:var`, backslashes, trailing blanks — stays.
        assert_eq!(
            whole_line("\\copy  t from 'a b' :x \\\\ \\echo  "),
            Some(b"t from 'a b' :x \\\\ \\echo  ".to_vec())
        );
        assert_eq!(whole_line("\\copy \t "), None);
        assert_eq!(whole_line("\\copy"), None);
    }
}
