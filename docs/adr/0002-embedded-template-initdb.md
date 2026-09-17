# ADR-0002: initdb via an embedded template cluster (bridge strategy)

Status: accepted for M1 (2026-09-16); locale handling revised 2026-09-17
(ADR-0007 target matrix); superseded by M5 when pgrust gains `--boot`.

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

### Locale is not safe to bake in

The first revision of this ADR minted the image with
`initdb --no-locale --encoding=UTF8`, which quietly assumed the mint host's libc
was irrelevant. ADR-0007 makes musl and Darwin primary targets, so it is not.
Measured with PostgreSQL 18.6: templates minted by a **glibc** `initdb`, then
opened by a **musl** server.

| mint recipe | `datcollversion` | musl server on that datadir |
| ----------- | ---------------- | --------------------------- |
| `--no-locale` | `NULL` | clean |
| `--locale=C.UTF-8` (libc provider) | `NULL` | clean |
| `--locale-provider=builtin --builtin-locale=C.UTF-8` | `1` (PostgreSQL's own) | clean |
| `--locale=en_US.UTF-8` (libc provider) | `2.39` | starts, but every connection warns `database "template1" has no actual collation version, but a version was recorded` |

The fault line is not glibc versus musl. It is whether the image carries a
**libc-versioned** locale. `2.39` is the mint host's glibc version, so a baked
`en_US.UTF-8` image misbehaves on a different glibc release too — shipping one
image per libc would not fix it, it would need one image per libc *version*.

## Decision

The embedded image carries **bootstrap catalogs only**. Locale is not mint-time
state; `rinitdb` stamps it at run time, in the `postgres --single` phase this
ADR already runs, the way C `initdb` does:

1. Offline: mint with PostgreSQL 18.6, strip per-cluster and volatile files,
   pack, and record provenance (initdb version, options, sha256) in an embedded
   manifest.
2. At run time: validate options exactly as `initdb.c` does (same error text),
   create the directory tree with the same modes, expand the image, write a
   fresh `pg_control` (new system identifier, checksum flag per `-k`), render
   `postgresql.conf` / `pg_hba.conf` / `pg_ident.conf` from the vendored
   `.sample` files, then in `postgres --single`: set
   `datlocprovider` / `datcollate` / `datctype` / `datlocale` / `datcollversion`
   from the *host's* libc, import system collations, and apply the superuser
   name, password and text search config.

Locale correctness then comes from the machine creating the cluster, one image
serves every libc and every locale the host supports, and the matrix is no
longer frozen at mint time.

**pgrust supports this** (owner research, 2026-09-17). `--single` is a real
backend, not a stub: `main_main` dispatches `DispatchOption::Single` to
`postgres_single_user_main`, and `--boot` is the only refused mode. The session
is the bootstrap superuser, so the import function's `superuser()` check passes.
Both functions are ordinary SQL builtins —
`pg_import_system_collations(regnamespace) -> int4` (foid 3445,
`crates/backend/commands/collationcmds/src/import.rs`) and
`pg_collation_actual_version(oid) -> text` (foid 3448) — and import writes
`CollationForm.collversion` from `get_collation_actual_version()` as each row is
created. NAT-376 only has to pin a rev that already contains the port; the
`--single` script itself is NAT-383.

The image is minted `--no-locale --encoding=UTF8`, so the default database is C
and `datcollversion` is NULL by design. After expand, `rinitdb` runs the two
statements `initdb.c` runs:

```sql
UPDATE pg_collation SET collversion = pg_collation_actual_version(oid) WHERE collname = 'unicode';
SELECT pg_import_system_collations('pg_catalog');
```

The import is skipped unless the user asked for a locale, and
`ALTER DATABASE ... REFRESH COLLATION VERSION` is added only when the minted
template's recorded version and the expand host disagree.

**What a collation version is, per provider.** A NULL is not a failure:

| provider | locale | `collversion` |
| -------- | ------ | ------------- |
| builtin | `C`, `C.UTF-8`, `PG_UNICODE_FAST` | `"1"` |
| libc | `C`, `C.*`, `POSIX` | NULL, as in C PostgreSQL |
| libc | anything else, linux-gnu | `gnu_get_libc_version()`, e.g. `2.39` |
| libc | anything else, macOS or musl | NULL |
| ICU | any loaded langtag | `ucol_getVersion`; skipped entirely when libicu is not loadable |

So the apple and musl lanes must treat a NULL libc `collversion` as correct.
That is a lane expectation, not a bug to chase.

The invariant that makes this safe is expressed as a type, not a comment: the
set of locales that may be baked is closed, and no member of it is
libc-versioned. `--no-locale` is the recipe in use; `BuiltinCUtf8` is admissible
if a UTF-8 ctype template is ever wanted, because its version is PostgreSQL's
own (`"1"`), not libc's.

```rust
/// Locales that may be baked into the embedded image. Both are versionless or
/// versioned by PostgreSQL itself, never by libc — see the table above.
enum BakedLocale { C, BuiltinCUtf8 }
```

## Consequences

- Cluster creation is an unpack plus a short single-user session: milliseconds,
  no C toolchain, no share directory on the host.
- Run-time stamping is more work in `--single` than baking was, but the pgrust
  capability it needs is confirmed present, so M1 can commit to it on the gnu
  lane. Expand-time failures to handle: no `locale` on PATH gives ERROR
  `could not execute command "locale -a"`, which kills the statement but not
  `--single`; zero usable locales gives WARNING `no usable system locales were
  found` and the function still returns; re-import is idempotent, since existing
  names are skipped.
- Locales the host's libc does not have still fail, but they now fail on the
  machine that would have to support them, with that libc's own error, instead
  of being excluded at mint time. musl accepts locale names glibc rejects, which
  is a divergence in its own right — see `docs/divergences.md`.
- The template must be re-minted when pgrust or PostgreSQL 18.x changes catalog
  contents; the manifest makes drift visible.
- Whether one image serves both x86\_64 and aarch64 is untested (ADR-0007);
  deferred to v2.
- M5 (`genbki` port + upstream `--boot`) turns the image into an optional fast
  path and restores the faithful algorithm.
