//! `QL_HELP`, the table of SQL commands `\help` describes: the `sql_help.c`
//! and `sql_help.h` PostgreSQL 18.6's `src/bin/psql/create_help.pl` generates
//! from the SQL reference pages, vendored byte for byte under
//! `crates/rpsql/share/` (see its `README.md` for provenance).
//!
//! C compiles the generated file; this port reads it. [`ql_help`] parses it
//! once, on the first `\help`, into [`HelpEntry`]s: each entry's syntax
//! function (`appendPQExpBuffer(buf, "<format>", _("<param>"), …)`) is
//! evaluated to the text it appends. The parser knows exactly the shapes
//! `create_help.pl` prints (`create_help.pl:182`-`:219`) and nothing more; a
//! unit test parses the vendored file, so a shape it does not know fails
//! `cargo test`, not a user's `\help`.

use std::sync::OnceLock;

/// `sql_help.c`, as `create_help.pl` wrote it.
const SQL_HELP_C: &str = include_str!("../share/sql_help.c");

/// `sql_help.h`, as `create_help.pl` wrote it.
const SQL_HELP_H: &str = include_str!("../share/sql_help.h");

/// One element of `QL_HELP` (`struct _helpStruct`, `create_help.pl:74`), its
/// syntax function already run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpEntry {
    /// `cmd`: the command name, e.g. `ALTER TABLE`.
    pub cmd: String,
    /// `help`: the one-line description.
    pub help: String,
    /// `docbook_id`: the page's id, which names its URL.
    pub docbook_id: String,
    /// What `syntaxfunc` appends: the synopsis, parameters filled in.
    pub syntax: String,
}

/// The parsed `QL_HELP` table, in its order (sorted by command name), without
/// the `NULL` end-of-list marker.
///
/// # Panics
/// When the vendored file is not in `create_help.pl`'s shape; the unit tests
/// parse it, so that cannot reach a user.
#[must_use]
pub fn ql_help() -> &'static [HelpEntry] {
    static TABLE: OnceLock<Vec<HelpEntry>> = OnceLock::new();
    TABLE.get_or_init(|| parse(SQL_HELP_C).unwrap_or_else(|err| panic!("sql_help.c: {err}")))
}

/// `QL_MAX_CMD_LEN` (`create_help.pl:224`): the longest `cmd`.
///
/// # Panics
/// When the vendored `sql_help.h` has no such line; the unit tests read it.
#[must_use]
pub fn ql_max_cmd_len() -> usize {
    header_define("QL_MAX_CMD_LEN")
}

/// A `#define NAME\t<n>` of `sql_help.h`.
fn header_define(name: &str) -> usize {
    SQL_HELP_H
        .lines()
        .find_map(|line| {
            let rest = line.strip_prefix("#define ")?.strip_prefix(name)?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .unwrap_or_else(|| panic!("sql_help.h: no #define {name}"))
}

/// A C token of the generated file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A string literal, escapes resolved.
    Str(String),
    /// An identifier or keyword.
    Ident(String),
    /// A decimal integer.
    Int(usize),
    /// Any other single character: `(`, `)`, `{`, `,`, …
    Punct(char),
}

/// Split C source into [`Token`]s, dropping whitespace, comments and
/// preprocessor lines.
fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'\n' => {
                line_start = true;
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' if line_start => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..].find("*/").ok_or("an unterminated comment")?;
                i += 2 + end + 2;
            }
            b'"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    match bytes.get(i) {
                        None | Some(b'\n') => return Err("an unterminated string".into()),
                        Some(b'"') => break,
                        Some(b'\\') => {
                            s.push(match bytes.get(i + 1) {
                                Some(b'n') => '\n',
                                Some(b'"') => '"',
                                Some(b'\\') => '\\',
                                other => return Err(format!("the escape \\{other:?}")),
                            });
                            i += 2;
                        }
                        Some(_) => {
                            // Up to the next quote or backslash, whole UTF-8
                            // characters at a time.
                            let run = src[i..].find(['"', '\\', '\n']).unwrap_or(src.len() - i);
                            s.push_str(&src[i..i + run]);
                            i += run;
                        }
                    }
                }
                i += 1;
                tokens.push(Token::Str(s));
            }
            b'0'..=b'9' => {
                let run = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
                let n = src[i..i + run].parse().map_err(|e| format!("{e}"))?;
                tokens.push(Token::Int(n));
                i += run;
            }
            c if c == b'_' || c.is_ascii_alphabetic() => {
                let run = bytes[i..]
                    .iter()
                    .take_while(|b| **b == b'_' || b.is_ascii_alphanumeric())
                    .count();
                tokens.push(Token::Ident(src[i..i + run].to_owned()));
                i += run;
            }
            _ => {
                let ch = src[i..].chars().next().unwrap_or_default();
                tokens.push(Token::Punct(ch));
                i += ch.len_utf8();
            }
        }
        line_start = false;
    }
    Ok(tokens)
}

/// A cursor over the tokens, with the expectations the grammar needs.
struct Cursor {
    tokens: Vec<Token>,
    at: usize,
}

impl Cursor {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Result<Token, String> {
        let token = self.peek().cloned().ok_or("unexpected end of file")?;
        self.at += 1;
        Ok(token)
    }

    fn eat(&mut self, want: &Token) -> bool {
        let hit = self.peek() == Some(want);
        if hit {
            self.at += 1;
        }
        hit
    }

    fn expect(&mut self, want: &Token) -> Result<(), String> {
        let got = self.next()?;
        if &got == want {
            Ok(())
        } else {
            Err(format!("expected {want:?}, found {got:?}"))
        }
    }

    fn punct(&mut self, c: char) -> Result<(), String> {
        self.expect(&Token::Punct(c))
    }

    fn ident(&mut self, name: &str) -> Result<(), String> {
        self.expect(&Token::Ident(name.to_owned()))
    }

    fn any_ident(&mut self) -> Result<String, String> {
        match self.next()? {
            Token::Ident(name) => Ok(name),
            other => Err(format!("expected an identifier, found {other:?}")),
        }
    }

    /// One string literal, or several adjacent ones, concatenated.
    fn string(&mut self) -> Result<String, String> {
        let mut s = match self.next()? {
            Token::Str(s) => s,
            other => return Err(format!("expected a string, found {other:?}")),
        };
        while let Some(Token::Str(more)) = self.peek() {
            s.push_str(more);
            self.at += 1;
        }
        Ok(s)
    }

    /// `<macro>("<string>")`: `_(…)` or `N_(…)`, both no-ops without NLS.
    fn wrapped_string(&mut self, macro_name: &str) -> Result<String, String> {
        self.ident(macro_name)?;
        self.punct('(')?;
        let s = self.string()?;
        self.punct(')')?;
        Ok(s)
    }
}

/// `appendPQExpBuffer`'s formatting, for the only conversions the generated
/// file uses: `%s` and `%%` (`create_help.pl:150`-`:158`).
fn format_syntax(format: &str, params: &[String]) -> Result<String, String> {
    let mut out = String::new();
    let mut params = params.iter();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push_str(params.next().ok_or("too few parameters")?),
            Some('%') => out.push('%'),
            other => return Err(format!("the conversion %{other:?}")),
        }
    }
    if params.next().is_some() {
        return Err("too many parameters".into());
    }
    Ok(out)
}

/// Parse the whole generated `sql_help.c`: the syntax functions
/// (`create_help.pl:182`), then the `QL_HELP` table (`:200`).
fn parse(src: &str) -> Result<Vec<HelpEntry>, String> {
    let mut cur = Cursor {
        tokens: tokenize(src)?,
        at: 0,
    };

    // static void
    // sql_help_<id>(PQExpBuffer buf)
    // {
    //     appendPQExpBuffer(buf, "<format>", _("<param>"), …);
    // }
    let mut functions = std::collections::HashMap::new();
    while cur.eat(&Token::Ident("static".into())) {
        cur.ident("void")?;
        let name = cur.any_ident()?;
        cur.punct('(')?;
        cur.ident("PQExpBuffer")?;
        cur.ident("buf")?;
        cur.punct(')')?;
        cur.punct('{')?;
        cur.ident("appendPQExpBuffer")?;
        cur.punct('(')?;
        cur.ident("buf")?;
        cur.punct(',')?;
        let format = cur.string()?;
        let mut params = Vec::new();
        while cur.eat(&Token::Punct(',')) {
            params.push(cur.wrapped_string("_")?);
        }
        cur.punct(')')?;
        cur.punct(';')?;
        cur.punct('}')?;
        let syntax = format_syntax(&format, &params).map_err(|e| format!("{name}: {e}"))?;
        functions.insert(name, syntax);
    }

    // const struct _helpStruct QL_HELP[] = {
    //     {"<cmd>", N_("<help>"), "<docbook_id>", sql_help_<id>, <nl_count>},
    //     …
    //     {NULL, NULL, NULL}
    // };
    for word in ["const", "struct", "_helpStruct", "QL_HELP"] {
        cur.ident(word)?;
    }
    for c in ['[', ']', '=', '{'] {
        cur.punct(c)?;
    }
    let mut table = Vec::new();
    loop {
        cur.punct('{')?;
        if cur.eat(&Token::Ident("NULL".into())) {
            for _ in 0..2 {
                cur.punct(',')?;
                cur.ident("NULL")?;
            }
            cur.punct('}')?;
            break;
        }
        let cmd = cur.string()?;
        cur.punct(',')?;
        let help = cur.wrapped_string("N_")?;
        cur.punct(',')?;
        let docbook_id = cur.string()?;
        cur.punct(',')?;
        let function = cur.any_ident()?;
        cur.punct(',')?;
        let Token::Int(_nl_count) = cur.next()? else {
            return Err(format!("{cmd}: expected nl_count"));
        };
        cur.punct('}')?;
        cur.punct(',')?;
        let syntax = functions
            .get(&function)
            .ok_or_else(|| format!("{cmd}: no function {function}"))?
            .clone();
        table.push(HelpEntry {
            cmd,
            help,
            docbook_id,
            syntax,
        });
    }
    cur.punct('}')?;
    cur.punct(';')?;
    match cur.peek() {
        None => Ok(table),
        Some(extra) => Err(format!("trailing {extra:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    /// The digests `crates/rpsql/share/README.md` records for what
    /// `create_help.pl` writes from tag `REL_18_6`.
    #[test]
    fn the_vendored_files_are_create_help_pl_output() {
        assert_eq!(
            hex(&rlibpq::sha256::sha256(SQL_HELP_C.as_bytes())),
            "06ff27b69db1ba286fd52bea85f4aa0d032864f6da56b5fad75da06a8c89bbb2",
            "crates/rpsql/share/sql_help.c"
        );
        assert_eq!(
            hex(&rlibpq::sha256::sha256(SQL_HELP_H.as_bytes())),
            "1853cac5dddef301121a3736d06d027e9d6cac04312088f24a0d995c676a175b",
            "crates/rpsql/share/sql_help.h"
        );
    }

    #[test]
    fn the_table_agrees_with_the_header() {
        let table = ql_help();
        assert_eq!(table.len(), header_define("QL_HELP_COUNT"));
        assert_eq!(
            table.iter().map(|e| e.cmd.len()).max(),
            Some(ql_max_cmd_len())
        );
        assert!(table.windows(2).all(|w| w[0].cmd < w[1].cmd), "sorted");
    }

    #[test]
    fn a_syntax_function_is_evaluated_with_its_parameters() {
        let abort = &ql_help()[0];
        assert_eq!(abort.cmd, "ABORT");
        assert_eq!(abort.help, "abort the current transaction");
        assert_eq!(abort.docbook_id, "sql-abort");
        assert_eq!(
            abort.syntax,
            "ABORT [ WORK | TRANSACTION ] [ AND [ NO ] CHAIN ]"
        );
        let alter_aggregate = &ql_help()[1];
        assert!(
            alter_aggregate
                .syntax
                .starts_with("ALTER AGGREGATE name ( aggregate_signature ) RENAME TO new_name\n"),
            "{:?}",
            alter_aggregate.syntax
        );
        assert!(!ql_help().iter().any(|e| e.syntax.contains("%s")));
    }

    #[test]
    fn several_names_share_one_page() {
        let by_cmd = |cmd: &str| ql_help().iter().find(|e| e.cmd == cmd).unwrap();
        assert_eq!(by_cmd("SELECT").docbook_id, "sql-select");
        assert_eq!(by_cmd("TABLE").docbook_id, "sql-select");
        assert_eq!(by_cmd("WITH").docbook_id, "sql-select");
    }

    #[test]
    fn the_format_knows_only_s_and_percent() {
        assert_eq!(
            format_syntax("a %s b %% c", &["x".into()]).unwrap(),
            "a x b % c"
        );
        assert!(format_syntax("%d", &[]).is_err());
        assert!(format_syntax("%s", &[]).is_err());
        assert!(format_syntax("", &["x".into()]).is_err());
    }

    #[test]
    fn the_tokenizer_resolves_escapes_and_joins_nothing_itself() {
        assert_eq!(
            tokenize("#include \"x\"\n/* c */ f(\"a\\n\" \"\\\"b\", 12);").unwrap(),
            vec![
                Token::Ident("f".into()),
                Token::Punct('('),
                Token::Str("a\n".into()),
                Token::Str("\"b".into()),
                Token::Punct(','),
                Token::Int(12),
                Token::Punct(')'),
                Token::Punct(';'),
            ]
        );
        assert!(tokenize("\"\\t\"").is_err());
    }
}
