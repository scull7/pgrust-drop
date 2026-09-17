# AGENTS.md — rules for working in pgrust-drop

Read this first, then the Linear issue you are working (project **pgrust-drop**, team NAT), then the ADRs under `docs/adr/` (0001 layout/vendoring, 0002 embedded-template initdb, 0003 licensing, 0004 usage-rs, 0005 line editor, 0006 TLS, 0007 upstream source).

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
| `rpsql`   | `src/bin/psql/` ported fresh from C (ADR-0003)                         |
| `pgdrop`  | the multicall binary: `pgdrop initdb | psql | postgres | start`        |

## What "upstream" means

"Upstream" is genuine PostgreSQL 18.6, and nothing else:

- git tag `REL_18_6`, commit `724edf9bde9d356724ad384a2e196edc3c9f80f7`
  (`github.com/postgres/postgres`), or
- the release tarball `postgresql-18.6.tar.bz2`, published sha256
  `555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f`.

pgrust (`malisper/pgrust`) is not upstream. Neither is the PostgreSQL 18.6 tree
vendored in pgrust at `crates/postgres-18.6-reference/`: it carries undeclared
local modifications, while its own `README-WHY-THIS-IS-HERE.md` claims to be a
pristine extract of the tag. Compared file-by-file against the pristine
tarball's 7,284 files: 7,281 are byte-identical, `src/port/win32ver.rc` is
absent, and two differ — `src/backend/utils/misc/postgresql.conf.sample`
(41 added lines of pgrust-specific GUCs under a `# PGRUST` header) and
`src/test/regress/data/streets.data` (one word).

**Never vendor content from that tree, and never cite it as the authority for a
`file:line`.** Vendor and cite from the tag or the tarball. Read it for
orientation — it is a convenient local copy and 7,281 of its files are exact —
but it is a convenience, not an authority: confirm against the tag or the
tarball before anything taken from it lands.

That distinction is not pedantry. `crates/rinitdb/share/postgresql.conf.sample`
was vendored from that tree and carried the 41 `# PGRUST` lines into an
MIT-licensed crate, against ADR-0003 (pgrust is AGPL-3.0). Re-vendored from
pristine sources in PR #11. See ADR-0007.

## Method: steal the tests

Every behaviour is proved the way pgrust proves itself: run the *upstream* test
against the C tool and against ours, then diff stdout, stderr and exit code
byte-for-byte after justified normalizations (see `testkit`). When a reference
binary is missing the test prints `SKIP (flagged, not silent)` and passes; it
never silently narrows.

Porting rule: keep upstream test names as Rust test names (grep-able), keep the
order, and cite the upstream file and line in a comment.

**Accepted gap, NAT-374.** `crates/rinitdb/src/control.rs` carries hand-computed
byte offsets (`offset`) and padding runs (`PADDING`) for `ControlFileData` on a
64-bit `MAXIMUM_ALIGNOF 8` build. Today they are proved only *self-consistent*:
`the_fields_and_the_padding_tile_the_struct` checks that the two tile
`[0, SIZEOF_CONTROL_FILE_DATA)` exactly. Nothing yet proves they match what a
real C compiler lays out — the assertion that would is the stolen
`command_like(['pg_controldata', $datadir], …)` run against the C binary
(`assert_data_page_checksum_version`, `crates/rinitdb/tests/t_001_initdb.rs:654`),
and it prints `SKIP (flagged, not silent)` because this box has no PostgreSQL 18.
That is accepted for now. **Remove the skip once NAT-374 lands the reference
binaries in CI** — not by weakening the gate, by letting it run. Until then,
treat the offset tables as unverified against C.

## CLI framework

All CLIs use **usage-rs** (`usage = { package = "usage-rs" }`). Constraint: the
`--help` / `--version` text of `initdb` and `psql` must stay byte-identical to
upstream, so those two flags are intercepted before the usage-rs parser runs,
exactly as `initdb.c` does with its `argv[1]` fast path. Parse errors are
usage-rs's (clap-shaped, exit 2), not glibc getopt's; the stolen
`program_options_handling_ok` only requires a nonzero exit and a non-empty
stderr. Long-option abbreviation (glibc unique-prefix) is a documented
divergence. See ADR-0004.

## Targets and lanes (ADR-0007)

The product ships to **Alpine/musl** (Omen devices) and **aarch64 macOS**
(developer laptops); glibc is the odd one out. A byte-diff gate is only valid
when both sides link the same C library, so `testkit::reference` keys discovery
on the compiled target's libc and each lane has its own variable
(`PGDROP_REF_BIN_{GNU,MUSL,APPLE}`). Get the reference binaries with
`scripts/fetch-ref-binaries.sh`; never point one lane's variable at another
lane's directory. `scripts/setup-branch-ruleset.sh` applies the matching `main`
ruleset (owner-run, needs `gh` as a repository admin). CI runs musl on every push (`container: alpine`), then gnu and
apple on pull requests via `needs: musl`; `gnu` is the required check. Docker is
not a dependency of the product, the harness or the developer workflow — CI's
container does not change that.

## Rust rules

- Toolchain pinned to 1.96.0 (pgrust's pin). `cargo fmt --check`,
  `cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic`,
  `cargo test --all-features` must pass before a push. Run the musl lane too
  (`--target x86_64-unknown-linux-musl`); it is the one CI gates every push.
- stdlib first. **No new dependencies without explicit approval.** Approved so
  far (owner, 2026-09-16): `usage-rs` (all CLIs), `thiserror` (typed errors),
  `rustls` (rlibpq TLS, ADR-0006), `redox_liner` (rpsql line editing,
  ADR-0005). `anyhow` is not approved.
- **`thiserror` is deliberately not universal.** It is used where an error's
  text is a Rust string: `rinitdb` (`error.rs`, `control.rs`) and `testkit`
  (`gate.rs`, `pattern.rs`). `rlibpq` and `rpsql` depend on it not at all and
  hand-write `Display` + `impl std::error::Error`. That is on purpose, not an
  oversight: every `rlibpq` error type exposes `message() -> Vec<u8>` and
  `Display` is a thin `from_utf8_lossy` over it, because a server's bytes and a
  user's conninfo token reach stderr unmangled only if the message is built as
  bytes — which `#[error("…")]`, a `&str` format string over `Display` fields,
  cannot express. Do not "unify" these on `thiserror`; it would break byte
  fidelity against C libpq. (`rpsql`'s one error type, `print::PrintError`, is
  hand-written for consistency with `rlibpq`, not for byte fidelity.)
- Licensing (ADR-0003): `testkit`, `rinitdb`, `rlibpq`, `rpsql` are MIT and are
  ported from PostgreSQL's C sources only. **Never copy code, comments or test
  corpora from pgrust into them.** `pgdrop` is AGPL-3.0 because it links pgrust.
- Separate Data / Calculations / Actions. Parsing, planning, rendering are pure
  functions with unit tests; process spawning, filesystem, sockets live at the
  edge in thin functions.
- Newtypes and enums for domain invariants; no stringly-typed state machines.
- Each PR is one reviewable chunk (~10 minutes). Keep changes atomic.

## Unattended runs

`docs/nightshift/ORCHESTRATOR.md` is the prompt for an overnight session: a
Fable orchestrator that only dispatches Opus workers issue by issue on a
`nightshift/<date>` branch and opens one PR. Workers follow this file.

## Change hygiene

**Linear is the single source of truth for project state.** Status, decisions,
risks and follow-ups live on the Linear issue — project **pgrust-drop**, team
NAT, issues NAT-372 … — not in a file in this repo.

- A meaningful change updates its Linear issue (what, why, checks run, risks,
  follow-ups) and moves the issue's state. Work that is not on the issue did
  not happen.
- The PR description carries the human-readable narrative; the Linear issue
  carries the state. Do not duplicate either into a log file.
- `progress.md` is retired. It stays in the repo as a historical archive of
  work up to 2026-09-16. Do not extend it, do not read it as current state, do
  not delete it.
- Never weaken a gate to get green. A failing stolen test is a bug report.
- When scripting checks, branch on the command's exit status, not on grepped
  output: `cargo clippy … 2>log && echo OK` — a pipeline through `grep` returns
  grep's status and hides failures.

Two things stay in the repo, because they are not project state:

- **ADRs** (`docs/adr/`) — durable architecture decisions. A new decision gets
  a new ADR; a reversal gets an amendment on the ADR it reverses. Never a
  Linear comment instead.
- **`docs/divergences.md`** — every deliberate divergence from upstream, with
  the reason and the test that pins it. It binds behaviour to tests, so it
  travels with the code.
