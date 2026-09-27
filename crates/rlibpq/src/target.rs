//! Whether a server that accepted the connection is the kind
//! `target_session_attrs` asked for.
//!
//! Ported from `src/interfaces/libpq/fe-connect.c`: `CONNECTION_CHECK_TARGET`
//! (`:4380`), with the two queries it falls back to when the server did not
//! report the answer at startup — `CONNECTION_CHECK_WRITABLE` (`:4555`) and
//! `CONNECTION_CHECK_STANDBY` (`:4620`) — and the parameter statuses
//! `pqSaveParameterStatus` keeps for it (`fe-exec.c:1158`-`:1203`).
//!
//! Everything here is pure; `Connection::connect` sends the query, reads its
//! answer and moves on to the next host when the check says so.

use crate::hosts::TargetServerType;
use crate::result::{ExecStatus, QueryResult};

/// `PGTernaryBool`, `libpq-int.h:253`-`:259`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgBool {
    /// `PG_BOOL_UNKNOWN`: not reported, not yet asked.
    Unknown,
    /// `PG_BOOL_YES`.
    Yes,
    /// `PG_BOOL_NO`.
    No,
}

impl PgBool {
    /// `fe-exec.c:1196`, `:1201`: exactly `on` is yes, anything else no.
    fn from_parameter(value: Option<&[u8]>) -> Self {
        match value {
            None => PgBool::Unknown,
            Some(b"on") => PgBool::Yes,
            Some(_) => PgBool::No,
        }
    }
}

/// What the check knows of a server: the three `PGconn` fields
/// `CONNECTION_CHECK_TARGET` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerState {
    /// `conn->default_transaction_read_only`.
    pub default_transaction_read_only: PgBool,
    /// `conn->in_hot_standby`.
    pub in_hot_standby: PgBool,
    /// `conn->sversion`: `server_version` in numeric form, 0 when unknown.
    pub sversion: u32,
}

impl ServerState {
    /// The state after startup, from the ParameterStatus values the server
    /// sent, looked up by name.
    pub fn from_parameters<'a>(parameter: impl Fn(&[u8]) -> Option<&'a [u8]>) -> Self {
        ServerState {
            default_transaction_read_only: PgBool::from_parameter(parameter(
                b"default_transaction_read_only",
            )),
            in_hot_standby: PgBool::from_parameter(parameter(b"in_hot_standby")),
            sversion: parameter(b"server_version").map_or(0, sversion),
        }
    }
}

/// `fe-exec.c:1158`-`:1193`: `server_version` as `sscanf("%d.%d.%d")` reads
/// it — `9.6.1` is 90601, `10.1` is 100001, `9.6devel` 90600, `10devel`
/// 100000, and anything without a leading number 0.
#[must_use]
pub fn sversion(value: &[u8]) -> u32 {
    let (fields, rest) = scan_int(value);
    let Some(vmaj) = fields else { return 0 };
    let (vmin, rest) = match rest.strip_prefix(b".") {
        Some(rest) => scan_int(rest),
        None => (None, rest),
    };
    let vrev = match (vmin, rest.strip_prefix(b".")) {
        (Some(_), Some(rest)) => scan_int(rest).0,
        _ => None,
    };
    match (vmin, vrev) {
        // "old style, e.g. 9.6.1"
        (Some(vmin), Some(vrev)) => (100 * vmaj + vmin) * 100 + vrev,
        // "new style, e.g. 10.1"
        (Some(vmin), None) if vmaj >= 10 => 100 * 100 * vmaj + vmin,
        // "old style without minor version, e.g. 9.6devel"
        (Some(vmin), None) => (100 * vmaj + vmin) * 100,
        // "new style without minor version, e.g. 10devel"
        (None, _) => 100 * 100 * vmaj,
    }
}

/// One `%d`: leading white space, an optional sign, then digits. A value
/// that does not fit, or is negative, counts as not read — no server
/// version is either.
fn scan_int(value: &[u8]) -> (Option<u32>, &[u8]) {
    let start = value
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    let rest = &value[start..];
    let rest = rest.strip_prefix(b"+").unwrap_or(rest);
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if digits == 0 {
        return (None, value);
    }
    let number = std::str::from_utf8(&rest[..digits])
        .ok()
        .and_then(|digits| digits.parse().ok());
    (number, &rest[digits..])
}

/// A query `CONNECTION_CHECK_TARGET` sends when the server did not report
/// what it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckQuery {
    /// `SHOW transaction_read_only` (`fe-connect.c:4409`), answered in
    /// `CONNECTION_CHECK_WRITABLE`.
    TransactionReadOnly,
    /// `SELECT pg_catalog.pg_is_in_recovery()` (`:4467`), answered in
    /// `CONNECTION_CHECK_STANDBY`.
    IsInRecovery,
}

impl CheckQuery {
    /// The query as sent.
    #[must_use]
    pub fn sql(self) -> &'static [u8] {
        match self {
            CheckQuery::TransactionReadOnly => b"SHOW transaction_read_only",
            CheckQuery::IsInRecovery => b"SELECT pg_catalog.pg_is_in_recovery()",
        }
    }

    /// `CONNECTION_CHECK_WRITABLE` (`:4572`-`:4601`) or
    /// `CONNECTION_CHECK_STANDBY` (`:4637`-`:4651`): fold the query's first
    /// result into `state`.
    ///
    /// # Errors
    /// [`TargetRejection::QueryFailed`] when that result is not one row of
    /// tuples (`:4604`, `:4654`).
    pub fn answer(
        self,
        result: Option<&QueryResult>,
        state: &mut ServerState,
    ) -> Result<(), TargetRejection> {
        let value = result
            .filter(|result| result.status() == ExecStatus::TuplesOk && result.ntuples() == 1)
            // PQgetvalue gives "" for a NULL.
            .map(|result| result.value(0, 0).unwrap_or_default())
            .ok_or(TargetRejection::QueryFailed(self))?;
        match self {
            // "transaction_read_only = on proves that at least one of
            // default_transaction_read_only and in_hot_standby is on, but we
            // don't actually know which" (:4578).
            CheckQuery::TransactionReadOnly => {
                let answer = if value.starts_with(b"on") {
                    PgBool::Yes
                } else {
                    PgBool::No
                };
                state.default_transaction_read_only = answer;
                state.in_hot_standby = answer;
            }
            CheckQuery::IsInRecovery => {
                state.in_hot_standby = if value.starts_with(b"t") {
                    PgBool::Yes
                } else {
                    PgBool::No
                };
            }
        }
        Ok(())
    }
}

/// Why a server that accepted the connection was left for the next host:
/// the message `libpq_append_conn_error` records for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRejection {
    /// `fe-connect.c:4426` — `read-write` found a read-only session.
    SessionIsReadOnly,
    /// `fe-connect.c:4428` — `read-only` found a read-write session.
    SessionIsNotReadOnly,
    /// `fe-connect.c:4481` — `primary` found a hot standby.
    InHotStandby,
    /// `fe-connect.c:4483` — `standby` or `prefer-standby` found a primary.
    NotInHotStandby,
    /// `fe-connect.c:4608`, `:4658` — the check's own query did not answer.
    QueryFailed(CheckQuery),
}

impl TargetRejection {
    /// The bytes `libpq_append_conn_error` appends, without its newline.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            TargetRejection::SessionIsReadOnly => b"session is read-only".to_vec(),
            TargetRejection::SessionIsNotReadOnly => b"session is not read-only".to_vec(),
            TargetRejection::InHotStandby => b"server is in hot standby mode".to_vec(),
            TargetRejection::NotInHotStandby => b"server is not in hot standby mode".to_vec(),
            // The message names the query without its `pg_catalog.`, as C's
            // format argument does.
            TargetRejection::QueryFailed(CheckQuery::TransactionReadOnly) => {
                b"\"SHOW transaction_read_only\" failed".to_vec()
            }
            TargetRejection::QueryFailed(CheckQuery::IsInRecovery) => {
                b"\"SELECT pg_is_in_recovery()\" failed".to_vec()
            }
        }
    }
}

impl std::fmt::Display for TargetRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for TargetRejection {}

/// What `CONNECTION_CHECK_TARGET` does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetCheck {
    /// "We are open for business!" (`:4517`).
    Accept,
    /// Send the query and check again with its answer.
    Ask(CheckQuery),
    /// Close the connection politely and try the next host — not the next
    /// address of this one (`:4430`-`:4439`, `:4485`-`:4494`).
    Reject(TargetRejection),
}

/// `CONNECTION_CHECK_TARGET`, `fe-connect.c:4380`-`:4519`.
///
/// `second_pass` is `SERVER_TYPE_PREFER_STANDBY_PASS2` (`:3014`): once
/// every host has been tried for a standby, `prefer-standby` goes round
/// again settling for anything, as `any` does.
#[must_use]
pub fn check_target(
    target: TargetServerType,
    second_pass: bool,
    state: &ServerState,
) -> TargetCheck {
    match target {
        TargetServerType::ReadWrite | TargetServerType::ReadOnly => {
            // :4398
            if state.default_transaction_read_only == PgBool::Unknown
                || state.in_hot_standby == PgBool::Unknown
            {
                return TargetCheck::Ask(CheckQuery::TransactionReadOnly);
            }
            // :4417
            let read_only_server = state.default_transaction_read_only == PgBool::Yes
                || state.in_hot_standby == PgBool::Yes;
            match (target, read_only_server) {
                (TargetServerType::ReadWrite, true) => {
                    TargetCheck::Reject(TargetRejection::SessionIsReadOnly)
                }
                (TargetServerType::ReadOnly, false) => {
                    TargetCheck::Reject(TargetRejection::SessionIsNotReadOnly)
                }
                _ => TargetCheck::Accept,
            }
        }
        TargetServerType::PreferStandby if second_pass => TargetCheck::Accept,
        TargetServerType::Primary | TargetServerType::Standby | TargetServerType::PreferStandby => {
            // :4454 — "Servers before 9.0 don't have that function, but by
            // the same token they don't have any standby mode".
            let in_hot_standby = if state.sversion < 90000 {
                PgBool::No
            } else {
                state.in_hot_standby
            };
            match (target, in_hot_standby) {
                (_, PgBool::Unknown) => TargetCheck::Ask(CheckQuery::IsInRecovery),
                (TargetServerType::Primary, PgBool::Yes) => {
                    TargetCheck::Reject(TargetRejection::InHotStandby)
                }
                (TargetServerType::Standby | TargetServerType::PreferStandby, PgBool::No) => {
                    TargetCheck::Reject(TargetRejection::NotInHotStandby)
                }
                _ => TargetCheck::Accept,
            }
        }
        TargetServerType::Any => TargetCheck::Accept,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(read_only: PgBool, hot_standby: PgBool) -> ServerState {
        ServerState {
            default_transaction_read_only: read_only,
            in_hot_standby: hot_standby,
            sversion: 180_006,
        }
    }

    #[test]
    fn server_version_is_read_as_sscanf_reads_it() {
        // fe-exec.c:1167-:1192, one case per branch.
        assert_eq!(sversion(b"9.6.1"), 90601);
        assert_eq!(sversion(b"10.1"), 100_001);
        assert_eq!(sversion(b"18.6"), 180_006);
        assert_eq!(sversion(b"9.6devel"), 90600);
        assert_eq!(sversion(b"10devel"), 100_000);
        assert_eq!(sversion(b"18.6 (Debian 18.6-1.pgdg+1)"), 180_006);
        assert_eq!(sversion(b" 17.2"), 170_002);
        assert_eq!(sversion(b"devel"), 0);
        assert_eq!(sversion(b""), 0);
    }

    #[test]
    fn startup_parameters_fill_the_state_as_pq_save_parameter_status_does() {
        let reported = |name: &[u8]| match name {
            b"default_transaction_read_only" => Some(&b"off"[..]),
            b"in_hot_standby" => Some(&b"on"[..]),
            b"server_version" => Some(&b"18.6"[..]),
            _ => None,
        };
        assert_eq!(
            ServerState::from_parameters(reported),
            state(PgBool::No, PgBool::Yes)
        );
        assert_eq!(
            ServerState::from_parameters(|_| None),
            ServerState {
                default_transaction_read_only: PgBool::Unknown,
                in_hot_standby: PgBool::Unknown,
                sversion: 0,
            }
        );
    }

    #[test]
    fn read_write_and_read_only_look_at_either_flag() {
        use TargetCheck::{Accept, Ask, Reject};
        use TargetServerType::{ReadOnly, ReadWrite};
        let (yes, no, unknown) = (PgBool::Yes, PgBool::No, PgBool::Unknown);
        for (target, read_only, hot_standby, expected) in [
            (ReadWrite, no, no, Accept),
            (
                ReadWrite,
                yes,
                no,
                Reject(TargetRejection::SessionIsReadOnly),
            ),
            (
                ReadWrite,
                no,
                yes,
                Reject(TargetRejection::SessionIsReadOnly),
            ),
            (ReadOnly, no, yes, Accept),
            (ReadOnly, yes, no, Accept),
            (
                ReadOnly,
                no,
                no,
                Reject(TargetRejection::SessionIsNotReadOnly),
            ),
            // :4398 — either one unknown means asking.
            (ReadWrite, unknown, no, Ask(CheckQuery::TransactionReadOnly)),
            (ReadOnly, no, unknown, Ask(CheckQuery::TransactionReadOnly)),
        ] {
            assert_eq!(
                check_target(target, false, &state(read_only, hot_standby)),
                expected,
                "{target:?} {read_only:?} {hot_standby:?}"
            );
        }
    }

    #[test]
    fn primary_standby_and_prefer_standby_look_at_in_hot_standby_alone() {
        use TargetCheck::{Accept, Ask, Reject};
        use TargetServerType::{PreferStandby, Primary, Standby};
        let (yes, no, unknown) = (PgBool::Yes, PgBool::No, PgBool::Unknown);
        for (target, hot_standby, expected) in [
            (Primary, no, Accept),
            (Primary, yes, Reject(TargetRejection::InHotStandby)),
            (Standby, yes, Accept),
            (Standby, no, Reject(TargetRejection::NotInHotStandby)),
            (PreferStandby, yes, Accept),
            (PreferStandby, no, Reject(TargetRejection::NotInHotStandby)),
            (Primary, unknown, Ask(CheckQuery::IsInRecovery)),
            (Standby, unknown, Ask(CheckQuery::IsInRecovery)),
        ] {
            // default_transaction_read_only does not enter into it.
            for read_only in [yes, no, unknown] {
                assert_eq!(
                    check_target(target, false, &state(read_only, hot_standby)),
                    expected,
                    "{target:?} {hot_standby:?}"
                );
            }
        }
    }

    #[test]
    fn a_server_before_9_0_is_never_a_standby() {
        // :4454
        let old = ServerState {
            default_transaction_read_only: PgBool::Unknown,
            in_hot_standby: PgBool::Unknown,
            sversion: 80400,
        };
        assert_eq!(
            check_target(TargetServerType::Primary, false, &old),
            TargetCheck::Accept
        );
        assert_eq!(
            check_target(TargetServerType::Standby, false, &old),
            TargetCheck::Reject(TargetRejection::NotInHotStandby)
        );
    }

    #[test]
    fn any_and_the_second_prefer_standby_pass_accept_anything() {
        for hot_standby in [PgBool::Yes, PgBool::No, PgBool::Unknown] {
            let server = state(PgBool::Unknown, hot_standby);
            assert_eq!(
                check_target(TargetServerType::Any, false, &server),
                TargetCheck::Accept
            );
            assert_eq!(
                check_target(TargetServerType::PreferStandby, true, &server),
                TargetCheck::Accept
            );
        }
    }

    fn rows(values: &[Option<&[u8]>]) -> QueryResult {
        let mut result = QueryResult::new(ExecStatus::TuplesOk);
        for value in values {
            result.push_row(vec![value.map(<[u8]>::to_vec)]);
        }
        result
    }

    #[test]
    fn show_transaction_read_only_answers_both_flags() {
        // :4587 — strncmp(val, "on", 2): a prefix, not the whole value.
        for (value, expected) in [
            (&b"on"[..], PgBool::Yes),
            (b"on-ish", PgBool::Yes),
            (b"off", PgBool::No),
            (b"", PgBool::No),
        ] {
            let mut server = state(PgBool::Unknown, PgBool::Unknown);
            CheckQuery::TransactionReadOnly
                .answer(Some(&rows(&[Some(value)])), &mut server)
                .unwrap();
            assert_eq!(server, state(expected, expected), "{value:?}");
        }
    }

    #[test]
    fn pg_is_in_recovery_answers_in_hot_standby_alone() {
        // :4643 — strncmp(val, "t", 1); a NULL reads as "".
        for (value, expected) in [
            (Some(&b"t"[..]), PgBool::Yes),
            (Some(b"f"), PgBool::No),
            (None, PgBool::No),
        ] {
            let mut server = state(PgBool::Unknown, PgBool::Unknown);
            CheckQuery::IsInRecovery
                .answer(Some(&rows(&[value])), &mut server)
                .unwrap();
            assert_eq!(server, state(PgBool::Unknown, expected), "{value:?}");
        }
    }

    #[test]
    fn an_answer_that_is_not_one_row_fails_the_check() {
        // :4604, :4654
        let failed = QueryResult::new(ExecStatus::FatalError);
        let two = rows(&[Some(b"t"), Some(b"t")]);
        for query in [CheckQuery::TransactionReadOnly, CheckQuery::IsInRecovery] {
            for result in [None, Some(&failed), Some(&two)] {
                let mut server = state(PgBool::Unknown, PgBool::Unknown);
                assert_eq!(
                    query.answer(result, &mut server),
                    Err(TargetRejection::QueryFailed(query))
                );
                assert_eq!(server, state(PgBool::Unknown, PgBool::Unknown));
            }
        }
    }

    #[test]
    fn the_rejections_are_upstreams_messages() {
        for (rejection, expected) in [
            (TargetRejection::SessionIsReadOnly, "session is read-only"),
            (
                TargetRejection::SessionIsNotReadOnly,
                "session is not read-only",
            ),
            (
                TargetRejection::InHotStandby,
                "server is in hot standby mode",
            ),
            (
                TargetRejection::NotInHotStandby,
                "server is not in hot standby mode",
            ),
            (
                TargetRejection::QueryFailed(CheckQuery::TransactionReadOnly),
                "\"SHOW transaction_read_only\" failed",
            ),
            (
                TargetRejection::QueryFailed(CheckQuery::IsInRecovery),
                "\"SELECT pg_is_in_recovery()\" failed",
            ),
        ] {
            assert_eq!(rejection.to_string(), expected);
        }
    }
}
