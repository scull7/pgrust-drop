//! The server's check on the names in `postgresql.conf`, which C `initdb`
//! reaches when its bootstrap backend reads the file it has just written.
//!
//! `ProcessConfigFileInternal` (`src/backend/utils/misc/guc.c:282`) looks up
//! every name the file assigns. A name that is neither a parameter of this
//! build nor a valid custom (dotted) name is reported, one `LOG` line each
//! (`guc.c:428`), and then the whole file is refused (`guc.c:611`). Values are
//! only judged once every name has passed, so a file with an unknown name is
//! refused on its names alone. ADR-0002 replaces the bootstrap backend with the
//! embedded template, so nothing in `rinitdb` would read the file; this module
//! is that first step of the read, ported, and `crate::run` reports it where C
//! does: after the configuration files are written, with the data directory
//! then removed.
//!
//! What is not ported, and so not refused (`docs/divergences.md`): a line
//! whose first token is not a name (`guc-file.l`'s syntax errors), the
//! `include` directives, and every value.
//!
//! Pure throughout.

/// Every parameter a standard PostgreSQL 18.6 server knows, in the order of
/// the five `ConfigureNames*` arrays of `src/backend/utils/misc/guc_tables.c`.
///
/// "Standard" is a build with none of the developer symbols defined, which is
/// what `pg_config_manual.h` leaves by default and what every reference lane
/// installs. The 13 entries under an `#ifdef` inside the arrays are therefore
/// left out, as such a build leaves them out: `debug_copy_parse_plan_trees`,
/// `debug_write_read_parse_plan_trees` and `debug_raw_expression_coverage_test`
/// (`DEBUG_NODE_TESTS_ENABLED`, `guc_tables.c:1345`), `log_btree_build_stats`
/// (`BTREE_BUILD_STATS`, `:1470`), `trace_locks`, `trace_userlocks`,
/// `trace_lwlocks` and `debug_deadlocks` (`LOCK_DEBUG`, `:1562`),
/// `trace_syncscan` (`TRACE_SYNCSCAN`, `:1771`), `optimize_bounded_sort`
/// (`DEBUG_BOUNDED_SORT`, `:1785`), `wal_debug` (`WAL_DEBUG`, `:1800`),
/// `trace_lock_oidmin` and `trace_lock_table` (`LOCK_DEBUG`, `:2716`).
pub const PARAMETER_NAMES: [&str; 405] = [
    // ConfigureNamesBool[], guc_tables.c:800
    "enable_seqscan",
    "enable_indexscan",
    "enable_indexonlyscan",
    "enable_bitmapscan",
    "enable_tidscan",
    "enable_sort",
    "enable_incremental_sort",
    "enable_hashagg",
    "enable_material",
    "enable_memoize",
    "enable_nestloop",
    "enable_mergejoin",
    "enable_hashjoin",
    "enable_gathermerge",
    "enable_partitionwise_join",
    "enable_partitionwise_aggregate",
    "enable_parallel_append",
    "enable_parallel_hash",
    "enable_partition_pruning",
    "enable_presorted_aggregate",
    "enable_async_append",
    "enable_self_join_elimination",
    "enable_group_by_reordering",
    "enable_distinct_reordering",
    "geqo",
    "is_superuser",
    "allow_alter_system",
    "bonjour",
    "track_commit_timestamp",
    "ssl",
    "ssl_passphrase_command_supports_reload",
    "ssl_prefer_server_ciphers",
    "fsync",
    "ignore_checksum_failure",
    "zero_damaged_pages",
    "ignore_invalid_pages",
    "full_page_writes",
    "wal_log_hints",
    "wal_init_zero",
    "wal_recycle",
    "log_checkpoints",
    "trace_connection_negotiation",
    "log_disconnections",
    "log_replication_commands",
    "debug_assertions",
    "exit_on_error",
    "restart_after_crash",
    "remove_temp_files_after_crash",
    "send_abort_for_crash",
    "send_abort_for_kill",
    "log_duration",
    "debug_print_parse",
    "debug_print_rewritten",
    "debug_print_plan",
    "debug_pretty_print",
    "log_parser_stats",
    "log_planner_stats",
    "log_executor_stats",
    "log_statement_stats",
    "track_activities",
    "track_counts",
    "track_cost_delay_timing",
    "track_io_timing",
    "track_wal_io_timing",
    "update_process_title",
    "autovacuum",
    "trace_notify",
    "log_lock_waits",
    "log_lock_failures",
    "log_recovery_conflict_waits",
    "log_hostname",
    "transform_null_equals",
    "default_transaction_read_only",
    "transaction_read_only",
    "default_transaction_deferrable",
    "transaction_deferrable",
    "row_security",
    "check_function_bodies",
    "array_nulls",
    "default_with_oids",
    "logging_collector",
    "log_truncate_on_rotation",
    "trace_sort",
    "integer_datetimes",
    "krb_caseins_users",
    "gss_accept_delegation",
    "escape_string_warning",
    "standard_conforming_strings",
    "synchronize_seqscans",
    "recovery_target_inclusive",
    "summarize_wal",
    "hot_standby",
    "hot_standby_feedback",
    "in_hot_standby",
    "allow_system_table_mods",
    "ignore_system_indexes",
    "allow_in_place_tablespaces",
    "lo_compat_privileges",
    "quote_all_identifiers",
    "data_checksums",
    "syslog_sequence_numbers",
    "syslog_split_messages",
    "parallel_leader_participation",
    "jit",
    "jit_debugging_support",
    "jit_dump_bitcode",
    "jit_expressions",
    "jit_profiling_support",
    "jit_tuple_deforming",
    "data_sync_retry",
    "wal_receiver_create_temp_slot",
    "event_triggers",
    "sync_replication_slots",
    "md5_password_warnings",
    "vacuum_truncate",
    // ConfigureNamesInt[], guc_tables.c:2163
    "archive_timeout",
    "post_auth_delay",
    "default_statistics_target",
    "from_collapse_limit",
    "join_collapse_limit",
    "geqo_threshold",
    "geqo_effort",
    "geqo_pool_size",
    "geqo_generations",
    "deadlock_timeout",
    "max_standby_archive_delay",
    "max_standby_streaming_delay",
    "recovery_min_apply_delay",
    "wal_receiver_status_interval",
    "wal_receiver_timeout",
    "max_connections",
    "superuser_reserved_connections",
    "reserved_connections",
    "min_dynamic_shared_memory",
    "shared_buffers",
    "vacuum_buffer_usage_limit",
    "shared_memory_size",
    "shared_memory_size_in_huge_pages",
    "num_os_semaphores",
    "commit_timestamp_buffers",
    "multixact_member_buffers",
    "multixact_offset_buffers",
    "notify_buffers",
    "serializable_buffers",
    "subtransaction_buffers",
    "transaction_buffers",
    "temp_buffers",
    "port",
    "unix_socket_permissions",
    "log_file_mode",
    "data_directory_mode",
    "work_mem",
    "maintenance_work_mem",
    "logical_decoding_work_mem",
    "max_stack_depth",
    "temp_file_limit",
    "vacuum_cost_page_hit",
    "vacuum_cost_page_miss",
    "vacuum_cost_page_dirty",
    "vacuum_cost_limit",
    "autovacuum_vacuum_cost_limit",
    "max_files_per_process",
    "max_prepared_transactions",
    "statement_timeout",
    "lock_timeout",
    "idle_in_transaction_session_timeout",
    "transaction_timeout",
    "idle_session_timeout",
    "vacuum_freeze_min_age",
    "vacuum_freeze_table_age",
    "vacuum_multixact_freeze_min_age",
    "vacuum_multixact_freeze_table_age",
    "vacuum_failsafe_age",
    "vacuum_multixact_failsafe_age",
    "max_locks_per_transaction",
    "max_pred_locks_per_transaction",
    "max_pred_locks_per_relation",
    "max_pred_locks_per_page",
    "authentication_timeout",
    "pre_auth_delay",
    "max_notify_queue_pages",
    "wal_decode_buffer_size",
    "wal_keep_size",
    "min_wal_size",
    "max_wal_size",
    "checkpoint_timeout",
    "checkpoint_warning",
    "checkpoint_flush_after",
    "wal_buffers",
    "wal_writer_delay",
    "wal_writer_flush_after",
    "wal_skip_threshold",
    "max_wal_senders",
    "max_replication_slots",
    "max_slot_wal_keep_size",
    "wal_sender_timeout",
    "idle_replication_slot_timeout",
    "commit_delay",
    "commit_siblings",
    "extra_float_digits",
    "log_min_duration_sample",
    "log_min_duration_statement",
    "log_autovacuum_min_duration",
    "log_parameter_max_length",
    "log_parameter_max_length_on_error",
    "bgwriter_delay",
    "bgwriter_lru_maxpages",
    "bgwriter_flush_after",
    "effective_io_concurrency",
    "maintenance_io_concurrency",
    "io_max_combine_limit",
    "io_combine_limit",
    "io_max_concurrency",
    "io_workers",
    "backend_flush_after",
    "max_worker_processes",
    "max_logical_replication_workers",
    "max_sync_workers_per_subscription",
    "max_parallel_apply_workers_per_subscription",
    "max_active_replication_origins",
    "log_rotation_age",
    "log_rotation_size",
    "max_function_args",
    "max_index_keys",
    "max_identifier_length",
    "block_size",
    "segment_size",
    "wal_block_size",
    "wal_retrieve_retry_interval",
    "wal_segment_size",
    "wal_summary_keep_time",
    "autovacuum_naptime",
    "autovacuum_vacuum_threshold",
    "autovacuum_vacuum_max_threshold",
    "autovacuum_vacuum_insert_threshold",
    "autovacuum_analyze_threshold",
    "autovacuum_freeze_max_age",
    "autovacuum_multixact_freeze_max_age",
    "autovacuum_worker_slots",
    "autovacuum_max_workers",
    "max_parallel_maintenance_workers",
    "max_parallel_workers_per_gather",
    "max_parallel_workers",
    "autovacuum_work_mem",
    "tcp_keepalives_idle",
    "tcp_keepalives_interval",
    "ssl_renegotiation_limit",
    "tcp_keepalives_count",
    "gin_fuzzy_search_limit",
    "effective_cache_size",
    "min_parallel_table_scan_size",
    "min_parallel_index_scan_size",
    "server_version_num",
    "log_temp_files",
    "track_activity_query_size",
    "gin_pending_list_limit",
    "tcp_user_timeout",
    "huge_page_size",
    "debug_discard_caches",
    "client_connection_check_interval",
    "log_startup_progress_interval",
    "scram_iterations",
    // ConfigureNamesReal[], guc_tables.c:3880
    "seq_page_cost",
    "random_page_cost",
    "cpu_tuple_cost",
    "cpu_index_tuple_cost",
    "cpu_operator_cost",
    "parallel_tuple_cost",
    "parallel_setup_cost",
    "jit_above_cost",
    "jit_optimize_above_cost",
    "jit_inline_above_cost",
    "cursor_tuple_fraction",
    "recursive_worktable_factor",
    "geqo_selection_bias",
    "geqo_seed",
    "hash_mem_multiplier",
    "bgwriter_lru_multiplier",
    "seed",
    "vacuum_cost_delay",
    "autovacuum_vacuum_cost_delay",
    "autovacuum_vacuum_scale_factor",
    "autovacuum_vacuum_insert_scale_factor",
    "autovacuum_analyze_scale_factor",
    "checkpoint_completion_target",
    "log_statement_sample_rate",
    "log_transaction_sample_rate",
    "vacuum_max_eager_freeze_failure_rate",
    // ConfigureNamesString[], guc_tables.c:4171
    "archive_command",
    "archive_library",
    "restore_command",
    "archive_cleanup_command",
    "recovery_end_command",
    "recovery_target_timeline",
    "recovery_target",
    "recovery_target_xid",
    "recovery_target_time",
    "recovery_target_name",
    "recovery_target_lsn",
    "primary_conninfo",
    "primary_slot_name",
    "client_encoding",
    "log_line_prefix",
    "log_timezone",
    "DateStyle",
    "default_table_access_method",
    "default_tablespace",
    "temp_tablespaces",
    "createrole_self_grant",
    "dynamic_library_path",
    "extension_control_path",
    "krb_server_keyfile",
    "bonjour_name",
    "lc_messages",
    "lc_monetary",
    "lc_numeric",
    "lc_time",
    "session_preload_libraries",
    "shared_preload_libraries",
    "local_preload_libraries",
    "search_path",
    "server_encoding",
    "server_version",
    "role",
    "session_authorization",
    "log_destination",
    "log_directory",
    "log_filename",
    "syslog_ident",
    "event_source",
    "TimeZone",
    "timezone_abbreviations",
    "unix_socket_group",
    "unix_socket_directories",
    "listen_addresses",
    "data_directory",
    "config_file",
    "hba_file",
    "ident_file",
    "external_pid_file",
    "ssl_library",
    "ssl_cert_file",
    "ssl_key_file",
    "ssl_ca_file",
    "ssl_crl_file",
    "ssl_crl_dir",
    "synchronous_standby_names",
    "default_text_search_config",
    "ssl_tls13_ciphers",
    "ssl_ciphers",
    "ssl_groups",
    "ssl_dh_params_file",
    "ssl_passphrase_command",
    "application_name",
    "cluster_name",
    "wal_consistency_checking",
    "jit_provider",
    "backtrace_functions",
    "debug_io_direct",
    "synchronized_standby_slots",
    "restrict_nonsystem_relation_kind",
    "oauth_validator_libraries",
    "output_plugin_libraries",
    "log_connections",
    // ConfigureNamesEnum[], guc_tables.c:5017
    "backslash_quote",
    "bytea_output",
    "client_min_messages",
    "compute_query_id",
    "constraint_exclusion",
    "default_toast_compression",
    "default_transaction_isolation",
    "transaction_isolation",
    "IntervalStyle",
    "icu_validation_level",
    "log_error_verbosity",
    "log_min_messages",
    "log_min_error_statement",
    "log_statement",
    "syslog_facility",
    "session_replication_role",
    "synchronous_commit",
    "archive_mode",
    "recovery_target_action",
    "track_functions",
    "stats_fetch_consistency",
    "wal_compression",
    "wal_level",
    "dynamic_shared_memory_type",
    "shared_memory_type",
    "file_copy_method",
    "file_extend_method",
    "wal_sync_method",
    "xmlbinary",
    "xmloption",
    "huge_pages",
    "huge_pages_status",
    "recovery_prefetch",
    "debug_parallel_query",
    "password_encryption",
    "plan_cache_mode",
    "ssl_min_protocol_version",
    "ssl_max_protocol_version",
    "recovery_init_sync_method",
    "debug_logical_replication_streaming",
    "io_method",
];

/// `map_old_guc_names` (`guc.c:190`): obsolete names still accepted, each as
/// the parameter it became.
pub const OLD_PARAMETER_NAMES: [(&str, &str); 3] = [
    ("sort_mem", "work_mem"),
    ("vacuum_mem", "maintenance_work_mem"),
    ("ssl_ecdh_curve", "ssl_groups"),
];

/// The directives `ParseConfigFp` handles itself before any name is looked up
/// (`guc-file.l:437`, `:452`, `:467`). They name no parameter.
const DIRECTIVES: [&str; 3] = ["include_dir", "include_if_exists", "include"];

/// `guc_name_compare` (`guc.c:1300`): equal after an ASCII-only downcasing.
#[must_use]
pub fn guc_name_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// `find_option` (`guc.c:1235`) without placeholders: whether `name` is a
/// parameter of this build, directly or through [`OLD_PARAMETER_NAMES`].
#[must_use]
pub fn is_parameter(name: &str) -> bool {
    PARAMETER_NAMES.iter().any(|known| guc_name_eq(known, name))
        || OLD_PARAMETER_NAMES
            .iter()
            .any(|(old, _)| guc_name_eq(old, name))
}

/// `valid_custom_variable_name` (`guc.c:1076`): two or more identifiers
/// joined by dots, an identifier being a letter, `_` or a high-bit byte,
/// then those or digits or `$`.
#[must_use]
pub fn valid_custom_variable_name(name: &str) -> bool {
    let mut saw_sep = false;
    let mut name_start = true;
    for &byte in name.as_bytes() {
        if byte == b'.' {
            if name_start {
                return false; // empty name component
            }
            saw_sep = true;
            name_start = true;
        } else if byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80 {
            name_start = false;
        } else if !name_start && (byte.is_ascii_digit() || byte == b'$') {
            // okay as non-first character
        } else {
            return false;
        }
    }
    !name_start && saw_sep
}

/// One `unrecognized configuration parameter` report (`guc.c:428`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrecognized {
    /// The name as the file spells it.
    pub name: String,
    /// 1-based, as `ConfigFileLineno` counts.
    pub line: usize,
}

/// Every name in `conf` the server would report as unrecognized, in file
/// order: `ParseConfigFp` (`guc-file.l:350`) for the names, then the lookup
/// loop of `ProcessConfigFileInternal` (`guc.c:395`-`:434`).
#[must_use]
pub fn unrecognized_parameters(conf: &str) -> Vec<Unrecognized> {
    conf.split('\n')
        .enumerate()
        .filter_map(|(index, line)| {
            let name = option_name(line)?;
            let known = DIRECTIVES
                .iter()
                .any(|directive| guc_name_eq(directive, name))
                || is_parameter(name)
                || valid_custom_variable_name(name);
            (!known).then(|| Unrecognized {
                name: name.to_owned(),
                line: index + 1,
            })
        })
        .collect()
}

/// The name a configuration-file line assigns, when its first token is one:
/// `guc-file.l`'s `ID` or `QUALIFIED_ID` (`:84`-`:85`), after the leading
/// `[ \t\r]` the scanner eats (`:94`).
///
/// `None` for a blank or comment line, and for a line whose first token is
/// anything else. That includes a name the scanner reads as a longer
/// `UNQUOTED_STRING` (`:87`), such as `a-b` or `a.b.c`: flex takes the longest
/// match, so the line is a syntax error there, which this port does not report.
fn option_name(line: &str) -> Option<&str> {
    let line = line.trim_start_matches([' ', '\t', '\r']);
    let bytes = line.as_bytes();
    let letter = |byte: u8| byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80;
    let letter_or_digit = |byte: u8| letter(byte) || byte.is_ascii_digit();
    let unquoted =
        |byte: u8| letter_or_digit(byte) || matches!(byte, b'-' | b'.' | b'_' | b':' | b'/');

    if !bytes.first().copied().is_some_and(letter) {
        return None;
    }
    let token_end = bytes
        .iter()
        .position(|&byte| !unquoted(byte))
        .unwrap_or(bytes.len());
    let token = &line[..token_end];
    let is_id = |part: &str| {
        part.as_bytes().first().copied().is_some_and(letter) && part.bytes().all(letter_or_digit)
    };
    let name = match token.split_once('.') {
        None => is_id(token),
        Some((head, rest)) => is_id(head) && is_id(rest),
    };
    name.then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_matched_whatever_its_case_and_old_names_still_count() {
        assert!(is_parameter("work_mem"));
        assert!(is_parameter("WORK_MEM"));
        assert!(is_parameter("DateStyle"));
        assert!(is_parameter("datestyle"));
        assert!(is_parameter("Sort_Mem"));
        assert!(!is_parameter("foo"));
        // Compiled out of a standard build (guc_tables.c:1562, LOCK_DEBUG).
        assert!(!is_parameter("trace_locks"));
    }

    #[test]
    fn the_table_holds_each_name_once() {
        let mut seen = std::collections::BTreeSet::new();
        for name in PARAMETER_NAMES {
            assert!(seen.insert(name.to_ascii_lowercase()), "{name} twice");
        }
        for (old, new) in OLD_PARAMETER_NAMES {
            assert!(!seen.contains(old), "{old} is an old name, not a parameter");
            assert!(seen.contains(new), "{old} maps to {new}, which must exist");
        }
    }

    #[test]
    fn a_custom_name_is_two_or_more_identifiers_joined_by_dots() {
        for good in [
            "x.y",
            "plpgsql.variable_conflict",
            "a.b.c",
            "a_1.b$2",
            "é.x",
        ] {
            assert!(valid_custom_variable_name(good), "{good}");
        }
        for bad in [
            "foo", "", ".", "x.", ".x", "x..y", "1x.y", "x.1y", "x-y.z", "x.$y",
        ] {
            assert!(!valid_custom_variable_name(bad), "{bad}");
        }
    }

    #[test]
    fn the_file_is_read_as_the_scanner_reads_it() {
        let conf = "# comment\n\
                    \n\
                    work_mem = 4MB\n\
                    \t Foo = bar\t# trailing comment\n\
                    #bar = 1\n\
                    x.y = 1\n\
                    include_dir 'conf.d'\n\
                    baz 2\n\
                    a-b = 1\n\
                    a.b.c = 1\n\
                    1abc = 1\n";
        assert_eq!(
            unrecognized_parameters(conf),
            vec![
                Unrecognized {
                    name: "Foo".to_owned(),
                    line: 4,
                },
                Unrecognized {
                    name: "baz".to_owned(),
                    line: 8,
                },
            ]
        );
    }

    #[test]
    fn the_rendered_sample_assigns_no_unknown_name() {
        let rendered = crate::conf::render_postgresql_conf(
            crate::conf::POSTGRESQL_CONF_SAMPLE,
            &crate::conf::Settings::default(),
        );
        assert_eq!(unrecognized_parameters(&rendered), Vec::new());
    }

    #[test]
    fn every_name_the_sample_mentions_is_in_the_table() {
        // The sample lists its parameters commented out; uncommenting any of
        // them must give a file the server accepts.
        let mut checked = 0;
        for line in crate::conf::POSTGRESQL_CONF_SAMPLE.lines() {
            let Some(body) = line.strip_prefix('#') else {
                continue;
            };
            let Some(name) = option_name(body) else {
                continue;
            };
            if body[name.len()..].starts_with(" = ") && !DIRECTIVES.contains(&name) {
                assert!(is_parameter(name), "{name}");
                checked += 1;
            }
        }
        assert!(checked > 300, "only {checked} assignments found");
    }
}
