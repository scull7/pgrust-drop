#!/usr/bin/env bash
# Bump the pgrust pin (ADR-0001: a rev-pinned Cargo git dependency).
#
# Usage:
#   scripts/pgrust-rev.sh [--repo OWNER/pgrust] [REF]
#   scripts/pgrust-rev.sh --show
#
#   REF     a branch, tag or full commit id; default: `musl-build` on
#           scull7/pgrust, `main` on any other repository
#   --repo  the repository to pin; default: the one pinned now. Pass
#           malisper/pgrust to switch back to upstream once it carries the
#           musl fixes (ADR-0001, amendment 2026-09-23).
#   --show  print the pinned repository and rev, and exit
#
# Resolves REF to a commit, checks the commit carries what this repo depends on
# (below), rewrites the `main_main` line in the workspace Cargo.toml and
# refreshes Cargo.lock. Then run the checks, including a musl build.
#
# It records nothing: a bump is recorded on its Linear issue (project
# pgrust-drop) -- the new rev, why that rev, the checks run. progress.md is
# retired (AGENTS.md, "Change hygiene").
set -euo pipefail

MANIFEST="$(cd "$(dirname "$0")/.." && pwd)/Cargo.toml"
readonly MANIFEST

# Paths the pinned rev must contain. ADR-0002's run-time locale stamping
# (NAT-383) needs pgrust's collation-import port: import.rs plus builtins 3445
# (pg_import_system_collations) and 3448 (pg_collation_actual_version), which
# builtins.rs registers.
readonly REQUIRED_PATHS=(
  crates/backend/main/main_main/Cargo.toml
  crates/backend/commands/collationcmds/src/import.rs
  crates/backend/commands/collationcmds/src/builtins.rs
)
readonly REQUIRED_FOIDS=(3445 3448)

readonly PIN_RE='^main_main = { git = "https://github.com/\([^"]*\)", rev = "\([0-9a-f]\{40\}\)" }$'

pinned() { sed -n "s|$PIN_RE|\\$1|p" "$MANIFEST"; }

repo=""
ref=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h | --help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    --show) echo "$(pinned 1) $(pinned 2)"; exit 0 ;;
    --repo) repo="$2"; shift 2 ;;
    -*) echo "pgrust-rev: unknown option $1" >&2; exit 2 ;;
    *) ref="$1"; shift ;;
  esac
done

old_repo="$(pinned 1)"
old_rev="$(pinned 2)"
if [ -z "$old_repo" ] || [ -z "$old_rev" ]; then
  echo "pgrust-rev: no 'main_main = { git = …, rev = … }' line in $MANIFEST" >&2
  exit 1
fi
repo="${repo:-$old_repo}"
if [ -z "$ref" ]; then
  if [ "$repo" = scull7/pgrust ]; then ref=musl-build; else ref=main; fi
fi

if [[ "$ref" =~ ^[0-9a-f]{40}$ ]]; then
  new_rev="$ref"
else
  # A branch or tag; for an annotated tag the peeled (^{}) line, listed last, wins.
  new_rev="$(git ls-remote "https://github.com/$repo" "$ref" "$ref^{}" | awk 'END { print $1 }')"
  if [ -z "$new_rev" ]; then
    echo "pgrust-rev: $ref does not name a ref on github.com/$repo" >&2
    exit 1
  fi
fi

raw="https://raw.githubusercontent.com/$repo/$new_rev"
for path in "${REQUIRED_PATHS[@]}"; do
  if ! curl -fsSI --retry 3 -o /dev/null "$raw/$path"; then
    echo "pgrust-rev: $repo@$new_rev lacks $path; not pinning it" >&2
    exit 1
  fi
done
builtins="$(curl -fsS --retry 3 "$raw/crates/backend/commands/collationcmds/src/builtins.rs")"
for foid in "${REQUIRED_FOIDS[@]}"; do
  if ! tr -d ' ' <<<"$builtins" | grep -q "foid:$foid,"; then
    echo "pgrust-rev: $repo@$new_rev does not register builtin $foid; not pinning it" >&2
    exit 1
  fi
done

if [ "$repo" = "$old_repo" ] && [ "$new_rev" = "$old_rev" ]; then
  echo "pgrust-rev: already pinned to $repo@$new_rev"
  exit 0
fi

sed -i.bak "s|^main_main = { git = \"https://github.com/$old_repo\", rev = \"$old_rev\" }\$|main_main = { git = \"https://github.com/$repo\", rev = \"$new_rev\" }|" "$MANIFEST"
rm -f "$MANIFEST.bak"
if [ "$(pinned 1) $(pinned 2)" != "$repo $new_rev" ]; then
  echo "pgrust-rev: failed to rewrite the pin in $MANIFEST" >&2
  exit 1
fi
# --workspace re-resolves the changed git source and nothing else in the lock.
cargo update --manifest-path "$MANIFEST" --workspace

echo "pgrust-rev: $old_repo@$old_rev -> $repo@$new_rev"
echo "Update the comment above the pin in Cargo.toml if the reason for it changed,"
echo "and record the bump (rev, why this rev, checks run) on its Linear issue."
