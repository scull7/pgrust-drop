# ADR-0003: Licensing

Status: accepted (owner decision 2026-09-16); amended 2026-09-17 — the wall
was breached through pgrust's vendored PostgreSQL tree, and the source of
vendored content is now pinned by ADR-0008 (see Amendment below).

## Context

- pgrust is AGPL-3.0 (its `LICENSE`); its `NOTICE` records that it is closely
  based on PostgreSQL (PostgreSQL License, permissive).
- This repository started MIT (`LICENSE`, 2026, Nathan Sculli).
- `pgdrop` links pgrust's server, so the combined binary is a derivative work
  under AGPL-3.0 whatever we do.
- `rinitdb`, `rlibpq`, `rpsql` and `testkit` port PostgreSQL C code, which is
  permissively licensed, and can stay MIT as long as no pgrust code is copied
  into them.

## Decision

Per-crate licensing:

| crate                                   | license          |
| --------------------------------------- | ---------------- |
| `testkit`, `rinitdb`, `rlibpq`, `rpsql` | MIT              |
| `pgdrop`                                | AGPL-3.0-only    |

- `rpsql` is written fresh from PostgreSQL's `src/bin/psql` C sources, not
  seeded from pgrust's Rust psql. pgrust's port may be *read* to learn how a C
  idiom was handled, but no code, comments or test corpora are copied from it
  into an MIT crate. The same rule holds for `rlibpq` (port `fe-auth-scram.c`
  and friends from C; do not lift `pgclient`). This wall is what reversed
  ADR-0001's plan to take the six auth primitives as pgrust git dependencies —
  see that ADR's 2026-09-16 amendment.
- The root `LICENSE` stays MIT; `crates/pgdrop/LICENSE` carries the AGPL-3.0
  text; `NOTICE.md` explains the split and credits PostgreSQL and pgrust.
- `Cargo.toml` `license` fields state the per-crate license so `cargo
  metadata` and license scanners see it.

## Consequences

- `pgdrop` release artifacts ship under AGPL-3.0 with source availability.
- Contributors to the MIT crates must port from C, which costs the head start
  pgrust's psql would have given (M3 grows by roughly one issue).
- A pgrust test corpus is never copied; our gate corpora come from PostgreSQL's
  regress suite or are written here.

## Amendment 2026-09-17: "ported from PostgreSQL's C sources" means the tag

The Decision says the MIT crates are "ported from PostgreSQL's C sources only"
and that no pgrust code is copied into them. It did not say where PostgreSQL's
sources come from, and `AGENTS.md` answered that question wrongly: it defined
"upstream" as the PostgreSQL 18.6 tree vendored inside pgrust. That tree is not
pristine.

`crates/rinitdb/share/postgresql.conf.sample` was vendored from it and carried
41 lines of pgrust-specific GUCs — pgrust's own AGPL-3.0 content, under a
`# PGRUST` header — into an MIT-licensed crate. The wall held in intent and
failed in practice, because following the written instruction produced the
breach. PR #11 re-vendored the file from pristine sources.

Upstream is now defined as genuine PostgreSQL 18.6 only: tag `REL_18_6`, commit
`724edf9bde9d356724ad384a2e196edc3c9f80f7`, or the release tarball
`postgresql-18.6.tar.bz2` (sha256
`555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f`). pgrust's
vendored tree is not a source for vendored content and not the authority for a
`file:line`. See ADR-0008 for the measurement and the full rule.

The Decision above is unchanged. This amendment records what it takes to make
it true: the wall is only as good as the tree the MIT crates copy from.
