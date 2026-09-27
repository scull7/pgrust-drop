# Vendored PostgreSQL share files (NAT-408)

Everything under this directory except this README is compiled into `pgdrop`
(`crates/pgdrop/build.rs`) and written, once per build, to
`$XDG_CACHE_HOME/pgdrop/<version>-<digest>/share`, which the server is then
pointed at through `PGRUST_PGSHAREDIR` and, for `timezone/`, `PGRUST_TZDIR`
(`crates/pgdrop/src/share.rs`). They are the files `make install` of
PostgreSQL 18.6 puts under `share/`, **byte for byte**. Do not reformat,
re-wrap or "fix" them.

| directory       | upstream source                          | upstream install list                    |
| --------------- | ---------------------------------------- | ---------------------------------------- |
| `timezonesets/` | `src/timezone/tznames/`                  | `src/timezone/tznames/Makefile:15`-`:19` |
| `tsearch_data/` | `src/backend/tsearch/dicts/`             | `src/backend/tsearch/Makefile:17`-`:21`  |
| `tsearch_data/` | `src/backend/snowball/stopwords/*.stop`  | `src/backend/snowball/Makefile:75`-`:90` |
| `timezone/`     | `zic` over `src/timezone/data/tzdata.zi` | `src/timezone/Makefile:56`-`:58`         |

`timezone/` is not a copy but a build product: the compiled timezone database
(IANA tzdata release 2026c, the one `REL_18_6` carries), exactly as a
PostgreSQL 18.6 built without `--with-system-tzdata` installs it. It is
generated, never edited, by

    scripts/vendor-timezone.sh postgresql-18.6.tar.bz2

which checks the tarball's published SHA-256, builds `zic` from
`src/timezone/zic.c`, runs `zic -d share/timezone src/timezone/data/tzdata.zi`
with no further options (`src/timezone/Makefile:33`), refuses unless two runs
agree, and writes the SHA-256 of every file it produced to
`crates/pgdrop/timezone.sha256`. `zic` hard-links the names a `Link` line
makes; here they are plain files.

## Provenance

Copied from the `postgres/postgres` tag `REL_18_6`, commit
`724edf9bde9d356724ad384a2e196edc3c9f80f7`. The release tarball
`postgresql-18.6.tar.bz2` (published SHA-256
`555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f`) is the
equivalent source (ADR-0008).

The SHA-256 of every file as upstream ships it is recorded in
`crates/pgdrop/src/share.rs` (`UPSTREAM`), and the test
`each_embedded_file_is_the_file_postgresql_18_6_installs` asserts the embedded
set against it, file for file, on every `cargo test`;
`each_embedded_timezone_file_is_zic_output_of_postgresql_18_6` does the same
for `timezone/` against `crates/pgdrop/timezone.sha256`, which re-running the
script on a clean checkout must leave unchanged. Recompute the copied files'
digests
from a pristine tree with

    sha256sum src/timezone/tznames/{Africa,America,Antarctica,Asia,Atlantic,Australia,Etc,Europe,Indian,Pacific}.txt \
              src/timezone/tznames/{Default,Australia,India} \
              src/backend/tsearch/dicts/*.{syn,ths,affix,dict} \
              src/backend/snowball/stopwords/*.stop

**Do not re-vendor any of these from pgrust's `crates/postgres-18.6-reference/`
tree** (ADR-0008); take them from the tag or the tarball only.

## Licence

Part of the PostgreSQL distribution, under its licence (`COPYRIGHT` at the
tag):

> Portions Copyright (c) 1996-2026, PostgreSQL Global Development Group
>
> Portions Copyright (c) 1994, The Regents of the University of California
>
> Permission to use, copy, modify, and distribute this software and its
> documentation for any purpose, without fee, and without a written agreement
> is hereby granted, provided that the above copyright notice and this
> paragraph and the following two paragraphs appear in all copies.

`src/timezone/data/tzdata.zi`, and so `timezone/`, is in the public domain
(`src/timezone/data/tzdata.zi:3`).

The `*.stop` stopword lists come from the Snowball project's website
(`src/backend/snowball/README:68`), whose work PostgreSQL redistributes under
Snowball's BSD-style licence (`src/backend/snowball/README:8`).
