# ADR-0010: rlibpq waits on its socket with `rustix::event::poll`

Status: accepted — decided 2026-09-27 by Nathan (owner), NAT-520. Supersedes
the 2026-09-23 choice of a hand-declared `poll(2)`, which the owner rejected
on review (PR #77).

## Context

C libpq never blocks inside `send()`. `pqSendSome` (`fe-misc.c:971`) writes
what the non-blocking socket takes, and while the rest will not go it reads
what the server has sent (`:1103`) and waits for the socket to become readable
*or* writable (`pqWait(true, true)`, `:1115`). Without that, a pipeline whose
requests and replies both overflow the socket buffers deadlocks: the server
blocks writing replies nobody reads, the client blocks writing requests the
server is not reading. `test_pipelined_insert` and `test_uniqviol`
(`src/test/modules/libpq_pipeline/libpq_pipeline.c:1007`, `:2024`) also need
`PQsetnonblocking`, `PQsocket` and a readiness wait of their own.

The Rust standard library has no readiness wait, so some system call is
unavoidable. The alternatives, briefly:

- **Hand-declared `extern "C"`** for `poll(2)`, in one module exempt from
  `#![deny(unsafe_code)]`, with the `pollfd` layout and `nfds_t` cfg'd per
  platform (PR #77): no new dependency, but new `unsafe` code in an MIT
  crate. **Rejected** by the owner.
- **`nix`**: a safe `poll` too, but a much wider crate over `libc`.
- **Timed writes plus a non-blocking read loop**: no system call, but not C's
  behaviour — a busy or timed loop, and a divergence row.
- **`rustix`** (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT): a safe
  `poll` over a borrowed descriptor. **Chosen.**

## Decision

`rustix` 1.x is an approved dependency of `rlibpq` (owner, 2026-09-27), with
`default-features = false` and only the `event` and `std` features.
`crates/rlibpq/src/poll.rs` uses only its safe API:
`rustix::event::poll(&mut [PollFd], Option<&Timespec>)` with
`PollFd::from_borrowed_fd` on the socket's `BorrowedFd` (`AsFd`) and the
`PollFlags` constants, so this crate declares no foreign function and holds
no `pollfd` layout of its own.

On top of it, the API mirrors libpq: `poll::socket_poll` is `PQsocketPoll`
(`fe-misc.c:1285`), `poll::socket_check` is `pqSocketCheck` (`:1229`,
retrying `EINTR` as `:1259` does; rustix reports it as `Errno::INTR`,
`io::ErrorKind::Interrupted`), and `Socket::wait` on a `Stream` is `pqWait`.
`PQsocketPoll`'s timeout is whole milliseconds (`:1305`-`:1317`), handed to
rustix as a `Timespec`, clamped to `c_int::MAX` ms because on Darwin, which
has no `ppoll`, rustix passes milliseconds to `poll(2)` and refuses more. A
socket `Connection::connect` opens is non-blocking at the OS level, as C's
is (`pg_set_noblock`, `fe-connect.c:3368`); `conn->nonblocking` only decides
whether libpq waits on it.

## Consequences

- `rlibpq` stays `#![deny(unsafe_code)]` with no exception and has no
  `unsafe` code; the `unsafe` lives inside `rustix`. `rinitdb`, `testkit`
  and `rpsql` are unaffected.
- New crates in `rlibpq`'s tree: `rustix` and `bitflags`, plus
  `linux-raw-sys` on Linux (rustix makes the system call itself, on glibc
  and musl alike) and `libc` and `errno` on Darwin. All are MIT or
  Apache-2.0; none is a pgrust crate, so the license wall (ADR-0003) is
  unaffected. `rpsql` and `pgdrop` reach them through `rlibpq`.
- `Connection<S>` needs `S: Socket` (`Read + Write` plus `wait`), so a
  scripted test stream implements `wait`; the scripted streams never return
  `WouldBlock`, so theirs is `unreachable!`.
