# Vendored PostgreSQL configuration templates

These three files are copied **byte for byte** from the PostgreSQL 18.6 source
tree (as vendored in pgrust at `crates/postgres-18.6-reference/`). They are the
templates C `initdb` reads in `setup_config()` (`src/bin/initdb/initdb.c:1283`),
so a single changed byte here is a changed byte in every cluster this port
creates. Do not reformat, re-wrap or "fix" them.

| file                     | upstream path                                     |
| ------------------------ | ------------------------------------------------- |
| `postgresql.conf.sample` | `src/backend/utils/misc/postgresql.conf.sample`   |
| `pg_hba.conf.sample`     | `src/backend/libpq/pg_hba.conf.sample`            |
| `pg_ident.conf.sample`   | `src/backend/libpq/pg_ident.conf.sample`          |

Upstream ships them without a per-file licence header; they are part of the
PostgreSQL distribution and carry its licence:

> Portions Copyright (c) 1996-2025, PostgreSQL Global Development Group
> Portions Copyright (c) 1994, Regents of the University of California
>
> Permission to use, copy, modify, and distribute this software and its
> documentation for any purpose, without fee, and without a written agreement
> is hereby granted, provided that the above copyright notice and this
> paragraph and the following two paragraphs appear in all copies.

See `docs/adr/0003-licensing.md`: PostgreSQL files may be vendored here with
their licence intact; pgrust's own Rust sources may not.
