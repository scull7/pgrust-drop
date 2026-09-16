# Test stealing: the conformance method

pgrust proves itself by running PostgreSQL's own regression suite against a
pgrust server and diffing byte-for-byte against the vendored expected output.
pgrust's Rust `psql` does the same with `crates/bin/psql/gate/run-gate.sh`: the
same SQL corpus through PGDG psql 18 and the Rust psql, against both a stock
PostgreSQL 18 server and a pgrust server, stdout/stderr/exit code diffed after
three justified normalizations.

pgrust-drop copies that *method* for every crate (the method, not pgrust's corpus files: ADR-0003).

## Sources (PostgreSQL 18.6, vendored in pgrust)

| ours      | upstream tests                                                                                         |
| --------- | ------------------------------------------------------------------------------------------------------ |
| `rinitdb` | `src/bin/initdb/t/001_initdb.pl` (334 lines); datadir tree diff vs C initdb                            |
| `rlibpq`  | `src/interfaces/libpq/t/001_uri.pl` … `006_service.pl`, `test/libpq_uri_regress.c`, `libpq_testclient.c`, `src/test/modules/libpq_pipeline` (9 traces) |
| `rpsql`   | `src/bin/psql/t/001_basic.pl`, `020_cancel.pl`, regress `psql.sql` (+`psql.out` 6982 lines), `psql_crosstab.sql`, `psql_pipeline.sql`; gate corpus written here or cut from regress (never copied from pgrust, ADR-0003) |
| helpers   | `src/test/perl/PostgreSQL/Test/Utils.pm` (`program_help_ok`, `command_ok`, `check_mode_recursive`, …) |

## Rules

1. Port a Perl test file to one Rust integration test file with the same name
   (`t_001_initdb.rs`), the same assertions, in the same order, with the same
   test names as strings.
2. Where the upstream test drives a binary, drive **both** the reference C
   binary and ours, and assert the outputs are identical, not merely that ours
   "looks right".
3. Every normalizer (`Time: XXX ms`, `PID NNN`, system identifier, …) is a pure
   function with a one-line justification next to it.
4. Missing reference binary → `SKIP (flagged, not silent)` locally, a hard
   failure in CI. `.github/workflows/ci.yml` installs the PGDG `postgresql-18`
   and `postgresql-client-18` packages and runs the suite with
   `PGDROP_REF_BIN=/usr/lib/postgresql/18/bin` and `PGDROP_REQUIRE_REF=1`, so
   the gates for `initdb`, `pg_ctl`, `pg_controldata`, `pg_checksums` and
   `psql` are real there and cannot quietly stop running. The one exception is
   `libpq_uri_regress`: it is a PostgreSQL *test* program built from
   `src/interfaces/libpq/test/`, no PGDG package ships it, so
   `testkit::reference::UNSHIPPED_TOOLS` exempts it by name and its gate still
   flagged-skips in CI (NAT-374). Announce the skip with
   `testkit::reference::skip` / `announce_skip`, never `println!` or
   `eprintln!`: libtest captures both and replays them only for a failing test
   or under `--nocapture`, so a skip announced that way is invisible in the log
   of a passing CI run — a silently narrowed gate.
5. A divergence we accept on purpose goes in `docs/divergences.md` with the
   test that pins the divergent behaviour.

## Reference binary discovery

`PGDROP_REF_BIN` if set, else the four defaults in
`crates/testkit/src/reference.rs` (`DEFAULT_REF_DIRS`), in order:

1. `/usr/lib/postgresql/18/bin` — PGDG Debian/Ubuntu
2. `/opt/homebrew/opt/postgresql@18/bin` — Homebrew, Apple silicon
3. `/usr/local/opt/postgresql@18/bin` — Homebrew, Intel
4. `/opt/homebrew/bin`

(matching pgrust's own sim-sweep search order).

## `PGDROP_REQUIRE_REF`: making the gates bite

A machine without PostgreSQL 18 cannot run a gate at all, so the default is a
flagged skip that passes — right for a laptop, wrong for CI, where a skipped
gate proves nothing while looking green.

Set `PGDROP_REQUIRE_REF` to `1` or `true` and a missing reference binary
becomes a test failure naming the tool and every directory searched. Unset,
empty, `0` or anything else keeps the permissive default; the decision is the
pure `reference::policy_from_env` / `reference::missing_ref_action` pair, so
both halves are unit-tested with no environment and no filesystem.

Tools listed in `reference::UNSHIPPED_TOOLS` are exempt even under the strict
policy, because requiring a binary no package ships would only make CI red for
a reason nobody can fix. Today that list is exactly `libpq_uri_regress`.

To reproduce CI locally:

```sh
PGDROP_REF_BIN=/usr/lib/postgresql/18/bin PGDROP_REQUIRE_REF=1 cargo test --all-features
```

## Gates still skipped, and what it costs

`a_real_control_file_round_trips_byte_for_byte` (in
`crates/rinitdb/tests/t_001_initdb.rs`) is the **only** test that would prove
the hand-computed struct offset and padding tables in
`crates/rinitdb/src/control.rs` match the `ControlFileData` layout a real C
compiler produces — `control.rs`'s own round-trip test uses an image this
port synthesized, so it cannot catch a layout this port and PostgreSQL disagree
about. As long as it skips, that layout is unverified and every `pg_control`
byte we write is an assumption.

It needs a real data directory: `PGDROP_REF_PGDATA`, or the reference `initdb`.
With neither it takes a direct `reference::announce_skip` path rather than
`reference::skip`, so `PGDROP_REQUIRE_REF` does **not** catch it — what makes
it run in CI is the PGDG install step, not the strict policy. Routing it
through `reference::skip` so the strict policy covers it too is a follow-up.

**This skip must be removed once NAT-374 lands reference binaries.** It is not
a permanent exemption and must never be added to `UNSHIPPED_TOOLS`: PGDG ships
`pg_controldata` and `initdb`. This note stays here until the round-trip has
actually run green at least once.
