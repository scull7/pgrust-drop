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
4. Missing reference binary → `SKIP (flagged, not silent)`. CI installs the
   PGDG 18 packages so the gate is real there.
5. A divergence we accept on purpose goes in `docs/divergences.md` with the
   test that pins the divergent behaviour.

## Reference binary discovery

`PGDROP_REF_BIN` if set, else `/usr/lib/postgresql/18/bin`, else
`/opt/homebrew/opt/postgresql@18/bin`, else `/opt/homebrew/bin` (matching
pgrust's own sim-sweep search order).
