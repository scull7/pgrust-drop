#!/usr/bin/env bash
# The MIT/AGPL wall (ADR-0003), checked mechanically.
#
# pgrust is AGPL-3.0. Only the AGPL crate `pgdrop` may depend on it, directly or
# transitively; the MIT crates (testkit, rinitdb, rlibpq, rpsql) must not
# reach a single pgrust crate through any normal, build or dev edge, on any
# target, under any feature. The owner's approval of pgrust's dependency tree
# (2026-09-23) is for pgdrop only.
#
# A pgrust crate is recognised by its source: a git URL whose repository is
# named `pgrust` (malisper's or scull7's fork). As a guard against this check
# passing vacuously -- a changed `cargo tree` format, a renamed source --
# pgdrop's tree must contain pgrust crates.
#
# Usage: scripts/check-license-wall.sh   (exit 0: wall holds; 1: breached)
set -euo pipefail

readonly MIT_CRATES=(testkit rinitdb rlibpq rpsql)
readonly PGRUST_SOURCE='github\.com/[^/]*/pgrust[?#]'

# Capture first, so a failing `cargo tree` fails the script instead of feeding
# an empty tree to grep.
tree_of() {
  cargo tree --locked --all-features -p "$1" -e normal,build,dev --target all \
    --prefix none --format '{p}'
}

status=0
for crate in "${MIT_CRATES[@]}"; do
  tree="$(tree_of "$crate")"
  if grep -E "$PGRUST_SOURCE" <<<"$tree" >/dev/null; then
    echo "license wall breached: $crate (MIT) depends on pgrust (AGPL-3.0):" >&2
    # sed, not head: head exits early and pipefail would report its SIGPIPE.
    grep -E "$PGRUST_SOURCE" <<<"$tree" | sed 's/ (\*)$//' | sort -u | sed -n '1,20p' >&2
    status=1
  else
    echo "ok: $crate reaches no pgrust crate"
  fi
done

tree="$(tree_of pgdrop)"
if ! grep -E "$PGRUST_SOURCE" <<<"$tree" >/dev/null; then
  echo "check-license-wall: pgdrop's tree shows no pgrust crate; the pattern" \
    "'$PGRUST_SOURCE' no longer matches, so this check proves nothing" >&2
  status=1
fi

exit "$status"
