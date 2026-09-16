# Deliberate divergences from upstream

Each entry: what differs, why, and the test that pins the divergent behaviour.

| tool      | divergence                                                                                         | reason                                  | pinned by                          |
| --------- | -------------------------------------------------------------------------------------------------- | --------------------------------------- | ---------------------------------- |
| rinitdb   | Bad-option error text is usage-rs's (clap-shaped) and exit status is 2, not glibc getopt's and 1. | ADR-0004: all CLIs on usage-rs           | `crates/rinitdb/tests/t_001_initdb.rs::program_options_handling_ok` |
| rinitdb   | Long-option unique-prefix abbreviation (`--no-syn` for `--no-sync`) is rejected.                   | usage-rs has no getopt abbreviation      | `crates/rinitdb/src/cli.rs` unit test `abbreviation_is_rejected` |
| gate      | A gate compares the two outputs after three named normalizations (`Time: …` line, `PID n`, system identifier), not as raw bytes. | Each value is nondeterministic per run or per cluster; each normalizer cites the upstream `printf` it rewrites and nothing else is normalized. | `crates/testkit/src/normalize.rs` unit tests (`timing_*`, `pid_*`, `system_identifier_*`) plus `gate::tests::a_normalizer_does_not_hide_a_real_difference_on_the_same_line` |
| gate      | A failing gate renders its `diff -U3` in process instead of shelling out to diff(1) as pg_regress does (`src/test/regress/pg_regress.c:1537`). | Diagnostics only — the pass/fail verdict is a byte comparison, never the rendering — and it keeps the gates free of an external diff(1). | `crates/testkit/src/diff.rs` unit tests (`a_changed_line_is_shown_with_three_lines_of_context`, `a_missing_final_newline_is_flagged`) |
