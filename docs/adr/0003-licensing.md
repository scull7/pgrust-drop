# ADR-0003: Licensing

Status: accepted (owner decision 2026-09-16).

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
