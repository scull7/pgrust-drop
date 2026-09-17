# ADR-0001: Monorepo layout and how pgrust is vendored

Status: accepted (owner decision 2026-09-16): pgrust is a rev-pinned Cargo git
dependency; amended 2026-09-16 — the six auth primitives are ported from
PostgreSQL C into `rlibpq` instead of linked, because ADR-0003 makes `rlibpq`
MIT (see Amendment below).

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
- Bumping pgrust is one line plus a note on the Linear issue (`progress.md` is
  retired — see AGENTS.md, "Change hygiene").
- License consequences are in ADR-0003.

## Amendment 2026-09-16: the auth primitives are ported, not linked

The Decision above names six pgrust crates — `scram_common`, `pg_hmac`,
`pg_md5`, `saslprep`, `pg_b64`, `timingsafe_bcmp` — as git dependencies "for
auth primitives". That path is closed. ADR-0003 makes `rlibpq` MIT and pgrust
is AGPL-3.0, so depending on those crates would link an AGPL crate into an MIT
one. All six are ported from the PostgreSQL C sources into `rlibpq` instead:

| pgrust crate       | ported into                | from                                       |
| ------------------ | -------------------------- | ------------------------------------------ |
| `pg_md5`           | `crates/rlibpq/src/md5.rs` | `src/common/md5.c`, `md5_common.c`         |
| (SHA-256)          | `src/sha256.rs`            | `src/common/sha2.c`                        |
| `pg_hmac`          | `src/hmac.rs`              | `src/common/hmac.c` (the non-OpenSSL build)|
| `pg_b64`           | `src/base64.rs`            | `src/common/base64.c`                      |
| `scram_common`     | `src/scram.rs`             | `src/common/scram-common.c`, `fe-auth-scram.c` |
| `saslprep`         | `src/scram.rs` (`saslprep`)| `src/common/saslprep.c`, ASCII fast path only |
| `timingsafe_bcmp`  | `src/scram.rs:66`          | `src/port/timingsafe_bcmp.c` (`#else` arm) |

The check is mechanical: `crates/rlibpq/Cargo.toml` has an empty
`[dependencies]`. `rlibpq` links nothing at all, pgrust included.

SASLprep is ported only as far as `pg_saslprep`'s pure-ASCII fast path
(`saslprep.c:1067`); full normalization needs RFC 3454's tables and
`unicode_norm.c`. That narrowing is a recorded divergence, not a silent one —
see `docs/divergences.md`.

The rest of the Decision stands. The server (`main_main`) is still planned as a
rev-pinned git dependency of `pgdrop`, which is why `pgdrop` is AGPL-3.0-only
(ADR-0003). It is not in any manifest yet — vendoring pgrust is NAT-376 — so
the first consequence above describes the build `pgdrop` will have, not the one
it has today.
