# ADR-0010: rlibpq waits on its socket with a hand-declared `poll(2)`

Status: accepted (owner decision on NAT-520, 2026-09-23).

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

The Rust standard library has no readiness wait. `rlibpq` is
`#![deny(unsafe_code)]`, and no `libc` dependency is approved. The options
recorded on NAT-520:

| option                                                          | cost                                                                                      |
| --------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| 1. a small, audited `poll(2)` FFI in one module                 | `unsafe` in an MIT crate, confined to one module; the `pollfd` layout is ours to get right |
| 2. the `libc` crate as a dependency of `rlibpq`                 | a new dependency                                                                          |
| 3. timed writes plus a non-blocking read loop                   | no `unsafe`, but not C's behaviour: a busy or timed loop, and a divergence row            |

## Decision

Option 1 (owner, 2026-09-23). `crates/rlibpq/src/poll.rs` declares exactly
one C library symbol, `poll`, in one private module, `poll::sys`, the only
place in the crate where `unsafe_code` is allowed. It is linked from the C
library `std` already links on every target (glibc, musl, libSystem), so no
package is added.

- `struct pollfd` is `{ int fd; short events; short revents; }` on all three C
  libraries; `nfds_t` is `unsigned long` on Linux (glibc and musl) and
  `unsigned int` on Darwin, chosen by `cfg`. Any other target fails to compile
  rather than guess.
- `POLLIN`, `POLLOUT`, `POLLERR`, `POLLHUP`, `POLLNVAL` and `EBADF` have the
  same values on Linux and Darwin.
- The layout (size, alignment, field offsets) and the width of `nfds_t` are
  pinned by unit tests, and the constants by behaviour: a socket pair that is
  fresh, has input, has hung up, or has a full send buffer must be reported
  as such.

On top of it, the safe API mirrors libpq: `poll::socket_poll` is
`PQsocketPoll` (`fe-misc.c:1285`), `poll::socket_check` is `pqSocketCheck`
(`:1229`, retrying `EINTR`), and `Socket::wait` on a `Stream` is `pqWait`. A
socket `Connection::connect` opens is non-blocking at the OS level, as C's is
(`pg_set_noblock`, `fe-connect.c:3368`); `conn->nonblocking` only decides
whether libpq waits on it.

## Consequences

- `rlibpq` stays `#![deny(unsafe_code)]` everywhere but `poll::sys`, which is
  one `extern` declaration and one call with a `SAFETY` note. `rinitdb`,
  `testkit` and `rpsql` are unaffected.
- No new dependency, and nothing new for the musl or Apple lanes to install.
- `Connection<S>` now needs `S: Socket` (`Read + Write` plus `wait`), so a
  scripted test stream implements `wait`; the scripted streams never return
  `WouldBlock`, so theirs is `unreachable!`.
- A new target needs its `nfds_t` added to `poll::sys` before it builds.
