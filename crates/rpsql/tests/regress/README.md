# Vendored PostgreSQL regression tests: `psql`, `psql_crosstab`

Four files, copied **byte for byte** from PostgreSQL 18.6. They are the tests
`t_regress_psql.rs` and `t_regress_psql_crosstab.rs` steal: each script, and
the output `pg_regress` expects from it. Do not reformat them — `psql.out` has trailing spaces that are part
of what psql prints.

| file               | upstream path                            | sha256                                                             |
| ------------------ | ---------------------------------------- | ------------------------------------------------------------------ |
| `psql.sql`         | `src/test/regress/sql/psql.sql`          | `8454d0e81f90bae39f6ffd213cd2c921e2bcfa8c2984cd3112e90840585dc457` |
| `expected/psql.out`| `src/test/regress/expected/psql.out`     | `588bf1582a4deff3708e37f9b51c7879f83ca8be103656f0df6990d8257e8dc7` |
| `psql_crosstab.sql` | `src/test/regress/sql/psql_crosstab.sql` | `7159d1605cad80cf2f810174cc47b9d71b4d2386b0a533a0daa9e48eeaf3052d` |
| `expected/psql_crosstab.out` | `src/test/regress/expected/psql_crosstab.out` | `44039026efc4430898aaae7b35f4da5a72ac83bab67130dac9158b7b2dec4502` |

## Provenance

Taken from the `postgres/postgres` repository at tag `REL_18_6`, commit
`724edf9bde9d356724ad384a2e196edc3c9f80f7` (ADR-0008), where their git blob
ids are `f4a4486be795427c5e795e6c5904dcacd35805f0` and
`506f1ed0a1f0988ae11a76e6ee1f14ba867e7439` (`psql`), and
`5a4511389de69a5a4738c4999db2930722b10765` and
`e09e3310165853e1b3e49191e5828ec3da22daad` (`psql_crosstab`). Not from
pgrust's vendored tree.

The digests above are what PostgreSQL 18.6 ships, and each test file's
`the_vendored_files_are_the_ones_postgresql_18_6_ships` asserts its two files
against them on every `cargo test`. Recompute them from upstream with

    git -C postgres show REL_18_6:src/test/regress/sql/psql.sql | sha256sum
    git -C postgres show REL_18_6:src/test/regress/expected/psql.out | sha256sum
    git -C postgres show REL_18_6:src/test/regress/sql/psql_crosstab.sql | sha256sum
    git -C postgres show REL_18_6:src/test/regress/expected/psql_crosstab.out | sha256sum

## Sections

`psql_crosstab.sql` is short and every block needs the table its first one
creates, so it is gated whole. `mod.rs` cuts `psql.sql` and `psql.out` into
sections at the comment headers `psql.sql` opens its topics with (a `--` line
after an empty line), so each is gated against its own slice of `psql.out`.
See `regress::split`.
