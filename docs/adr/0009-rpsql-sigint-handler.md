# ADR-0009: rpsql's SIGINT handler is installed with `signal-hook`

Status: accepted — decided 2026-09-27 by Nathan (owner), NAT-405.

## Context

`t/020_cancel.pl` requires Ctrl-C to cancel the running query without ending
psql. Upstream installs `handle_sigint` for SIGINT with `pqsignal`
(`src/fe_utils/cancel.c:153`, `:189`); the handler sets `cancel_pressed`
(`src/bin/psql/common.c:323`) and sends the cancel request itself with
`PQcancel`, reporting on stderr with a bare `write()` (`cancel.c:163`-`:173`).

The Rust standard library has no way to install a signal handler, so some C
call is unavoidable. The alternatives, briefly:

- **Hand-declared `extern "C"`** for `signal(2)`, `write(2)` and `errno`
  (PR #61): no new dependency, but new `unsafe` code in an MIT crate that is
  `#![deny(unsafe_code)]`. **Rejected** by the owner.
- **`libc` directly**: still `unsafe` at every call.
- **`ctrlc`**: SIGINT only, no hook for `cancel_pressed` in the handler itself.
- **`signal-hook`** (MIT/Apache-2.0): safe APIs for exactly the two things
  needed. **Chosen.**

## Decision

`signal-hook` 0.3 is an approved dependency of `rpsql` (owner, 2026-09-27).
`crates/rpsql/src/cancel.rs` uses only its safe API:

- `signal_hook::flag::register(SIGINT, cancel_pressed)`: the handler sets
  `cancel_pressed`, as `psql_cancel_callback` does.
- `signal_hook::iterator::Signals` on a thread of its own, `psql-cancel`:
  woken by each SIGINT, it sends the cancel request and writes the report,
  holding the mutex around `cancelConn` — the shape of the Windows arm of
  `cancel.c` (`:195`-`:224`), which also cancels from a thread of its own
  under `cancelConnLock`. `rlibpq::Cancel::cancel` allocates, so it is not
  async-signal-safe and cannot run in the handler.

`signal-hook` installs its handler with `SA_RESTART` and saves and restores
`errno` around it, as `pqsignal` does (`src/port/pqsignal.c:141`, `:88`,
`:112`). The report goes to a duplicate of descriptor 2, as `write_stderr`
writes to it directly (`cancel.c:31`), because the main thread holds `std`'s
`Stderr` lock for the whole session.

## Consequences

- `rpsql` stays `#![deny(unsafe_code)]` with no exception; so do `rlibpq`,
  `rinitdb` and `testkit`. The `unsafe` lives inside `signal-hook` and its
  registry.
- New crates in `rpsql`'s tree: `signal-hook`, `signal-hook-registry`, `errno`
  and `libc`, all MIT or Apache-2.0. None is a pgrust crate, so the license
  wall (ADR-0003) is unaffected.
- The cancel is sent a thread hop after the signal, not inside it; the window
  that opens is recorded in `docs/divergences.md`; it is timing-only and no
  test pins it.
- Interactive mode's `siglongjmp` out of waiting for input
  (`common.c:315`-`:319`) has no Rust equivalent; NAT-405's line-editor slice
  handles SIGINT at the prompt through the editor instead.

## Amendment 2026-09-27: `pgdrop start --foreground` uses it too

`pgdrop start --foreground` (NAT-409) stays attached to the server it
starts. It must survive SIGINT, SIGTERM, SIGHUP and SIGQUIT and forward them
to the server, as pg_ctl forwards SIGINT while it waits for a server to start
(`src/bin/pg_ctl/pg_ctl.c:851`-`:872`). It uses the same crate, safe API only:
`signal_hook::iterator::Signals` on a thread of its own, which sends the
server its shutdown signal through `pgdrop stop`'s `kill`. `pgdrop` stays
`#![deny(unsafe_code)]`.

This adds no dependency. `signal-hook` is already in `pgdrop`'s tree through
`rpsql` (this ADR) and through pgrust, whose whole transitive tree is
approved for `pgdrop` (owner, 2026-09-23; ADR-0001's second amendment).
`pgdrop` now names it directly, and `Cargo.lock` gains only that edge.
