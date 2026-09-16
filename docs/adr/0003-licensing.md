# ADR-0003: Licensing

Status: **decision needed** from the repository owner.

## Context

- pgrust is AGPL-3.0 (its `LICENSE`); its `NOTICE` records that it is closely
  based on PostgreSQL (PostgreSQL License).
- This repository is MIT (`LICENSE`, 2026, Nathan Sculli).
- `pgdrop` links pgrust's server → the combined binary is a derivative work
  under AGPL-3.0.
- `rpsql` is seeded from pgrust's `crates/bin/psql` → AGPL-3.0 code.
- `rinitdb`, `rlibpq`, `testkit` port PostgreSQL C code (PostgreSQL License,
  permissive) and can be MIT/PostgreSQL-licensed if they do not copy pgrust code.

## Options

1. Move the whole repository to AGPL-3.0 (simplest, matches pgrust).
2. Dual layout: MIT for `rinitdb`/`rlibpq`/`testkit`, AGPL-3.0 for `rpsql` and
   `pgdrop`, with per-crate `license` fields and a top-level notice.
3. Write `rpsql` fresh from the C sources instead of seeding from pgrust (keeps
   MIT possible for it; costs the head start).

## Recommendation

Option 2 now (per-crate `license` in `Cargo.toml`, `NOTICE.md` naming both
upstreams), revisit if pgrust's licensing changes.
