#!/usr/bin/env bash
# Fetch the C PostgreSQL 18.6 reference binaries for this machine's lane.
#
# The byte-diff gates need a reference `initdb`/`postgres` built against the
# same C library as the binary under test (ADR-0007). Maven Central publishes
# vanilla 18.6.0 builds for every lane we support, including musl, which is the
# only source that covers Alpine and macOS from one place.
#
# Usage:
#   scripts/fetch-ref-binaries.sh [--dest DIR] [--lane gnu|musl|apple] [--print-env]
#
# Prints the `export` line the gates need. In CI, append it to $GITHUB_ENV.
#
# Not included: `psql`. These bundles ship initdb, postgres, pg_ctl and libpq
# only; the rpsql gate (M3) needs its own reference, see ADR-0007.
set -euo pipefail

readonly PG_VERSION="18.6.0"
readonly GROUP_PATH="io/zonky/test/postgres"
readonly BASE_URL="https://repo1.maven.org/maven2/${GROUP_PATH}"

dest="${PWD}/.ref"
lane=""
print_env_only=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dest) dest="$2"; shift 2 ;;
    --lane) lane="$2"; shift 2 ;;
    --print-env) print_env_only=1; shift ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# The lane is a property of the build being tested, not of what happens to be
# installed: a glibc host can have the musl loader present for cross-testing, so
# detection keys on the distribution's own libc and `--lane` overrides it.
detect_lane() {
  case "$(uname -s)" in
    Darwin) echo apple ;;
    Linux) if [ -e /etc/alpine-release ]; then echo musl; else echo gnu; fi ;;
    *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
  esac
}

[ -n "$lane" ] || lane="$(detect_lane)"

case "$(uname -m)" in
  x86_64|amd64) arch=amd64; arch_v8=amd64 ;;
  arm64|aarch64) arch=arm64v8; arch_v8=arm64v8 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

case "$lane" in
  gnu)   classifier="linux-${arch_v8}" ;;
  musl)  classifier="linux-${arch_v8}-alpine" ;;
  apple) classifier="darwin-${arch}" ;;
  *) echo "unknown lane: $lane (expected gnu, musl or apple)" >&2; exit 2 ;;
esac

case "$lane" in
  gnu) env_var=PGDROP_REF_BIN_GNU ;;
  musl) env_var=PGDROP_REF_BIN_MUSL ;;
  apple) env_var=PGDROP_REF_BIN_APPLE ;;
esac

prefix="${dest}/${lane}-${arch_v8}"
bin_dir="${prefix}/bin"

if [ "$print_env_only" = 1 ]; then
  echo "${env_var}=${bin_dir}"
  exit 0
fi

if [ -x "${bin_dir}/initdb" ]; then
  echo "already present: ${bin_dir}" >&2
  echo "${env_var}=${bin_dir}"
  exit 0
fi

for tool in curl unzip tar; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done

artifact="embedded-postgres-binaries-${classifier}"
url="${BASE_URL}/${artifact}/${PG_VERSION}/${artifact}-${PG_VERSION}.jar"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "fetching ${artifact} ${PG_VERSION}" >&2
curl -fsSL --retry 3 -o "${work}/pg.jar" "$url"
unzip -qo "${work}/pg.jar" -d "${work}/jar"

txz="$(find "${work}/jar" -name '*.txz' -print -quit)"
[ -n "$txz" ] || { echo "no .txz inside ${artifact}" >&2; exit 1; }

mkdir -p "$prefix"
tar -xJf "$txz" -C "$prefix"

[ -x "${bin_dir}/initdb" ] || { echo "no initdb in ${bin_dir}" >&2; exit 1; }
"${bin_dir}/initdb" --version >&2

echo "${env_var}=${bin_dir}"
