#!/usr/bin/env bash
# Regenerate crates/rpsql/share/sql_help.c and sql_help.h, the \help text
# rpsql embeds (NAT-403).
#
# It does what PostgreSQL 18.6's build does for psql (the `sql_help`
# custom_target, src/bin/psql/meson.build:37; the `sql_help.h` rule,
# src/bin/psql/Makefile:58): run
#
#   perl src/bin/psql/create_help.pl --docdir doc/src/sgml/ref \
#        --outdir <dir> --basename sql_help
#
# over the SQL reference pages. The generator and the pages come from a
# pristine PostgreSQL 18.6 tree (ADR-0008): the tag REL_18_6 checked out, or
# the release tarball extracted by scripts/fetch-upstream-src.sh; never from
# pgrust's crates/postgres-18.6-reference/ tree. The output depends only on
# the pages (the script sorts both the files and the entries), so the same
# tree gives the same bytes on every host.
#
# Usage:
#   scripts/vendor-sql-help.sh <pristine PostgreSQL 18.6 tree>
#
# Commit both files together, with the digests in
# crates/rpsql/share/README.md and crates/rpsql/src/sql_help.rs updated in
# the same commit.
set -euo pipefail

tree="${1:?usage: scripts/vendor-sql-help.sh <pristine PostgreSQL 18.6 tree>}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! grep -q "^  version: '18.6',$" "$tree/meson.build" 2>/dev/null; then
  echo "vendor-sql-help: $tree is not a PostgreSQL 18.6 tree" >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
perl "$tree/src/bin/psql/create_help.pl" --docdir "$tree/doc/src/sgml/ref" \
  --outdir "$work" --basename sql_help
cp "$work/sql_help.c" "$work/sql_help.h" "$repo/crates/rpsql/share/"
if command -v sha256sum >/dev/null; then
  (cd "$repo/crates/rpsql/share" && sha256sum sql_help.c sql_help.h)
else
  (cd "$repo/crates/rpsql/share" && shasum -a 256 sql_help.c sql_help.h)
fi
