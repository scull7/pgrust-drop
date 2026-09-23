# The template cluster image

`template.img` is a PostgreSQL 18.6 data directory, minted by C `initdb`,
stripped of its per-cluster files and packed into one file. `rinitdb` embeds
it (`include_bytes!`, `crates/rinitdb/src/image.rs`) and expands it to create
a cluster, because pgrust has no `postgres --boot` to build the catalogs with
(ADR-0002). `template.manifest` records where it came from.

Do not edit either file by hand, and do not re-pack the image with anything
but the script below: it is checked byte for byte.

## Provenance

Minted by `scripts/mint-template-image.sh`, which runs
`crates/rinitdb/examples/mint_template.rs`:

    initdb -D <dir> --no-locale --encoding=UTF8 -U postgres -A trust --no-sync

(`rinitdb::image::MINT_ARGS`), then strips the files `rinitdb::image::strip`
names — the configuration files, `postmaster.opts`, the top-level
`PG_VERSION`, `global/pg_control`, `pg_stat/pgstat.stat` and every file under
`pg_wal/` — and packs the rest in format version 1 (`rinitdb::image`).

The binary that minted it is Alpine Linux 3.24's `postgresql18` package,
PostgreSQL 18.6 linked against musl (`/usr/libexec/postgresql18/initdb`). The
tool refuses any other release and any binary whose ELF dynamic loader is not
musl's, and it mints twice and refuses unless both mints pack to the same
bytes. What `template.manifest` records:

| key       | value                                                              |
| --------- | ------------------------------------------------------------------ |
| `format`  | `1`                                                                |
| `initdb`  | `initdb (PostgreSQL) 18.6`                                         |
| `libc`    | `musl`                                                             |
| `options` | `--no-locale --encoding=UTF8 -U postgres -A trust --no-sync`       |
| `bytes`   | `23633969`                                                         |
| `sha256`  | `c2f04ac2821873e38ec8b9c2e9fc5c4a5c7c704bf969c5fdbdf5c36b5fb261e4` |

The test `the_embedded_image_is_the_one_the_manifest_records` in
`crates/rinitdb/src/image.rs` asserts the embedded bytes against the
manifest's length and digest, and the manifest's recipe against `MINT_ARGS`,
on every `cargo test`. Check it by hand with

    sha256sum crates/rinitdb/image/template.img

### What the host contributes

`initdb` always runs `SELECT pg_import_system_collations('pg_catalog')`
(`setup_collation`, `src/bin/initdb/initdb.c:1781` at `REL_18_6`), even under
`--no-locale`, so `pg_collation` in the image holds whatever the minting host
offered:

- **libc:** none. The minting host has no `locale` program, so the import's
  `locale -a` found nothing. On a musl host that has one, the rows it adds
  carry a NULL `collversion` (ADR-0002), which is why the image is minted on
  the musl lane.
- **ICU:** 805 rows, each with the `collversion` of the host's libicu (78.1,
  versions `153.136…`). They describe the ICU that minted the image, not the
  one a cluster will run with; NAT-383's post-expand fixups have to account
  for them.
- **builtin:** 3 rows, version `1` (PostgreSQL's own), and the 2 `c`-provider
  rows and `default`, as on every host.

So re-minting on a host with a different libicu, or with `locale` on `PATH`,
changes the image and its digest. Commit a re-mint's image and manifest
together, and say why on the Linear issue.

## Licence

The image is output of PostgreSQL's `initdb` over PostgreSQL's catalog data
(`src/include/catalog/*.dat` through the `postgres.bki` generated from them,
then what `initdb` loads in single-user mode: `src/backend/catalog/*.sql`,
`src/backend/catalog/sql_features.txt` and
`src/backend/snowball/snowball*.sql.in`), so it is part of the PostgreSQL
distribution's work and carries its licence (`COPYRIGHT` at `REL_18_6`):

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

No pgrust code ran to make it: the minting `initdb` and `postgres` are
PostgreSQL's own C binaries, so no AGPL-3.0 content reaches this MIT crate
(ADR-0003). `NOTICE.md` at the repository root states each crate's licence.
