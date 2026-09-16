# Vendored PostgreSQL configuration templates

These three files are copied **byte for byte** from the PostgreSQL 18.6 source
tree. They are the templates C `initdb` reads in `setup_config()`
(`src/bin/initdb/initdb.c:1283`), so a single changed byte here is a changed
byte in every cluster this port creates. Do not reformat, re-wrap or "fix"
them.

| file                     | upstream path                                     |
| ------------------------ | ------------------------------------------------- |
| `postgresql.conf.sample` | `src/backend/utils/misc/postgresql.conf.sample`   |
| `pg_hba.conf.sample`     | `src/backend/libpq/pg_hba.conf.sample`            |
| `pg_ident.conf.sample`   | `src/backend/libpq/pg_ident.conf.sample`          |

## Provenance

Vendored from the official PostgreSQL 18.6 release tarball,
`https://ftp.postgresql.org/pub/source/v18.6/postgresql-18.6.tar.bz2`, whose
published SHA-256 (`…/postgresql-18.6.tar.bz2.sha256`) is

    555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f

and cross-checked against the `postgres/postgres` GitHub mirror at tag
`REL_18_6`, commit `724edf9bde9d356724ad384a2e196edc3c9f80f7`. The two agree
byte for byte. The SHA-256 of each file as vendored:

| file                     | sha256                                                             |
| ------------------------ | ------------------------------------------------------------------ |
| `postgresql.conf.sample` | `93e3da1fc8d667ba086ee239e5f053581739648a90566f191666cc63de16357f` |
| `pg_hba.conf.sample`     | `e3abfe29646ac6ece67e92d0b5255eb3b35d2878d23c7ea6bbd94f100054c168` |
| `pg_ident.conf.sample`   | `bf8f1664dc42eeb78a71a3746afb6b6c93805bda55ff4504f796dbe339d1fa50` |

**Do not re-vendor these from pgrust's `crates/postgres-18.6-reference/`
tree.** It is not a pristine PostgreSQL checkout: pgrust modifies it, and its
`postgresql.conf.sample` carries a 41-line `# PGRUST` section documenting
pgrust-only GUCs (`connection_queue_size`, `pgrust.admission_bypass`,
`shared_catalog_cache`, …). That tree was the stated source of an earlier
vendoring here, which is how those settings came to be written into every
cluster `rinitdb` created. Re-vendor only from an official PostgreSQL release
tarball or the `postgres/postgres` tag, and record the version and digests
above.

`crates/rinitdb/src/conf.rs` pins each file's length and digest, and asserts
that `postgresql.conf.sample` carries only upstream's section banners and no
namespaced (extension-style) setting — the two properties a mere digest, which
blesses whatever was vendored, cannot establish.

## Licence

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
their licence intact; pgrust's own sources — including its modifications to
its vendored PostgreSQL tree — may not, because pgrust is AGPL-3.0 and this
crate is MIT.
