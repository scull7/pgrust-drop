# ADR-0004: usage-rs for every CLI

Status: accepted (owner decision, 2026-09-16); amended 2026-09-16 — the
`psql` half of the first consequence is not true yet and lands with NAT-399
(see Amendment below); amended 2026-09-26 — NAT-399 landed it (see the second
amendment).

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

- For `rinitdb`, `program_help_ok` / `program_version_ok` pass and
  `help_and_version_match_reference_initdb` diffs both flags against the C
  binary with no normalizer at all
  (`crates/rinitdb/tests/t_001_initdb.rs:176`); it prints
  `SKIP (flagged, not silent)` where PostgreSQL 18 is not installed. For
  `rpsql` only `program_version_ok` passes today — see the amendment.
- Divergences pinned in `docs/divergences.md`: glibc getopt error text and exit
  status 1 become usage-rs text and exit 2; glibc unique-prefix long-option
  abbreviation (`--no-syn`) is not supported.
- `pgdrop` uses `#[usage(multicall)]` for `argv[0]` dispatch; completions and
  man pages come from the emitted usage spec.

## Amendment 2026-09-16: the psql half is not true yet

`rpsql` does not print upstream's `--help` text. `run`
(`crates/rpsql/src/lib.rs:127`) answers `Invocation::PrintHelp` by writing

```
psql: error: --help is not implemented yet (Linear NAT-399)
```

to **stderr** and exiting **1** — not upstream's text on stdout with status 0.
The stolen `program_help_ok('psql')` is therefore declared in upstream's
position and `#[ignore]`d with NAT-399 as its reason
(`crates/rpsql/tests/t_001_basic.rs:31`), the only `#[ignore]` in the
repository. That is deliberate and stays: keeping the assertion with a named
reason makes the gap visible, where deleting it would hide it. Do not weaken
it to something `rpsql` can pass today.

`program_version_ok('psql')` does pass; `psql --version` is real. There is no
`--help`/`--version` byte-diff gate against C `psql` at all yet, unlike
`rinitdb`'s; it lands with the rest of NAT-399.

The Decision above is unchanged — the `argv[1]`-only fast path in front of the
parser, printing checked-in upstream text, is how `rpsql` will do it too. Only
psql's text (`usage()`, `slashUsage()`, `helpVariables()` in `help.c`) is not
written yet. The first consequence holds for `initdb` today and for `psql` when
NAT-399 lands.

## Amendment 2026-09-26: the psql half is true

NAT-399 ports `usage()`, `slashUsage()` and `helpVariables()` from `help.c`
into `crates/rpsql/src/help.rs` as checked-in upstream text, which is what the
Decision asked for. `run` now answers `Invocation::PrintHelp` with that text on
stdout and exit 0. The first amendment's `#[ignore]` on
`program_help_ok('psql')` is gone, and so is the last `#[ignore]` in the
repository.

The byte-diff gate the first amendment said was missing exists:
`help_matches_c_psql` in `crates/rpsql/tests/t_001_basic.rs` diffs `--help`,
`--help=commands` and `--help=variables` against C `psql` as raw bytes, and
`version_matches_c_psql` diffs `--version` under `normalize::EXTRA_VERSION`,
the same way `rinitdb`'s gate does. Both print `SKIP (flagged, not silent)`
where PostgreSQL 18 is not installed. Every CI lane installs it and sets
`PGDROP_REQUIRE_REF=1`, so the gates run there for real.

psql's option loop can also exit early, which `initdb`'s cannot: `-V`,
`--version`, a standalone `-?` and `--help[=topic]` stop it wherever they
appear (`startup.c:666`, `:690`, `:704`), not only as `argv[1]`. The fast path
still runs first. After it, `startup::help_or_version` walks argv in getopt
order over `long_options[]` and returns the first early exit that comes before
any option getopt would refuse. usage-rs never sees those flags, so it keeps
its built-in help and version flags disabled. That walk is the one that already
kept `-c`/`-f` in argv order (`startup::actions`); both now read it.

The first consequence now holds for `psql` as it does for `initdb`.
