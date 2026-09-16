# progress.md — running log

Newest first. Each entry: what, why, checks run, risks, follow-ups.
Linear: project *pgrust-drop* (team NAT). GitHub: `scull7/pgrust-drop`.

## 2026-09-16 — testkit byte-diff gate runner (NAT-374)

**What**
- `testkit::gate`: `Gate { reference, candidate, args, stdin, normalizers }` →
  `GateReport { stdout_diff, stderr_diff, rc }`. `gate::compare` is the whole
  verdict as a pure function over two `CommandOutcome`s; `Gate::run` is the one
  action that spawns. `Gate::for_tool` reuses the existing `PGDROP_REF_BIN` →
  PGDG → Homebrew discovery and yields `None` so the caller prints
  `SKIP (flagged, not silent)`.
- `testkit::normalize`: three justified normalizers as pure
  `fn(&str) -> String`, each citing the upstream printf it rewrites —
  `Time: …` (`src/bin/psql/common.c:608`, covering all four duration forms),
  `PID n` (`common.c:755`), system identifier
  (`src/include/catalog/pg_control.h:107`). `DEFAULT` is the three in order.
- `testkit::diff`: in-process Myers diff rendered as `diff -U3`, matching
  pg_regress's `pretty_diff_opts` (`src/test/regress/pg_regress.c:65`), with
  diff(1)'s `\ No newline at end of file` marker.
- `testkit::run_with_stdin`: feeds stdin from a helper thread while the parent
  drains both pipes, so a tool that writes more than a pipe buffer before
  reading its input cannot deadlock a gate; `EPIPE` from a child that exits
  early is the child's business, not a failure.
- `rinitdb`'s `help_and_version_match_reference_initdb` now runs through the
  gate instead of `assert_eq!` on two byte vectors.

**Why**
The gate is how every later issue proves itself (`docs/test-stealing.md`), so
it had to exist before the ports that lean on it. The pure/action split is what
makes the comparison testable here, where no C reference binary exists.

**Checks run** (Rust 1.96.0, this container, no PostgreSQL installed)
- `cargo fmt --all --check` exit 0.
- `cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic`
  exit 0. `tests/gate.rs` needs its own crate-level `doc_markdown` allow, like
  the other integration tests: the CLI's `-W clippy::pedantic` outranks the
  workspace `[lints]` table.
- `cargo test --all-features` exit 0, 80 tests (was 46): +30 testkit unit
  (8 diff, 10 normalize, 12 gate), +4 testkit integration.
- The gate itself was proved non-vacuous by hand, since the byte-diff gate here
  can only SKIP: with `PGDROP_REF_BIN` pointing at a directory whose `initdb`
  is a symlink to `rinitdb`, the gate runs live and passes with no SKIP line;
  pointing it at `/bin/echo` fails the test with the unified diff.

**Risks**
- A mismatching stream that is not valid UTF-8 is reported as "differs and is
  not valid UTF-8" with the first differing byte offset rather than diffed.
  That is deliberate: `from_utf8_lossy` maps every invalid byte to U+FFFD and
  could call two different outputs equal.
- The diff renderer stops looking for a minimal edit script past
  `MAX_EDIT_DISTANCE` (4096) and prints one whole-file replacement hunk. It
  only affects how a failure reads; the verdict is always the byte comparison.
- The gate hands both binaries the same environment but does not pin one
  (`LC_ALL`, `TZ`, `PGCLIENTENCODING`). Both sides see the same values, so a
  comparison stays honest, but two machines can gate on different text.

**Follow-ups**
- Pinning a gate environment is worth its own issue once a gate runs something
  locale-sensitive (M3, rpsql).
- `crates/pgdrop/Cargo.toml` sets both `license` and `license-file`, so every
  cargo invocation warns. Pre-existing, one line, not touched here.

## 2026-09-16 — Owner decisions applied (NAT-375, NAT-392, NAT-398, NAT-405)

**What**
- ADR-0001 accepted: pgrust as a rev-pinned Cargo git dependency.
- ADR-0003 accepted: per-crate licensing. `testkit`/`rinitdb`/`rlibpq`/`rpsql`
  MIT, ported from PostgreSQL C only (no pgrust code copied); `pgdrop`
  AGPL-3.0-only (`crates/pgdrop/LICENSE`, `NOTICE.md`, `license` fields).
  rpsql will be written fresh from `src/bin/psql` C sources.
- ADR-0005: rpsql line editing with `redox_liner` (`noline` rejected: no
  completion hook, no history file, MPL-2.0).
- ADR-0006: rlibpq TLS backend is `rustls` behind a `tls` feature with a
  `NoTls` fallback; crypto provider chosen in NAT-392.
- PR opened for the branch so CI runs.

**Checks run**: `cargo fmt --check`, pedantic clippy, `cargo test` (46 tests)
still green; the license text for pgdrop is the AGPL-3.0 file from pgrust's
repository verbatim.

**Follow-ups**: NAT-398 rewritten (port from C, not import); NAT-389/388/391
now say "read pgrust for how, port from C"; M3 gains no new issue yet, the
`describe.c`/`print.c` ports were already scoped as fresh work.

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
