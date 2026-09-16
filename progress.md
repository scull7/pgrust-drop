# progress.md — running log

Newest first. Each entry: what, why, checks run, risks, follow-ups.
Linear: project *pgrust-drop* (team NAT). GitHub: `scull7/pgrust-drop`.

## 2026-09-16 — NAT-378 rinitdb pre-flight validation

**What**
- `rinitdb::validate`: `validate(&Options, &Environment, &dyn FsProbe) -> Result<Plan, InitdbError>`,
  the whole of `initdb.c`'s `main()` from the getopt switch arms down to
  `create_xlog_or_symlink`, as one pure calculation. The only thing it asks of
  the outside world is `FsProbe::check_dir`, a port of `pg_check_dir`
  (`src/port/pgcheckdir.c:32`) with its six outcomes as an enum, so all 33 unit
  tests run against a table of fake directories and touch no disk.
- `rinitdb::error`: fifteen `InitdbError` variants, one per `pg_fatal` /
  `pg_log_error` site, each citing its `initdb.c` line. `render()` reproduces
  `pg_log_generic_v` (`src/common/logging.c:99`) exactly — the `error:`,
  `detail:` and `hint:` lines, and the fact that a hint carrying an embedded
  newline prints its second line *without* the `initdb: hint: ` prefix, which
  is what the `lost+found` case needs.
- `rinitdb::encoding`: `pg_encname_tbl[]`, `clean_encoding_name`,
  `pg_char_to_encoding` and `PG_VALID_BE_ENCODING` from
  `src/common/encnames.c` and `src/include/mb/pg_wchar.h`. `--encoding` had to
  be validated for real rather than pattern-matched for "utf8": the
  `--builtin-locale=C.UTF-8` rule turns on the encoding's *identity*, so
  `UTF8`, `UTF-8` and `Unicode` must all pass and `SJIS` (a client-only
  encoding, not an unknown name) must fail.
- `crates/rinitdb/tests/t_001_initdb.rs`: eleven more cases from
  `001_initdb.pl`, upstream names and upstream order, each asserting both the
  stolen `command_fails` and the exact stderr C writes, then running the gate.

**Why the order is the deliverable, not just the messages.** Several of these
conditions hold at once on the test's own command lines and C reports exactly
one of them: `--sync-only` returns before the superuser and locale checks;
`--pwprompt` + `--pwfile` is caught before the data directory is even resolved;
`create_data_directory` runs before `create_xlog_or_symlink`, so a non-empty
PGDATA outranks a bad `--waldir`. Five unit tests pin orderings rather than
messages.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 194 tests (was 125). Every gate prints
`SKIP (flagged, not silent)`: there is no PostgreSQL 18 on this box
(`docs/nightshift/2026-09-16.md`).

**Gate scope.** Six of the twelve new gates are judged on stderr and the exit
status only, through a new `testkit::Scope`. By the time C reaches those six
errors
it has already printed "The files belonging to this database system will be
owned by …" and its `creating directory … ok` progress, which rinitdb produces
only when cluster creation exists (NAT-379 … NAT-387); the issue's Acceptance
scopes them to "stderr + rc" for exactly that reason. This is not a quiet
narrowing: the stdout difference is still compared, still rendered, and
`assert_clean` prints it behind `OUT OF SCOPE (flagged, not silent)`. The other
six stay strict, because C prints nothing on stdout before those errors either. Row in `docs/divergences.md`.

**Risks**
- The transcribed stderr is only as good as the transcription until a
  PostgreSQL 18 binary exists to diff against. Mitigated by asserting the exact
  bytes in the integration test as well as in the unit test, so the two have to
  be wrong the same way.
- `--set foo=bar` is listed in the issue's Cases but is **not** a pre-flight
  error in C: `initdb` never validates a GUC name. It writes the setting into
  `postgresql.conf` (`initdb.c:1430`) and passes it to the child
  `postgres --boot`, which is what rejects it, after the data directory has
  been created. The issue's Goal sentence — "every case … that needs no server"
  — excludes it. Faking it with a name allowlist would also be wrong: custom
  GUCs such as `plpgsql.check_asserts` are legal `-c` arguments. Left for the
  cluster-creation issues; `-c NAME` with no `=` (`initdb.c:3273`), which *is*
  pre-flight, is implemented.

**Follow-ups**
- `canonicalize_path` (`src/port/path.c`) is not ported, so paths appear in
  messages as typed (`docs/divergences.md`).
- Still unvalidated, and none of them in this issue's Acceptance:
  `--wal-segsize` (`option_parse_int` plus the power-of-two rule,
  `initdb.c:3466`), `--sync-method` (`parse_sync_method`), the authentication
  methods (`check_authmethod_valid`), and `-E` left unset, which C derives from
  `LC_CTYPE` via `nl_langinfo` and `Plan::encoding` therefore leaves `None`.
- Unchanged from the entries below (the `pgdrop` manifest sets both `license`
  and `license-file`).

## 2026-09-16 — NAT-373 review fixes

**What** (all five reviewer findings were real)
- `pattern::at_start`: under `(?m)`, `^` matched at the position past a
  trailing newline, where Perl refuses — `MBOL` requires `!NEXTCHR_IS_EOS`
  (`regexec.c`). `"a\n" =~ /^$/m` is nomatch in Perl but matched here, so a
  stolen `command_like` carrying a `/m`-anchored empty-line pattern would have
  passed on output Perl rejects: precisely the fidelity `docs/divergences.md`
  promises. Now guarded with `pos < chars.len()`, pinned by
  `multiline_start_refuses_the_position_after_a_trailing_newline`.
- `files::walk`: an ignore-list hit skipped the entry *and* its whole subtree.
  Upstream's `wanted` only `return`s and never sets `$File::Find::prune`, so
  `File::Find` still descends and every file under an ignored directory is
  still checked. Ignoring `pg_wal` therefore stopped checking everything below
  it — a silently narrowed gate. The walk now descends first and filters the
  entry afterwards; `an_ignored_directory_is_still_descended_into` builds
  `data/pg_wal/badfile` at 0644 and demands the violation upstream reports.
- `files::walk`: `read_dir` propagated `NotFound` while the `stat` above it
  tolerated the same error, so a directory the running server deletes mid-walk
  (the `pg_stat` case the doc comment cites) was a hard error instead of a
  warn-and-skip. Both calls now warn and continue; a dangling symlink pins it.
- `pattern`: `\s` used `is_ascii_whitespace`, which leaves out the vertical
  tab; Perl's `\s` has matched `\x0b` since 5.18. Added, with the whole ASCII
  set asserted in `perl_whitespace_includes_the_vertical_tab`.
- `run`: the joined command line was built on every call, passing ones
  included, and the `program_*` wrappers had started allocating an `OsString`
  per literal flag. Both are failure-path-only data: `assert_clean` now formats
  inside the `if`, and the flag helpers never build an `OsString` at all.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 125 tests (was 121; +4 regression tests).
The rinitdb gate still prints `SKIP (flagged, not silent)`.

**Risks**: none new. The two `must` fixes both make the checks stricter, so a
test that passed on the old code and fails now was passing wrongly.

**Follow-ups**: unchanged from the entry below (the `pgdrop` manifest sets both
`license` and `license-file`).

## 2026-09-16 — testkit TAP helpers completed (NAT-373)

**What** (the rest of the `Utils.pm` port list NAT-373 names)
- `testkit::pattern`: the Perl-regex subset `command_like` and
  `command_fails_like` need, on the standard library alone. The issue left the
  choice open between the `regex` crate and substring/glob matching; `regex` is
  not on the approved list and substring matching would quietly weaken every
  stolen `qr//` that uses `\d+`, `.*`, a class or an anchor, so the patterns
  are matched verbatim by a parser plus a Thompson NFA simulation. Supported:
  literals and escapes, `.`, classes with ranges and negation, `\d \D \w \W
  \s \S`, groups, alternation, `* + ? {n} {n,} {n,m}`, and `^`/`$` with Perl's
  semantics (including `$` matching before a trailing newline, which every
  stolen `…8$` pattern depends on). Anything outside the subset —
  backreferences, lookaround, `\b`, `/x` — is a `PatternError` at construction,
  never a silent mismatch. The NFA simulation, rather than a backtracker, is
  why `(a*)*b` against 2000 `a`s is linear instead of exponential; that case is
  a test.
- `checks::command_ok` / `command_fails` / `command_like` /
  `command_fails_like` (Utils.pm:866, :883, :1006, :1059) as pure functions over
  a `CommandOutcome`, with the spawning wrappers in `run`. Each keeps upstream's
  exact assertion set: `command_ok` looks only at the exit status because
  `run_log` reports nothing else, `command_like` also demands an empty stderr,
  `command_fails_like` does not look at stdout.
- `testkit::files`: `slurp_file` (Utils.pm:512, with the optional offset, bytes
  not `String`) and `check_mode_recursive` (Utils.pm:601) — pure
  `mode_violations` / `is_ignored` over a `Vec<Entry>`, with a `walk` action
  that follows symlinks like `File::Find`'s `follow_fast`, remembers
  `(device, inode)` so a symlink loop ends the walk, and tolerates `ENOENT` for
  the files a running server deletes under it.

**Why**: NAT-373's acceptance is a unit test for every check plus rinitdb's
first integration test on the `program_*` helpers; the latter already landed
with NAT-374, the former needed these six helpers, and they are what the
upstream `t/*.pl` files call on nearly every line after the `program_*` block.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 121 tests (was 84). The rinitdb gate still
prints `SKIP (flagged, not silent)`: no PostgreSQL 18 on this box.

**Risks**: the pattern subset is the one real risk. A stolen `qr//` outside it
fails to compile rather than mismatching, so the failure is loud, but a future
test may need a construct that has to be added (`\b` and lookaround are the
likely ones). Character classes fold only ASCII case under `(?i)`; no upstream
pattern uses `(?i)` on non-ASCII.

**Follow-ups**: `crates/pgdrop/Cargo.toml` sets both `license` and
`license-file`, which cargo warns about on every build — one of the two should
go (not touched here, it is outside this issue). Otherwise unchanged from the
entries below.

## 2026-09-16 — NAT-374 review fixes

**What** (all five reviewer findings were real)
- `diff`: the Myers trace kept a full-width diagonal array per distance, so
  memory was `D * (rows + cols)`, not the `D^2` the doc claimed — 543 MB to
  render two all-different 4k-line outputs, ~13 GB for two 100k-line psql
  outputs, which OOM-kills the test process and destroys the gate failure the
  renderer exists to explain. Each distance now records only the `2d + 1`
  diagonals reachable at it, and `MAX_EDIT_DISTANCE` is 1024, so the trace is
  `(D + 1)^2` words ≈ 8 MB whatever the inputs. The common head and tail are
  also trimmed before the search and only `CONTEXT_LINES` of each are
  reattached, so a one-line change in a 50k-line output costs a handful of
  edits. Measured: the diff tests now pass under `ulimit -v` of 48 MB, and the
  testkit unit suite went from 0.82 s to 0.04 s.
- `reference::skip` / `announce_skip`: the `SKIP (flagged, not silent)` line was
  a `println!` in a *passing* test, which libtest captures and replays only on
  failure or under `--nocapture` — so CI's plain `cargo test --all-features`
  showed no trace of the repo's only real gate being skipped. Verified both
  ways: `println!` and `eprintln!` are captured, a write to the `io::stderr()`
  handle is not. `cargo test --all-features` now prints the SKIP line.
- `gate`: `run` returns `GateError::Spawn { side, path, source }` (thiserror,
  approved) instead of a bare `io::Result`, so "the reference is not installed"
  and "the candidate is not built" are told apart.
- `gate`: per-stream verdict is a `StreamDiff` enum (`Match` / `Text` /
  `Binary { at, reference_len, candidate_len }`) instead of an
  `Option<String>` folding three outcomes into one string.
- `normalize::pid`: anchored on a word boundary. It matched "PID " anywhere, so
  `"RAPID 42 rows"` became `"RAPID <pid> rows"` and a DEFAULT-normalized gate
  could have masked a real numeric difference.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 84 tests (was 80; +4 regression tests:
`one_change_in_a_long_output_stays_small`,
`past_the_edit_budget_the_whole_file_is_replaced`,
`pid_does_not_fire_inside_a_word`, `skip_message_carries_the_flag`). Both live
gate controls re-run and still correct (reference symlinked to `rinitdb`
passes; `/bin/echo` fails with the diff).

**Risks**: the edit budget is now 1024 rather than 4096, so a mismatch needing
more than 1024 edits renders as one whole-file replacement hunk instead of a
minimal script. That is a readability trade for a hard memory bound; the
verdict is still the byte comparison, never the rendering.

**Follow-ups**: unchanged from the entry below.

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
