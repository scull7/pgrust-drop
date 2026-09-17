# ADR-0007: Upstream is genuine PostgreSQL, not pgrust's vendored tree

Status: accepted (owner decision 2026-09-17), after the ADR-0003 breach in
`crates/rinitdb/share/postgresql.conf.sample`.

## Context

`AGENTS.md` defined "upstream" as "the PostgreSQL 18.6 tree vendored in pgrust
at `crates/postgres-18.6-reference/`, and pgrust itself". Every crate in this
repo ports from upstream and cites upstream by `file:line`, so that definition
decided where vendored bytes and citations actually came from.

That tree presents itself as pristine. Its `README-WHY-THIS-IS-HERE.md` says it
is "the complete, pristine PostgreSQL 18.6 source tree, extracted with `git
archive` from the upstream `REL_18_6` tag … no local patches". A file-by-file
comparison against both the `REL_18_6` tag and the released tarball shows the
claim is false. Of the pristine tree's 7,284 files:

- 7,281 are byte-identical in pgrust's copy;
- `src/port/win32ver.rc` is absent (upstream's own `.gitignore` travels with a
  `git archive`, and the README does declare this one);
- two differ, undeclared:
  - `src/backend/utils/misc/postgresql.conf.sample` — 41 added lines of
    pgrust-specific GUCs under a `# PGRUST` header;
  - `src/test/regress/data/streets.data` — one word (`Fleet` → `CI`),
    referenced nowhere in this repo.

So the tree is 99.96% exact and wrong in exactly the place it hurt. Of the 63
distinct upstream paths cited across the MIT crates, exactly one falls in that
divergent set — `postgresql.conf.sample` — and that is the one that was
vendored. `crates/rinitdb/share/postgresql.conf.sample` carried the 41 `#
PGRUST` lines, AGPL-3.0 content from pgrust, into an MIT-licensed crate,
breaching the wall ADR-0003 defines. PR #11 re-vendored it from pristine
sources. This ADR fixes the instruction that caused it.

A tree that is *almost* authoritative is worse than one that is obviously not:
nothing about reading it signals which file is the one that lies.

## Decision

"Upstream" means genuine PostgreSQL 18.6, identified by either:

- git tag `REL_18_6`, commit `724edf9bde9d356724ad384a2e196edc3c9f80f7`
  (`github.com/postgres/postgres`), or
- the release tarball `postgresql-18.6.tar.bz2`, published sha256
  `555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f`.

pgrust (`malisper/pgrust`) is not upstream, and neither is its
`crates/postgres-18.6-reference/` tree.

- Content vendored into this repo is taken from the tag or the tarball. Never
  from pgrust's tree.
- A `file:line` citation is a claim about the tag or the tarball, and is
  resolved against one of them.
- pgrust's tree may still be read for orientation — it is a convenient local
  copy and 7,281 of its files are exact. It is a convenience, not an authority:
  anything taken from it is confirmed against the tag or the tarball before it
  lands. Stating the allowed use is part of the rule; a rule that reads as
  impractical gets ignored quietly.

## Consequences

- Vendoring and citation work needs a pristine 18.6 tree (tag checkout or
  verified tarball) on hand, not just pgrust's checkout.
- The two known divergent paths are the ones to watch: a citation into
  `postgresql.conf.sample` or `streets.data` resolved against pgrust's tree is
  wrong by construction.
- pgrust's README-WHY-THIS-IS-HERE.md is not to be trusted on this point. It is
  pgrust's file, not ours; we do not fix it, we do not rely on it.
- ADR-0003's MIT/AGPL wall now has a source rule behind it, not just an
  intention — see that ADR's 2026-09-17 amendment.
