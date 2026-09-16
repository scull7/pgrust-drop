# Deliberate divergences from upstream

Each entry: what differs, why, and the test that pins the divergent behaviour.

| tool      | divergence                                                                                         | reason                                  | pinned by                          |
| --------- | -------------------------------------------------------------------------------------------------- | --------------------------------------- | ---------------------------------- |
| rinitdb   | Bad-option error text is usage-rs's (clap-shaped) and exit status is 2, not glibc getopt's and 1. | ADR-0004: all CLIs on usage-rs           | `crates/rinitdb/tests/t_001_initdb.rs::program_options_handling_ok` |
| rinitdb   | Long-option unique-prefix abbreviation (`--no-syn` for `--no-sync`) is rejected.                   | usage-rs has no getopt abbreviation      | `crates/rinitdb/src/cli.rs` unit test `abbreviation_is_rejected` |
