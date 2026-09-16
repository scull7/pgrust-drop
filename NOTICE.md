# NOTICE

pgrust-drop is licensed per crate (see `docs/adr/0003-licensing.md`):

- `crates/testkit`, `crates/rinitdb`, `crates/rlibpq`, `crates/rpsql`: MIT
  (root `LICENSE`). These crates are ports of PostgreSQL source code, which is
  distributed under the PostgreSQL License by the PostgreSQL Global Development
  Group. No code from pgrust is copied into them.
- `crates/pgdrop`: GNU Affero General Public License v3.0
  (`crates/pgdrop/LICENSE`). It links pgrust (https://github.com/malisper/pgrust,
  AGPL-3.0) and is therefore a derivative work of it.

PostgreSQL is a trademark of the PostgreSQL Community Association of Canada.
