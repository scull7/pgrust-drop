# progress.md — RETIRED historical archive

> **This file is closed. Do not add entries.**
>
> **Linear is the project's source of truth** — project *pgrust-drop*, team NAT:
> https://linear.app/scull7/project/pgrust-drop
>
> Status, decisions, risks and follow-ups belong on the Linear issue. The PR
> description carries the narrative of a change. See `AGENTS.md` ("Change
> hygiene") for the rule, and NAT-421 for why it changed.
>
> Two things still live in the repo rather than Linear, because they are
> durable artifacts rather than project state: `docs/adr/` (architecture
> decisions) and `docs/divergences.md` (each row pins a deliberate divergence
> to the test that proves it).
>
> What follows is the log as it stood through 2026-09-17, kept because it
> records how the project got here. Newest first.

## 2026-09-16 — Review of the nightshift branch, and the gates run for the first time

**What**

A two-part code review of the `nightshift/2026-09-16` branch (style and
architecture), then six PRs against that branch, one reviewable chunk each:

| PR | Branch | Chunk |
| -- | ------ | ----- |
| #4 | `claude/fix-ci-gates`     | CI installs PGDG 18; `PGDROP_REQUIRE_REF` strict mode |
| #5 | `claude/fix-rlibpq-style` | named `AUTH_REQ_*`; cast invariants; `pg_config` cfg arms |
| #6 | `claude/fix-records`      | ADR-0001/0004 amendments; `AGENTS.md` notes; license field |
| #7 | `claude/fix-test-gates`   | `Gate::for_tool_or_skip`; two weak tests replaced |
| #8 | `claude/fix-rpsql-bytes`  | byte-exact error path; dispatch dedup; borrowing `VarView` |
| #9 | `claude/fix-rlibpq-port`  | `port` validated with upstream's two messages |
| #10 | `claude/progress-log`     | this entry |
| #11 | `claude/fix-vendored-samples` | strip the `# PGRUST` block; re-vendor all three samples from pristine 18.6 |

**Why**

The branch was green everywhere and proving nothing. `.github/workflows/ci.yml`
never installed PostgreSQL, so all 31 byte-diff gates printed
`SKIP (flagged, not silent)` and passed. `docs/test-stealing.md` rule 4 claimed
the opposite ("CI installs the PGDG 18 packages so the gate is real there"),
which is the sentence a future reviewer would have trusted. AGENTS.md's "never
weaken a gate to get green" was honoured inside the gate code and defeated by
the pipeline around it.

**The gates ran for the first time (PR #4, job 104943279701): 30 passed, 4 failed**

- `help_and_version_match_reference_initdb` — PGDG builds with
  `--with-extra-version`, so C prints `18.6 (Ubuntu 18.6-1.pgdg24.04+2)` where
  `help.rs:5` has the stock `18.6`. Harness gap, not a port bug; wants a fourth
  justified normalizer in `testkit/src/normalize.rs`. **The `--help` half passed
  byte-for-byte** — the 41-option usage text is now proven against a real C
  binary for the first time.
- `existing_nonempty_xlog_directory` and `relative_xlog_directory_not_allowed` —
  one underlying gap. C's `initialize_data_directory()` creates PGDATA first
  (`made_new_pgdata`, `initdb.c:2907`) and checks `--waldir` second, so on exit
  `cleanup_directories_atexit()` (`initdb.c:771`) logs
  `initdb: removing data directory "…"`. rinitdb validates in a pure pre-flight
  before any mkdir, so there is nothing to remove and no line to print.
  Corroboration: the sibling `existing_data_directory` passed, because there C
  also bails before `made_new_pgdata`. Note this is an *ordering* divergence —
  porting the handler alone will not produce the bytes; PGDATA must be created
  before `--waldir` is validated, which `validate.rs:1056` currently pins the
  other way. NAT-385/NAT-387.
- `the_configuration_files_match_reference_initdb` — two hunks, nothing else
  differs in `postgresql.conf` (the other 886 lines are byte-identical):
  1. `unix_socket_directories` — Debian's `--with-socketdir=/var/run/postgresql`
     against our stock `/tmp`. **Pre-declared**: `docs/divergences.md:21` already
     said "the byte-diff gate against it is what would surface it." The
     whitespace difference (space vs TAB before the comment) is also correct —
     `replace_guc_value` tabs to column 40, which fits a tab after the 33-column
     `'/tmp'` but only one space after the 48-column `'/var/run/postgresql'`.
     That our renderer got the `/tmp` case exactly right is evidence the
     algorithm port is faithful.
  2. A 41-line `# PGRUST` tail. See below — this one is serious.

**Finding: pgrust content is vendored inside an MIT crate**

`crates/rinitdb/share/postgresql.conf.sample` lines 890-930 are a
`# PGRUST` section of pgrust-specific GUCs (`pgrust.admission_bypass`,
`shared_catalog_cache`, `preload_contrib`, …). The file itself says "Settings
specific to pgrust (not present in PostgreSQL)."

The vendoring was faithful; the source was not. All three samples are
byte-identical to pgrust's `crates/postgres-18.6-reference/` tree (verified by
sha256 against a fresh clone of `malisper/pgrust`), and **that tree is not a
pristine PostgreSQL 18.6 checkout** — pgrust added its own settings to it.
`pg_hba.conf.sample` and `pg_ident.conf.sample` are clean.

Two consequences:
- **Licensing.** pgrust is AGPL-3.0. ADR-0003 says never copy pgrust code,
  comments or test corpora into the MIT crates. `share/README.md` asserts these
  files are "copied **byte for byte** from the PostgreSQL 18.6 source tree" and
  carry the PostgreSQL licence. That claim was false for this file.
- **Correctness.** rinitdb writes pgrust-only GUCs into every cluster it
  creates. Real initdb does not.

Nothing caught it because `conf.rs:711`
(`the_embedded_templates_are_the_postgresql_18_6_bytes_we_vendored`) pins
length + digest, so it pins the *contaminated* bytes and passes. It proves
"unchanged since we vendored"; its name claims "is PostgreSQL 18.6". Those are
different assertions, and the gap was invisible until something diffed against a
real PostgreSQL. Neither of the two code reviews found it; the gate did, on its
first run.

**Resolved by PR #11.** All three samples re-vendored wholesale from pristine
PostgreSQL 18.6, verified two independent ways that agree byte for byte: the
release tarball (published sha256
`555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f`, checked) and
the `REL_18_6` tag (`724edf9bde9d356724ad384a2e196edc3c9f80f7`). The diff proved
to be a single hunk removing exactly the 41-line `# PGRUST` tail — zero drift in
the other 889 lines, so nothing had to be quietly absorbed. `pg_hba.conf.sample`
and `pg_ident.conf.sample` were genuinely pristine already; their pinned digests
needed no change, which corroborates it independently. `postgresql.conf.sample`
is now 32 652 B, digest re-pinned to `0x364a_ac11_a839_c67f`.

The digest pin was kept and two assertions added beside it, both with
expectations written *from upstream* rather than derived from the file — which
is exactly what a digest cannot do, since it is computed from the file and so
blessed the contaminated bytes: `the_conf_sample_carries_only_upstreams_sections`
(the 15 banner titles PG 18.6 ships, in order — catches an appended or renamed
vendor block, and a missing upstream section) and
`no_setting_in_the_conf_sample_is_namespaced_like_an_extension` (no assigned
name contains a `.`, since core GUCs are never namespaced — catches a vendor GUC
hidden inside an existing section, where no new banner would appear). Both were
proven non-vacuous by stashing the old bytes back: both fail, suite exits 101.

No existing test had pinned the contaminated output, so nothing was deleted or
weakened to accommodate the fix.

**Checks run** (Rust 1.96.0, gating on exit status, never on grepped output)

- Branch head `15891ca` re-verified independently: `cargo fmt --all --check`,
  pedantic clippy and `cargo test --all-features` all exit 0; 644 passed,
  1 ignored, 31 flagged SKIPs. The branch's own claims were accurate.
- Every one of the six PRs passes all three gates on its own head.
- Strict mode proved to bite: `PGDROP_REQUIRE_REF=1 cargo test --all-features`
  exits 101 with 29 gates failing instead of skipping; without it, 0.

**Risks / open questions**

- `AGENTS.md` still defines "upstream" as the tree vendored in pgrust at
  `crates/postgres-18.6-reference/`. That definition is what let the
  contamination in and should name genuine upstream (`REL_18_6` @
  `724edf9bde9d356724ad384a2e196edc3c9f80f7`, or the tarball with its published
  sha256). `share/README.md` and ADR-0003 carry the same instruction.

**Provenance audit: the blast radius is one file**

Against two independent pristine sources that agree (tarball sha256
`555610c2…` verified, and `REL_18_6` @ `724edf9b`), pgrust's reference tree is,
of 7 284 files, **7 281 byte-identical, 2 differing, 1 absent**
(`src/port/win32ver.rc`, a gap that tree does declare). Only two differ:
`postgresql.conf.sample` (the `# PGRUST` block, fixed in #11) and
`src/test/regress/data/streets.data` (one word on line 1378, collateral from a
rename, referenced nowhere here). Of the 63 distinct upstream paths cited across
the MIT crates, exactly one intersects that divergent set — the sample file. For
the other 62, pgrust's copy and pristine are bit-identical, so "which tree was
followed" is moot and no divergence could have leaked.

So the earlier worry that every port and citation rested on suspect content was
the right precaution and the wrong prediction. The definition was dangerous in
principle; in practice it cost exactly one file.

Also verified clean against pristine 18.6: `pg_hba`/`pg_ident` samples, the
crc32c table (generated from the polynomial, not transcribed; its 16 pinned
literals match), `initdb`'s `subdirs[]` (23), `PG_ENV_KEYS` (30),
`PQconninfoOptions[]` (50 rows, field by field), the `001_uri.pl` corpus (63
rows x 3 fields, byte-identical and in order), `MON_LENGTHS`/`YEAR_LENGTHS`, and
the `initdb --help` text (reassembled from the C `printf` literals; identical
but for placeholder spelling).

No pgrust **Rust** code, comments or test data is present in the MIT crates,
established four ways: highest normalized-line overlap anywhere is 19%
(`hmac.rs`, i.e. RFC 4231 vectors and standard round arithmetic); of comment
lines >= 40 chars, pgrust has 197 227 distinct and this repo 3 824, sharing
**two** — one a `------` rule, one a verbatim PostgreSQL C comment both projects
copied from the same source; citation sets share 3 of 5 571 vs 856; and pgrust's
URI test corpus shares **zero** lines with `t_001_uri.rs`. The two
near-identical functions (`is_create_routine`, `is_copy_from_stdin`) are
transliterations of 6-line C predicates in `psqlscan.l:1006`/`:1019` that any
competent port must converge on.

**New defect found by the audit: ~32 wrong `file:line` citations**

Not contamination — transcription errors. They match neither pgrust's tree
(identical to pristine for these files) nor PG 15/16/17, and the deltas are
irregular, which rules out a version shift. Of the 241 citations that could be
mechanically anchored to a named C function, 209 (87%) land inside it; the
failures cluster hard. `crates/rlibpq/src/result.rs` is the worst: **11 of 11
anchored citations are wrong**. Spot-verified independently: `:309` cites
`fe-exec.c:3432` for `PQntuples`, which is a comment terminator (real: 3512);
`:303` cites `:3441` for `PQnfields`, which is the line `ExecStatusType` (real:
3520); `:297` cites `:3219` for `PQresultStatus`, which is
`case PGASYNC_READY_MORE:` (real: 3442). Also `rlibpq/src/md5.rs:94` names
`src/include/md5_int.h`, a path that does not exist (real:
`src/include/common/md5_int.h`).

This matters because the porting rule's whole value is that a reviewer can
follow a citation to the C and check the port. A citation that lands on a
comment terminator cannot be checked, and silently wastes the reviewer's time.
The ~748 citations with no backticked identifier near them could not be
mechanically anchored and remain unverified.
- PR #4's CI is red by design. Merging locks in enforcement so no future gate
  can silently skip; holding keeps the branch green and the hole open.
- `QuoteType::ShellArg` folds into `Plain` in `rpsql`. Confirmed unreachable —
  nothing in the workspace produces it — so it is latent, not live. It is still
  a trap: whoever implements backquote expansion gets an unquoted substitution
  where C shell-quotes.
- PR #5's Windows `#[cfg]` arm for `DEFAULT_PGSOCKET_DIR` is the only behaviour
  change across the six PRs that nothing in CI builds.
- PR #9 now refuses a comma-separated `port` list, which the URI parser really
  does produce. It was already broken (silently port 5432 against a literal host
  `a,b`); it now fails loudly. Full fidelity needs multi-host support.
- The config gate panics on first mismatch, so `postgresql.auto.conf`,
  `pg_hba.conf` and `pg_ident.conf` were never compared this run, and only the
  `conf-trust` case ran. "Config files match" is not yet established.

**Follow-ups** (now tracked in Linear, which is the source of truth from here on)

- NAT-423 propagate the new rules into `ORCHESTRATOR.md` and `README.md`, which
  still instruct workers to append to this file and to cite pgrust's tree;
  NAT-417 report pgrust's reference-tree provenance defect upstream (deferred);
  NAT-418 the licensing defect and its fix; NAT-419 the ~32 wrong `file:line`
  citations; NAT-420 `cleanup_directories_atexit` + `--waldir` check ordering;
  NAT-421 this process change; NAT-422 the smaller review follow-ups.
- NAT-374: `libpq_uri_regress` ships in no PGDG package, so its two gates still
  skip; `UNSHIPPED_TOOLS` exempts it by name.
- NAT-399: `psql --help`. Note there is no `--help`/`--version` byte-diff gate
  against C psql at all — ADR-0004's claim described a gate that does not exist.
- Six `Gate::for_tool` call sites remain to migrate to `for_tool_or_skip`
  (3 in `rinitdb/tests`, 3 in `rpsql/tests`); `for_tool` can go private after.
- `rinitdb::validate::strerror` is borrowed by `pgdrop`; the `%m` formatter
  wants a shared home.
- A cross-crate assertion that `rlibpq` and `rinitdb` agree on
  `DEFAULT_PGSOCKET_DIR` — `pgdrop` depends on both and is the natural host.
- Corrections found by the implementing agents, recorded because the review was
  wrong and the code is right: `invalid port number` lives at `fe-connect.c:3046`
  (not `connectOptions2`), and `port=abc` yields `invalid integer value …` from
  `pqParseIntParam` (`:8231`) instead; upstream's `001_uri.pl` genuinely repeats
  `postgresql://host/db`, so "all URIs distinct" would have been a wrong
  invariant; `scan::step`'s `too_many_lines` allow was stale (25 lines) and was
  deleted rather than justified.

## 2026-09-16 — NAT-385 review fixes

**One correction to the record, three real fixes.**

- **The record.** 7a43f8c's commit message opens "`timezone` and `log_timezone`
  were a hard-coded GMT. They now come from `select_default_timezone`", and the
  progress.md entry it landed said the same. That is false: the commit does not
  touch `conf.rs`, `Settings::default()` still carries `Some("GMT")`
  (`conf.rs:221`), and nothing outside the two tests calls
  `select_default_timezone` — the entry's own Follow-ups paragraph said so, so
  it contradicted its own **What**. The commit message cannot be amended; this
  entry and the corrected **What** below it are the correction. What 7a43f8c
  actually landed is the selection as a library, with its gate and its pin.
  Wiring it in is `test_config_settings` (`initdb.c:1140`), which is NAT-381's
  and is not started.
- **The `/etc/localtime` pin did not bite.** It asserted
  `target.ends_with(&chosen)`, which on this machine
  (`/etc/localtime -> /usr/share/zoneinfo/Etc/UTC`) also passes for `"UTC"` —
  and `"UTC"` is exactly what a regression would produce, because the
  brute-force scan's `zone_name_pref` prefers the bare name over `Etc/UTC`
  (measured: the scan really does answer `"UTC"` here). It now reconstructs the
  tails `check_system_link_file` walks (`findtimezone.c:566`) and asserts the
  answer is the *first* one whose file carries `/etc/localtime`'s bytes, which
  is `"Etc/UTC"`. The pin passing while the scan answers `"UTC"` is what shows
  it is no longer vacuous.
- **`localtime()` allocated per call.** `PgTm::zone` was a `String`, where
  upstream's `tm_zone` is a `const char *` into `sp->chars`
  (`localtime.c:1337`). `Probe::score` calls `localtime` once per test time, so
  a brute-force scan paid up to 5200 allocations per candidate. `PgTm` now
  borrows `&[u8]` out of its `State` — bytes, because `strcmp` is what upstream
  compares — with `zone_name()` for the two callers that render it. The scan
  over all 506 zones went from 186 ms to 14.8 ms.
- **Citation drift.** Sixteen `localtime.c` citations named lines 1-16 off the
  code they describe (the transitions loop, the leap-second loop, the
  trailing-no-op `while`, the `tm_wday` computation and a dozen function
  headers), which blunts the grep-ability the porting rule exists for. Every
  `localtime.c:` line in `tz.rs` was then re-resolved against the reference
  tree; all 45 now land on the statement, comment or declaration they name.
  The `findtimezone.c` citations were already exact and are unchanged.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 644 tests, unchanged, 31 SKIP-flagged gate
lines, unchanged. No behaviour changed: the borrowed abbreviation compares the
same bytes, and the pin is strictly stronger.

## 2026-09-16 — NAT-385 rinitdb default timezone selection

**What**. A port of `select_default_timezone`
(`src/bin/initdb/findtimezone.c`), as a library with its gate. It is not yet
wired into config rendering — see Follow-ups: `Settings::default()` still
carries `Some("GMT")` (`conf.rs:221`) and the only callers of
`select_default_timezone` are the two tests. Two modules:

- `rinitdb::tz` is the read half of PostgreSQL's timezone library
  (`src/timezone/localtime.c`): `tzload` over a TZif image, `tzparse` for a
  POSIX TZ string, `localsub`/`timesub`, and `pg_tz_acceptable`. It is pure —
  `tzload` takes the file's bytes, not a path — and it is the piece everything
  else stands on, so it was checked against the machine's own C library before
  anything was built on it: 4990 instants across all 499 zones of
  `/usr/share/zoneinfo`, every field `compare_tm` compares, zero differences
  against `date`. That covers the v2 footer splice (`localtime.c:417`), which
  zic's "slim" output makes load-bearing: without it every DST zone freezes at
  its last stored transition.
- `rinitdb::findtimezone` is the selection itself over a `TzSource` trait — the
  timezone directory, the `/etc/localtime` symlink, `getenv("TZ")` and the
  clock are the only things it reads. `Probe` holds the test-date set and what
  the system's zone makes of it; `score`, the `zone_name_pref` tie-break, the
  symlink shortcut, the `STD<ofs>DST` constructed names and the `Etc/GMT±N`
  last resort are all upstream's, cited line by line.

**Why the scoring survives at all**. Upstream scores candidates against the C
library's `localtime()`. This port cannot call it (no libc, `deny(unsafe_code)`),
so it reads the definition the C library itself reads — `$TZ`, else
`/etc/localtime` — with its own reader. That is the first of three divergence
rows; the other two are the timezone directory (`PGRUST_TZDIR` or a system
zoneinfo, which is upstream's own `SYSTEMTZDIR` arm) and `build_time_t`'s
`mktime`. None of them touches the scoring, the tie-break or the shortcut, so
on any machine whose C library resolves its zone from those two places — every
Linux and macOS one — the answer is upstream's.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 644 tests (611 before; 33 added), 31
SKIP-flagged gate lines (30 before; the new gate is the one).

**Gate**: the issue's acceptance criterion, `grep -E '^(log_)?timezone'` over C
initdb's `postgresql.conf` against this port's rendering, in four cases — `TZ`
deleted (which is what `001_initdb.pl:42` says its "successful creation" case
exists for, and on this machine the `/etc/localtime` path), then a named zone,
a POSIX-style name and a GMT-offset name. No PostgreSQL 18 on this box, so it
SKIP-flags. It is not the only pin: `the_default_time_zone_is_the_one_etc_localtime_names`
runs the real search on the real machine and asserts the answer is the zone
`/etc/localtime` points at, and it passes here.

**Risks**. The brute-force scan is the path least exercised on this machine —
`/etc/localtime` is a symlink, so the shortcut wins and the scan is only
reached in the unit tests, over a `FakeTz` database. Its tie-break is a total
order, so the answer does not depend on directory order, but a machine without
that symlink is where a difference would first show.

**Follow-ups**. Nothing calls `findtimezone::default_timezone()` yet:
`Settings` is still built by hand, because `test_config_settings`
(`initdb.c:1140`) — the probe stage that fills in `max_connections`,
`shared_buffers`, the DSM implementation *and* the time zone — is NAT-381's.
The function and its gate are ready for it. `Settings::default()` still carries
`Some("GMT")`, which is what a machine with no timezone database lands on and
what the struct's own doc comment calls a unit-test convenience.

## 2026-09-16 — NAT-398 review fixes

**What** (four of the seven findings were real bugs, two were tests or docs
claiming more than they checked, one was a needless clone)
- `-1`/`--single-transaction` parsed and then did nothing: it was read only by
  the "no actions" fatal check and appeared in neither the refusal list nor any
  BEGIN/COMMIT. `rpsql -X -1 -c 'create table t(n int)' -c 'selec 1'` left `t`
  committed where C psql discards it. The wrapper is now ported
  (`startup.c:366`, `:432`): BEGIN before the action list, then ROLLBACK when
  `ON_ERROR_STOP` made a failure fatal and COMMIT otherwise — upstream's own
  asymmetry, and the reason it is safe is that the server has already aborted
  the transaction, so that COMMIT discards the work by itself. A failed BEGIN
  skips the actions *and* the COMMIT under `ON_ERROR_STOP`, which is what
  upstream's `goto error` does. The choice is the pure `single_txn_finish`,
  tested over all four combinations.
- `echo_line` echoed for `ECHO=all` as well as `ECHO=queries`, but `SendQuery`
  echoes only for the latter (`common.c:1158`); `ECHO=all` is echoed per input
  line in `MainLoop` (`mainloop.c:360`) and per action in `main`
  (`startup.c:386`), both of which this port already did. Every query was
  printed twice under `-a`. The `MainLoop` test that should have caught it
  asserted `stdout.starts_with("select 1;\n")`, which the duplicate satisfies;
  it now counts occurrences and runs two statements.
- `print_aligned_text` had no `pg_wcsformat` (`mbprint.c:398`): a cell holding
  a newline went into the table raw and mis-measured its column, so
  `select E'a\nb'` broke the frame instead of rendering `a       +` / `b`.
  Cells and headers are now split per newline, the width is the widest line
  rather than the whole string, continued lines carry `pg_asciiformat`'s
  `nl_right` `+`, and a column that has run out of lines is blank-padded. This
  was silent corruption, which is exactly what the module's stated policy —
  refuse what it cannot render — exists to avoid.
- `\c` through `-c` fell past `CommandResult::Connect` to `EXIT_SUCCESS`, so
  `rpsql -X -c '\c otherdb'` did nothing and claimed to have done it. It now
  refuses with the same message `MainLoop` already used.

**Two tests and a doc corrected**. `an_unimplemented_format_is_refused` spawned
the binary and asserted only a nonzero exit, which the connection failure
supplied — its own comment admitted as much — so it never reached the `--csv`
refusal it was named for. It moved into `print.rs`, where the refusal is
decided, and now covers six formats and asserts the message names NAT-400. The
unported `psqlrc` handling (`startup.c:702`) was described only in progress.md
and Linear; it is a deliberate divergence and now has a row in
`docs/divergences.md` with a test that pins the flag and fails loudly if a
reader lands. progress.md's claim that "-l, -o, -L and -1 parse but are refused
at the point of use" was false for `-1` — it is now implemented, and the
sentence names only the three that are refused.

**One cleanup**. `run_session` cloned the whole action list, every `-c` string
included, on each run to dodge a borrow; it takes the list with
`std::mem::take` instead.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 611 tests (605 before; one weak
integration test removed, seven unit tests added), 30 SKIP-flagged gate lines,
unchanged.

**Risks**. The single-transaction wrapper has never run against a server: the
BEGIN/COMMIT statements and the `AcceptResult` check around them are reasoned
from the C, and the first live run is where they are proved. The newline
rendering is pinned against hand-computed expectations from `print.c`, not
against C psql's bytes, for the same reason every gate here skips.

## 2026-09-16 — NAT-398 rpsql startup/mainloop/command skeleton

**What**. `rpsql` stops being a `--version` stub and becomes psql's skeleton,
ported from the C (ADR-0003: nothing from pgrust's Rust psql). Ten modules,
each tracking one upstream file: `scan` is `fe_utils/psqlscan.l` as a
hand-written state machine over the same ten start conditions; `slash` is the
add-on lexer `psqlscanslash.l` bolted onto the same buffer stack; `variables`
is `variables.c`; `settings` is `settings.h`; `startup` is `startup.c`'s
`long_options[]` and `parse_psql_options`; `mainloop` is `MainLoop`; `common`
is `SendQuery`; `command` is `HandleSlashCmds` with the four commands the issue
names (`\q`, `\c`, `\echo`, `\set`, plus the `\unset`/`\qecho`/`\warn`
that share their code); `prompt` is `prompt.c`; `print` is as much of
`fe_utils/print.c` as the Acceptance line needs.

**Why this shape**. Upstream is one `pset` global that the lexer, the assign
hooks and the printer all reach into. Two substitutions keep the split the
issue's Design asks for. First, the assign hooks become *data*: `Assign` names
which `pset` field each variable controls, and `VariableSpace::settings` is one
pure function that derives every hook-owned field, so the twenty C hooks that
write a global become one calculation with one test. Second, the two actions
are behind traits — `Executor` for the connection, `LineSource` for input — so
`MainLoop`, `SendQuery` and every backslash command are tested without a
server, against results built by feeding wire frames through rlibpq's
`QueryRunner`.

The lexer is the one place fidelity is expensive and the tests say why: flex
picks the longest match and breaks ties by rule order, and the rules whose
*consumed length* differs from the naive reading are the ones that matter.
`=--` is a `=` operator and then a comment, not a three-character operator
(`psqlscan.l:824`); `+/*` is `+` and a comment start (`:263`); `1..10` throws
the `..` back (`:322`); a string continuation needs a newline (`:167`). Each is
a named test.

**Scope held**. `--help` is NAT-399's, so the stolen `program_help_ok` is
declared `#[ignore]` with that reason rather than deleted — the file keeps
upstream's order and the gap is visible. The rest of `print.c` is NAT-400's, so
`-H`, `--csv`, expanded and wrapped are refused with an error instead of
approximated. `\d` is NAT-401's, `\if`/`\gset` NAT-402's, `\copy`/`\g`
NAT-403's, interactive input and `\c`'s reconnect NAT-405's; each is named at
the point where it is missing.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 605 tests (471 before), 1 ignored, 30
SKIP-flagged gate lines (27 before; the three new ones are the Acceptance
`-X -c 'select 1'` gate, the SQL-corpus gate and `--version`).

**Risks**. The Acceptance gate has never executed: it needs a PostgreSQL 18
psql *and* a cluster, and this box has neither, so it skips on both counts
rather than on one. It is written to demand both — a gate over two connection
failures would prove nothing about `select 1` — and the aligned output it would
compare is instead pinned by unit tests that spell the bytes out. The lexer is
reasoned from the flex rules, not diffed against flex; the cases above are the
ones where that reasoning could be wrong, and each is tested, but the backend's
scan.l has rules psql's copy inherits that no test here exercises.

**Follow-ups**. (1) `print.c`'s width measurement counts characters, not
display columns — recorded in `docs/divergences.md`, and NAT-400 must close it
with `pg_wcssize`'s tables. (2) `-l`, `-o` and `-L` parse but are refused at the
point of use. (3) `psqlrc` processing (`process_psqlrc`, `startup.c:702`) is not
ported at all — `-X` is honoured by never looking for the file, which is right
for the Acceptance line but wrong for a psql without `-X`; now recorded in
`docs/divergences.md` rather than only here.

## 2026-09-16 — NAT-389 review fixes

**What** (all six findings were real; two were holes, four were tests or
harnesses claiming more than they checked)
- `NegotiateProtocolVersion` read the unsupported-parameter count as `u32` and
  handed it to `Vec::with_capacity`. `pqGetNegotiateProtocolVersion3` reads it
  into an `int` and refuses a negative one (`fe-protocol3.c:1475`); read
  unsigned, the frame `76 00 00 00 0C 00 03 00 00 FF FF FF FF` asks for 103 GB
  and aborts the process where C libpq reports a connection error. The count is
  now signed and refused when negative, with upstream's message, and nothing
  reserves capacity from it at all — the strings are pushed one at a time and
  `cstring` stops at the end of the body, so a large *positive* count costs
  only the bytes that are there. The arm moved into
  `decode_negotiate_protocol_version`, which also kept `decode` under clippy's
  line limit. Checked both ways: the reviewer's exact frame, and `7f ff ff ff`
  with one string, which now ends as `insufficient data in "v" message`.
- The `PG_DIAG_INTERNAL_POSITION` arm appended ` at character %s`
  unconditionally. `pqBuildErrorMessage3` suppresses that text whenever the
  verbosity is not terse *and* `PG_DIAG_INTERNAL_QUERY` is present
  (`fe-protocol3.c:1112`), because it draws a cursor over that query instead —
  so every error inside a PL/pgSQL `EXECUTE` read differently here than through
  C libpq. Suppressed now; the query still reaches the reader on the `QUERY:`
  line, which no test had been exercising either.
- The error gate asserted that `error_message()` equals psql's stderr byte for
  byte, which `docs/divergences.md` says in the same breath is impossible: for
  a simple query libpq *does* keep the query (`fe-exec.c:1484`), so C psql
  prints `LINE 1:` and a caret this port does not draw. It was green only
  because no machine here can run it. The gate now compares at
  `VERBOSITY terse` — the one setting where upstream renders the position as
  text (`:1099`), which is exactly what this port renders — so it is a real
  byte comparison over the same fields, and the default-verbosity difference is
  printed as `OUT OF SCOPE (flagged, not silent)` rather than dropped.
- The md5 gate never exercised md5: PostgreSQL 18 defaults
  `password_encryption` to scram-sha-256, so the `--pwfile` verifier was a
  SCRAM verifier and an `md5` pg_hba line still drew `AUTH_REQ_SASL`. The
  cluster is now built with `-c password_encryption=md5` for that case.
- A BackendKeyData arriving mid-query reported the character `R`; it reports
  `K` now.
- `md5_encrypt_is_the_hash_of_password_then_salt` re-derived `md5_encrypt` from
  `md5_hash` and asserted nothing about the two-step auth hash. It, the
  authenticator test and the md5 wire test now all assert fixed strings from
  the system `md5sum` — `md5("secretalice")` and the two salted digests — so
  the bytes on the wire are checked against values this code did not produce.

**Divergence row rewritten**. The cursor-display row named only
`PG_DIAG_STATEMENT_POSITION`; it now states both cases — the statement position
still renders as text because the result carries no query to point at, the
internal position no longer does — and says the gate compares at terse.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 471 tests (469 before), 27 SKIP-flagged
gate lines, unchanged.

**Risks**. The gates still have never executed; the terse comparison is
reasoned from the C, not observed. The negotiation checks upstream makes around
the count — downgrade to a higher version, to pre-3.0, to the non-existent 3.1,
and "negotiated but asks for no changes" (`fe-protocol3.c:1456`-`:1484`) — are
still not made: this port requests 3.0 and ignores the reply's version. That is
a follow-up, not part of these findings.

## 2026-09-16 — NAT-389 rlibpq protocol v3 core

**What**. `rlibpq` can now connect, authenticate and run simple queries.
Ported from the C: `message` is the version 3 wire format (`fe-protocol3.c`
framing, every backend message the simple-query path sees, the six frontend
messages it sends), `auth` is `pg_fe_sendauth`'s switch as a state machine over
`AuthRequest`s, `scram` is `fe-auth-scram.c`'s client plus `scram-common.c`,
`result` is `PGresult` — `ExecStatus`, the `PG_DIAG_*` fields and
`pqBuildErrorMessage3`'s rendering — and `connection` is the only part that
touches a socket. The hash primitives SCRAM and md5 need are ported from
PostgreSQL's own C as well (`base64.c`, `md5.c` + `md5_common.c`, `sha2.c`,
`hmac.c`): ADR-0003 forbids lifting pgrust's `scram_common`/`pg_hmac`/`pg_md5`
crates, which are AGPL.

**Why this shape**. The issue's Design splits data / calculations / actions and
the split is load-bearing here: decoding, the authentication decision and the
whole SCRAM exchange are pure functions of their inputs, with the one value
that cannot be — `pg_strong_random`'s nonce (`fe-auth-scram.c:363`) — passed in
rather than drawn. That is what lets a captured trace be replayed: the tests
drive complete trust / md5 / SCRAM sessions over a scripted stream and then
check the bytes the client wrote, not just the values it computed.

**How it is proved**. RFC 1321 (MD5), FIPS 180-4 (SHA-256), RFC 4231 (HMAC,
including the two long-key cases), RFC 4648 (base64) and RFC 7677 §3 for SCRAM.
The RFC 7677 case is deliberately two tests: `the_rfc_7677_vector` computes the
transcript's own `p=` and `v=` from its inputs through the primitives, and
`the_libpq_exchange` runs the same key material through `ScramClient` in the
form libpq actually sends — `n=` rather than the RFC's `n=user`, because
`fe-auth-scram.c:387` leaves the user name to the startup packet. Keeping them
apart is what caught that difference: the first draft asserted the RFC's proof
against the libpq message and was wrong, and the transcript is what said so.

**Gates**. `crates/rlibpq/tests/t_protocol3.rs` starts a PostgreSQL 18 cluster
and compares `select version()`, a syntax error's rendering and all four local
authentication methods against C `psql`. There is no PostgreSQL 18 on this box
(`docs/nightshift/2026-09-16.md`), so each prints `SKIP (flagged, not silent)`
and passes: 23 gate lines → 27. No upstream TAP file was stolen because there
is none to steal for this — `src/interfaces/libpq/t/` has no simple-query test
and `src/test/authentication/` is outside the sparse checkout — so the cases
are named for what they pin, and that is called out in the file.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 469 tests (392 before), 27 SKIP-flagged
gate lines.

**Risks**. The four gates have never executed: no PostgreSQL 18 exists here, so
their live path is unproven code. Everything they would check is covered
offline by the replay tests, but the first machine with PGDG 18 should expect
to fix the harness rather than the port. COPY, pipelining, cancel requests and
TLS are all out of scope and the runner refuses their messages rather than
guessing (`ProtocolError::UnexpectedResponse`).

**Follow-ups**. Four new divergence rows: protocol 3.0 instead of 3.2, no
environment-driven GUCs in the startup packet, SASLprep only as far as its
ASCII fast path, and no syntax-cursor display in a rendered error. SASLprep in
full needs `unicode_norm.c` and its tables and is worth its own issue. The
acceptance sentence — rpsql switching off its private `proto.rs` — cannot be
exercised yet: rpsql is still the `--version` stub, M3 has not started, and
nothing has a private `proto.rs` to switch off.

## 2026-09-16 — NAT-382 review fixes

**What** (three of the four findings were real; the fourth was real but its
prescribed line numbers were not)
- The stolen `checksums are enabled in control file` passed the literal
  `DataChecksums::Enabled`, so nothing on its path read the default it exists
  to pin — `001_initdb.pl:72` says checksums are enabled *by default*, and
  `$datadir` is the cluster `successful creation` (`:51`-`:59`) makes with no
  checksum switch on its command line. The case now goes through the real
  parser on a command line that names neither switch, asserts neither flag is
  set, and takes its setting from `DataChecksums::resolve([])`. Checked
  non-vacuous by flipping `#[default]` onto `Disabled`: the case fails, and its
  two siblings — which name `--no-data-checksums` explicitly — keep passing,
  which is the split that says the default is what this one is reading.
- Five hunks of re-indented string continuations in tests this issue does not
  touch (`the_data_directory_tree_matches_reference_initdb`,
  `existing_data_directory`, the three `--locale-provider builtin` cases) are
  reverted. Rust strips leading whitespace after a line-continuation `\`, so
  the gated messages never changed and nothing was failing; it was diff noise
  from a `cargo fmt` pass that reflowed the enclosing calls, and it left
  byte-exact assertions visually misaligned. `cargo fmt --all --check` is exit
  0 with the original indentation restored, so rustfmt was never asking for it.
  The only deletions this commit makes against be4a45a are now inside the one
  test it means to change.
- `crc_is_valid` built the whole 8192-byte image through `to_bytes()` and then
  read four bytes back out of it with `u32_at`; it now compares against
  `crc32c(&self.to_bytes()[..offset::CRC])` directly, which is also the
  expression `WriteControlFile` uses (`xlog.c:4290`).

**What the reviewer got half right**. The two `pg_controldata` citations in
`testkit::control` were indeed inconsistent (`:74`/`:323` in one, `:75`/`:324`
in the other). But the correction offered — ":74 and :324" — pairs an argv line
with a `qr//` line: in `001_initdb.pl`, `:74` and `:323` are the
`[ 'pg_controldata', $datadir… ]` arguments and `:75` and `:324` are the two
`qr/Data page checksum version:…/` patterns. Since both comments are about the
patterns, both now cite **`:75` and `:324`**. Verified against the file rather
than taken on the finding's word; the porting rule makes these the grep path
back to upstream, so a confidently wrong pair would be worse than the
inconsistency it replaced.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 392 tests, 23 gate lines
`SKIP (flagged, not silent)`, both unchanged. `cargo build --locked
--all-targets` exit 0.

**Risks**. None new; no behaviour changed. `crc_is_valid` computes the same
checksum over the same bytes, and the reverted hunks are whitespace inside
string literals that the language discards.

## 2026-09-16 — NAT-382 rinitdb pg_control rewrite

**What**. `rinitdb::control` is `ControlFileData` (`src/include/catalog/pg_control.h:101`)
as Rust data plus `parse` / `to_bytes`, and `rewrite` — the one pure function
this issue is about: a template cluster's `pg_control` bytes in, the new
cluster's bytes out, with a fresh `SystemIdentifier`, the asked-for
`data_checksum_version` and a recomputed CRC. `rinitdb::crc32c` is the CRC-32C
underneath it, ported from `src/port/pg_crc32c_sb8.c` and the four macros in
`pg_crc32c.h`. `testkit::control` gains `read_control_file`, the small reader
the stolen `pg_controldata` assertions need on a machine without PostgreSQL 18.

**Why this shape**. `pg_control` is written as raw struct memory —
`WriteControlFile` memcpys `ControlFileData` into a zeroed 8192-byte buffer
(`xlog.c:4333`) — so the file *is* the C ABI's layout, interior padding
included. The port is therefore an offset table (`control::offset`) plus the
ten padding runs (`control::PADDING`) that no field owns, and the two are
required to tile `[0, sizeof(ControlFileData))` exactly. Every offset was taken
from `pg_control.h` under the C rules for a 64-bit build and then checked
against the compiler: a standalone C file repeating upstream's declarations
prints `sizeof(ControlFileData) == 296`, `offsetof(…, crc) == 292` and every
field position the table claims. The first three offsets were checked a second
way, against a real `pg_control` on this box (a PostgreSQL 16 cluster's), which
reads `pg_control_version` 1300 at byte 8 — the native byte order the port
relies on, confirmed on a file this port did not write.

ADR-0002 is why `rewrite` exists at all: a cluster is an unpack of a pre-minted
image, so the two fields `InitControlFile` derives per cluster
(`system_identifier`, `xlog.c:4217`; `data_checksum_version`, `:4231`) have to
be replaced afterwards rather than computed during a bootstrap that does not
happen here.

The CRC-32C table is generated from the reflected Castagnoli polynomial in a
`const fn` rather than transcribed as 2048 literals, and pinned against the
sixteen values upstream prints at `pg_crc32c_sb8.c:112`-`:115` plus the
canonical `"123456789"` check value.

`testkit` gets its own small reader instead of calling `rinitdb::control`,
because `testkit` must not depend on the crate it tests. The duplication is
four offsets, and `t_001_initdb.rs::the_two_control_file_readers_agree` is the
test that stops them drifting — checked non-vacuous by moving testkit's
`data_checksum_version` offset, which fails that test and the stolen
`checksums are enabled in control file` with it.

**Divergences** (both new rows in `docs/divergences.md`). `generate` forces each
system identifier past the last one this process handed out; upstream derives
one per `initdb` process and would repeat inside a microsecond, which ADR-0002's
unpack reaches easily and the issue's acceptance forbids. When the clock has
moved the value is upstream's unchanged; when it has not, the previous value
plus one, spending only the 12 pid bits `xlog.c:5090` calls "a little extra
uniqueness". And `crc32c` is the portable per-byte recurrence, not the SSE 4.2 /
ARMv8 / slicing-by-8 implementation the build would have picked — same function,
and intrinsics are out of reach under `#![deny(unsafe_code)]`.

**Stolen tests**. `checksums are enabled in control file` (001_initdb.pl:72-76),
`successful creation without data checksums` / `checksums are disabled in
control file` (:315-325) and `pg_checksums fails with data checksum disabled`
(:327-332). The `command_ok` halves need a finished cluster and land with it
(NAT-381 … NAT-387); the `command_like` halves are made here twice — against
the C `pg_controldata` when the box has one, and against the control file this
port wrote either way, with the stolen `qr//` run unchanged over the line
`pg_controldata.c:337` prints. `pg_checksums` has no port and is the C tool or a
flagged skip.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 392 tests (was 363), 23 gate lines
`SKIP (flagged, not silent)` (was 19: `pg_controldata` twice, `pg_checksums`
once, and the new real-`pg_control` round trip). `cargo build --locked
--all-targets` exit 0. Non-vacuity checked four ways: moving
`DATA_CHECKSUM_VERSION` by four bytes fails the round trip, the tiling test and
the rewrite test; moving testkit's copy fails the two cross-reader tests;
dropping the forcing in `generate` fails
`generate_never_repeats_within_a_process`.

**Risks**. The acceptance criterion "round-trip is byte-identical on a *real*
pg_control" cannot be exercised here — there is no PostgreSQL 18 on this box —
so `a_real_control_file_round_trips_byte_for_byte` is a gate that skips,
flagged, and takes its cluster either from `PGDROP_REF_PGDATA` or from running
a reference `initdb`. What runs unconditionally is the same round trip over an
image this port synthesized, which cannot catch a layout this port and
PostgreSQL disagree about; the compiler check described above is the substitute
and it is not in the test suite. The offset table is a 64-bit `MAXIMUM_ALIGNOF
8` layout only, which is what `maxAlign` makes upstream reject too.

**Follow-ups**. `InitControlFile` also mints a fresh
`mock_authentication_nonce` with `pg_strong_random` (`xlog.c:4205`); `rewrite`
leaves the template's in place, so every cluster expanded from one image would
share a nonce that is meant to be cluster-unique. That needs a
`pg_strong_random` port (`/dev/urandom` through the standard library) and is
outside this issue's Acceptance, so it is a finding for NAT-381's reviewer
rather than a change made here. Nothing wires `rewrite` into `run` yet — `Plan::Create`
still reports that cluster initialization is unimplemented — because there is no
template to rewrite until NAT-381 lands.

## 2026-09-16 — NAT-416 review fixes

**What** (two findings were real, the third is a history the branch cannot undo)
- `install-links` promised "an error naming the path" and then named a path the
  operator could not copy back. Every `InstallError` path field was a `String`
  built from `Path::display()`, so `install-links $'/tmp/b\xffad'` with a file
  in the way reported `"/tmp/b\u{FFFD}ad/psql"` — the `0xFF` came out as the
  three bytes `EF BF BD`. The paths are `PathBuf`s now, the message is built as
  an `OsString` by `InstallError::message`, and `write_os_line` puts it on the
  stream as bytes, so the name survives end to end. `LinkOp::note` had the same
  defect and got the same treatment: a `created "…"` line is a path the caller
  may well feed back to a shell.

  `Display` cannot be that message — `std::fmt` has no byte-preserving path —
  so `thiserror`'s derive could no longer be the single home for the wording.
  `message()` is that home; `Display` is it with the substitution `fmt` forces,
  and a unit assertion pins the two together. With the derive gone, `thiserror`
  is no longer a pgdrop dependency, and its manifest and lockfile lines are
  removed in this one commit.
- `APPLETS` was defined as `Applet::ALL`, which made
  `assert_eq!(APPLETS, Applet::ALL)` a tautology dressed as a guard. The alias
  is gone; `run` and the tests plan over `Applet::ALL` directly, and the test
  now earns its name by checking that the planned links are named after every
  applet the dispatcher answers to.
- **Not fixable here**: 577a03e added `thiserror` to pgdrop's manifest while
  its `Cargo.lock` line landed in cffab72, so `cargo build --locked` fails at
  577a03e alone. Amending and force-pushing are forbidden (AGENTS.md), so the
  hazard stays at exactly that one commit and is recorded here for a bisect
  that lands on it. `cargo build --locked --all-targets` is exit 0 at this
  commit and at every commit after it; this one removes the dependency, so
  manifest and lockfile move together.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 363 tests (was 361), 19 gates still
`SKIP (flagged, not silent)`. `cargo build --locked --all-targets` exit 0.
Non-vacuity checked by putting `Path::display()` back inside `quoted`: the two
new byte-fidelity tests fail (the unit one over `render`, the integration one
over the binary's real stderr and stdout) and nothing else does.

**Risks**. Off Unix `write_os_line` still goes through `to_string_lossy`,
because no stable byte view of an `OsStr` exists there; Windows paths are
UTF-16 and lose nothing unless they hold an unpaired surrogate.

**Follow-ups**. `rinitdb`'s `InitdbError` holds its paths as `String`s filled
from `Path::display()` too, so a non-UTF-8 `--pgdata` is reported with the same
substitution while C's `%s` writes the bytes. That is a port with stolen tests
and a byte-diff gate, so it is a finding for its own issue, not a change to
make from here; noted for NAT-378's reviewer.

## 2026-09-16 — NAT-416 pgdrop install-links

**What**. `pgdrop install-links DIR` creates one symlink per applet —
`DIR/initdb`, `DIR/psql`, `DIR/postgres` — pointing at
`std::env::current_exe()`, so a directory on `PATH` and a ported TAP suite see
the plain tool names the dispatcher has answered to since NAT-406.
`crates/pgdrop/src/install.rs` is the command; `dispatch` gained the
`install-links` usage-rs subcommand (`dir: PathBuf`, `--force`), a
`Dispatch::InstallLinks` arm, and `Applet::ALL` so the list of names to install
is the dispatcher's own rather than a second copy of it. `thiserror` (approved)
joins pgdrop's dependencies for `InstallError`.

**Why this shape**. The command has no counterpart in the PostgreSQL tree —
upstream installs three executables, not one multicall binary — so there is no
C behaviour to port, no stolen test to steal and no byte-diff gate that could
judge it, and nothing to record in `docs/divergences.md`: nothing here diverges
from an upstream that has no opinion. The tests are therefore this command's
own, and they are the issue's Acceptance list verbatim.

The house split still applies: `LinkOp` is the data, `link_plan` is the whole
decision as a pure function of the executable path, the directory and a
`LinkProbe`, unit-tested against a map of fake entries with no temporary files,
and `apply` is the only function that touches a disk. `link_plan` refuses the
*whole* plan at the first name held by something that is not a symlink, before
`apply` has created anything: a directory that already holds a real `psql` is
one this command has no business writing into, and refusing it whole means the
next run starts from the state the operator inspected rather than from a
half-installed one. `--force` replaces symlinks and only symlinks; it never
removes a file or a directory. `current_exe()` must come back absolute, because
a relative link target resolves against the link's directory, not the caller's.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 361 tests (was 344), 19 gates still
`SKIP (flagged, not silent)` and no new one. Non-vacuity checked by two
perturbations of `link_plan`: making `--force` a no-op fails
`force_replaces_a_link_that_points_elsewhere` and nothing else, and letting a
regular file be replaced like a link fails
`a_regular_file_in_the_way_is_an_error_naming_the_path` and nothing else.

**Risks**. `Replace` is `remove_file` then `symlink`, not a rename: there is a
window in which the name is absent rather than pointing at the old binary.
`install-links` is an installation step, not something a running suite races
with, and the alternative trades that window for a stray temporary name in a
directory on `PATH`.

**Follow-ups**. The integration tests are `#![cfg(unix)]`; the Windows arm of
`symlink` (`symlink_file`, which needs developer mode or a privilege) is
written but untested here. `pgdrop start` (NAT-409) is the natural user of
these links and can reuse `link_plan` for its ephemeral bin directory.

## 2026-09-16 — NAT-384 review fixes

**What** (one finding was a real bug, two were real cleanups)
- `walkdir` has two failure sites and the port had folded them into one.
  `opendir` failing means the directory is skipped whole —
  `could not open directory` (`file_utils.c:304`) and an early return that
  never reaches the trailing `fsync` at `:347`. A `readdir` that fails
  part-way is a different message in a different place: C acts on the names it
  did read, prints `could not read directory` (`:337`) after them, and still
  fsyncs the directory. `RealFs::read_dir` used `?` on the failing item, so
  the second became the first: `$PGDATA/base` on an NFS mount going stale
  mid-walk was reported with the wrong text and left both the names already
  read and `base` itself unsynced. The message existed nowhere in the port.
  `read_dir` now returns a `DirListing` — the names, plus the `readdir` error
  as its own field — and only `opendir` is the `Err`. Sixth `InitdbError` of
  the walk; the new test pins all three parts (entry synced, warning after it,
  directory synced last) and fails when the two messages are swapped back.
- `succeeds_with` called `testkit::command_ok` and then `testkit::run` on the
  same argv, so each of the three stolen cases spawned rinitdb twice and
  fsynced its tree twice. One spawn now, with `testkit::checks::command_ok`
  — the pure check the helper wraps — applied to that outcome, so the stolen
  assertion is still there and the work is done once.
- The `match` on `SyncMethod` in `plan()` reads as prose but is load-bearing:
  it is the exhaustiveness guard that stops a third `DataDirSyncMethod`
  inheriting the fsync walk by accident. Kept, and the comment now says that
  is what it is rather than restating the divergence.

**Not a divergence**. The `readdir` split is upstream's behaviour, now
implemented, so no `docs/divergences.md` row: the two rows that issue added
are unchanged and still accurate.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 344 tests (was 343), 19 gates still
`SKIP (flagged, not silent)`. Non-vacuity checked by pointing the `readdir`
warning back at `CouldNotOpenDirectory`: the new test fails.

**Follow-ups**: `Sync to disk skipped.` is now on NAT-387's Acceptance list
explicitly, with the constant and the `CreatePlan` flag it needs named there,
rather than living only as a unit test here.

## 2026-09-16 — NAT-384 rinitdb sync options

**What**. `--sync-only` stops being a validation-only path: `rinitdb::sync`
is `sync_pgdata` (`src/common/file_utils.c:99`) over `walkdir` (`:290`) and
`fsync_fname` (`:400`), `parse_sync_method`
(`src/fe_utils/option_utils.c:90`) in the `case 19:` arm it belongs to
(`initdb.c:3389`), and the three messages `initdb.c:3447`, `:2127` and
`:3516` as constants. `SyncPlan` gained `sync_method`; `CreatePlan` gained
`do_sync`, `sync_method` and `sync_data_files`, which is all NAT-387 needs to
call the same code at the end of a real cluster creation. Six new
`InitdbError` variants, one per `pg_log_error` site the walk can reach.
Stolen: `sync only` (`001_initdb.pl:78`), `--no-sync-data-files` (`:79`) and
`sync method syncfs` (`:83`, both branches), each byte-diff gated.

**Why this shape**. `plan()` is a calculation over a `SyncProbe`, so all of
`walkdir` — the recursion, the rule that symlinks are followed under
`pg_tblspc` and nowhere else, the `exclude_dir` of `--no-sync-data-files`,
and `pg_wal` being walked a second time when it is a symlink — is unit-tested
against a map of fake directories with no temporary files at all. The
`pg_log_error` lines the walk emits are `SyncOp::Warn` ops rather than side
effects of the walk: C interleaves them with the syncing, and where a line
lands in stderr is exactly what the byte-diff gate compares, so it has to be
part of the calculation's output and not of its plumbing. The four `errno`
values `fsync_fname` tests by name are spelled as POSIX numbers with a
comment saying so; no `ErrorKind` covers `EBADF`, and `%m` is errno-shaped
anyway.

Upstream syncs the cluster its `successful creation` case left behind. That
cluster does not exist yet, so the three stolen cases run over the directory
tree `build_layout` makes (NAT-380's real parse → validate → lay out → apply
path, not a fixture). `sync_pgdata` walks whatever is in front of it and the
gate walks the very same tree through C initdb, so the comparison is exact
either way; it widens to a finished cluster with NAT-387.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 343 tests (was 328). 19 gates print
`SKIP (flagged, not silent)`; three of them are new and all three are
`Scope::Everything`, since C prints nothing on stdout here but the progress
line this port now prints too. Each new case was checked non-vacuous:
breaking `CHECK_OK` fails the unit test, and an extra byte on stdout fails
all three stolen cases.

**Risks**: the two new `docs/divergences.md` rows. `--sync-method=syncfs` is
accepted wherever a Linux C initdb accepts it but performs the fsync walk —
`syncfs(2)` is unreachable from the standard library, the crate is
`#![deny(unsafe_code)]` and `libc` is not approved. The fsync walk is the
stronger of the two for `$PGDATA` (it syncs the directory entries as well),
and on a readable cluster both print nothing, so the gated bytes are
identical; the residual is the error text on an unreadable subtree. The
`pre_sync_fname` hint pass is absent, which is the `PG_FLUSH_DATA_WORKS`-undef
build upstream itself supports. Separately, `--sync-only` on a directory that
is not a cluster prints C's `could not stat file ".../pg_wal"` and
`could not open directory ".../pg_tblspc"` and still exits 0 — verified by
hand against the C source, not against a C binary, because there is none here.

**Follow-ups**: `Sync to disk skipped.` (`initdb.c:3516`) is pinned as bytes
by a unit test but has no command line to reach it until NAT-387 implements
cluster creation; the `do_sync` flag it hangs off is already on `CreatePlan`.
`initdb.c:3389` exits 1 on a bad `--sync-method` *before* `atexit` registers
the cleanup — matching today, but worth re-checking when NAT-385 adds
`cleanup_directories_atexit`.

## 2026-09-16 — NAT-388 rlibpq conninfo and URI parsing

**What**. The connection-string front end of libpq, ported from
`src/interfaces/libpq/fe-connect.c`: `PQconninfoOptions[]` (`:200`) as
`rlibpq::conninfo::CONNINFO_OPTIONS`, all fifty rows in upstream's order;
`conninfo_parse` (`:6290`) and `conninfo_uri_parse_options` (`:6813`) with its
netloc loop, the percent-decoder (`:7187`) and the query-parameter splitter
(`:7054`); `conninfo_add_defaults` (`:6624`) over an explicit `Env`; and the
`libpq_uri_regress` printer (`test/libpq_uri_regress.c:52`) as a pure function
with a thin `src/bin/libpq_uri_regress.rs` around it. `tests/t_001_uri.rs` is
all 63 rows of `t/001_uri.pl`, extracted from the Perl table rather than
retyped, plus a byte-diff gate over the same rows.

**Why this shape**. Values are bytes (`RawText`), not `String`. Percent-decoding
is a byte operation — `?application_name=%C3` is a URI C accepts — so a `String`
port would have to either reject it or map it to U+FFFD, and the error messages
quote the offending token back with `%s`. For the same reason `ConnError`
renders once, as bytes, in `ConnError::message`, and `Display` is derived from
that: one spelling of each format string to drift, and a non-UTF-8 token still
reaches stderr as the bytes C wrote. The parsers walk a slice through
`cstr::at`, which reads past the end as NUL, so every loop bound is upstream's
(`while (*p && *p != ':')`) instead of a restated `index < len`. Order in the
option table is load-bearing, not cosmetic: the regress printer walks the parsed
options and the defaults in lockstep and says so itself ("XXX this coding assumes
that PQconninfoOption structs always have the keywords in the same order").
Defaults take an `Env` value rather than calling `getenv`, which keeps
`PQconndefaults` a calculation and lets each unit test state the environment it
is talking about.

`testkit` grew the environment control this needed: `testkit::Environment` is
`Utils.pm:105`'s `BEGIN` block as data — the thirty `PG*` keys it deletes, in
its order, `LC_MESSAGES=C` and `PGAPPNAME` — plus `run_in` and `Gate::with_env`.
The stolen expectations were written against that scrubbed environment and mean
nothing outside it; three of the rows then override `PGSSLROOTCERT` on top. The
key list is copied including what upstream leaves off it (`PGOPTIONS`,
`PGAPPNAME`, `PGSSLNEGOTIATION`, …), because adding keys would make our stolen
tests pass where upstream's fail.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 328 tests (was 270). 16 gates print
`SKIP (flagged, not silent)`; the new one is `libpq_uri_regress`, which is a
PostgreSQL test program and not an installed one, so `PGDROP_REF_BIN` has to
point at a built `src/interfaces/libpq/test/` for it to go live. The stolen
table was checked non-vacuous by breaking `get_hexdigit`: one row fails and the
failure names the URI.

**Risks**: the three new `docs/divergences.md` rows. The `sslmode` default is
`disable` here and `prefer` in any libpq built with SSL — no `001_uri.pl` row
can see the difference, but the gate against a real helper would, on a case
outside the table, until NAT-392 adds the `tls` feature. `parseServiceInfo` is
absent (NAT-393). The default user is `USER`/`LOGNAME`, so a machine whose user
is literally named `otheruser` or `uri-user` would fail rows that name those —
the same exposure upstream has through `getpwuid`.

**Follow-ups**: NAT-393 (service file, passfile) and NAT-394 (multi-host,
`load_balance_hosts`, `target_session_attrs`) are the two issues that turn
parsed values into behaviour; the comma-separated host and port lists this
parser already builds have unit tests here but no stolen ones yet. `rinitdb`'s
`Environment::from_process` treats an exported but empty `USER` as a user name
where `rlibpq`'s now falls through to `LOGNAME`; worth reconciling when someone
is next in `rinitdb::validate`.

## 2026-09-16 — NAT-379 review fixes

**What** (one finding was a real build break, three were real cleanups)
- `multiple_set_options_with_different_case` carried a bare `#[test]` while the
  `create_plan` helper it calls is `#[cfg(unix)]`, so off Unix the whole test
  target failed to compile with E0425 — invisible here because CI is
  ubuntu-latest and because nothing in the case is itself platform-specific.
  Guarded like its four siblings, with a comment saying the guard belongs to
  the helper and not to the stolen case. Audited the rest of the file rather
  than fixing the one report: every caller of `create_plan` / `build_layout`
  is now inside a `cfg(unix)` item, checked mechanically, not by eye. A
  non-Unix toolchain is not installed on this box, so the fix is reasoned from
  the cfg structure and not compiled; unguarding `create_plan` instead would
  keep the case running on Windows and is the better answer if anyone ever has
  a target to prove it on.
- The "templates compiled in rather than read from `share_path`" divergence
  cited `splitting_and_joining_is_the_identity_on_every_template`, which only
  proves `readfile`/`writefile` round-trips, and the gate, which always skips
  here — so it had no pin at all, which is what AGENTS.md asks for. It now has
  one that bites: the byte length and an FNV-1a digest of each of the three
  templates. Compiling the templates in is what makes their bytes part of this
  crate's observable behaviour, so drift has to be deliberate; a stray reformat
  or a line-ending conversion now fails the suite instead of shipping. Checked
  non-vacuous by appending one byte to `pg_ident.conf.sample` — the test fails
  and names the file. A second test asserts the four `@…@` tokens are still in
  the hba template, since a replacement against a template that lost its token
  is a silent no-op.
- `#[allow(clippy::too_many_lines)]` sat on the whole `mod tests`, exempting
  every test function and standing as the repo's only lint suppression. It
  turned out to be unnecessary outright, not merely too broad: with it deleted,
  pedantic clippy exits 0 with nothing to scope it down to.
- `render_pg_ident_conf` built and joined a 72-element `Vec<String>` to return
  its argument unchanged. `sample.to_owned()`, with the comment explaining that
  upstream's `readfile` → `writefile` really is the identity when no token is
  replaced.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 270 tests (was 268). All 15 gates still
print `SKIP (flagged, not silent)`.

**Risks**: none new. The digest test is the only one that can fail for a
non-bug reason (a genuine PostgreSQL 18.x template change); its doc comment
says what to do then.

**Follow-ups**: unchanged from the entry below.

## 2026-09-16 — NAT-379 rinitdb config generation

**What**. `setup_config()` (`initdb.c:1283`) as pure functions over the three
`.sample` templates, vendored byte for byte into `crates/rinitdb/share/` with
their provenance and the PostgreSQL licence in a README beside them (ADR-0003
allows PostgreSQL files here; pgrust's Rust is what may not come). New
`rinitdb::conf`: upstream's two string surgeons transcribed rather than
reimagined — `replace_token` (`:473`, first occurrence *per line*) and
`replace_guc_value` (`:528`, case-insensitive match, the file's spelling of the
name kept, the trailing comment carried over at its original de-tabified
column) — plus `guc_value_requires_quotes` (`:644`), `escape_quotes`
(`src/port/quotes.c:34`), `pretty_wal_size` (`:1266`), the `shared_buffers`
MB/kB choice, `AuthMethods` (the `-A` arm at `:3246` with its ident↔peer
mirrors, then `check_authmethod_unspecified`), and `render_*` for all four
files in `setup_config`'s order. New `rinitdb::pg_config` holds the constants C
gets from `pg_config.h` / `pg_config_manual.h`, each with its defining line.

**Why this shape**. The probe results (`max_connections`, `shared_buffers`, the
time zone, the DSM implementation) are `test_config_settings`' business, and
that needs a backend; they arrive as `Settings` fields so the rendering is a
calculation now and does not have to wait for the probing. Order is part of the
contract, not a detail: a `-c` override re-aligns the comment on a line an
earlier replacement has already rewritten, so `replace_guc_value` is applied
twice and the second alignment depends on the first's output length. The render
therefore replays upstream's exact sequence.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 268 tests (was 219). All 15 gates print
`SKIP (flagged, not silent)`; the new one is
`the_configuration_files_match_reference_initdb`.

ec00620's own message says 257, which is wrong and cannot be amended: it came
from `grep -cE '^test [a-z_:]+ \.\.\. ok'`, a pattern that silently drops every
test whose name contains a digit (`..._c_utf_8_...`, `t_001_...`). The count
here and below is the sum of the `test result:` lines, which is what the 219
baseline was measured with too. A greppable count is exactly the kind of check
AGENTS.md says to take from an exit status instead; the exit status was 0 both
times, and only the number in the prose was ever wrong.

**How the renderer was verified without a PostgreSQL 18 reference**. The box has
Ubuntu's PostgreSQL *16* initdb, which is not a reference binary and is not
wired into any gate. Off to the side, and then deleted, it was used to check the
*algorithm*: run C initdb 16 against its own `share/` templates, then feed each
of the 17 lines it rewrote back through `replace_guc_value` with the value and
the comment flag read off C's own output. All 17 came back byte-identical,
including the doubly-applied `work_mem` that two `-c` switches cause, and
`render_pg_hba_conf` / `render_pg_ident_conf` / `render_postgresql_auto_conf`
reproduced C 16's three files exactly for trust, md5 and scram-sha-256. That is
evidence about the arithmetic, not about PostgreSQL 18; the real gate is still
skipped and still flagged.

**Risks**. The new gate has never run green against a real PostgreSQL 18, only
been reasoned about, and it takes four values from C's progress output because
the probing stage is not ported — the comment on it says so at length. Two of
them landed a bug worth naming: `-A md5` puts md5 on *both* sides and C refuses
that without a superuser password (`check_need_password`, `:2597`), so that case
carries a `--pwfile`. `locale_date_order` (`:2143`) is `setlocale` + `strftime`
and is not reachable from the standard library, so `DateOrder` is an input with
upstream's `DATEORDER_MDY` default rather than something computed.

**Follow-ups**
- `check_authmethod_valid` (`:2582`) and `check_need_password` (`:2597`) are
  still unported: `initdb -A bogus` is accepted by `validate` today and would
  only fail later. They belong with the rest of the pre-flight (NAT-378's
  ground), and their two error strings are the whole of the work.
- No gate can run as root — C initdb refuses to (`:2504`), which is how this
  session found the md5 case. If CI runs as root, every gate that builds a
  cluster will skip for a *different* reason than a missing binary, and that
  reason is not flagged anywhere. A `SKIP (flagged, not silent)` for it would
  keep the distinction honest.
- `locale_date_order`, `select_default_timezone`, `choose_dsm_implementation`
  and `find_matching_ts_config` are the four actions `Settings` is waiting for.

## 2026-09-16 — NAT-380 review fixes

**What** (all four reviewer findings were real)
- `pg_mkdir_p` could be handed the empty path. `Path::ancestors` ends a
  *relative* path with `""`, and `Path::new("").exists()` is false, so the
  take-while collected it and the first `mkdir` was `mkdir("")` — ENOENT.
  `initdb -D mydata` is a legal command line (`initdb.c:2634` canonicalizes a
  relative `--pgdata`, it does not reject one), so this was reachable the
  moment `run()` was wired up; every test used an absolute temp path, which is
  why the suite was green. The ancestor walk is now the pure
  `layout::missing_ancestors(path, exists)`, unit-tested from a fake `exists`
  over `mydata`, `a/b`, `./data`, `/tmp/x/y` and `""` with no disk and no
  current-directory games. Verified end to end as well: all three relative
  forms now build the full tree, `a/b` creating the intermediate `a` too.
- `write_file` called `sync_all()`. C's `write_version_file` (`initdb.c:1024`)
  only `fprintf`s and `fclose`s; initdb's one durability pass is the end-of-run
  `sync_pgdata` that `--no-sync` suppresses (`:3508`). Dropped, rather than
  recorded as a divergence, because the fsync pass is its own issue and an
  extra fsync here would have to come back out when it lands.
- The acceptance gate only proved `tree_listing ⊆ C's cluster`, so a
  subdirectory `layout` failed to create was invisible to it while the
  Acceptance says "identical". It now also intersects C's tree with the paths
  this stage owns and asserts set equality both ways. Confirmed non-vacuous by
  dropping `pg_stat` from the layout loop: the old assertion passed, the new
  one fails and prints both sets.
- The five new `InitdbError` variants had no test. The existing
  `every_remaining_variant_renders_its_upstream_sentence` is a hand-listed set,
  not an exhaustive one, so it had quietly stopped being every variant. They
  now have their byte-exact renderings, and
  `a_failed_filesystem_op_reports_its_upstream_pg_fatal` drives four of the
  five through real failing syscalls (mkdir under a regular file, chmod on a
  missing directory, symlink onto a taken path, open inside a missing
  directory) so the `%m` text is the kernel's. The fifth is `fprintf` failing
  mid-write, which needs a full filesystem; only its rendering is pinned.

**Why the empty path got through.** The unit tests covered the op *list*,
which is pure and was correct; `apply` was covered only through integration
tests, and those all used `TempDir`, which is absolute. Making the ancestor
walk a calculation rather than a detail of the action is what makes the
relative case testable at all.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 219 tests (was 215). All 14 gates still
print `SKIP (flagged, not silent)`. The permission cases were re-run under
umask 022, 077, 070 and 000, and the tree gate re-run live against the
stand-in reference.

**Risks**: none new. `missing_ancestors` is pure and covered; the gate got
strictly stricter; dropping the fsync cannot change any mode or name.

**Follow-ups**: unchanged from the entry below, plus one small one —
`every_remaining_variant_renders_its_upstream_sentence` claims a completeness
it does not enforce. An exhaustive `match` mapping each variant to the test
that covers it would make a new variant fail to compile until it has one.

## 2026-09-16 — NAT-380 rinitdb datadir layout, permissions, --waldir symlink

**What**
- `rinitdb::layout`: a pure `layout(&CreatePlan) -> Vec<FsOp>` holding
  everything `initialize_data_directory` (`initdb.c:3049`) does before it
  starts a backend — `create_data_directory` (`:2890`),
  `create_xlog_or_symlink` (`:2948`), the 23-entry `subdirs[]` loop (`:3068`)
  and the top-level `write_version_file(NULL)` (`:3086`) — plus one action,
  `apply`, that carries the ops out. `tree_listing` is the same data as
  names-and-modes relative to PGDATA, which is what the acceptance gate
  compares.
- `rinitdb::file_perm`: `PG_DIR_MODE_*` / `PG_FILE_MODE_*` / `PG_MODE_MASK_*`
  and `SetDataDirectoryCreatePerm` (`src/common/file_perm.c:34`) as one
  `DataDirPerm` value on the plan, instead of C's three process globals. `-g`
  (`initdb.c:3359`) is the only thing that moves it, and it moves every mode in
  the tree at once.
- Five more `InitdbError` variants, one per `pg_fatal` site `apply` can reach:
  `mkdir` (`:2903`, `:2974`, `:3022`, `:3079`), `chmod` (`:2917`, `:2989`),
  `symlink` (`:3015`), and the open and the write in `write_version_file`
  (`:1035`, `:1038`).
- Stolen cases: `check_pgdata_permissions` (001_initdb.pl:67),
  `check_pgdata_permissions_with_group_access` (:105 and :108) and
  `waldir_becomes_a_pg_wal_symlink` (:56), each driving the real path — parse,
  validate, lay out, apply — over a temporary directory.

**Why the modes are set, not umasked.** C calls `umask(pg_mode_mask)` once
(`initdb.c:3057`) and passes `pg_dir_create_mode` to every `mkdir`. `umask` is
not reachable from the standard library and no libc dependency is approved, so
each op names the mode the entry must end up with and `apply` creates it at
that mode and then chmods it to exactly that mode. The end state is identical —
the mask only ever clears bits the create mode does not carry, which
`the_mask_never_touches_the_create_modes` pins — and creating at the mode first
means the process umask can only make an entry stricter, never laxer, in the
window before the chmod. Both permission cases were re-run under umask 022,
077, 070 and 000; 070 is the one that would have caught a naive `DirBuilder`
(it masks `0750` down to `0700`), and 000 is the one that would have caught a
missing chmod. The divergence is recorded in `docs/divergences.md`.

**The gate.** `the_data_directory_tree_matches_reference_initdb` runs C initdb
twice, default and `--allow-group-access`, and requires every entry
`tree_listing` claims — PGDATA itself, the 23 subdirectories, `pg_wal` and
`PG_VERSION` — to exist in C's cluster as the same kind with C's mode, plus
`PG_VERSION` byte-identical. It is one entry at a time rather than a whole-tree
diff because C's finished cluster is a strict superset of this stage (the
backend adds `base/4`, `base/5` and every relation file); an entry rinitdb
invents still fails it. There is no PostgreSQL 18 on this box so it prints
`SKIP (flagged, not silent)`, so it was proved non-vacuous against a stand-in
reference that builds the same tree: it passes on a correct tree while ignoring
the superset entries, and fails on a wrong subdirectory mode, a missing
subdirectory, a wrong PGDATA mode and a `PG_VERSION` reading `17`.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 215 tests (was 195). All 14 gates print
`SKIP (flagged, not silent)`.

**Risks**: `layout`/`apply` are not yet wired into `run()`, which still stops
after validation with its "not implemented yet" message. Wiring it in now would
leave a half-built data directory behind on every invocation, and C's cleanup
path (`made_new_pgdata`, `--no-clean`, `initdb.c:3125`) is not ported; `run()`
gets the whole success path in one piece when there is a cluster to finish.

**Follow-ups**
- `canonicalize_path` (`src/port/path.c:337`) is still unported, so the
  `pg_wal` symlink target is the string typed rather than the cleaned one. The
  existing divergence row now names the symlink; NAT-381's config files are
  what will force the port.
- The cleanup path (`made_new_pgdata` / `found_existing_pgdata` and the
  `--no-clean` switch) has no port yet and belongs with the `run()` wiring.

## 2026-09-16 — NAT-378 review fixes

**What** (all three reviewer findings were real)
- `help::try_help` now holds the message of
  `pg_log_error_hint("Try \"%s --help\" for more information.", progname)`,
  and both users build on it: `help::try_help_hint` prefixes it for the bare
  hint `lib.rs` prints on the `getopt_long` `default:` arm, and
  `error::InitdbError::hints` returns it for the three `pg_log_error` sites
  that add it (`initdb.c:3274`, `:3400`, `:3420`). It had been written out as
  two independent `format!` literals, identical today and free to drift
  tomorrow, which for a byte-diff gate is the whole ballgame.
  `the_try_help_hint_is_the_same_text_the_bare_hint_path_prints` asserts the
  rendered error ends with the bare-hint line, so the two cannot separate
  without a test failing.
- Counts corrected: twelve cases landed from `001_initdb.pl`, not eleven (the
  file has sixteen tests, four of them from NAT-373/NAT-374). The entry below
  and `docs/nightshift/2026-09-16.md` both said eleven; 1eaeb94 fixed the two
  other counts in the same sentence and missed this one.
- The gate-scope paragraph below was left with an orphaned word on its own
  line and a 109-column line by 1eaeb94's reflow; rewrapped, along with the
  one other over-wide line in the entry.

**Why the process note.** 1eaeb94 existed only to fix numbers that were
written before the last tests landed, and it introduced the formatting damage
the reviewer caught. The order that avoids both: run the checks first, then
write the counts into the log, then commit once.

**Checks run**: `cargo fmt --all --check` exit 0, pedantic clippy exit 0,
`cargo test --all-features` exit 0 — 195 tests (was 194; +1 for the hint
test). All 13 gates still print `SKIP (flagged, not silent)`.

**Risks**: none. The dedup is a pure refactor with a test pinning the shared
result, and the rest is prose.

**Follow-ups**: unchanged from the entry below.

## 2026-09-16 — NAT-378 rinitdb pre-flight validation

**What**
- `rinitdb::validate`: a pure
  `validate(&Options, &Environment, &dyn FsProbe) -> Result<Plan, InitdbError>`,
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
- `crates/rinitdb/tests/t_001_initdb.rs`: twelve more cases from
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
`cargo test --all-features` exit 0 — 195 tests (was 125). Every gate prints
`SKIP (flagged, not silent)`: there is no PostgreSQL 18 on this box
(`docs/nightshift/2026-09-16.md`).

**Gate scope.** Six of the twelve new gates are judged on stderr and the exit
status only, through a new `testkit::Scope`. By the time C reaches those six
errors it has already printed "The files belonging to this database system
will be owned by …" and its `creating directory … ok` progress, which rinitdb
produces only when cluster creation exists (NAT-379 … NAT-387); the issue's
Acceptance scopes them to "stderr + rc" for exactly that reason. This is not a
quiet narrowing: the stdout difference is still compared, still rendered, and
`assert_clean` prints it behind `OUT OF SCOPE (flagged, not silent)`. The other
six stay strict, because C prints nothing on stdout before those errors
either. Row in `docs/divergences.md`.

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
