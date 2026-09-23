# ADR-0001: Monorepo layout and how pgrust is vendored

Status: accepted (owner decision 2026-09-16): pgrust is a rev-pinned Cargo git
dependency; amended 2026-09-16 — the six auth primitives are ported from
PostgreSQL C into `rlibpq` instead of linked, because ADR-0003 makes `rlibpq`
MIT (see Amendment below); amended 2026-09-23 — pinned to a scull7 fork
carrying musl build fixes pending upstream, and pgrust's dependency tree
approved for `pgdrop` only (see the second Amendment).

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

## Amendment 2026-09-23: pinned to a scull7 fork until upstream builds on musl

Made by the owner while landing NAT-376, which is where the Decision's git
dependency first entered a manifest.

**What broke.** At the rev we meant to pin, malisper/pgrust `79ad992` (`main`,
"v0.3", the first rev carrying the collation-import port ADR-0002 needs),
pgrust does not compile for `x86_64-unknown-linux-musl`, the product's primary
target (ADR-0007). Two call sites use items the `libc` crate binds for glibc
only: `libc::getentropy` in `pg_strong_random` and the `libc::LC_*_MASK`
constants in `pg_locale`. With those two fixed, the whole tree builds on musl.
Nothing else in pgrust needed a change.

**Decision.** The pin is a fork, `github.com/scull7/pgrust`, branch
`musl-build`, pinned at `75f1d3985d9841de3ddfa8c139ca308df0f80fcd`: `79ad992`
plus two commits, both behind `cfg(target_env = "musl")`. `118a287` carries the
two fixes; `75f1d39` makes the musl `getrandom` arm retry `EINTR` and resume
after a short read, as glibc's and musl's `getentropy` do, instead of falling
back to `/dev/urandom` (answering a review on malisper/pgrust#115). The same
commits are that pull request to malisper/pgrust. Once upstream has the fixes,
we switch back to malisper/pgrust at the first upstream rev that contains them
(`scripts/pgrust-rev.sh --repo malisper/pgrust`) and delete the fork branch.
It is still a Cargo git dependency pinned by `rev`. The Decision named a git
submodule as the fallback for local patches; a fork keeps the manifest shape and
the lock file honest, and a submodule would add nothing.

**The squash risk, and why the fork branch matters.** pgrust squashes `main`
to one commit per release and keeps each previous release only on an
`archive/v0.x-main-*` branch. A rev pin stays fetchable only while some ref
reaches the commit. On the fork, the `musl-build` branch is that ref. Do not
delete or force-push it while the pin points at it. When the pin moves back to
malisper/pgrust, the pinned upstream rev has the same exposure, so a bump
should record which upstream branch reaches it.

**Dependencies (owner, 2026-09-23).** pgrust's whole transitive dependency
tree (about 870 pgrust crates plus third-party crates such as `openssl` with
`vendored`, `zstd-sys` and `mimalloc`) is approved **in `pgdrop` only**. It must
never reach the MIT crates (`testkit`, `rinitdb`, `rlibpq`, `rpsql`), through
any edge. `scripts/check-license-wall.sh` fails if one of them can reach a
pgrust crate, and every CI lane runs it.

**Build requirements.** pgrust's tree needs a C compiler; `perl` and `make`
(`openssl-src` builds OpenSSL from source); and, for release-family profiles,
libre2 plus a C++ compiler. pgrust's `regexp_alt` build script refuses to build
a release without RE2, and a dev build without it falls back to the
Spencer-only engine. CI builds dev profiles with `PGRUST_FORCE_NO_RE2=1`, so
every lane takes that fallback on purpose rather than depending on whether a
runner image happens to have libre2. A release `pgdrop` with RE2, including a
static musl build (Alpine packages no static libre2), is later work.

**Cost.** A debug build of `pgdrop` with the pgrust tree takes about 3 minutes
on 4 cores, cold. With full debuginfo the tree made an 11 GB `target/`, so
`[profile.dev.package."*"]` keeps line tables only for dependencies (1.3 GB).
The fork is a 727 MB git repository. CI caches `~/.cargo/git`, the registry
and `target/` per lane (`Swatinem/rust-cache`), so a warm run fetches and
compiles none of pgrust.

**Linking.** `pgdrop` now links `main_main`. It reads only pgrust's
`PG_BACKEND_VERSIONSTR`, for `pgdrop postgres --version`. Running the server
in-process is NAT-407.
