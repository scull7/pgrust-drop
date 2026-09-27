# Vendored PostgreSQL regression test: `psql`

Two files, copied **byte for byte** from PostgreSQL 18.6. They are the test
`t_regress_psql.rs` steals: the script, and the output `pg_regress` expects
from it. Do not reformat them — `psql.out` has trailing spaces that are part
of what psql prints.

| file               | upstream path                            | sha256                                                             |
| ------------------ | ---------------------------------------- | ------------------------------------------------------------------ |
| `psql.sql`         | `src/test/regress/sql/psql.sql`          | `8454d0e81f90bae39f6ffd213cd2c921e2bcfa8c2984cd3112e90840585dc457` |
| `expected/psql.out`| `src/test/regress/expected/psql.out`     | `588bf1582a4deff3708e37f9b51c7879f83ca8be103656f0df6990d8257e8dc7` |

## Provenance

Taken from the `postgres/postgres` repository at tag `REL_18_6`, commit
`724edf9bde9d356724ad384a2e196edc3c9f80f7` (ADR-0008), where their git blob
ids are `f4a4486be795427c5e795e6c5904dcacd35805f0` and
`506f1ed0a1f0988ae11a76e6ee1f14ba867e7439`. Not from pgrust's vendored tree.

The digests above are what PostgreSQL 18.6 ships, and
`the_vendored_files_are_the_ones_postgresql_18_6_ships` asserts both files
against them on every `cargo test`. Recompute them from upstream with

    git -C postgres show REL_18_6:src/test/regress/sql/psql.sql | sha256sum
    git -C postgres show REL_18_6:src/test/regress/expected/psql.out | sha256sum

## Sections

`mod.rs` cuts both files into sections at the comment headers `psql.sql`
opens its topics with (a `--` line after an empty line), so each is gated
against its own slice of `psql.out`. See `regress::split`.
