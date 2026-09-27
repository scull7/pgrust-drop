#!/usr/bin/env bash
# Regenerate crates/pgdrop/share/timezone/, the compiled timezone database
# pgdrop embeds (NAT-408), and its digest manifest crates/pgdrop/timezone.sha256.
#
# It does what `make install` of PostgreSQL 18.6 does for share/timezone when
# built without --with-system-tzdata (src/timezone/Makefile:58): build zic from
# src/timezone/zic.c and run
#
#   zic -d <datadir>/timezone src/timezone/data/tzdata.zi
#
# with ZIC_OPTIONS empty (src/timezone/Makefile:33). The sources come from the
# pristine release tarball, checked against its published sha256 (ADR-0008);
# never from pgrust's crates/postgres-18.6-reference/ tree.
#
# Usage:
#   scripts/vendor-timezone.sh postgresql-18.6.tar.bz2
#
# (https://ftp.postgresql.org/pub/source/v18.6/postgresql-18.6.tar.bz2)
#
# Needs a C compiler and make. PostgreSQL 18's configure refuses to run
# without bison and flex even from a tarball; zic needs neither, so stand-ins
# that only answer --version are put on configure's command line. zic's output
# does not depend on the host (TZif is big-endian and carries no timestamp of
# its own), and two runs are compared before anything is written.
#
# zic hard-links the names a Link line makes; they are vendored as plain
# files, which is what git stores anyway. Commit the tree and the manifest
# together.
set -euo pipefail

tarball="${1:?usage: scripts/vendor-timezone.sh postgresql-18.6.tar.bz2}"
published=555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

actual="$(sha256 "$tarball" | cut -d' ' -f1)"
if [ "$actual" != "$published" ]; then
  echo "vendor-timezone: $tarball has sha256 $actual, not the published $published" >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
tar -xjf "$tarball" -C "$work"
src="$work/postgresql-18.6"

mkdir "$work/stub"
printf '#!/bin/sh\necho "bison (GNU Bison) 3.8.2"\n' >"$work/stub/bison"
printf '#!/bin/sh\necho "flex 2.6.4"\n' >"$work/stub/flex"
chmod +x "$work/stub/bison" "$work/stub/flex"

(
  cd "$src"
  ./configure --without-readline --without-zlib --without-icu \
    BISON="$work/stub/bison" FLEX="$work/stub/flex" >"$work/configure.log"
  make -C src/timezone zic >"$work/make.log"
)

"$src/src/timezone/zic" -d "$work/one" "$src/src/timezone/data/tzdata.zi"
"$src/src/timezone/zic" -d "$work/two" "$src/src/timezone/data/tzdata.zi"
diff -r "$work/one" "$work/two" >/dev/null || {
  echo "vendor-timezone: two zic runs differ; refusing" >&2
  exit 1
}

dest="$repo/crates/pgdrop/share/timezone"
rm -rf "$dest"
mkdir -p "$dest"
(cd "$work/one" && find . -type f | while read -r f; do
  mkdir -p "$dest/$(dirname "$f")"
  cp "$f" "$dest/$f"
done)

(cd "$repo/crates/pgdrop/share" && find timezone -type f | LC_ALL=C sort | while read -r f; do
  sha256 "$f"
done) >"$repo/crates/pgdrop/timezone.sha256"

echo "vendor-timezone: $(wc -l <"$repo/crates/pgdrop/timezone.sha256") files," \
  "tzdata $(head -1 "$src/src/timezone/data/tzdata.zi" | sed 's/^# version //')"
