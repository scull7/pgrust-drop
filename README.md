# pgrust-drop

A single self-contained [pgrust](https://github.com/malisper/pgrust) binary that
includes `initdb` and `psql`, all in Rust. Point a test suite at `pgdrop start`
and it has a Postgres in milliseconds, with no Docker and no PostgreSQL client
tools installed.

Status: **scaffolding**. See [`progress.md`](progress.md) for what works today
and the Linear project *pgrust-drop* for the plan.

## Layout

| crate            | what                                                              |
| ---------------- | ----------------------------------------------------------------- |
| `crates/rinitdb` | Rust `initdb`, tracking PostgreSQL 18.6 `src/bin/initdb`          |
| `crates/rlibpq`  | pure-Rust libpq client (native crate + C ABI), pgrust issue #40   |
| `crates/rpsql`   | Rust `psql`, seeded from pgrust `crates/bin/psql`                 |
| `crates/pgdrop`  | the multicall binary: `pgdrop initdb | psql | postgres | start`   |
| `crates/testkit` | ports of PostgreSQL's TAP helpers and byte-for-byte diff gates    |

## Method

Every tool is proved the way pgrust proves itself: the upstream PostgreSQL
test suites (`src/bin/initdb/t`, `src/bin/psql/t`, `src/interfaces/libpq/t`,
regress `psql*.sql`) are ported and run against both the C tools and ours,
diffing output byte-for-byte. See [`docs/test-stealing.md`](docs/test-stealing.md).

## Build

```bash
cargo build            # Rust 1.96.0 (rust-toolchain.toml), fetched by rustup
cargo test
```

Contributor rules: [`AGENTS.md`](AGENTS.md). Decisions: [`docs/adr/`](docs/adr/).
