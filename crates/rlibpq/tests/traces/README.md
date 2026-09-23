# Vendored libpq_pipeline trace files

These nine files are copied **byte for byte** from the PostgreSQL 18.6 source
tree, `src/test/modules/libpq_pipeline/traces/`. They are the oracle of
`src/test/modules/libpq_pipeline/t/001_libpq_pipeline.pl`, which runs each
test with `PQtrace` and `PQTRACE_SUPPRESS_TIMESTAMPS | PQTRACE_REGRESS_MODE`
(`libpq_pipeline.c:2352`) and compares the trace it wrote against the file
here (`001_libpq_pipeline.pl:72`). Do not reformat them: the separators are
tabs, and a changed byte is a failed comparison.

## Provenance

Taken from the `postgres/postgres` repository at tag `REL_18_6`, commit
`724edf9bde9d356724ad384a2e196edc3c9f80f7` (the pristine tag, not the tree
vendored in pgrust — see ADR-0008). Each file's git blob id equals the tag's
(`git hash-object` against `git ls-files -s` at the tag). Their SHA-256:

| file                           | sha256                                                             |
| ------------------------------ | ------------------------------------------------------------------ |
| `disallowed_in_pipeline.trace` | `b779cd6aeaddf5e83964028496abf2050094d38060e19ad06e591fc76724a226` |
| `multi_pipelines.trace`        | `88fa742d1dba202ff915302ac493747988531916c321c9b60a4a7c334f947e81` |
| `nosync.trace`                 | `793b7ffbb2200d57a6c640d652b0ab41c71d117046f3a60b1b2a89336b7c86a9` |
| `pipeline_abort.trace`         | `c3dab26ab7469fd6fbbc966ec60e7bb460427af73ea40d64fb2119f797284cc7` |
| `pipeline_idle.trace`          | `59cce7f0cd25151f3caca63e868651da6dd7790c173d65c16b08e5e80786a95c` |
| `prepared.trace`               | `8c1749dfab4a2be0d491028502410e0da35b875847a724ff3a9f8ccf7192b2cc` |
| `simple_pipeline.trace`        | `b359dd63118b8ea65f3351988745d9bfe150d9cb717c35817d655e1f6a2b1822` |
| `singlerow.trace`              | `9eb9b67fc7840e7396088bb86c917d0d4e5c297233cee706ce09c9b8b4a79ffd` |
| `transaction.trace`            | `3144e788a01bddb45efe355eda6dc11031a40421a203b78085396849e891b069` |

The test `each_trace_is_the_file_postgresql_18_6_ships` in
`crates/rlibpq/src/trace.rs` asserts every file here against these digests.
Recompute them from upstream with

    sha256sum postgresql-18.6/src/test/modules/libpq_pipeline/traces/*.trace

PostgreSQL is distributed under the PostgreSQL License, which permits this
copy; see `NOTICE.md` at the repository root.
