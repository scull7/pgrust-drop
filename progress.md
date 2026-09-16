# progress.md — running log

Newest first. Each entry: what, why, checks run, risks, follow-ups.
Linear: project *pgrust-drop* (team NAT). GitHub: `scull7/pgrust-drop`.

## 2026-09-16 — Project bootstrap (NAT-372, NAT-373, NAT-377, NAT-406)

**What**
- Created the Linear project *pgrust-drop* with milestones M0 Foundation, M1
  rinitdb, M2 rlibpq, M3 rpsql, M4 pgdrop binary, M5 Faithful bootstrap and
  44 issues (NAT-372 … NAT-415), each a ~10-minute reviewable chunk naming the
  upstream test it steals.
- Scaffolded the Cargo workspace (`testkit`, `rinitdb`, `rlibpq`, `rpsql`,
  `pgdrop`), pinned Rust 1.96.0 (pgrust's pin), CI (fmt, clippy pedantic,
  test), `AGENTS.md`, ADR-0001 … ADR-0004, `docs/test-stealing.md`,
  `docs/divergences.md`.
- `testkit`: ports of `program_help_ok`, `program_version_ok`,
  `program_options_handling_ok` as pure checks over a `CommandOutcome`.
- `rinitdb`: usage-rs CLI mirroring `initdb.c`'s 41 long options and
  `"A:c:dD:E:gkL:nNsST:U:WX:"` shorts; `--help`/`-?`/`--version`/`-V` fast path
  printing the upstream text verbatim; everything else exits with
  "not implemented yet" naming the Linear issue.
- `pgdrop`: multicall dispatcher skeleton (`pgdrop initdb …` and symlink
  `initdb`), other applets stubbed.

**Why**
pgrust ships no `initdb`/`psql`; its README makes users install PostgreSQL 18
client tools first and test suites reach for Docker. The plan removes that.

**Findings that shaped the plan**
- pgrust's `postgres` refuses `--boot` (`main_main/src/lib.rs:328`), so a
  faithful initdb port cannot run today → ADR-0002 embedded template image;
  M5 tracks the upstream `--boot` + `genbki` work.
- pgrust already has a Rust `psql` (`crates/bin/psql`, ~5k lines, gate script
  diffing vs PGDG psql) and an in-server client (`crates/interfaces/pgclient`)
  with conninfo/URI tests → rpsql seeds from it, rlibpq lifts auth/framing.
- pgrust #40 asks for a pure-Rust libpq: native crate + C-ABI `libpq.so`,
  GSSAPI feature-gated, cross-compilation friendly → M2 scope.
- pgrust reads share assets (timezone, tsearch) from `PGRUST_PGSHAREDIR` at
  runtime → M4 embeds them.
- Owner decision (this session): all CLIs use usage-rs → ADR-0004. Probe:
  `usage-rs` 6.9.1, MIT, 10 crates, builds in ~9 s on 1.96; needs
  `unknown_flags = "error"` (default swallows unknown flags into positionals)
  and `disable_help_flag`/`disable_version_flag` to keep upstream spellings.

**Checks run** (Rust 1.96.0, this container, no PostgreSQL installed)
- `cargo fmt --all --check` clean.
- `cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic` clean.
  Two style lints are allowed at crate level with a comment (`doc_markdown`,
  `module_name_repetitions`); `struct_excessive_bools` is allowed on the one
  struct that mirrors C's option table. Note: crate attributes are needed because
  the CLI's `-W clippy::pedantic` outranks the workspace `[lints]` table.
- `cargo test --all-features`: 46 tests pass (testkit 13, rinitdb 17 unit +
  4 integration, pgdrop 6 unit + 4 integration, rpsql 2). The byte-diff gate
  against C `initdb --help/--version` printed `SKIP (flagged, not silent)`
  here; it runs for real once CI installs the PGDG 18 packages (NAT-374).
- Not run: anything needing a pgrust build (nothing links pgrust yet).
- Correction: the first scaffold commit was pushed while clippy still had four
  findings (a grep in the check script hid the exit code). The follow-up commit
  fixes them; the numbers above are for the branch head. Lesson recorded in
  AGENTS.md: gate on the command's exit status, never on filtered output.
- CI note: `ci.yml` runs on pushes to `main` and on pull requests, so this
  branch gets its first CI run when a PR is opened.

**Risks / open questions**
- Licensing (ADR-0003): repo is MIT, pgrust is AGPL-3.0; `pgdrop` and `rpsql`
  are AGPL-derived. Owner decision needed.
- Embedded template limits the locale/encoding matrix until M5.
- TLS backend for rlibpq (NAT-392) and line editing for rpsql (NAT-405) are
  dependency decisions awaiting approval.
- GitHub API access to `malisper/pgrust` is not available from this
  environment (read-only clone works); upstream issue filing (NAT-414) needs
  the owner or a session with that repo attached.

**Follow-ups**
- NAT-374 gate runner, NAT-375 ADR sign-off, NAT-376 vendor pgrust.
