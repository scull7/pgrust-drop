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
   failure in CI. Each lane of `.github/workflows/ci.yml` installs one complete
   PostgreSQL 18 from its platform's packages (PGDG apt on gnu, Alpine
   `postgresql18` on musl, Homebrew `postgresql@18` on apple), points its lane
   variable (`PGDROP_REF_BIN_{GNU,MUSL,APPLE}`) at it and runs the suite with
   `PGDROP_REQUIRE_REF=1`, so the gates for `initdb`, `pg_ctl`,
   `pg_controldata`, `pg_checksums` and `psql` are real there and cannot
   quietly stop running. The one exception is `libpq_uri_regress`: it is a
   PostgreSQL *test* program built from `src/interfaces/libpq/test/`, no
   package ships it, so `testkit::reference::UNSHIPPED_TOOLS` exempts it by
   name and its gate still flagged-skips in CI (NAT-374). Announce the skip
   with `testkit::reference::skip` / `announce_skip`, never `println!` or
   `eprintln!`: libtest captures both and replays them only for a failing test
   or under `--nocapture`, so a skip announced that way is invisible in the log
   of a passing CI run — a silently narrowed gate.
5. A divergence we accept on purpose goes in `docs/divergences.md` with the
   test that pins the divergent behaviour.

## Reference binary discovery

Keyed on the libc the test binary was compiled against (`testkit::reference::Libc`,
ADR-0007): the lane's own variable (`PGDROP_REF_BIN_GNU`, `PGDROP_REF_BIN_MUSL`
or `PGDROP_REF_BIN_APPLE`) if set, then the lane-agnostic `PGDROP_REF_BIN` if
set, then the lane's install layouts in `Libc::default_dirs`:

- gnu: `/usr/lib/postgresql/18/bin` (PGDG Debian/Ubuntu), `/usr/pgsql-18/bin`
  (PGDG RHEL)
- musl: `/usr/libexec/postgresql18`, `/usr/lib/postgresql18/bin` (Alpine)
- apple: `/opt/homebrew/opt/postgresql@18/bin`, `/usr/local/opt/postgresql@18/bin`
  (Homebrew), `/Applications/Postgres.app/Contents/Versions/18/bin`

A directory found through one lane's variable is never consulted by another
lane, so a stray export cannot pair a musl build with a glibc reference.

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

## The data directory tree diff (NAT-386)

`the_finished_data_directory_matches_reference_initdb`
(`crates/rinitdb/tests/t_001_initdb.rs`) runs C `initdb` and `rinitdb` with the
same command line — the template's own recipe, `rinitdb::image::MINT_ARGS`,
plain and with `--allow-group-access` — and diffs the two data directories with
`testkit::tree`: every entry's presence, kind, permission bits, size and a
digest of its contents. Timestamps are not part of the manifest, so no
allowance is needed for them. Everything not in the allow-list below must be
identical, including the root's mode, every directory, `PG_VERSION` and
`pg_hba.conf`, `pg_ident.conf` and `postgresql.auto.conf`.

The allow-list is `TREE_ALLOWANCES` in that file. Each entry names the aspects
that may differ at its entries, and a narrower check runs in its place:

| entry | may differ | why | checked instead by |
| ----- | ---------- | --- | ------------------ |
| `global/pg_control` | contents | Every cluster gets its own system identifier and mock authentication nonce (`InitControlFile`, `xlog.c:4217`-`:4218`) and timestamps (`controldata_utils.c:197`); rinitdb's checkpoint is one segment past the template's redo pointer, where `pg_resetwal -f` puts it (ADR-0002, 2026-09-23 amendment). The CRC follows. | `pg_control_matches_the_reference`: every other field must be C's, byte for byte once the CRC is recomputed. With a twin reference (below), the checkpoint must be exactly one segment past C's redo pointer. Without one, the checkpoint's next XID, next OID and oldest XID are the template's and are taken from ours too. |
| every regular file under `pg_wal/` | presence, contents | C's segments hold the WAL its bootstrap and single-user sessions wrote. rinitdb writes one segment holding one shutdown checkpoint (ADR-0002, 2026-09-23 amendment). | `the_wal_is_the_segment_pg_control_names`: our `pg_wal` holds exactly one file, the segment our checkpoint is in, at the segment size. Its bytes are held to `pg_resetwal`'s by `crates/rinitdb/tests/first_segment.rs`. |
| `pg_stat/pgstat.stat` | presence | The statistics C initdb's own server wrote at shutdown (`pgstat_write_statsfile`, `pgstat.c:1570`). The template strips them (`image::STRIPPED_FILES`), and a server that finds no file starts from empty counters (`pgstat.c:1776`-`:1784`). This is a divergence and has a row in `docs/divergences.md`. | The gate asserts the file is absent from ours, so the allowance runs in one direction only. |
| `postgresql.conf` | contents | `setup_config` writes values the reference build and host decide: its `DEFAULT_PGSOCKET_DIR`, a distribution's patched sample, probed `max_connections`/`shared_buffers`, the host's time zone, and on macOS the host's locale. | `the_configuration_files_match_reference_initdb` and `the_time_zone_lines_match_reference_initdb`, which diff it byte for byte after accounting for each of those values. |

`postmaster.opts` is not on the list. Only a postmaster writes it
(`CreateOptsFile`, `postmaster.c:1288`), and initdb never starts one, so neither
tree has the file. If either side ever grew one, the gate would report it.

**Catalog files, and the twin rule.** The files the template image carries
(`image::keeps`: everything under `base/` and `global/` except `pg_control`,
plus `pg_xact`, `pg_multixact` and the rest) are compared byte for byte only
when the reference is the *template's twin*. That means C's tree, stripped,
holds exactly the template's files (`template_mismatches`). With any other
reference, the host imported other rows into `pg_collation`
(`pg_import_system_collations`, `initdb.c:1781`: ICU's version, `locale -a`),
so every OID assigned after them moves and no catalog file can match. That is
ADR-0002's design, not a defect. In that case those files are left out of both
trees and the gate prints `SKIP (flagged, not silent)`, naming how many
differ and the first one. `PGDROP_REQUIRE_REF` does not make this a failure,
because a reference that is not the mint host is not a missing tool.

What this costs today: the template was minted on Alpine 3.24 and every CI
lane has a different host (musl runs in `alpine:3.23`; gnu and apple differ in
libc and ICU). So no CI lane runs the catalog half live, and it prints the
flagged skip on all three. The twin branch of the `pg_control` check is
exercised without a reference by
`the_pg_control_check_takes_from_ours_only_what_it_names`, and the allow-list
by `the_tree_allowances_explain_only_what_they_name`. Two changes would make
the catalog half run live, and both are owner decisions: moving the musl
container to `alpine:3.24` (the mint host), or re-minting on `alpine:3.23`.

## The ICU branch (NAT-386)

`001_initdb.pl:114` runs one block of cases when `$ENV{with_icu} eq 'yes'` and
one case otherwise. Upstream takes `with_icu` from the build under test
(`src/bin/initdb/Makefile:64`, `src/bin/initdb/meson.build:34`). A reference
binary has no configuration to read, so `crates/rinitdb/tests/t_001_initdb.rs`
asks the binary instead. It runs the command line of `fails for encoding not
supported by ICU` (`:161`), which stops before anything is created in either
build. A build with ICU fails at the encoding check (`encoding mismatch`,
`initdb.c:2786`). A build without ICU fails earlier, in `icu_language_tag`
(`ICU is not supported in this build`, `:2362`). Any other outcome fails the
test; the probe never guesses. It runs once per test process.

rinitdb is a build without ICU (`rinitdb_is_a_build_without_icu`, and a row in
`docs/divergences.md`), so for ours the `else` case applies. That case is
`locale_provider_icu_fails_since_no_icu_support`. Each of the seven ICU-block
cases is still ported, under its upstream name and in order, and does three
things:

1. Ours must fail with exactly what a C build without ICU writes for the same
   command line: `locale must be specified if provider is icu` for `:116`,
   `ICU is not supported in this build` for the other six.
2. When the reference is built with ICU, the upstream assertion runs against
   it: `command_ok`, `command_like` or `command_fails_like`, with upstream's
   pattern.
3. Ours and the reference are byte-diffed (stderr and exit status) when they
   must agree: when the line fails before ICU is reached (`:116`), or when the
   reference is built without ICU as well. Otherwise the two builds are on
   different sides of `:114`, and the narrowing is printed
   `SKIP (flagged, not silent)`.

All three CI lanes' references are built with ICU, so step 2 runs live on every
lane and step 3 runs only for `:116`. The case of a reference built without ICU
is exercised without one by
`the_icu_probe_and_the_byte_diff_rule_read_both_builds`.
