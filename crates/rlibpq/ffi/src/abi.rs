//! The coverage matrix: which of the symbols C libpq exports this crate
//! exports, and how faithfully.
//!
//! The list of symbols is upstream's own `src/interfaces/libpq/exports.txt`,
//! vendored unmodified, so a symbol cannot be forgotten by forgetting to list
//! it. [`SHIMS`] records the ones this crate exports; every other symbol is
//! [`Coverage::NotYet`]. Tests hold three things together: every `SHIMS` entry
//! is an upstream export, every `#[no_mangle]` function in this crate is a
//! `SHIMS` entry and back, and `docs/libpq-abi.md` is exactly
//! [`render_matrix`]'s output. The last is regenerated with
//! `cargo run -p rlibpq-ffi --example libpq_abi_md > docs/libpq-abi.md`.
//!
//! All of it is pure: no symbol lookup, no filesystem.

use std::fmt;
use std::fmt::Write as _;

/// `src/interfaces/libpq/exports.txt` at `REL_18_6`, byte for byte.
pub const EXPORTS_TXT: &str = include_str!("../upstream/exports.txt");

/// One exported symbol: its name and the ordinal `exports.txt` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Export<'a> {
    pub name: &'a str,
    pub ordinal: u32,
}

/// Why `exports.txt` did not parse. `line` is 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportsError {
    /// Not `name ordinal`.
    Malformed { line: usize },
    /// Ordinals run 1, 2, 3 … with no gaps; this one broke the run.
    Ordinal {
        line: usize,
        expected: u32,
        found: u32,
    },
}

impl fmt::Display for ExportsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportsError::Malformed { line } => {
                write!(f, "exports.txt line {line}: expected `name ordinal`")
            }
            ExportsError::Ordinal {
                line,
                expected,
                found,
            } => write!(
                f,
                "exports.txt line {line}: ordinal {found}, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for ExportsError {}

/// Calculation: the symbols of an `exports.txt`, in file order.
///
/// The format is the one `src/tools/gen_export.pl:52`-`:56` reads: `#`
/// starts a comment line, and a symbol line is its name and its ordinal
/// separated by whitespace. Stricter than the Perl, which ignores any other
/// line: only blank lines are skipped here, so a vendoring accident fails.
///
/// # Errors
///
/// [`ExportsError`] for a line that is not `name ordinal`, or an ordinal out
/// of sequence.
pub fn parse_exports(text: &str) -> Result<Vec<Export<'_>>, ExportsError> {
    let mut exports = Vec::new();
    let mut expected: u32 = 1;
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        if raw.starts_with('#') || raw.trim().is_empty() {
            continue;
        }
        let mut words = raw.split_whitespace();
        let (Some(name), Some(ordinal), None) = (words.next(), words.next(), words.next()) else {
            return Err(ExportsError::Malformed { line });
        };
        let found: u32 = ordinal
            .parse()
            .map_err(|_| ExportsError::Malformed { line })?;
        if found != expected {
            return Err(ExportsError::Ordinal {
                line,
                expected,
                found,
            });
        }
        exports.push(Export {
            name,
            ordinal: found,
        });
        expected += 1;
    }
    Ok(exports)
}

/// How far this crate covers one exported symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Exported, and answers what C libpq answers.
    Implemented,
    /// Exported so a program links, but fails the way C reports an
    /// unsupported call instead of doing the work.
    Stubbed,
    /// Not exported: a program calling it does not link.
    NotYet,
}

impl Coverage {
    /// How `docs/libpq-abi.md` spells it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Coverage::Implemented => "implemented",
            Coverage::Stubbed => "stubbed with an error",
            Coverage::NotYet => "not yet",
        }
    }
}

/// One symbol this crate exports, and the C function it follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shim {
    pub name: &'static str,
    pub coverage: Coverage,
    /// The C definition at `REL_18_6`, relative to `src/interfaces/libpq/`,
    /// and which arm of it when the build configuration picks one.
    pub follows: &'static str,
}

const fn implemented(name: &'static str, follows: &'static str) -> Shim {
    Shim {
        name,
        coverage: Coverage::Implemented,
        follows,
    }
}

/// Every symbol this crate exports, in `exports.txt` order.
pub const SHIMS: [Shim; 75] = [
    implemented("PQconnectdb", "fe-connect.c:820"),
    implemented("PQsetdbLogin", "fe-connect.c:2231"),
    implemented("PQconndefaults", "fe-connect.c:2193"),
    implemented("PQfinish", "fe-connect.c:5301"),
    implemented("PQreset", "fe-connect.c:5315"),
    implemented("PQdb", "fe-connect.c:7472"),
    implemented("PQuser", "fe-connect.c:7480"),
    implemented("PQpass", "fe-connect.c:7488"),
    implemented("PQhost", "fe-connect.c:7505"),
    implemented("PQport", "fe-connect.c:7541"),
    implemented("PQtty", "fe-connect.c:7559"),
    implemented("PQoptions", "fe-connect.c:7567"),
    implemented("PQstatus", "fe-connect.c:7575"),
    implemented("PQerrorMessage", "fe-connect.c:7638"),
    implemented("PQsocket", "fe-connect.c:7664"),
    implemented("PQbackendPID", "fe-connect.c:7674"),
    implemented("PQexec", "fe-exec.c:2279"),
    implemented("PQresultStatus", "fe-exec.c:3442"),
    implemented("PQntuples", "fe-exec.c:3512"),
    implemented("PQnfields", "fe-exec.c:3520"),
    implemented("PQbinaryTuples", "fe-exec.c:3528"),
    implemented("PQfname", "fe-exec.c:3598"),
    implemented("PQfnumber", "fe-exec.c:3620"),
    implemented("PQftype", "fe-exec.c:3750"),
    implemented("PQfsize", "fe-exec.c:3761"),
    implemented("PQfmod", "fe-exec.c:3772"),
    implemented("PQcmdStatus", "fe-exec.c:3783"),
    implemented("PQoidStatus", "fe-exec.c:3796"),
    implemented("PQcmdTuples", "fe-exec.c:3853"),
    implemented("PQgetvalue", "fe-exec.c:3907"),
    implemented("PQgetlength", "fe-exec.c:3918"),
    implemented("PQgetisnull", "fe-exec.c:3932"),
    implemented("PQclear", "fe-exec.c:727"),
    implemented("PQresultErrorMessage", "fe-exec.c:3458"),
    implemented("PQresStatus", "fe-exec.c:3450"),
    implemented("PQoidValue", "fe-exec.c:3824"),
    implemented("PQconninfoFree", "fe-connect.c:7459"),
    implemented("PQconnectPoll", "fe-connect.c:2908, blocking"),
    implemented("PQconnectStart", "fe-connect.c:948, blocking"),
    implemented("PQresetPoll", "fe-connect.c:5367, blocking"),
    implemented("PQresetStart", "fe-connect.c:5348, blocking"),
    implemented("PQfreeNotify", "fe-exec.c:4080"),
    implemented("PQfreemem", "fe-exec.c:4063"),
    implemented("PQtransactionStatus", "fe-connect.c:7583"),
    implemented("PQparameterStatus", "fe-connect.c:7593"),
    implemented("PQexecParams", "fe-exec.c:2293"),
    implemented("PQresultErrorField", "fe-exec.c:3497"),
    implemented("PQftable", "fe-exec.c:3717"),
    implemented("PQftablecol", "fe-exec.c:3728"),
    implemented("PQfformat", "fe-exec.c:3739"),
    implemented("PQexecPrepared", "fe-exec.c:2340"),
    implemented("PQserverVersion", "fe-connect.c:7628"),
    implemented("PQgetssl", "fe-secure.c:452, without SSL"),
    implemented("PQprepare", "fe-exec.c:2323"),
    implemented("PQinitSSL", "fe-secure.c:117"),
    implemented("PQisthreadsafe", "fe-exec.c:4023"),
    implemented("PQnparams", "fe-exec.c:3946"),
    implemented("PQparamtype", "fe-exec.c:3957"),
    implemented("PQdescribePrepared", "fe-exec.c:2472"),
    implemented("PQdescribePortal", "fe-exec.c:2491"),
    implemented("PQconninfoParse", "fe-connect.c:6175"),
    implemented("PQinitOpenSSL", "fe-secure.c:129"),
    implemented("PQconnectdbParams", "fe-connect.c:765"),
    implemented("PQconnectStartParams", "fe-connect.c:867, blocking"),
    implemented("PQlibVersion", "fe-misc.c:65"),
    implemented("PQconninfo", "fe-connect.c:7415"),
    implemented("PQsslInUse", "fe-secure.c:103, without SSL"),
    implemented("PQsslStruct", "fe-secure.c:458, without SSL"),
    implemented("PQsslAttributeNames", "fe-secure.c:470, without SSL"),
    implemented("PQsslAttribute", "fe-secure.c:464, without SSL"),
    implemented("PQgssEncInUse", "fe-secure.c:513, without GSSAPI"),
    implemented("PQgetgssctx", "fe-secure.c:507, without GSSAPI"),
    implemented(
        "PQsetSSLKeyPassHook_OpenSSL",
        "fe-secure.c:491, without OpenSSL",
    ),
    implemented(
        "PQgetSSLKeyPassHook_OpenSSL",
        "fe-secure.c:485, without OpenSSL",
    ),
    implemented(
        "PQdefaultSSLKeyPassHook_OpenSSL",
        "fe-secure.c:497, without OpenSSL",
    ),
];

/// Calculation: this crate's [`Shim`] for `name`, if it exports it.
#[must_use]
pub fn shim(name: &str) -> Option<&'static Shim> {
    SHIMS.iter().find(|shim| shim.name == name)
}

/// Calculation: `docs/libpq-abi.md`, the coverage of every symbol in
/// `exports`.
#[must_use]
pub fn render_matrix(exports: &[Export<'_>]) -> String {
    let count = |coverage: Coverage| {
        exports
            .iter()
            .filter(|export| shim(export.name).map_or(Coverage::NotYet, |s| s.coverage) == coverage)
            .count()
    };
    let mut out = String::new();
    out.push_str(
        "# libpq C ABI coverage\n\
         \n\
         <!-- Generated; do not edit. Regenerate with\n\
         \x20    cargo run -p rlibpq-ffi --example libpq_abi_md > docs/libpq-abi.md\n\
         \x20    `rlibpq-ffi`'s abi::tests fail when this file drifts from its source. -->\n\
         \n\
         Every symbol PostgreSQL 18.6's libpq exports, from\n\
         `src/interfaces/libpq/exports.txt` at `REL_18_6` (vendored unmodified as\n\
         `crates/rlibpq/ffi/upstream/exports.txt`), and how far `rlibpq-ffi`\n\
         covers it:\n\
         \n\
         - **implemented**: exported, and answers what C libpq answers;\n\
         - **stubbed with an error**: exported so a program links, but reports the\n\
         \x20 call as unsupported;\n\
         - **not yet**: not exported, so a program calling it does not link.\n\
         \n\
         The library is `libpq.a` (crate `rlibpq-ffi`, library name `pq`). There is no\n\
         `libpq.so` yet: rustup's musl target links the C runtime statically and drops\n\
         a `cdylib` crate type, and musl is the lane CI gates first (ADR-0007).\n\
         \n\
         \"Follows\" is the C definition at `REL_18_6`, relative to\n\
         `src/interfaces/libpq/`. Where C picks an arm by build configuration, the arm\n\
         is the one a libpq built without SSL, OpenSSL or GSSAPI takes, because\n\
         `rlibpq` has none of the three yet. \"blocking\" marks a call C makes step\n\
         by step without waiting and `rlibpq-ffi` makes in one blocking step\n\
         (`docs/divergences.md`).\n\
         \n",
    );
    let _ = writeln!(
        out,
        "{} of {} symbols implemented, {} stubbed with an error, {} not yet.",
        count(Coverage::Implemented),
        exports.len(),
        count(Coverage::Stubbed),
        count(Coverage::NotYet),
    );
    out.push_str("\n| ordinal | symbol | coverage | follows |\n|--:|---|---|---|\n");
    for export in exports {
        let (coverage, follows) =
            shim(export.name).map_or((Coverage::NotYet, ""), |s| (s.coverage, s.follows));
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} |",
            export.ordinal,
            export.name,
            coverage.label(),
            follows
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream() -> Vec<Export<'static>> {
        parse_exports(EXPORTS_TXT).expect("exports.txt parses")
    }

    /// `exports.txt` at `REL_18_6` runs from `PQconnectdb 1` (`:3`) to
    /// `appendPQExpBufferVA 210` (`:212`).
    #[test]
    fn exports_txt_lists_210_symbols_with_ordinals_1_to_210() {
        let exports = upstream();
        assert_eq!(exports.len(), 210);
        assert_eq!(
            exports.first(),
            Some(&Export {
                name: "PQconnectdb",
                ordinal: 1
            })
        );
        assert_eq!(
            exports.last(),
            Some(&Export {
                name: "appendPQExpBufferVA",
                ordinal: 210
            })
        );
    }

    #[test]
    fn parse_exports_refuses_a_malformed_line_and_a_gap() {
        assert_eq!(
            parse_exports("# c\nPQa 1\nPQb\n"),
            Err(ExportsError::Malformed { line: 3 })
        );
        assert_eq!(
            parse_exports("PQa 1\nPQb x\n"),
            Err(ExportsError::Malformed { line: 2 })
        );
        assert_eq!(
            parse_exports("PQa 1\n\nPQb 3\n"),
            Err(ExportsError::Ordinal {
                line: 3,
                expected: 2,
                found: 3
            })
        );
    }

    #[test]
    fn every_shim_is_an_upstream_export_in_exports_txt_order() {
        let exports = upstream();
        let ordinals: Vec<u32> = SHIMS
            .iter()
            .map(|shim| {
                exports
                    .iter()
                    .find(|export| export.name == shim.name)
                    .unwrap_or_else(|| panic!("{} is not in exports.txt", shim.name))
                    .ordinal
            })
            .collect();
        assert!(ordinals.is_sorted(), "SHIMS out of order: {ordinals:?}");
    }

    /// Calculation: the names of the `#[unsafe(no_mangle)]` functions in one
    /// source file — the function each attribute is on.
    fn no_mangle_functions(source: &str) -> Vec<&str> {
        let mut names = Vec::new();
        let mut marked = false;
        for line in source.lines().map(str::trim) {
            if line == "#[unsafe(no_mangle)]" {
                marked = true;
            } else if marked && let Some(at) = line.find("extern \"C\" fn ") {
                let rest = &line[at + "extern \"C\" fn ".len()..];
                names.push(&rest[..rest.find('(').expect("fn NAME(")]);
                marked = false;
            }
        }
        names
    }

    /// The matrix cannot claim a symbol the library does not export, nor miss
    /// one it does. Every module holding shims is listed here; a new one has
    /// to be added, and the `#[no_mangle]` count below catches forgetting.
    #[test]
    fn every_no_mangle_function_is_a_shim_and_every_shim_is_one() {
        let mut exported: Vec<&str> = [
            include_str!("accessors.rs"),
            include_str!("conn.rs"),
            include_str!("conninfo.rs"),
            include_str!("extended.rs"),
            include_str!("misc.rs"),
            include_str!("result.rs"),
            include_str!("secure.rs"),
        ]
        .into_iter()
        .flat_map(no_mangle_functions)
        .collect();
        exported.sort_unstable();
        let mut shims: Vec<&str> = SHIMS.iter().map(|shim| shim.name).collect();
        shims.sort_unstable();
        assert_eq!(exported, shims);
        assert_eq!(
            include_str!("lib.rs").matches("no_mangle)]").count(),
            0,
            "shims live in a module listed in this test, not in lib.rs"
        );
    }

    #[test]
    fn no_mangle_functions_reads_the_name_under_each_attribute() {
        let source = "#[unsafe(no_mangle)]\npub unsafe extern \"C\" fn PQa(x: i32) {}\n\
                      pub extern \"C\" fn helper() {}\n\
                      #[unsafe(no_mangle)]\npub extern \"C\" fn PQb() -> i32 {}\n";
        assert_eq!(no_mangle_functions(source), ["PQa", "PQb"]);
    }

    #[test]
    fn docs_libpq_abi_md_is_render_matrix_of_exports_txt() {
        let committed = include_str!("../../../../docs/libpq-abi.md");
        assert!(
            committed == render_matrix(&upstream()),
            "docs/libpq-abi.md is stale; regenerate it with\n    \
             cargo run -p rlibpq-ffi --example libpq_abi_md > docs/libpq-abi.md"
        );
    }

    #[test]
    fn render_matrix_counts_and_lists_every_symbol() {
        let exports = [
            Export {
                name: "PQping",
                ordinal: 1,
            },
            Export {
                name: "PQfreemem",
                ordinal: 2,
            },
        ];
        let matrix = render_matrix(&exports);
        assert!(
            matrix.contains("1 of 2 symbols implemented, 0 stubbed with an error, 1 not yet.\n")
        );
        assert!(matrix.contains("| 1 | `PQping` | not yet |  |\n"));
        assert!(matrix.contains("| 2 | `PQfreemem` | implemented | fe-exec.c:4063 |\n"));
    }
}
