# AGENTS.md — rules for working in pgrust-drop

Read this first, then `progress.md`, then the ADRs under `docs/adr/`.

## What this repo is

One self-contained pgrust binary (`pgdrop`) with an embedded `initdb` and `psql`,
all in Rust, so a test suite needs neither Docker nor the PostgreSQL client tools.
Tracked in Linear: project **pgrust-drop** (team NAT), issues NAT-372 … NAT-415.

Crates (`crates/`):

| crate     | tracks upstream                                                        |
| --------- | ---------------------------------------------------------------------- |
| `testkit` | `src/test/perl/PostgreSQL/Test/Utils.pm` helpers + byte-diff gates     |
| `rinitdb` | `src/bin/initdb/` (PostgreSQL 18.6)                                    |
| `rlibpq`  | `src/interfaces/libpq/` (pure Rust, native crate + C ABI; pgrust #40)  |
| `rpsql`   | `src/bin/psql/` seeded from pgrust `crates/bin/psql`                   |
| `pgdrop`  | the multicall binary: `pgdrop initdb | psql | postgres | start`        |

"Upstream" means the PostgreSQL 18.6 tree vendored in pgrust at
`crates/postgres-18.6-reference/`, and pgrust itself (`malisper/pgrust`).

## Method: steal the tests

Every behaviour is proved the way pgrust proves itself: run the *upstream* test
against the C tool and against ours, then diff stdout, stderr and exit code
byte-for-byte after justified normalizations (see `testkit`). When a reference
binary is missing the test prints `SKIP (flagged, not silent)` and passes; it
never silently narrows.

Porting rule: keep upstream test names as Rust test names (grep-able), keep the
order, and cite the upstream file and line in a comment.

## CLI framework

All CLIs use **usage-rs** (`usage = { package = "usage-rs" }`). Constraint: the
`--help` / `--version` text of `initdb` and `psql` must stay byte-identical to
upstream, so those two flags are intercepted before the usage-rs parser runs,
exactly as `initdb.c` does with its `argv[1]` fast path. Parse errors are
usage-rs's (clap-shaped, exit 2), not glibc getopt's; the stolen
`program_options_handling_ok` only requires a nonzero exit and a non-empty
stderr. Long-option abbreviation (glibc unique-prefix) is a documented
divergence. See ADR-0004.

## Rust rules

- Toolchain pinned to 1.96.0 (pgrust's pin). `cargo fmt --check`,
  `cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic`,
  `cargo test --all-features` must pass before a push.
- stdlib first. **No new dependencies without explicit approval.** Approved:
  `usage-rs` (all CLIs), `thiserror` (typed errors). `anyhow` is not approved.
- Separate Data / Calculations / Actions. Parsing, planning, rendering are pure
  functions with unit tests; process spawning, filesystem, sockets live at the
  edge in thin functions.
- Newtypes and enums for domain invariants; no stringly-typed state machines.
- Each PR is one reviewable chunk (~10 minutes). Keep changes atomic.

## Change hygiene

- Update `progress.md` for every meaningful change (what, why, checks run,
  risks, follow-ups) and move the Linear issue.
- Record every deliberate divergence from upstream in `docs/divergences.md`
  with the reason and the test that pins it.
- Never weaken a gate to get green. A failing stolen test is a bug report.
