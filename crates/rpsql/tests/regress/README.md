# Vendored PostgreSQL regression tests: `psql`, `psql_crosstab`, `psql_pipeline`, `largeobject`

Nine files, copied **byte for byte** from PostgreSQL 18.6. They are the tests
`t_regress_psql.rs`, `t_regress_psql_crosstab.rs`,
`t_regress_psql_pipeline.rs` and `t_regress_largeobject.rs` steal: each script,
the output `pg_regress` expects from it, and the `data/tenk.data` that
`largeobject.sql` imports. Do not reformat them — `psql.out` has trailing spaces that are part
of what psql prints.

| file               | upstream path                            | sha256                                                             |
| ------------------ | ---------------------------------------- | ------------------------------------------------------------------ |
| `psql.sql`         | `src/test/regress/sql/psql.sql`          | `8454d0e81f90bae39f6ffd213cd2c921e2bcfa8c2984cd3112e90840585dc457` |
| `expected/psql.out`| `src/test/regress/expected/psql.out`     | `588bf1582a4deff3708e37f9b51c7879f83ca8be103656f0df6990d8257e8dc7` |
| `psql_crosstab.sql` | `src/test/regress/sql/psql_crosstab.sql` | `7159d1605cad80cf2f810174cc47b9d71b4d2386b0a533a0daa9e48eeaf3052d` |
| `expected/psql_crosstab.out` | `src/test/regress/expected/psql_crosstab.out` | `44039026efc4430898aaae7b35f4da5a72ac83bab67130dac9158b7b2dec4502` |
| `psql_pipeline.sql` | `src/test/regress/sql/psql_pipeline.sql` | `71e10a1e728d50199b5e5e0db27a08c0dc17ebd75f6345fd6f2c71202d9847de` |
| `expected/psql_pipeline.out` | `src/test/regress/expected/psql_pipeline.out` | `6b73b8e27cb811d7ca8c8f5d603b456f3db9d07c727b9a82ab20dd74556f24d5` |
| `largeobject.sql` | `src/test/regress/sql/largeobject.sql` | `31587d0c4006c12df38f6c2717ae1f021ae794de51bc9471467242ade3f0be14` |
| `expected/largeobject.out` | `src/test/regress/expected/largeobject.out` | `f124af7792de62956ddd82b919095c292c348e1a747b06a34a03bd04d519d5e4` |
| `data/tenk.data` | `src/test/regress/data/tenk.data` | `d62f34bdc0a25a5ba36f2dbe62a35479d9e51a7326d482e418091ea2ed40e484` |

## Provenance

Taken from the `postgres/postgres` repository at tag `REL_18_6`, commit
`724edf9bde9d356724ad384a2e196edc3c9f80f7` (ADR-0008), where their git blob
ids are `f4a4486be795427c5e795e6c5904dcacd35805f0` and
`506f1ed0a1f0988ae11a76e6ee1f14ba867e7439` (`psql`), and
`5a4511389de69a5a4738c4999db2930722b10765` and
`e09e3310165853e1b3e49191e5828ec3da22daad` (`psql_crosstab`), and
`468ef1d090b6d3cb9852ff524518d170e97f9cfc` and
`a931d63cafe76aff72c8fa44d5d1d9af4ebc967c` (`psql_pipeline`), and
`a4aee02e3a4ea7c369d7d684354eb8f9627f568f`,
`4921dd79aeec1716ceaba9334612dfefd8575146` and
`c9064c9c0325fe639d3ff2079436b3489eac9f97` (`largeobject.sql`,
`largeobject.out`, `tenk.data`). Not from pgrust's vendored tree.

The digests above are what PostgreSQL 18.6 ships, and each test file's
`the_vendored_files_are_the_ones_postgresql_18_6_ships` asserts its two files
against them on every `cargo test`. Recompute them from upstream with

    git -C postgres show REL_18_6:src/test/regress/sql/psql.sql | sha256sum
    git -C postgres show REL_18_6:src/test/regress/expected/psql.out | sha256sum
    git -C postgres show REL_18_6:src/test/regress/sql/psql_crosstab.sql | sha256sum
    git -C postgres show REL_18_6:src/test/regress/expected/psql_crosstab.out | sha256sum
    git -C postgres show REL_18_6:src/test/regress/sql/psql_pipeline.sql | sha256sum
    git -C postgres show REL_18_6:src/test/regress/expected/psql_pipeline.out | sha256sum
    git -C postgres show REL_18_6:src/test/regress/sql/largeobject.sql | sha256sum
    git -C postgres show REL_18_6:src/test/regress/expected/largeobject.out | sha256sum
    git -C postgres show REL_18_6:src/test/regress/data/tenk.data | sha256sum

## Sections

`psql_crosstab.sql` is short and every block needs the table its first one
creates, so it is gated whole. `mod.rs` cuts `psql.sql` and `psql.out` into
sections at the comment headers `psql.sql` opens its topics with (a `--` line
after an empty line), so each is gated against its own slice of `psql.out`.
See `regress::split`.

`psql_pipeline.sql` is cut the same way, and its sections are run in order
against one cluster — they share only the database — so each is gated
against its own slice of `psql_pipeline.out`. The sections that need a
command another issue owns are named, with the owner, in
`t_regress_psql_pipeline.rs`'s `NOT_YET`.

`largeobject.sql` is cut the same way, and `t_regress_largeobject.rs` runs
every section in order except those that need a command not on `main` yet
(`\getenv`, `\gset`, `\dl`, notices); the test file lists each with its
issue. `expected/largeobject_1.out`, upstream's variant for a `tenk.data`
checked out with CRLF line ends, is not vendored: this one is LF.
