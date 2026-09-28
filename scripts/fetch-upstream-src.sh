#!/usr/bin/env bash
# Fetch the pristine PostgreSQL 18.6 source tree the citation lint checks
# against (NAT-519).
#
# "Upstream" is PostgreSQL 18.6 and nothing else (AGENTS.md): the release
# tarball, checked against its published sha256, has exactly the 7,284 files
# of tag REL_18_6. pgrust's crates/postgres-18.6-reference/ is not it.
#
# Usage:
#   scripts/fetch-upstream-src.sh [--dest DIR]
#
# Prints the `export` line the lint needs. In CI, append it to $GITHUB_ENV.
# A git checkout of REL_18_6 (724edf9bde9d356724ad384a2e196edc3c9f80f7) works
# as well: point PGDROP_UPSTREAM_SRC at it directly.
set -euo pipefail

readonly VERSION="18.6"
readonly URL="https://ftp.postgresql.org/pub/source/v${VERSION}/postgresql-${VERSION}.tar.bz2"
readonly SHA256="555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f"

dest="${PWD}/.upstream"

while [ $# -gt 0 ]; do
  case "$1" in
    --dest) dest="$2"; shift 2 ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

tree="${dest}/postgresql-${VERSION}"
if [ ! -f "${tree}/configure" ]; then
  mkdir -p "$dest"
  tarball="${dest}/postgresql-${VERSION}.tar.bz2"
  [ -f "$tarball" ] || curl -fsSL --retry 3 -o "$tarball" "$URL"
  if command -v sha256sum >/dev/null; then
    actual="$(sha256sum "$tarball" | cut -d' ' -f1)"
  else
    actual="$(shasum -a 256 "$tarball" | cut -d' ' -f1)"
  fi
  if [ "$actual" != "$SHA256" ]; then
    echo "sha256 mismatch for ${tarball}: got ${actual}, want ${SHA256}" >&2
    rm -f "$tarball"
    exit 1
  fi
  tar -xjf "$tarball" -C "$dest"
fi

echo "export PGDROP_UPSTREAM_SRC=${tree}"
