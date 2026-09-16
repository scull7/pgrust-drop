# ADR-0001: Monorepo layout and how pgrust is vendored

Status: proposed (2026-09-16). Owner decision pending on the vendoring option.

## Context

pgrust (`malisper/pgrust`) is a Cargo workspace of several hundred crates
pinned to Rust 1.96.0, with the server binary at
`crates/backend/main/main_main` (`postgres`; it also exposes a lib target). Its
README states it "does not ship its own `initdb` or `psql` yet". It already
contains a Rust `psql` (`crates/bin/psql`, ~5k lines) and an in-server wire
client (`crates/interfaces/pgclient`) that depend on server-internal crates.

## Decision

One Cargo workspace, five crates: `testkit`, `rinitdb`, `rlibpq`, `rpsql`,
`pgdrop`. Each tool crate is a library with a thin `main.rs`; `pgdrop` links the
libraries and dispatches by subcommand or `argv[0]` (multicall).

pgrust is pulled in as a **Cargo git dependency pinned by rev** on the crates we
need (`main_main` for the server; `scram_common`, `pg_hmac`, `pg_md5`,
`saslprep`, `pg_b64`, `timingsafe_bcmp` for auth primitives). A git submodule is
the fallback only if we must patch pgrust locally before an upstream PR lands.

## Consequences

- Building `pgdrop` builds pgrust's server: needs `libre2-dev`/`pkg-config`
  (release), Rust 1.96.0, and a warm cache in CI (NAT-376).
- Bumping pgrust is one line plus a `progress.md` entry.
- License consequences are in ADR-0003.
