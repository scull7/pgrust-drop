# ADR-0009: rpsql's SIGINT handler declares its three C calls itself

Status: proposed (NAT-405, 2026-09-27) — needs the owner's acceptance in the
PR that introduces it, because it is the first `unsafe` code in an MIT crate.

## Context

`t/020_cancel.pl` requires Ctrl-C to cancel the running query without ending
psql. Upstream installs `handle_sigint` for SIGINT with `pqsignal`
(`src/fe_utils/cancel.c:153`, `:189`); the handler sets `cancel_pressed`
(`src/bin/psql/common.c:323`) and sends the cancel request itself with
`PQcancel`, reporting on stderr with a bare `write()` (`cancel.c:163`-`:173`).

The Rust standard library has no way to install a signal handler. Every
alternative needs a C call:

| option                                        | cost                                                                                           |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| the `libc` crate                              | a new dependency; not approved (AGENTS.md)                                                     |
| `signal-hook` or `ctrlc`                      | new dependencies, both built on `libc`; not approved                                           |
| hand-declared `extern "C"` for what is needed | `unsafe` in `rpsql`, which is `#![deny(unsafe_code)]`; no new dependency                       |
| no handler                                    | SIGINT's default action ends rpsql: NAT-405's Acceptance cannot be met                         |

## Decision

`crates/rpsql/src/cancel.rs` declares exactly three C library symbols in one
private module, `sys`, the only place in the crate where `unsafe_code` is
allowed:

- `signal(SIGINT, handler)`: BSD semantics on glibc, musl and Darwin — the
  handler stays installed and interrupted system calls restart — which is what
  `pqsignal` asks `sigaction` for with `SA_RESTART` (`src/port/pqsignal.c:141`).
  `sigaction` itself was not used because `struct sigaction`'s layout differs
  between the three C libraries, and `signal`'s signature does not.
- `write(fd, buf, len)`: the one call the handler makes, and how the report
  reaches descriptor 2, as `write_stderr` does (`cancel.c:31`).
- the thread's `errno` (`__errno_location` on Linux, `__error` on Darwin), saved
  and restored around the handler as `pqsignal`'s `wrapper_handler` does
  (`pqsignal.c:88`, `:112`).

`SIGINT` is 2 on every target rpsql ships to (ADR-0007).

The handler does only what is async-signal-safe: it stores `cancel_pressed` in
an atomic and writes one byte to a non-blocking Unix datagram socket.
`rlibpq::Cancel::cancel` allocates, so it is not called from the handler; a
thread woken by that byte sends the request and writes the report, holding the
mutex around `cancelConn` — the shape of the Windows arm of `cancel.c`
(`:195`-`:224`), which also cancels from a thread of its own under
`cancelConnLock`.

## Consequences

- `rlibpq`, `rinitdb` and `testkit` stay `#![deny(unsafe_code)]`, and so does
  every module of `rpsql` but `cancel::sys`.
- No new dependency. When ADR-0005's `redox_liner` lands, `libc` enters the
  tree through `termion`; that does not approve `libc` as a direct dependency,
  and this module does not need it.
- The cancel is sent a thread hop after the signal, not inside it; the window
  that opens is recorded in `docs/divergences.md` and pinned there.
- Interactive mode's `siglongjmp` out of waiting for input
  (`common.c:315`-`:319`) has no Rust equivalent; NAT-405's line-editor slice
  handles SIGINT at the prompt through the editor instead.
