# ADR-0004: usage-rs for every CLI

Status: accepted (owner decision, 2026-09-16).

## Context

`rinitdb` and `rpsql` must print `--help` and `--version` byte-identical to the
PostgreSQL 18.6 tools and should keep C's option surface (`-A METHOD`,
`--auth=METHOD`, clustered shorts like `-Nk`, `--noclean` as an alias of
`--no-clean`, `-?` for help). The owner wants all CLIs on
[usage-rs](https://github.com/jdx/usage) (`usage-rs` 6.x, MIT, 10 crates in the
tree, builds on Rust 1.96 in ~9 s).

## Decision

- Every binary declares its CLI with `#[derive(usage::Cli)]` and parses with
  `parse_from`, never `parse()`, so the program owns printing and exit codes.
- Containers set `unknown_flags = "error"` (the default let an unknown flag
  fall into a positional argument, which would defeat
  `program_options_handling_ok`), and disable the built-in `-h`/`--help` and
  `-V`/`--version` where upstream spells them differently.
- Upstream's `argv[1]`-only fast path for `--help`/`-?`/`--version`/`-V` is kept
  verbatim in front of the parser, printing the upstream text from a checked-in
  string (`usage()` in `initdb.c`, `help.c` for psql). usage-rs's own rendered
  help is exposed as `--help-usage` for docs/completions generation only.
- Parse failures use usage-rs's clap-shaped rendering and exit status 2.

## Consequences

- `program_help_ok` / `program_version_ok` pass byte-for-byte; the byte-diff gate
  on `--help`/`--version` vs the C binaries is exact.
- Divergences pinned in `docs/divergences.md`: glibc getopt error text and exit
  status 1 become usage-rs text and exit 2; glibc unique-prefix long-option
  abbreviation (`--no-syn`) is not supported.
- `pgdrop` uses `#[usage(multicall)]` for `argv[0]` dispatch; completions and
  man pages come from the emitted usage spec.
