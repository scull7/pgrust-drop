# ADR-0002: initdb via an embedded template cluster (bridge strategy)

Status: accepted for M1 (2026-09-16); superseded by M5 when pgrust gains `--boot`.

## Context

C `initdb` builds a cluster in three phases: `postgres --boot` fed with
`postgres.bki` creates the bootstrap catalogs in `template1`;
`postgres --single` runs the SQL setup scripts (`system_views.sql`,
`information_schema.sql`, `snowball_create.sql`, …); then `template0` and
`postgres` are copied from `template1`.

pgrust's server refuses the first phase:

```
$ postgres --boot
postgres: bootstrap mode (--boot) is not supported by pgrust
```

(`crates/backend/main/main_main/src/lib.rs:328`). pgrust's own comments confirm
datadirs "come from stock C initdb" (`janitor/src/bootstrap.rs`), and its
browser demo mints one with C initdb at build time and packs it into a VFS image
(`wasm/build.sh`). `postgres --single` **is** supported.

## Decision

`rinitdb` ships a pre-minted template cluster inside the binary:

1. Offline: `initdb -D tpl --no-locale --encoding=UTF8 -U postgres -A trust
   --no-sync` with PostgreSQL 18.6; strip per-cluster and volatile files; pack;
   record provenance (initdb version, options, sha256) in an embedded manifest.
2. At run time: validate options exactly as `initdb.c` does (same error text),
   create the directory tree with the same modes, expand the image, write a
   fresh `pg_control` (new system identifier, checksum flag per `-k`), render
   `postgresql.conf` / `pg_hba.conf` / `pg_ident.conf` from the vendored
   `.sample` files, then apply the rest (superuser name, password, text search
   config) with `postgres --single`.

## Consequences

- Cluster creation is an unpack plus a short single-user session: milliseconds,
  no C toolchain, no share directory on the host.
- The encoding/locale matrix is limited to what the embedded image(s) carry.
  Unsupported combinations fail with a clear error; each is listed in
  `docs/divergences.md`. `t/001_initdb.pl` cases that need them are gated.
- The template must be re-minted when pgrust or PostgreSQL 18.x changes catalog
  contents; the manifest makes drift visible.
- M5 (`genbki` port + upstream `--boot`) turns the image into an optional fast
  path and restores the faithful algorithm.
