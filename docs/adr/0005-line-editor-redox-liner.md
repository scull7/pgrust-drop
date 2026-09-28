# ADR-0005: Line editing in rpsql uses redox_liner

Status: accepted (owner decision 2026-09-16, between `noline` and `redox_liner`).

## Context

psql's interactive mode needs history (with a file), Emacs and Vi keymaps,
terminal-width awareness for `print.c`'s wrapping, and a hook for tab
completion so `t/010_tab_completion.pl` can be ported later. Candidates the
owner proposed:

| crate         | license | fits                                                                             |
| ------------- | ------- | -------------------------------------------------------------------------------- |
| `noline`      | MPL-2.0 | `no_std` editor over `embedded-io`; no completion hook, no history file; would add a second copyleft-ish license to an MIT crate |
| `redox_liner` | MIT     | readline-like: history + persistence, Emacs/Vi keymaps, `Completer` trait; deps `termion`, `unicode-width`, `itertools`, `strip-ansi-escapes`, `bytecount`; unix-only |

## Decision

`redox_liner` (0.5.x). Pipe/non-tty input bypasses it entirely, as psql does
when stdin is not a terminal, so every gate that feeds SQL on stdin is
unaffected by the editor.

## Consequences

- No Windows interactive mode (termion is unix-only); Windows is out of scope
  for pgdrop today.
- Tab completion (`tab-complete.in.c`) gets a natural home in a `Completer`
  implementation (later issue).
- The crate saw its last release in 2024; if it stalls we vendor or swap
  behind the same small `LineEditor` trait, which is why rpsql wraps it.

## Amendment, 2026-09-27: termion as rpsql's key source (Nathan, NAT-405)

`redox_liner` 0.5.3's `Context::read_line` builds a fresh `stdin().keys()`
for every line (`src/context.rs:129`), and termion 4's key iterator reads two <!-- citation-lint: allow: redox_liner 0.5.3's source, not upstream -->
bytes at a time and parks the second in the iterator. When a line's Enter is
the first of the two, the next line's first byte is dropped with the
iterator: a pasted `\echo ab\n\warn cd\n` ran `warn cd` as query text.
Statement-complete buffering cannot bring back a byte that was never
delivered, and liner does not re-export `termion::event::Key`, so nothing
short of termion itself can drive liner's `Editor` with a longer-lived
iterator.

Decision (owner, 2026-09-27): `termion = "4"` is an approved direct
dependency of `rpsql`, used **only** for raw mode (`IntoRawMode`) and one
session-lived `stdin().keys()` iterator. `input::LinerEditor` owns that
iterator and drives `liner::Editor` with `liner::Emacs` itself, as
`Context::handle_keys` does, instead of calling `Context::read_line`. It adds
no crate to the build: redox_liner already depends on termion 4. `rpsql`
stays `#![deny(unsafe_code)]` with no exception.

Rejected: a `[patch]` of liner (kept as the fallback, and as an optional
follow-up to send upstream), replacing the editor, termios through libc or
`stty`, and shipping the bug. rustyline and reedline are not options.
Bracketed paste is a later, optional slice.

The pty the interactive gates type into (`crates/rpsql/tests/pty/mod.rs`)
opens it through `rustix::pty`'s safe API (`openpt`, `grantpt`, `unlockpt`,
`ptsname`), a dev-dependency of `rpsql`'s tests only, never linked into
`rpsql` (owner, 2026-09-28, NAT-405; ADR-0010's amendment). It declares no
foreign function and holds no `unsafe` code.
