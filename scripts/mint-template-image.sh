#!/usr/bin/env bash
# Mint the committed template image, crates/rinitdb/image/template.img, and its
# provenance manifest (NAT-381, ADR-0002).
#
# The image is a committed blob, so no build needs PostgreSQL; this script is
# how it is (re)made. Run it on the musl lane, with PostgreSQL 18.6's initdb:
# the pack step (crates/rinitdb/examples/mint_template.rs) refuses any other
# release, and refuses an initdb or postgres whose ELF dynamic loader is not
# musl's, because on musl no libc release is baked into pg_collation.
# It mints twice and refuses unless both mints pack to the same bytes.
#
# Usage:
#   scripts/mint-template-image.sh [INITDB]
#
# INITDB defaults to $PGDROP_REF_BIN_MUSL/initdb (on Alpine,
# /usr/libexec/postgresql18/initdb from the postgresql18 package).
#
# Re-minting changes the image only if the catalogs changed. The host's
# `locale -a` and libicu decide which collation rows initdb imports, so the
# manifest's sha256 moves when those do too; commit the image and manifest
# together, and say why on the Linear issue.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
initdb="${1:-${PGDROP_REF_BIN_MUSL:?set PGDROP_REF_BIN_MUSL or pass the path to initdb}/initdb}"

cd "$repo"
cargo run --locked --quiet -p rinitdb --example mint_template -- \
  "$initdb" crates/rinitdb/image
