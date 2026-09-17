# Deliberate divergences from upstream

Each entry: what differs, why, and the test that pins the divergent behaviour.

| tool      | divergence                                                                                         | reason                                  | pinned by                          |
| --------- | -------------------------------------------------------------------------------------------------- | --------------------------------------- | ---------------------------------- |
| rinitdb   | Bad-option error text is usage-rs's (clap-shaped) and exit status is 2, not glibc getopt's and 1. | ADR-0004: all CLIs on usage-rs           | `crates/rinitdb/tests/t_001_initdb.rs::program_options_handling_ok` |
| rinitdb   | Long-option unique-prefix abbreviation (`--no-syn` for `--no-sync`) is rejected.                   | usage-rs has no getopt abbreviation      | `crates/rinitdb/src/cli.rs` unit test `abbreviation_is_rejected` |
| rinitdb   | Locale names are validated by the host libc, so `--locale=xx_ZZ.UTF-8` exits 1 on glibc and 0 on musl (musl's `setlocale` accepts any name and falls back to C). Expectations are per-lane, never hardcoded. | ADR-0007: musl and Darwin are primary targets; the reference must share our libc | `crates/rinitdb/tests/t_001_initdb.rs` (locale block, NAT-378) |
| rinitdb   | The embedded template carries no locale; `datcollate`/`datctype`/`datlocprovider`/`datcollversion` are stamped at run time from the host's libc.       | ADR-0002: a baked libc-versioned locale warns on every connection from a different libc *or glibc version* | ADR-0002 portability table; gate added with the `--single` phase |
