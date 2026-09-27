# Vendored sections of the `psql` regression test

Each pair here is one section of PostgreSQL 18.6's
`src/test/regress/sql/psql.sql` and the matching slice of
`src/test/regress/expected/psql.out`, cut **byte for byte** at the section's
comment header. pg_regress runs the whole file as
`psql -X -a -q -d <db> < psql.sql > psql.out 2>&1`
(`src/test/regress/pg_regress_main.c:74`-`:75`), so the `.out` slice is
stdout and stderr interleaved, with every input line echoed. Do not
reformat them: the indentation is tabs, and a changed byte is a failed
comparison.

| files                        | `psql.sql` lines | `psql.out` lines | section header               |
| ---------------------------- | ---------------- | ---------------- | ---------------------------- |
| `psql_if.sql`, `psql_if.out` | 908-1140         | 4529-4801        | `-- tests for \if ... \endif` |

## How they are used

- `crates/rpsql/src/mainloop.rs`, `the_server_free_blocks_of_the_if_section_match_psql_out`:
  the blocks of the section that send no query run through `main_loop`
  in-process, and their output is compared with the `.out` slice. No server,
  no reference binary; this runs on every `cargo test`.
- `crates/rpsql/tests/t_regress_psql.rs`, `if_section_matches_c_psql`: the
  whole section, piped through C psql and through rpsql against one
  PostgreSQL 18 cluster, stdout, stderr and exit status compared byte for
  byte. The lines that need a command another issue owns are cut out by
  name in that test, each with its owner.

## Provenance

Taken from the `postgres/postgres` repository at tag `REL_18_6`, commit
`724edf9bde9d356724ad384a2e196edc3c9f80f7` (the pristine tag, not the tree
vendored in pgrust — see ADR-0008): git blobs
`f4a4486be795427c5e795e6c5904dcacd35805f0` (`psql.sql`) and
`506f1ed0a1f0988ae11a76e6ee1f14ba867e7439` (`psql.out`). The slices'
SHA-256:

| file          | sha256                                                             |
| ------------- | ------------------------------------------------------------------ |
| `psql_if.sql` | `1d099f8a2847f2bcd4bd652214961c3d2fbf6212fd7dfa19cf91338702befb98` |
| `psql_if.out` | `0710ae643f596e4c8235139923128025a9a309c277c784a83caabf874ced5172` |

The test `each_section_is_the_slice_postgresql_18_6_ships` in
`crates/rpsql/src/mainloop.rs` asserts both files against these digests.

Recut them with `sed -n 908,1140p src/test/regress/sql/psql.sql` and
`sed -n 4529,4801p src/test/regress/expected/psql.out` from the tag.

## Licence

They are part of the PostgreSQL distribution and carry its licence
(`COPYRIGHT` at `REL_18_6`):

> Portions Copyright (c) 1996-2026, PostgreSQL Global Development Group
>
> Portions Copyright (c) 1994, The Regents of the University of California
>
> Permission to use, copy, modify, and distribute this software and its
> documentation for any purpose, without fee, and without a written agreement
> is hereby granted, provided that the above copyright notice and this
> paragraph and the following two paragraphs appear in all copies.
>
> IN NO EVENT SHALL THE UNIVERSITY OF CALIFORNIA BE LIABLE TO ANY PARTY FOR
> DIRECT, INDIRECT, SPECIAL, INCIDENTAL, OR CONSEQUENTIAL DAMAGES, INCLUDING
> LOST PROFITS, ARISING OUT OF THE USE OF THIS SOFTWARE AND ITS
> DOCUMENTATION, EVEN IF THE UNIVERSITY OF CALIFORNIA HAS BEEN ADVISED OF THE
> POSSIBILITY OF SUCH DAMAGE.
>
> THE UNIVERSITY OF CALIFORNIA SPECIFICALLY DISCLAIMS ANY WARRANTIES,
> INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY
> AND FITNESS FOR A PARTICULAR PURPOSE.  THE SOFTWARE PROVIDED HEREUNDER IS
> ON AN "AS IS" BASIS, AND THE UNIVERSITY OF CALIFORNIA HAS NO OBLIGATIONS TO
> PROVIDE MAINTENANCE, SUPPORT, UPDATES, ENHANCEMENTS, OR MODIFICATIONS.

See `docs/adr/0003-licensing.md`: PostgreSQL files may be vendored here with
their licence intact; pgrust's own sources — including its modifications to
its vendored PostgreSQL tree — may not, because pgrust is AGPL-3.0 and this
crate is MIT. `NOTICE.md` at the repository root states each crate's licence.
