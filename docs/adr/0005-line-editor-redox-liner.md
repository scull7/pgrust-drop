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
