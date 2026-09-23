//! The query-execution state of a `PGconn`, as a pure state machine: the
//! command queue, `asyncStatus`, `pipelineStatus`, the result being built,
//! and the row mode — everything `PQgetResult` and its callers decide with,
//! and nothing that touches a socket.
//!
//! Ported from `src/interfaces/libpq/fe-exec.c` — `PQsendQueryStart`
//! (`:1690`), `pqAppendCmdQueueEntry` (`:1356`), `PQgetResult`'s dispatch
//! (`:2140`), `PQenterPipelineMode` (`:3073`), `PQexitPipelineMode`
//! (`:3104`), `pqCommandQueueAdvance` (`:3173`), `pqPipelineProcessQueue`
//! (`:3211`), `pqPipelineSyncInternal` (`:3325`), `PQsendFlushRequest`
//! (`:3402`), `canChangeResultMode` (`:1942`), `pqRowProcessor` (`:1223`) —
//! and from `fe-protocol3.c`'s `pqParseInput3` (`:71`), whose per-message
//! rules are [`PipelineState::admit`] and [`PipelineState::apply`].
//!
//! `crate::Connection` owns one of these and does the actions around it:
//! it encodes and writes what a command sends, reads bytes, frames them,
//! asks [`PipelineState::admit`] whether the next message may be parsed
//! now, and traces each one it hands to [`PipelineState::apply`]. Keeping
//! the decisions here is what lets every transition be unit-tested without
//! a server, and what makes the order of a trace — which messages are
//! parsed before which are sent — a property of this module alone.
//!
//! The COPY states (`PGASYNC_COPY_*`) are here too: a COPY response parks
//! the connection in one (`fe-protocol3.c:411`-`:427`), and the decisions of
//! `PQputCopyData`, `PQputCopyEnd` (`fe-exec.c:2712`, `:2766`) and
//! `getCopyDataMessage` (`fe-protocol3.c:1795`) are
//! [`PipelineState::begin_put_copy`], [`PipelineState::put_copy_end`] and
//! [`PipelineState::copy_message`].

use std::collections::VecDeque;

use crate::message::{Backend, CopyFormat, ProtocolError, TransactionStatus};
use crate::result::{ExecStatus, QueryResult, ResultError, diag};

/// `PGQueryClass`, `libpq-int.h:318`: what the command at the head of the
/// queue asked for, which decides what the replies mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryClass {
    /// `PGQUERY_SIMPLE`: a Query message (`PQexec`, `PQsendQuery`).
    #[default]
    Simple,
    /// `PGQUERY_EXTENDED`: Parse (optional), Bind, Describe portal, Execute
    /// (`PQexecParams`, `PQexecPrepared`).
    Extended,
    /// `PGQUERY_PREPARE`: Parse only (`PQprepare`).
    Prepare,
    /// `PGQUERY_DESCRIBE`: Describe a statement or a portal.
    Describe,
    /// `PGQUERY_SYNC`: a Sync sent by `PQpipelineSync` or
    /// `PQsendPipelineSync`, whose one result is `PGRES_PIPELINE_SYNC`.
    Sync,
    /// `PGQUERY_CLOSE`: Close a statement or a portal.
    Close,
}

/// `PGpipelineStatus`, `libpq-fe.h:185` — what `PQpipelineStatus` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PipelineStatus {
    /// `PQ_PIPELINE_OFF`: one command at a time, each with its own Sync.
    #[default]
    Off,
    /// `PQ_PIPELINE_ON`.
    On,
    /// `PQ_PIPELINE_ABORTED`: an error arrived, and every queued command up
    /// to the next Sync is reported as `PGRES_PIPELINE_ABORTED` without the
    /// server being asked (`fe-protocol3.c:908`).
    Aborted,
}

/// `PGAsyncStatusType`, `libpq-int.h:213`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AsyncStatus {
    /// `PGASYNC_IDLE`: nothing is expected from the server.
    #[default]
    Idle,
    /// `PGASYNC_BUSY`: a command is running and its replies are parsed.
    Busy,
    /// `PGASYNC_READY`: a result is complete; parsing waits for the caller
    /// to collect it.
    Ready,
    /// `PGASYNC_READY_MORE`: a partial result (single-row or chunked mode)
    /// is ready, and more are coming from the same command.
    ReadyMore,
    /// `PGASYNC_PIPELINE_IDLE`: between two commands of a pipeline — the
    /// state in which `PQgetResult` returns the NULL that ends a command.
    PipelineIdle,
    /// `PGASYNC_COPY_IN`: the server waits for COPY data.
    CopyIn,
    /// `PGASYNC_COPY_OUT`: the server is sending COPY data.
    CopyOut,
    /// `PGASYNC_COPY_BOTH`: both at once (replication).
    CopyBoth,
}

impl AsyncStatus {
    /// One of the three COPY states.
    #[must_use]
    pub fn is_copy(self) -> bool {
        matches!(
            self,
            AsyncStatus::CopyIn | AsyncStatus::CopyOut | AsyncStatus::CopyBoth
        )
    }

    /// The `PGRES_COPY_*` a COPY state's `PQgetResult` reports
    /// (`getCopyResult`, `fe-exec.c:2211`-`:2220`).
    fn copy_status(self) -> Option<ExecStatus> {
        match self {
            AsyncStatus::CopyIn => Some(ExecStatus::CopyIn),
            AsyncStatus::CopyOut => Some(ExecStatus::CopyOut),
            AsyncStatus::CopyBoth => Some(ExecStatus::CopyBoth),
            _ => None,
        }
    }
}

/// `partialResMode`, `singleRowMode` and `maxChunkSize` (`libpq-int.h:468`)
/// as the three combinations libpq actually uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowMode {
    /// All rows in one `PGRES_TUPLES_OK` result.
    #[default]
    All,
    /// `PQsetSingleRowMode`: each row is its own `PGRES_SINGLE_TUPLE`.
    Single,
    /// `PQsetChunkedRowsMode`: up to this many rows per
    /// `PGRES_TUPLES_CHUNK`.
    Chunked(usize),
}

impl RowMode {
    /// `maxChunkSize`, when rows are handed over before the command ends.
    fn chunk_size(self) -> Option<usize> {
        match self {
            RowMode::All => None,
            RowMode::Single => Some(1),
            RowMode::Chunked(size) => Some(size),
        }
    }
}

/// A refusal libpq reports through `conn->errorMessage` and a zero return,
/// before anything is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineError {
    /// `fe-exec.c:3084`.
    NotIdle,
    /// `fe-exec.c:3120` and `:3141`.
    UncollectedResults,
    /// `fe-exec.c:3124`.
    Busy,
    /// `fe-exec.c:3334`.
    NotInPipelineMode,
    /// `fe-exec.c:1714` and `:3418`.
    CommandInProgress,
    /// `fe-exec.c:1461`: `"%s not allowed in pipeline mode"`, with the name
    /// of the function refused.
    NotAllowedInPipelineMode(&'static str),
    /// `fe-exec.c:2378`: `PQexec` and its blocking siblings.
    SynchronousInPipelineMode,
    /// `fe-exec.c:1744`: a command queued in pipeline mode while a COPY runs.
    QueueDuringCopy,
    /// `fe-exec.c:2719`, `:2773`, `:2841`: a COPY call with no COPY running.
    NoCopyInProgress,
    /// `fe-exec.c:2411`: `PQexec` (via `PQexecStart`) while COPY BOTH runs.
    ExecDuringCopyBoth,
    /// `fe-exec.c:3135`: leaving pipeline mode during a COPY. The C `case`
    /// appends its message and falls out of the `switch` with no `return`,
    /// into the queue check (`:3139`); the COPY command is still queued, so
    /// that check refuses as well and both messages are left.
    ExitDuringCopy,
    /// `fe-exec.c:3345`: a pipeline Sync while a COPY runs, which upstream
    /// calls unreachable.
    SyncDuringCopy,
}

impl PipelineError {
    /// The bytes `libpq_append_conn_error` appends, without its newline.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            PipelineError::NotIdle => b"cannot enter pipeline mode, connection not idle".to_vec(),
            PipelineError::UncollectedResults => {
                b"cannot exit pipeline mode with uncollected results".to_vec()
            }
            PipelineError::Busy => b"cannot exit pipeline mode while busy".to_vec(),
            PipelineError::NotInPipelineMode => {
                b"cannot send pipeline when not in pipeline mode".to_vec()
            }
            PipelineError::CommandInProgress => b"another command is already in progress".to_vec(),
            PipelineError::NotAllowedInPipelineMode(function) => {
                format!("{function} not allowed in pipeline mode").into_bytes()
            }
            PipelineError::SynchronousInPipelineMode => {
                b"synchronous command execution functions are not allowed in pipeline mode".to_vec()
            }
            PipelineError::QueueDuringCopy => b"cannot queue commands during COPY".to_vec(),
            PipelineError::NoCopyInProgress => b"no COPY in progress".to_vec(),
            PipelineError::ExecDuringCopyBoth => b"PQexec not allowed during COPY BOTH".to_vec(),
            PipelineError::ExitDuringCopy => b"cannot exit pipeline mode while in COPY\ncannot exit pipeline mode with uncollected results".to_vec(),
            // appendPQExpBufferStr, not libpq_append_conn_error: the newline
            // is upstream's own and is left off here like every other.
            PipelineError::SyncDuringCopy => {
                b"internal error: cannot send pipeline while in COPY".to_vec()
            }
        }
    }
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for PipelineError {}

/// Whether the next message in the input buffer may be parsed now —
/// `pqParseInput3`'s state gate (`fe-protocol3.c:153`-`:199`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// Parse it, trace it, hand it to [`PipelineState::apply`].
    Process,
    /// Leave it in the buffer and stop parsing: the caller has a result to
    /// collect first.
    Wait,
}

/// What a parsed message means outside the result being built: the
/// connection-level side effects of `pqParseInput3`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A NoticeResponse, an ErrorResponse that arrived while idle
    /// (`fe-protocol3.c:180`), or a notice libpq makes itself
    /// (`pqInternalNotice`, `fe-exec.c:944`) — for the notice processor.
    Notice(ResultError),
    /// A NotificationResponse (`getNotify`), for `PQnotifies`.
    Notification {
        pid: i32,
        channel: Vec<u8>,
        payload: Vec<u8>,
    },
    /// A ParameterStatus (`getParameterStatus`).
    ParameterStatus { name: Vec<u8>, value: Vec<u8> },
    /// A ReadyForQuery's transaction status (`getReadyForQuery`,
    /// `fe-protocol3.c:1763`).
    ReadyForQuery(TransactionStatus),
}

/// What `getCopyDataMessage` does with the next whole message during COPY
/// OUT or COPY BOTH (`fe-protocol3.c:1846`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyStep {
    /// A NotificationResponse, NoticeResponse or ParameterStatus: process it
    /// as usual, consume it, and look at the next one.
    Async,
    /// CopyData: hand its body to the caller.
    Data,
    /// The end of the COPY (`return -1`): leave the message in the buffer —
    /// the state has already moved on — and let `PQgetResult` read the
    /// command's result.
    End,
}

/// What `PQgetResult` does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Return this result.
    Result(QueryResult),
    /// Return NULL: the command is over (or nothing is running).
    Null,
    /// Nothing to return yet: read more from the server, parse it, and ask
    /// again (`fe-exec.c:2095`).
    Block,
}

/// The query-execution part of a `PGconn`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PipelineState {
    /// `conn->asyncStatus`.
    status: AsyncStatus,
    /// `conn->pipelineStatus`.
    pipeline: PipelineStatus,
    /// `conn->cmd_queue_head` … `cmd_queue_tail`: one query class per
    /// command sent and not yet completed.
    queue: VecDeque<QueryClass>,
    /// `conn->result`: the result being built. `pgHavePendingResult`
    /// (`libpq-int.h:936`) is this being `Some`; `conn->error_result` has no
    /// counterpart, since the libpq-internal errors that set it are
    /// connection errors here.
    result: Option<QueryResult>,
    /// `conn->saved_result`: in single-row or chunked mode, the TUPLES_OK
    /// result carrying the columns, parked while a partial result is out.
    saved_result: Option<QueryResult>,
    /// `partialResMode` / `singleRowMode` / `maxChunkSize`.
    row_mode: RowMode,
}

impl PipelineState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `PQpipelineStatus`.
    #[must_use]
    pub fn pipeline_status(&self) -> PipelineStatus {
        self.pipeline
    }

    /// `conn->asyncStatus`.
    #[must_use]
    pub fn async_status(&self) -> AsyncStatus {
        self.status
    }

    /// The query classes still queued, head first.
    #[must_use]
    pub fn queue(&self) -> Vec<QueryClass> {
        self.queue.iter().copied().collect()
    }

    /// `PQisBusy`'s answer once input has been parsed (`fe-exec.c:2064`).
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.status == AsyncStatus::Busy
    }

    /// `PQsendQueryStart`, `fe-exec.c:1690`, plus the pipeline refusal of
    /// `PQsendQueryInternal` (`:1459`) for a simple query: may a command of
    /// `class` be sent now?
    ///
    /// # Errors
    /// Another command is running outside pipeline mode, a COPY is running
    /// in it, or a simple query is asked for in pipeline mode.
    pub fn begin_send(&mut self, class: QueryClass) -> Result<(), PipelineError> {
        if self.status != AsyncStatus::Idle && self.pipeline == PipelineStatus::Off {
            return Err(PipelineError::CommandInProgress);
        }
        // fe-exec.c:1741 — nothing can be queued behind a COPY.
        if self.pipeline != PipelineStatus::Off && self.status.is_copy() {
            return Err(PipelineError::QueueDuringCopy);
        }
        if self.pipeline == PipelineStatus::Off {
            // fe-exec.c:1756 — this command's results come in immediately.
            self.clear_async_result();
            self.row_mode = RowMode::All;
        }
        if class == QueryClass::Simple && self.pipeline != PipelineStatus::Off {
            return Err(PipelineError::NotAllowedInPipelineMode("PQsendQuery"));
        }
        Ok(())
    }

    /// `PQexecStart`'s refusal, `fe-exec.c:2376`.
    ///
    /// # Errors
    /// In pipeline mode.
    pub fn begin_exec(&self) -> Result<(), PipelineError> {
        if self.pipeline == PipelineStatus::Off {
            Ok(())
        } else {
            Err(PipelineError::SynchronousInPipelineMode)
        }
    }

    /// Whether a command's own Sync is sent: "Add a Sync, unless in
    /// pipeline mode" (`fe-exec.c:1610`, `:1903`, `:2628`).
    #[must_use]
    pub fn sends_own_sync(&self) -> bool {
        self.pipeline == PipelineStatus::Off
    }

    /// `pqPipelineFlush`, `fe-exec.c:4047`: outside `PQ_PIPELINE_ON` every
    /// command is flushed at once; inside it, only past the threshold.
    #[must_use]
    pub fn flushes_now(&self, pending: usize) -> bool {
        self.pipeline != PipelineStatus::On || pending >= OUTBUFFER_THRESHOLD
    }

    /// `pqAppendCmdQueueEntry`, `fe-exec.c:1356`: a command has been sent.
    pub fn append(&mut self, class: QueryClass) {
        self.queue.push_back(class);
        match self.pipeline {
            PipelineStatus::Off | PipelineStatus::On => {
                if self.status == AsyncStatus::Idle {
                    self.status = AsyncStatus::Busy;
                }
            }
            // fe-exec.c:1382 — nothing will come from the server for it, so
            // do what PQgetResult would to consume the queue.
            PipelineStatus::Aborted => {
                if matches!(self.status, AsyncStatus::Idle | AsyncStatus::PipelineIdle) {
                    self.process_queue();
                }
            }
        }
    }

    /// `PQenterPipelineMode`, `fe-exec.c:3073`. Nothing is sent.
    ///
    /// # Errors
    /// A command is running outside pipeline mode.
    pub fn enter_pipeline_mode(&mut self) -> Result<(), PipelineError> {
        if self.pipeline != PipelineStatus::Off {
            return Ok(());
        }
        if self.status != AsyncStatus::Idle {
            return Err(PipelineError::NotIdle);
        }
        self.pipeline = PipelineStatus::On;
        Ok(())
    }

    /// `PQexitPipelineMode`, `fe-exec.c:3104`. `Ok(true)` means pipeline
    /// mode was left and the output buffer is to be flushed (`:3149`);
    /// `Ok(false)`, that it was not on and there is nothing to do.
    ///
    /// # Errors
    /// Results remain to be collected, or a command is still running.
    pub fn exit_pipeline_mode(&mut self) -> Result<bool, PipelineError> {
        if self.pipeline == PipelineStatus::Off
            && matches!(self.status, AsyncStatus::Idle | AsyncStatus::PipelineIdle)
            && self.queue.is_empty()
        {
            return Ok(false);
        }
        match self.status {
            AsyncStatus::Ready | AsyncStatus::ReadyMore => {
                return Err(PipelineError::UncollectedResults);
            }
            AsyncStatus::Busy => return Err(PipelineError::Busy),
            AsyncStatus::Idle | AsyncStatus::PipelineIdle => {}
            // fe-exec.c:3132 — see `ExitDuringCopy`: the COPY command is
            // still at the head of the queue, so the queue check refuses too.
            AsyncStatus::CopyIn | AsyncStatus::CopyOut | AsyncStatus::CopyBoth => {
                if !self.queue.is_empty() {
                    return Err(PipelineError::ExitDuringCopy);
                }
            }
        }
        if !self.queue.is_empty() {
            return Err(PipelineError::UncollectedResults);
        }
        self.pipeline = PipelineStatus::Off;
        self.status = AsyncStatus::Idle;
        Ok(true)
    }

    /// `pqPipelineSyncInternal`'s check, `fe-exec.c:3332`, before the Sync
    /// is put and [`PipelineState::append`]ed as [`QueryClass::Sync`].
    ///
    /// # Errors
    /// Not in pipeline mode.
    pub fn begin_pipeline_sync(&self) -> Result<(), PipelineError> {
        if self.pipeline == PipelineStatus::Off {
            Err(PipelineError::NotInPipelineMode)
        } else if self.status.is_copy() {
            // fe-exec.c:3340.
            Err(PipelineError::SyncDuringCopy)
        } else {
            Ok(())
        }
    }

    /// `PQsendFlushRequest`'s check, `fe-exec.c:3415`.
    ///
    /// # Errors
    /// A command is running outside pipeline mode.
    pub fn begin_flush_request(&self) -> Result<(), PipelineError> {
        if self.status != AsyncStatus::Idle && self.pipeline == PipelineStatus::Off {
            Err(PipelineError::CommandInProgress)
        } else {
            Ok(())
        }
    }

    /// `canChangeResultMode`, `fe-exec.c:1942`: only after a query has been
    /// launched and before any of its results arrived.
    fn can_change_result_mode(&self) -> bool {
        self.status == AsyncStatus::Busy
            && matches!(
                self.queue.front(),
                Some(QueryClass::Simple | QueryClass::Extended)
            )
            && self.result.is_none()
    }

    /// `PQsetSingleRowMode`, `fe-exec.c:1965`.
    pub fn set_single_row_mode(&mut self) -> bool {
        if self.can_change_result_mode() {
            self.row_mode = RowMode::Single;
            true
        } else {
            false
        }
    }

    /// `PQsetChunkedRowsMode`, `fe-exec.c:1982`.
    pub fn set_chunked_rows_mode(&mut self, chunk_size: usize) -> bool {
        if chunk_size > 0 && self.can_change_result_mode() {
            self.row_mode = RowMode::Chunked(chunk_size);
            true
        } else {
            false
        }
    }

    /// `pqParseInput3`'s gate, `fe-protocol3.c:153`-`:199`, for a message of
    /// type `id` at the head of the buffer — plus the one BUSY-state case
    /// that stops *before* a message (`:340`, a second RowDescription while
    /// a result is pending), which is why this takes `&mut self`.
    pub fn admit(&mut self, id: u8) -> Admit {
        // fe-protocol3.c:153 — NOTIFY and NOTICE in any state.
        if id == b'A' || id == b'N' {
            return Admit::Process;
        }
        match self.status {
            // fe-protocol3.c:166 — only IDLE deals with the message now.
            AsyncStatus::Idle => Admit::Process,
            AsyncStatus::Busy => {
                if id == b'T' && self.second_row_description() {
                    self.status = AsyncStatus::Ready;
                    Admit::Wait
                } else {
                    Admit::Process
                }
            }
            // fe-protocol3.c:166 — any other state waits, the COPY states
            // included: their data is read by `getCopyDataMessage` (see
            // `copy_message`), not here.
            AsyncStatus::Ready
            | AsyncStatus::ReadyMore
            | AsyncStatus::PipelineIdle
            | AsyncStatus::CopyIn
            | AsyncStatus::CopyOut
            | AsyncStatus::CopyBoth => Admit::Wait,
        }
    }

    /// `fe-protocol3.c:340`: a RowDescription that starts another result,
    /// rather than discarding (after an error) or filling one (the first, or
    /// a Describe's).
    fn second_row_description(&self) -> bool {
        match &self.result {
            None => false,
            Some(result) if result.status() == ExecStatus::FatalError => false,
            Some(_) => self.queue.front() != Some(&QueryClass::Describe),
        }
    }

    /// One admitted message in: `pqParseInput3`'s handling of it.
    ///
    /// # Errors
    /// The message cannot appear here at all — a DataRow with no preceding
    /// RowDescription, a field count that disagrees with it, a startup
    /// message, or a type this port does not handle yet (the COPY messages)
    /// or that libpq never provokes (PortalSuspended, `fe-protocol3.c:446`).
    /// Upstream turns these into an error result and carries on; here they
    /// end the exchange.
    pub fn apply(&mut self, message: Backend) -> Result<Option<Event>, ProtocolError> {
        match message {
            Backend::NotificationResponse {
                pid,
                channel,
                payload,
            } => Ok(Some(Event::Notification {
                pid,
                channel,
                payload,
            })),
            Backend::NoticeResponse(notice) => Ok(Some(Event::Notice(notice))),
            message if self.status == AsyncStatus::Idle => Ok(Some(Self::apply_idle(message))),
            message => self.apply_busy(message),
        }
    }

    /// `fe-protocol3.c:166`-`:196`: a message that arrived while nothing was
    /// running. An error is a notice, a ParameterStatus is taken, and
    /// anything else is dropped with a notice of libpq's own.
    fn apply_idle(message: Backend) -> Event {
        match message {
            Backend::ErrorResponse(error) => Event::Notice(error),
            Backend::ParameterStatus { name, value } => Event::ParameterStatus { name, value },
            other => Event::Notice(internal_notice(
                format!(
                    "message type 0x{:02x} arrived from server while idle",
                    message_id(&other)
                )
                .into_bytes(),
            )),
        }
    }

    /// The BUSY-state switch, `fe-protocol3.c:203`-`:447`.
    fn apply_busy(&mut self, message: Backend) -> Result<Option<Event>, ProtocolError> {
        let head = self.queue.front().copied();
        match message {
            Backend::CommandComplete(tag) => {
                self.result
                    .get_or_insert_with(|| QueryResult::new(ExecStatus::CommandOk))
                    .set_command_status(tag);
                self.status = AsyncStatus::Ready;
            }
            Backend::ErrorResponse(error) => {
                // pqGetErrorNotice3, fe-protocol3.c:907 — the pipeline is
                // aborted, and the error replaces whatever was being built.
                if self.pipeline != PipelineStatus::Off {
                    self.pipeline = PipelineStatus::Aborted;
                }
                self.clear_async_result();
                self.result = Some(QueryResult::with_error(ExecStatus::FatalError, error));
                self.status = AsyncStatus::Ready;
            }
            Backend::ReadyForQuery(xact) => {
                if self.pipeline == PipelineStatus::Off {
                    // fe-protocol3.c:246.
                    self.advance(true, false);
                    self.status = AsyncStatus::Idle;
                } else {
                    // fe-protocol3.c:233 — the end of a pipeline is a result
                    // of its own, and it clears the aborted state.
                    self.result = Some(QueryResult::new(ExecStatus::PipelineSync));
                    self.pipeline = PipelineStatus::On;
                    self.status = AsyncStatus::Ready;
                }
                return Ok(Some(Event::ReadyForQuery(xact)));
            }
            Backend::EmptyQueryResponse => {
                self.result
                    .get_or_insert_with(|| QueryResult::new(ExecStatus::EmptyQuery));
                self.status = AsyncStatus::Ready;
            }
            // fe-protocol3.c:266, :287, :351 — each is the result of its own
            // class of command, and nothing to any other.
            Backend::ParseComplete if head == Some(QueryClass::Prepare) => self.command_ok_ready(),
            Backend::CloseComplete if head == Some(QueryClass::Close) => self.command_ok_ready(),
            Backend::NoData if head == Some(QueryClass::Describe) => self.command_ok_ready(),
            // The same three otherwise, and BindComplete always, are nothing
            // to the result; so are data left over from a COPY OUT the caller
            // stopped reading early and the CopyDone `getCopyDataMessage`
            // leaves in the buffer, which are dropped (fe-protocol3.c:428,
            // :437).
            Backend::ParseComplete
            | Backend::CloseComplete
            | Backend::NoData
            | Backend::BindComplete
            | Backend::CopyData(_)
            | Backend::CopyDone => {}
            Backend::ParameterStatus { name, value } => {
                return Ok(Some(Event::ParameterStatus { name, value }));
            }
            Backend::RowDescription(fields) => {
                if self
                    .result
                    .as_ref()
                    .is_some_and(|r| r.status() == ExecStatus::FatalError)
                {
                    // fe-protocol3.c:320 — "We've already choked for some
                    // reason. Just discard the data".
                } else if head == Some(QueryClass::Describe) {
                    // getRowDescriptions, fe-protocol3.c:529 and :627 — a
                    // Describe fills the result ParameterDescription made
                    // (or a new COMMAND_OK one), and is done.
                    self.result
                        .get_or_insert_with(|| QueryResult::new(ExecStatus::CommandOk))
                        .set_fields(fields);
                    self.status = AsyncStatus::Ready;
                } else {
                    // `admit` has already held back a second one.
                    let mut result = QueryResult::new(ExecStatus::TuplesOk);
                    result.set_fields(fields);
                    self.result = Some(result);
                }
            }
            // getParamDescriptions, fe-protocol3.c:690 — a new COMMAND_OK
            // result holding the parameter types.
            Backend::ParameterDescription(types) => {
                let mut result = QueryResult::new(ExecStatus::CommandOk);
                result.set_params(types);
                self.result = Some(result);
            }
            Backend::DataRow(values) => self.data_row(values)?,
            // fe-protocol3.c:411-:427 — getCopyStart makes the COPY result,
            // and the connection waits in the COPY state for the caller.
            Backend::CopyInResponse(format) => self.copy_start(ExecStatus::CopyIn, &format),
            Backend::CopyOutResponse(format) => self.copy_start(ExecStatus::CopyOut, &format),
            Backend::CopyBothResponse(format) => self.copy_start(ExecStatus::CopyBoth, &format),
            // Both belong to the startup exchange; naming the byte that
            // actually arrived is the whole point of upstream's message
            // (`fe-protocol3.c:447`).
            Backend::Authentication(_) => return Err(ProtocolError::UnexpectedResponse(b'R')),
            Backend::BackendKeyData { .. } => {
                return Err(ProtocolError::UnexpectedResponse(b'K'));
            }
            // fe-protocol3.c:446 — PortalSuspended has no case of its own,
            // and neither has NegotiateProtocolVersion: it is read only by
            // the startup loop (`fe-connect.c:4148`), never mid-query.
            other @ (Backend::PortalSuspended
            | Backend::NegotiateProtocolVersion { .. }
            | Backend::Other { .. }) => {
                return Err(ProtocolError::UnexpectedResponse(message_id(&other)));
            }
            Backend::NotificationResponse { .. } | Backend::NoticeResponse(_) => {
                unreachable!("handled before the BUSY switch")
            }
        }
        Ok(None)
    }

    /// `getCopyStart`, `fe-protocol3.c:1707`, and the state it leaves
    /// (`:414`, `:419`, `:425`). The COPY result replaces whatever was being
    /// built, as `conn->result = result` does (`:1751`).
    fn copy_start(&mut self, status: ExecStatus, format: &CopyFormat) {
        self.result = Some(QueryResult::copy(status, format));
        self.status = match status {
            ExecStatus::CopyIn => AsyncStatus::CopyIn,
            ExecStatus::CopyOut => AsyncStatus::CopyOut,
            _ => AsyncStatus::CopyBoth,
        };
    }

    /// `PQputCopyData`'s check, `fe-exec.c:2716`: sending COPY data needs
    /// COPY IN or COPY BOTH.
    ///
    /// # Errors
    /// No COPY is taking data.
    pub fn begin_put_copy(&self) -> Result<(), PipelineError> {
        if matches!(self.status, AsyncStatus::CopyIn | AsyncStatus::CopyBoth) {
            Ok(())
        } else {
            Err(PipelineError::NoCopyInProgress)
        }
    }

    /// `PQputCopyEnd`, `fe-exec.c:2766`, less the sending: `Ok(true)` when
    /// a Sync must follow the CopyDone or CopyFail, because the COPY came
    /// from an extended-query command (`:2801`). Afterwards the connection is
    /// back to waiting for the command's result, or, from COPY BOTH, to
    /// reading COPY OUT (`:2810`).
    ///
    /// # Errors
    /// No COPY is taking data.
    pub fn put_copy_end(&mut self) -> Result<bool, PipelineError> {
        self.begin_put_copy()?;
        let sync = self
            .queue
            .front()
            .is_some_and(|class| *class != QueryClass::Simple);
        self.status = if self.status == AsyncStatus::CopyBoth {
            AsyncStatus::CopyOut
        } else {
            AsyncStatus::Busy
        };
        Ok(sync)
    }

    /// `PQgetCopyData`'s check, `fe-exec.c:2838`: reading COPY data needs
    /// COPY OUT or COPY BOTH.
    ///
    /// # Errors
    /// No COPY is sending data.
    pub fn begin_get_copy(&self) -> Result<(), PipelineError> {
        if matches!(self.status, AsyncStatus::CopyOut | AsyncStatus::CopyBoth) {
            Ok(())
        } else {
            Err(PipelineError::NoCopyInProgress)
        }
    }

    /// `getCopyDataMessage`'s switch, `fe-protocol3.c:1846`-`:1882`, for a
    /// whole message of type `id` at the head of the buffer during COPY OUT
    /// or COPY BOTH. The end of the COPY moves the state on here, and leaves
    /// the message where it is for `pqParseInput3` to read.
    pub fn copy_message(&mut self, id: u8) -> CopyStep {
        match id {
            b'A' | b'N' | b'S' => CopyStep::Async,
            b'd' => CopyStep::Data,
            // fe-protocol3.c:1862 — CopyDone ends COPY OUT, and turns COPY
            // BOTH into COPY IN.
            b'c' => {
                self.status = if self.status == AsyncStatus::CopyBoth {
                    AsyncStatus::CopyIn
                } else {
                    AsyncStatus::Busy
                };
                CopyStep::End
            }
            // fe-protocol3.c:1874 — anything else ends the COPY too.
            _ => {
                self.status = AsyncStatus::Busy;
                CopyStep::End
            }
        }
    }

    /// Whether a result is waiting to be handed over — `conn->result`.
    #[must_use]
    pub fn has_pending_result(&self) -> bool {
        self.result.is_some()
    }

    /// `PQexecStart` leaving a COPY OUT it was handed (`fe-exec.c:2399`):
    /// "we just switch back to BUSY and allow the remaining COPY data to be
    /// dropped on the floor".
    pub fn abandon_copy_out(&mut self) {
        if self.status == AsyncStatus::CopyOut {
            self.status = AsyncStatus::Busy;
        }
    }

    /// `fe-protocol3.c:383` and `pqRowProcessor`, `fe-exec.c:1223`.
    fn data_row(&mut self, values: Vec<Option<Vec<u8>>>) -> Result<(), ProtocolError> {
        match self.result.as_ref().map(QueryResult::status) {
            Some(ExecStatus::TuplesOk | ExecStatus::TuplesChunk) => {}
            // fe-protocol3.c:397 — discarded after an error.
            Some(ExecStatus::FatalError) => return Ok(()),
            _ => return Err(ProtocolError::DataWithoutRowDescription),
        }
        let chunk_size = self.row_mode.chunk_size();
        if chunk_size.is_some() && self.saved_result.is_none() {
            // fe-exec.c:1239 — a partial result carrying the columns, with
            // the original parked until the partial one is collected.
            let saved = self.result.take().expect("a TUPLES_OK result");
            let mut partial = QueryResult::new(if self.row_mode == RowMode::Single {
                ExecStatus::SingleTuple
            } else {
                ExecStatus::TuplesChunk
            });
            partial.set_fields(saved.fields().to_vec());
            self.saved_result = Some(saved);
            self.result = Some(partial);
        }
        let result = self.result.as_mut().expect("a result to add the row to");
        // fe-protocol3.c:796 — the field count must match "T".
        if values.len() != result.nfields() {
            return Err(ProtocolError::UnexpectedFieldCount);
        }
        result.push_row(values);
        if chunk_size.is_some_and(|size| result.ntuples() >= size) {
            self.status = AsyncStatus::ReadyMore;
        }
        Ok(())
    }

    /// `if (!pgHavePendingResult(conn)) conn->result =
    /// PQmakeEmptyPGresult(conn, PGRES_COMMAND_OK)`, then `PGASYNC_READY`
    /// (`fe-protocol3.c:271`).
    fn command_ok_ready(&mut self) {
        self.result
            .get_or_insert_with(|| QueryResult::new(ExecStatus::CommandOk));
        self.status = AsyncStatus::Ready;
    }

    /// `pqClearAsyncResult`, `fe-exec.c:785`.
    fn clear_async_result(&mut self) {
        self.result = None;
        self.saved_result = None;
    }

    /// `pqPrepareAsyncResult`, `fe-exec.c:857`: hand the result over and put
    /// back the parked one, if any.
    fn prepare_async_result(&mut self) -> QueryResult {
        let result = self.result.take();
        self.result = self.saved_result.take();
        // fe-exec.c:880 — no result at all is libpq's "no error text
        // available" error result. No path here reaches READY without one.
        result.unwrap_or_else(|| QueryResult::new(ExecStatus::FatalError))
    }

    /// `PQgetResult`'s dispatch once input has been parsed, `fe-exec.c:2140`.
    pub fn next_result(&mut self) -> Next {
        match self.status {
            AsyncStatus::Busy => Next::Block,
            AsyncStatus::Idle => Next::Null,
            AsyncStatus::PipelineIdle => {
                // fe-exec.c:2145 — the NULL ending a command, then on to the
                // next one.
                self.process_queue();
                Next::Null
            }
            AsyncStatus::Ready => {
                let result = self.prepare_async_result();
                // fe-exec.c:2162 — a chunk that is not full is returned with
                // the TUPLES_OK behind it still pending.
                if self.result.is_some() {
                    return Next::Result(result);
                }
                let sync = result.status() == ExecStatus::PipelineSync;
                self.advance(false, sync);
                if self.pipeline == PipelineStatus::Off {
                    self.status = AsyncStatus::Busy;
                } else {
                    self.status = AsyncStatus::PipelineIdle;
                    // fe-exec.c:2195 — no NULL after a pipeline sync.
                    if sync {
                        self.process_queue();
                    }
                }
                Next::Result(result)
            }
            AsyncStatus::ReadyMore => {
                let result = self.prepare_async_result();
                self.status = AsyncStatus::Busy;
                Next::Result(result)
            }
            AsyncStatus::CopyIn | AsyncStatus::CopyOut | AsyncStatus::CopyBoth => {
                Next::Result(self.copy_result())
            }
        }
    }

    /// `getCopyResult`, `fe-exec.c:2241`: the COPY result getCopyStart made,
    /// the first time; a fresh one of the same status every time after. The
    /// state does not change — the caller is to move the data.
    fn copy_result(&mut self) -> QueryResult {
        let status = self
            .status
            .copy_status()
            .expect("called in a COPY state only");
        if self.result.as_ref().map(QueryResult::status) == Some(status) {
            return self.prepare_async_result();
        }
        QueryResult::new(status)
    }

    /// `pqCommandQueueAdvance`, `fe-exec.c:3173`.
    fn advance(&mut self, is_ready_for_query: bool, got_sync: bool) {
        match self.queue.front() {
            None => {}
            Some(QueryClass::Simple) if !is_ready_for_query => {}
            Some(QueryClass::Sync) if !got_sync => {}
            Some(_) => {
                self.queue.pop_front();
            }
        }
    }

    /// `pqPipelineProcessQueue`, `fe-exec.c:3211`: start on the next queued
    /// command.
    fn process_queue(&mut self) {
        match self.status {
            AsyncStatus::Ready
            | AsyncStatus::ReadyMore
            | AsyncStatus::Busy
            | AsyncStatus::CopyIn
            | AsyncStatus::CopyOut
            | AsyncStatus::CopyBoth => return,
            AsyncStatus::Idle if self.queue.is_empty() => return,
            AsyncStatus::Idle | AsyncStatus::PipelineIdle => {}
        }
        self.row_mode = RowMode::All;
        let Some(&head) = self.queue.front() else {
            self.status = AsyncStatus::Idle;
            return;
        };
        self.clear_async_result();
        if self.pipeline == PipelineStatus::Aborted && head != QueryClass::Sync {
            // fe-exec.c:3275 — nothing comes from the server for a command
            // of an aborted pipeline; it is reported as aborted.
            self.result = Some(QueryResult::new(ExecStatus::PipelineAborted));
            self.status = AsyncStatus::Ready;
        } else {
            self.status = AsyncStatus::Busy;
        }
    }
}

/// Whether [`QueryRunner::push`]'s caller should keep reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// ReadyForQuery arrived: the command is over.
    Done,
}

/// One command sent outside pipeline mode, folded message by message into
/// what `PQexec` would collect: [`PipelineState`] driven as `PQgetResult`
/// drives it, with every result taken as soon as it is ready. No socket —
/// which is what makes it the way to build results from recorded replies.
#[derive(Debug, Clone, Default)]
pub struct QueryRunner {
    state: PipelineState,
    results: Vec<QueryResult>,
    notices: Vec<ResultError>,
    notifications: Vec<(i32, Vec<u8>, Vec<u8>)>,
    parameters: Vec<(Vec<u8>, Vec<u8>)>,
    transaction_status: Option<TransactionStatus>,
}

impl QueryRunner {
    /// A runner for a simple Query.
    #[must_use]
    pub fn new() -> Self {
        Self::for_class(QueryClass::Simple)
    }

    /// A runner for a command of the given class, just sent.
    #[must_use]
    pub fn for_class(class: QueryClass) -> Self {
        let mut state = PipelineState::new();
        // A fresh state is idle and out of pipeline mode: nothing refuses.
        let _ = state.begin_send(class);
        state.append(class);
        Self {
            state,
            ..Self::default()
        }
    }

    /// One message in. A COPY result ends the fold as it ends `PQexecFinish`
    /// (`fe-exec.c:2448`): the data transfer is the caller's, so the runner
    /// is [`Flow::Done`] from then on and takes nothing more.
    ///
    /// # Errors
    /// The message cannot appear here at all; see [`PipelineState::apply`].
    pub fn push(&mut self, message: Backend) -> Result<Flow, ProtocolError> {
        while self.state.admit(message_id(&message)) == Admit::Wait {
            if self.state.async_status().is_copy() {
                return Ok(Flow::Done);
            }
            self.collect();
        }
        match self.state.apply(message)? {
            None => {}
            Some(Event::Notice(notice)) => self.notices.push(notice),
            Some(Event::Notification {
                pid,
                channel,
                payload,
            }) => self.notifications.push((pid, channel, payload)),
            Some(Event::ParameterStatus { name, value }) => self.parameters.push((name, value)),
            Some(Event::ReadyForQuery(status)) => self.transaction_status = Some(status),
        }
        self.collect();
        let status = self.state.async_status();
        Ok(if status == AsyncStatus::Idle || status.is_copy() {
            Flow::Done
        } else {
            Flow::Continue
        })
    }

    /// Take every result that is ready, as `PQgetResult` would — and the
    /// COPY result once, where `PQexecFinish` stops.
    fn collect(&mut self) {
        while matches!(
            self.state.async_status(),
            AsyncStatus::Ready | AsyncStatus::ReadyMore
        ) {
            if let Next::Result(result) = self.state.next_result() {
                self.results.push(result);
            }
        }
        if self.state.async_status().is_copy()
            && self.state.has_pending_result()
            && let Next::Result(result) = self.state.next_result()
        {
            self.results.push(result);
        }
    }

    #[must_use]
    pub fn results(&self) -> &[QueryResult] {
        &self.results
    }

    #[must_use]
    pub fn into_results(self) -> Vec<QueryResult> {
        self.results
    }

    #[must_use]
    pub fn notices(&self) -> &[ResultError] {
        &self.notices
    }

    #[must_use]
    pub fn notifications(&self) -> &[(i32, Vec<u8>, Vec<u8>)] {
        &self.notifications
    }

    #[must_use]
    pub fn parameters(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.parameters
    }

    #[must_use]
    pub fn transaction_status(&self) -> Option<TransactionStatus> {
        self.transaction_status
    }
}

/// `OUTBUFFER_THRESHOLD`, `libpq-int.h:948`.
pub const OUTBUFFER_THRESHOLD: usize = 65536;

/// `pqInternalNotice`, `fe-exec.c:944`: the fields of a notice libpq makes
/// itself.
fn internal_notice(message: Vec<u8>) -> ResultError {
    ResultError::new(vec![
        (diag::MESSAGE_PRIMARY, message),
        (diag::SEVERITY, b"NOTICE".to_vec()),
        (diag::SEVERITY_NONLOCALIZED, b"NOTICE".to_vec()),
    ])
}

/// The type byte a decoded message came from, for the messages that name it.
#[must_use]
pub fn message_id(message: &Backend) -> u8 {
    match message {
        Backend::Authentication(_) => b'R',
        Backend::BackendKeyData { .. } => b'K',
        Backend::ParameterStatus { .. } => b'S',
        Backend::ReadyForQuery(_) => b'Z',
        Backend::RowDescription(_) => b'T',
        Backend::DataRow(_) => b'D',
        Backend::CommandComplete(_) => b'C',
        Backend::EmptyQueryResponse => b'I',
        Backend::ErrorResponse(_) => b'E',
        Backend::NoticeResponse(_) => b'N',
        Backend::NotificationResponse { .. } => b'A',
        Backend::NegotiateProtocolVersion { .. } => b'v',
        Backend::ParseComplete => b'1',
        Backend::BindComplete => b'2',
        Backend::CloseComplete => b'3',
        Backend::NoData => b'n',
        Backend::PortalSuspended => b's',
        Backend::ParameterDescription(_) => b't',
        Backend::CopyInResponse(_) => b'G',
        Backend::CopyOutResponse(_) => b'H',
        Backend::CopyBothResponse(_) => b'W',
        Backend::CopyData(_) => b'd',
        Backend::CopyDone => b'c',
        Backend::Other { id, .. } => *id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::FieldDescription;

    fn int4_field(name: &[u8]) -> FieldDescription {
        FieldDescription {
            name: name.to_vec(),
            tableid: 0,
            columnid: 0,
            typid: 23,
            typlen: 4,
            atttypmod: -1,
            format: 0,
        }
    }

    fn text_field(name: &[u8]) -> FieldDescription {
        FieldDescription {
            typid: 25,
            typlen: -1,
            ..int4_field(name)
        }
    }

    fn error(sqlstate: &[u8]) -> ResultError {
        ResultError::new(vec![
            (diag::SEVERITY, b"ERROR".to_vec()),
            (diag::SQLSTATE, sqlstate.to_vec()),
            (diag::MESSAGE_PRIMARY, b"boom".to_vec()),
        ])
    }

    fn ready(status: TransactionStatus) -> Backend {
        Backend::ReadyForQuery(status)
    }

    /// `SELECT $1`'s replies: `traces/simple_pipeline.trace` lines 6-10.
    fn select_one() -> Vec<Backend> {
        vec![
            Backend::ParseComplete,
            Backend::BindComplete,
            Backend::RowDescription(vec![int4_field(b"?column?")]),
            Backend::DataRow(vec![Some(b"1".to_vec())]),
            Backend::CommandComplete(b"SELECT 1".to_vec()),
        ]
    }

    /// An INSERT through `PQsendQueryParams`: `traces/pipeline_abort.trace`
    /// lines 26-29.
    fn insert() -> Vec<Backend> {
        vec![
            Backend::ParseComplete,
            Backend::BindComplete,
            Backend::NoData,
            Backend::CommandComplete(b"INSERT 0 1".to_vec()),
        ]
    }

    /// `pqParseInput3` over the replies that have "arrived": parse while
    /// the state admits, and collect the side effects.
    fn parse(state: &mut PipelineState, input: &mut VecDeque<Backend>) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(message) = input.front() {
            if state.admit(message_id(message)) == Admit::Wait {
                break;
            }
            let message = input.pop_front().expect("peeked");
            events.extend(state.apply(message).expect("a message libpq accepts"));
        }
        events
    }

    /// `PQgetResult` with every reply already arrived: the status of the
    /// result, or `None` for NULL. Panics where the real call would block
    /// forever.
    fn get(state: &mut PipelineState, input: &mut VecDeque<Backend>) -> Option<ExecStatus> {
        loop {
            parse(state, input);
            match state.next_result() {
                Next::Result(result) => return Some(result.status()),
                Next::Null => return None,
                Next::Block => assert!(!input.is_empty(), "PQgetResult would block"),
            }
        }
    }

    /// A pipelined command: `PQsendQueryParams` & co. in pipeline mode.
    fn send(state: &mut PipelineState, class: QueryClass) {
        state.begin_send(class).expect("the command may be sent");
        state.append(class);
    }

    fn sync(state: &mut PipelineState) {
        state.begin_pipeline_sync().expect("in pipeline mode");
        state.append(QueryClass::Sync);
    }

    /// Feed a command of `class`, sent outside pipeline mode, its replies and
    /// return every result up to the NULL.
    fn replay(class: QueryClass, replies: Vec<Backend>) -> Vec<QueryResult> {
        let mut state = PipelineState::new();
        state.begin_send(class).unwrap();
        state.append(class);
        let mut input: VecDeque<Backend> = replies.into();
        let mut results = Vec::new();
        loop {
            parse(&mut state, &mut input);
            match state.next_result() {
                Next::Result(result) => results.push(result),
                Next::Null => return results,
                Next::Block => assert!(!input.is_empty(), "PQgetResult would block"),
            }
        }
    }

    /// `test_simple_pipeline`, `libpq_pipeline.c:1593`, as transitions: the
    /// result, the NULL ending the command, the sync result with no NULL
    /// before it, then the NULL of an empty queue — and `PQexitPipelineMode`
    /// refused at each point it is refused upstream (`:1626`, `:1647`).
    #[test]
    fn test_simple_pipeline() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        send(&mut state, QueryClass::Extended);
        assert_eq!(state.exit_pipeline_mode(), Err(PipelineError::Busy));
        sync(&mut state);

        let mut input: VecDeque<Backend> = select_one().into();
        input.push_back(ready(TransactionStatus::Idle));
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::TuplesOk));
        assert_eq!(get(&mut state, &mut input), None);
        assert!(state.exit_pipeline_mode().is_err(), "the sync is still due");
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::PipelineSync));
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(state.pipeline_status(), PipelineStatus::On);
        assert_eq!(state.exit_pipeline_mode(), Ok(true));
        assert_eq!(state.pipeline_status(), PipelineStatus::Off);
        assert_eq!(state.async_status(), AsyncStatus::Idle);
    }

    /// `test_multi_pipelines`, `libpq_pipeline.c:468`: three pipelines
    /// queued before any result is read are three rounds of result, NULL,
    /// sync — with no NULL between a sync and the next command's result.
    #[test]
    fn test_multi_pipelines() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        let mut input = VecDeque::new();
        for _ in 0..3 {
            send(&mut state, QueryClass::Extended);
            sync(&mut state);
            input.extend(select_one());
            input.push_back(ready(TransactionStatus::Idle));
        }
        for round in 0..3 {
            assert_eq!(
                get(&mut state, &mut input),
                Some(ExecStatus::TuplesOk),
                "{round}"
            );
            assert_eq!(get(&mut state, &mut input), None, "{round}");
            if round < 2 {
                assert!(state.exit_pipeline_mode().is_err(), "{round}");
            }
            assert_eq!(
                get(&mut state, &mut input),
                Some(ExecStatus::PipelineSync),
                "{round}"
            );
        }
        assert_eq!(state.exit_pipeline_mode(), Ok(true));
    }

    /// `test_pipeline_abort`'s first two pipelines, `libpq_pipeline.c:705`,
    /// over the replies of `traces/pipeline_abort.trace` lines 26-36: after
    /// the error, the queued INSERT is `PGRES_PIPELINE_ABORTED` without a
    /// word from the server, and the sync clears the aborted state.
    #[test]
    fn test_pipeline_abort() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        for _ in 0..3 {
            send(&mut state, QueryClass::Extended);
        }
        sync(&mut state);
        send(&mut state, QueryClass::Extended);
        sync(&mut state);

        let mut input: VecDeque<Backend> = insert().into();
        input.push_back(Backend::ErrorResponse(error(b"42883")));
        input.push_back(ready(TransactionStatus::Idle));
        input.extend(insert());
        input.push_back(ready(TransactionStatus::Idle));

        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::CommandOk));
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::FatalError));
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(state.pipeline_status(), PipelineStatus::Aborted);
        assert_eq!(
            get(&mut state, &mut input),
            Some(ExecStatus::PipelineAborted)
        );
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(state.pipeline_status(), PipelineStatus::Aborted);
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::PipelineSync));
        assert_eq!(state.pipeline_status(), PipelineStatus::On);
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::CommandOk));
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::PipelineSync));
        assert_eq!(get(&mut state, &mut input), None);
        assert!(input.is_empty());
    }

    /// `test_transaction`, `libpq_pipeline.c:1876`, over the replies of
    /// `traces/transaction.trace` lines 37-55: the order of results the
    /// upstream loop walks through, four syncs and all.
    #[test]
    fn test_transaction() {
        use ExecStatus::{CommandOk, FatalError, PipelineAborted, PipelineSync};
        use QueryClass::{Extended, Prepare};

        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        for class in [Prepare, Extended, Extended, Extended, Extended] {
            send(&mut state, class);
        }
        sync(&mut state);
        send(&mut state, Extended);
        sync(&mut state);
        send(&mut state, Extended);
        send(&mut state, Extended);
        sync(&mut state);
        sync(&mut state);

        let mut input: VecDeque<Backend> = vec![
            Backend::ParseComplete,
            Backend::ParseComplete,
            Backend::BindComplete,
            Backend::NoData,
            Backend::CommandComplete(b"BEGIN".to_vec()),
            Backend::ParseComplete,
            Backend::ErrorResponse(error(b"22012")),
            ready(TransactionStatus::InError),
            Backend::ErrorResponse(error(b"25P02")),
            ready(TransactionStatus::InError),
            Backend::BindComplete,
            Backend::NoData,
            Backend::CommandComplete(b"ROLLBACK".to_vec()),
        ]
        .into();
        input.extend(insert());
        input.push_back(ready(TransactionStatus::Idle));
        input.push_back(ready(TransactionStatus::Idle));

        let mut seen = Vec::new();
        let mut syncs = 4;
        while syncs > 0 {
            let status = get(&mut state, &mut input);
            seen.push(status);
            if status == Some(PipelineSync) {
                syncs -= 1;
            }
        }
        assert_eq!(
            seen,
            [
                Some(CommandOk),
                None,
                Some(CommandOk),
                None,
                Some(FatalError),
                None,
                Some(PipelineAborted),
                None,
                Some(PipelineAborted),
                None,
                Some(PipelineSync),
                Some(FatalError),
                None,
                Some(PipelineSync),
                Some(CommandOk),
                None,
                Some(CommandOk),
                None,
                Some(PipelineSync),
                Some(PipelineSync),
            ]
        );
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(state.exit_pipeline_mode(), Ok(true));
    }

    /// `test_nosync`, `libpq_pipeline.c:613`: commands with no sync at all
    /// still produce a result and a NULL each, and the queue empties.
    #[test]
    fn test_nosync() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        let mut input = VecDeque::new();
        for _ in 0..10 {
            send(&mut state, QueryClass::Extended);
            input.extend(select_one());
        }
        state.begin_flush_request().unwrap();
        for _ in 0..10 {
            assert_eq!(get(&mut state, &mut input), Some(ExecStatus::TuplesOk));
            assert_eq!(get(&mut state, &mut input), None);
        }
        assert!(state.queue().is_empty());
        assert_eq!(state.async_status(), AsyncStatus::Idle);
    }

    /// `test_pipeline_idle`, `libpq_pipeline.c:1526`: after a result and its
    /// NULL the connection is idle again; a new command makes it busy, and
    /// exiting then is refused; after the result alone (no NULL collected)
    /// it is `PIPELINE_IDLE` and exiting is allowed. A NOTICE in the middle
    /// of a result set is an event, not a result.
    #[test]
    fn test_pipeline_idle() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        send(&mut state, QueryClass::Extended);
        state.begin_flush_request().unwrap();
        let mut input: VecDeque<Backend> = select_one().into();
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::TuplesOk));
        assert_eq!(get(&mut state, &mut input), None);
        send(&mut state, QueryClass::Extended);
        assert_eq!(state.exit_pipeline_mode(), Err(PipelineError::Busy));
        assert!(
            PipelineError::Busy
                .message()
                .starts_with(b"cannot exit pipeline mode")
        );
        input.extend(select_one());
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::TuplesOk));
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(state.exit_pipeline_mode(), Ok(true));

        state.enter_pipeline_mode().unwrap();
        send(&mut state, QueryClass::Extended);
        let mut input: VecDeque<Backend> = vec![
            Backend::ParseComplete,
            Backend::BindComplete,
            Backend::RowDescription(vec![int4_field(b"pg_advisory_unlock")]),
            Backend::NoticeResponse(error(b"01000")),
            Backend::DataRow(vec![Some(b"f".to_vec())]),
            Backend::CommandComplete(b"SELECT 1".to_vec()),
        ]
        .into();
        let events = parse(&mut state, &mut input);
        assert_eq!(events, [Event::Notice(error(b"01000"))]);
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::TuplesOk));
        assert_eq!(state.async_status(), AsyncStatus::PipelineIdle);
        assert_eq!(state.exit_pipeline_mode(), Ok(true));
    }

    /// `test_disallowed_in_pipeline`, `libpq_pipeline.c:408`: the refusals,
    /// with upstream's messages, and the no-op arms of enter and exit.
    #[test]
    fn test_disallowed_in_pipeline() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        assert_eq!(state.pipeline_status(), PipelineStatus::On);
        assert_eq!(
            state.begin_exec().unwrap_err().message(),
            b"synchronous command execution functions are not allowed in pipeline mode"
        );
        assert_eq!(
            state.begin_send(QueryClass::Simple).unwrap_err().message(),
            b"PQsendQuery not allowed in pipeline mode"
        );
        assert_eq!(state.enter_pipeline_mode(), Ok(()), "already on: a no-op");
        assert!(!state.is_busy());
        assert_eq!(state.exit_pipeline_mode(), Ok(true));
        assert_eq!(state.pipeline_status(), PipelineStatus::Off);
        assert_eq!(state.exit_pipeline_mode(), Ok(false), "already off");
        assert_eq!(state.begin_exec(), Ok(()));
        assert_eq!(state.begin_send(QueryClass::Simple), Ok(()));
    }

    /// `test_prepared`'s pipeline, `libpq_pipeline.c:1263`-`:1310`, over
    /// `traces/prepared.trace` lines 4-7: the Parse is one COMMAND_OK
    /// result, and the Describe another carrying the parameter and column
    /// types.
    #[test]
    fn test_prepared() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        send(&mut state, QueryClass::Prepare);
        send(&mut state, QueryClass::Describe);
        sync(&mut state);
        let mut input: VecDeque<Backend> = vec![
            Backend::ParseComplete,
            Backend::ParameterDescription(vec![23]),
            Backend::RowDescription(vec![int4_field(b"?column?"), text_field(b"?column?")]),
            ready(TransactionStatus::Idle),
        ]
        .into();
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::CommandOk));
        assert_eq!(get(&mut state, &mut input), None);
        parse(&mut state, &mut input);
        let Next::Result(described) = state.next_result() else {
            panic!("the Describe's result");
        };
        assert_eq!(described.status(), ExecStatus::CommandOk);
        assert_eq!(described.paramtype(0), Some(23));
        assert_eq!(described.ftype(1), Some(25));
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::PipelineSync));
    }

    /// `PQenterPipelineMode` refuses while a command runs outside pipeline
    /// mode (`fe-exec.c:3082`), and the refusals that guard a busy
    /// connection outside pipeline mode (`:1712`, `:3416`) do not apply
    /// inside it.
    #[test]
    fn a_busy_connection_cannot_enter_pipeline_mode_or_take_another_command() {
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        assert_eq!(state.enter_pipeline_mode(), Err(PipelineError::NotIdle));
        assert_eq!(
            state.begin_send(QueryClass::Extended),
            Err(PipelineError::CommandInProgress)
        );
        assert_eq!(
            state.begin_flush_request(),
            Err(PipelineError::CommandInProgress)
        );
        assert_eq!(
            state.begin_pipeline_sync(),
            Err(PipelineError::NotInPipelineMode)
        );
        assert_eq!(
            state.exit_pipeline_mode(),
            Err(PipelineError::Busy),
            "upstream's switch reports a busy connection even outside pipeline mode"
        );
    }

    /// `PQexitPipelineMode` with a result ready but not collected, and with
    /// commands still queued behind a collected one (`fe-exec.c:3117`,
    /// `:3139`).
    #[test]
    fn uncollected_results_keep_pipeline_mode_on() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        send(&mut state, QueryClass::Extended);
        send(&mut state, QueryClass::Extended);
        let mut input: VecDeque<Backend> = select_one().into();
        parse(&mut state, &mut input);
        assert_eq!(state.async_status(), AsyncStatus::Ready);
        assert_eq!(
            state.exit_pipeline_mode(),
            Err(PipelineError::UncollectedResults)
        );
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::TuplesOk));
        assert_eq!(state.async_status(), AsyncStatus::PipelineIdle);
        assert_eq!(
            state.exit_pipeline_mode(),
            Err(PipelineError::UncollectedResults)
        );
    }

    /// A command queued while the pipeline is already aborted is reported as
    /// aborted straight away (`pqAppendCmdQueueEntry`, `fe-exec.c:1382`).
    #[test]
    fn a_command_queued_into_an_aborted_pipeline_is_aborted_at_once() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        send(&mut state, QueryClass::Extended);
        let mut input: VecDeque<Backend> = vec![Backend::ErrorResponse(error(b"42883"))].into();
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::FatalError));
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(state.async_status(), AsyncStatus::Idle);
        send(&mut state, QueryClass::Extended);
        assert_eq!(state.async_status(), AsyncStatus::Ready);
        assert_eq!(
            get(&mut state, &mut input),
            Some(ExecStatus::PipelineAborted)
        );
    }

    /// Outside pipeline mode a simple query's results end at ReadyForQuery,
    /// which is not a result (`fe-protocol3.c:246`), and several statements
    /// in one query string are several results.
    #[test]
    fn a_multi_statement_query_produces_one_result_per_statement() {
        let results = replay(
            QueryClass::Simple,
            vec![
                Backend::CommandComplete(b"CREATE TABLE".to_vec()),
                Backend::CommandComplete(b"INSERT 0 1".to_vec()),
                ready(TransactionStatus::Idle),
            ],
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].command_status(), b"CREATE TABLE");
        assert_eq!(results[1].command_status(), b"INSERT 0 1");

        let results = replay(
            QueryClass::Simple,
            vec![Backend::EmptyQueryResponse, ready(TransactionStatus::Idle)],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::EmptyQuery);
    }

    /// `PQdescribePrepared`: the replies of `traces/prepared.trace` lines
    /// 4-7 (ParseComplete aside) make *one* COMMAND_OK result carrying both
    /// the parameter types and the columns (`fe-protocol3.c:527`).
    #[test]
    fn a_describe_statement_is_one_command_ok_result_with_params_and_fields() {
        let results = replay(
            QueryClass::Describe,
            vec![
                Backend::ParameterDescription(vec![23]),
                Backend::RowDescription(vec![int4_field(b"?column?"), text_field(b"?column?")]),
                ready(TransactionStatus::Idle),
            ],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::CommandOk);
        assert_eq!(results[0].nparams(), 1);
        assert_eq!(results[0].paramtype(0), Some(23));
        assert_eq!(results[0].paramtype(1), None);
        assert_eq!(results[0].nfields(), 2);
        assert_eq!(results[0].ftype(0), Some(23));
        assert_eq!(results[0].ftype(1), Some(25));
        assert_eq!(results[0].ntuples(), 0);
    }

    /// A Describe of a statement that returns no rows: ParameterDescription
    /// then NoData is still one COMMAND_OK result (`fe-protocol3.c:351`);
    /// and a Describe of a portal, which has no ParameterDescription, gets a
    /// fresh one.
    #[test]
    fn a_describe_of_something_without_rows_is_still_a_result() {
        let results = replay(
            QueryClass::Describe,
            vec![
                Backend::ParameterDescription(vec![23, 25]),
                Backend::NoData,
                ready(TransactionStatus::Idle),
            ],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::CommandOk);
        assert_eq!(results[0].nparams(), 2);
        assert_eq!(results[0].nfields(), 0);

        let results = replay(
            QueryClass::Describe,
            vec![
                Backend::RowDescription(vec![int4_field(b"?column?")]),
                ready(TransactionStatus::InTransaction),
            ],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::CommandOk);
        assert_eq!(results[0].nparams(), 0);
        assert_eq!(results[0].ftype(0), Some(23));
    }

    /// ParseComplete is a result only for `PQprepare`, CloseComplete only for
    /// a Close, NoData only for a Describe (`fe-protocol3.c:266`, `:287`,
    /// `:351`); to every other class they are nothing.
    #[test]
    fn each_completion_message_is_a_result_only_for_its_own_class() {
        let ready = ready(TransactionStatus::Idle);
        for (message, class) in [
            (Backend::ParseComplete, QueryClass::Prepare),
            (Backend::CloseComplete, QueryClass::Close),
            (Backend::NoData, QueryClass::Describe),
        ] {
            let results = replay(class, vec![message.clone(), ready.clone()]);
            assert_eq!(results.len(), 1, "{message:?} for {class:?}");
            assert_eq!(results[0].status(), ExecStatus::CommandOk);

            for other in [
                QueryClass::Simple,
                QueryClass::Extended,
                QueryClass::Prepare,
                QueryClass::Describe,
                QueryClass::Close,
            ] {
                if other != class {
                    assert!(
                        replay(other, vec![message.clone(), ready.clone()]).is_empty(),
                        "{message:?} for {other:?}"
                    );
                }
            }
        }
        assert!(replay(QueryClass::Extended, vec![Backend::BindComplete, ready]).is_empty());
    }

    /// `PQexecParams("SELECT $1", …, {"1"})`: the replies of
    /// `traces/simple_pipeline.trace` lines 6-11 make one TUPLES_OK result
    /// with the row and the tag.
    #[test]
    fn an_extended_query_collects_its_rows() {
        let mut replies = select_one();
        replies.push(ready(TransactionStatus::Idle));
        let results = replay(QueryClass::Extended, replies);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::TuplesOk);
        assert_eq!(results[0].value(0, 0), Some(&b"1"[..]));
        assert_eq!(results[0].command_status(), b"SELECT 1");
    }

    /// `traces/prepared.trace` lines 12-15: a Describe of a statement that
    /// does not exist gets ErrorResponse and ReadyForQuery back, and its one
    /// result is the error.
    #[test]
    fn an_error_is_the_only_result_of_its_command() {
        let results = replay(
            QueryClass::Describe,
            vec![
                Backend::ErrorResponse(error(b"26000")),
                ready(TransactionStatus::Idle),
            ],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::FatalError);
        assert_eq!(
            results[0].error().and_then(ResultError::sqlstate),
            Some(&b"26000"[..])
        );
    }

    /// An error mid-result discards the rows so far (`fe-protocol3.c:915`):
    /// the error is the only result.
    #[test]
    fn an_error_discards_the_partial_result() {
        let results = replay(
            QueryClass::Extended,
            vec![
                Backend::RowDescription(vec![text_field(b"a")]),
                Backend::DataRow(vec![Some(b"x".to_vec())]),
                Backend::ErrorResponse(error(b"22012")),
                ready(TransactionStatus::InError),
            ],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::FatalError);
    }

    /// Once the result being built is an error, a RowDescription or DataRow
    /// is discarded rather than misfiled (`fe-protocol3.c:320`, `:397`).
    #[test]
    fn rows_after_an_error_result_are_discarded() {
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        state.result = Some(QueryResult::with_error(
            ExecStatus::FatalError,
            error(b"XX000"),
        ));
        assert_eq!(state.admit(b'T'), Admit::Process);
        state
            .apply(Backend::RowDescription(vec![text_field(b"a")]))
            .unwrap();
        state
            .apply(Backend::DataRow(vec![Some(b"x".to_vec())]))
            .unwrap();
        let result = state.result.as_ref().unwrap();
        assert_eq!(result.status(), ExecStatus::FatalError);
        assert_eq!(result.ntuples(), 0);
    }

    /// A second RowDescription while a TUPLES_OK result is pending stops the
    /// parse *before* it and makes the result ready (`fe-protocol3.c:340`):
    /// the message is left for after the result is collected.
    #[test]
    fn a_second_row_description_waits_for_the_first_result() {
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        let mut input: VecDeque<Backend> = vec![
            Backend::RowDescription(vec![text_field(b"a")]),
            Backend::RowDescription(vec![text_field(b"b")]),
            Backend::CommandComplete(b"SELECT 0".to_vec()),
            ready(TransactionStatus::Idle),
        ]
        .into();
        parse(&mut state, &mut input);
        assert_eq!(input.len(), 3, "the second T is still in the buffer");
        assert_eq!(state.async_status(), AsyncStatus::Ready);
        let Next::Result(first) = state.next_result() else {
            panic!("a result");
        };
        assert_eq!(first.fname(0), Some(&b"a"[..]));
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::TuplesOk));
        assert_eq!(get(&mut state, &mut input), None);
    }

    /// The two malformed-stream cases `pqParseInput3` names for "D", which
    /// end the exchange here.
    #[test]
    fn a_data_row_out_of_place_is_refused() {
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        assert_eq!(
            state.apply(Backend::DataRow(vec![None])),
            Err(ProtocolError::DataWithoutRowDescription)
        );
        state
            .apply(Backend::RowDescription(vec![text_field(b"a")]))
            .unwrap();
        assert_eq!(
            state.apply(Backend::DataRow(vec![None, None])),
            Err(ProtocolError::UnexpectedFieldCount)
        );
    }

    /// PortalSuspended has no case in `pqParseInput3` (libpq never sets a
    /// row limit), so it is the "unexpected response" default
    /// (`fe-protocol3.c:446`).
    #[test]
    fn a_portal_suspended_is_an_unexpected_response() {
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Extended).unwrap();
        state.append(QueryClass::Extended);
        assert_eq!(
            state.apply(Backend::PortalSuspended),
            Err(ProtocolError::UnexpectedResponse(b's'))
        );
    }

    /// NegotiateProtocolVersion is a startup message: the BUSY switch of
    /// `pqParseInput3` (`fe-protocol3.c:203`-`:447`) has no case for it, so
    /// mid-query it is the "unexpected response" default (`:446`).
    #[test]
    fn a_negotiate_protocol_version_is_an_unexpected_response() {
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Extended).unwrap();
        state.append(QueryClass::Extended);
        assert_eq!(
            state.apply(Backend::NegotiateProtocolVersion {
                newest: 0x0003_0000,
                unrecognized: Vec::new(),
            }),
            Err(ProtocolError::UnexpectedResponse(b'v'))
        );
    }

    /// In IDLE state every message is parsed: an error is a notice, a
    /// ParameterStatus is taken, anything else is dropped with libpq's own
    /// notice (`fe-protocol3.c:166`-`:196`). NOTICE and NOTIFY are parsed
    /// in every state, even one waiting for the caller (`:153`).
    #[test]
    fn what_arrives_while_idle_is_a_notice_or_a_parameter() {
        let mut state = PipelineState::new();
        assert_eq!(state.admit(b'Z'), Admit::Process);
        assert_eq!(
            state.apply(Backend::ErrorResponse(error(b"57P01"))),
            Ok(Some(Event::Notice(error(b"57P01"))))
        );
        assert_eq!(
            state.apply(Backend::ParameterStatus {
                name: b"a".to_vec(),
                value: b"b".to_vec()
            }),
            Ok(Some(Event::ParameterStatus {
                name: b"a".to_vec(),
                value: b"b".to_vec()
            }))
        );
        let Ok(Some(Event::Notice(notice))) = state.apply(ready(TransactionStatus::Idle)) else {
            panic!("a notice");
        };
        assert_eq!(
            notice.field(diag::MESSAGE_PRIMARY),
            Some(&b"message type 0x5a arrived from server while idle"[..])
        );
        assert_eq!(notice.field(diag::SEVERITY), Some(&b"NOTICE"[..]));

        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        state
            .apply(Backend::CommandComplete(b"SELECT 0".to_vec()))
            .unwrap();
        assert_eq!(state.admit(b'Z'), Admit::Wait);
        assert_eq!(state.admit(b'N'), Admit::Process);
        assert_eq!(state.admit(b'A'), Admit::Process);
    }

    /// `PQsetSingleRowMode` over `traces/pipeline_abort.trace` lines 49-56:
    /// each row is a SINGLE_TUPLE result; the error ends the command.
    #[test]
    fn single_row_mode_hands_over_each_row() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        send(&mut state, QueryClass::Extended);
        sync(&mut state);
        assert!(state.set_single_row_mode());
        let mut input: VecDeque<Backend> = vec![
            Backend::ParseComplete,
            Backend::BindComplete,
            Backend::RowDescription(vec![text_field(b"?column?")]),
            Backend::DataRow(vec![Some(b"0.33".to_vec())]),
            Backend::DataRow(vec![Some(b"0.50".to_vec())]),
            Backend::DataRow(vec![Some(b"1.00".to_vec())]),
            Backend::ErrorResponse(error(b"22012")),
            ready(TransactionStatus::Idle),
        ]
        .into();
        for row in [&b"0.33"[..], b"0.50", b"1.00"] {
            parse(&mut state, &mut input);
            let Next::Result(result) = state.next_result() else {
                panic!("a row");
            };
            assert_eq!(result.status(), ExecStatus::SingleTuple);
            assert_eq!(result.value(0, 0), Some(row));
        }
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::FatalError));
        assert_eq!(get(&mut state, &mut input), None);
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::PipelineSync));
    }

    /// Without an error, single-row mode ends with an empty TUPLES_OK
    /// carrying the tag; chunked mode hands over full chunks, then the
    /// partial last one, then that TUPLES_OK (`fe-exec.c:2162`).
    #[test]
    fn a_partial_result_mode_ends_with_an_empty_tuples_ok() {
        let rows = |n: usize| -> VecDeque<Backend> {
            let mut input: VecDeque<Backend> =
                vec![Backend::RowDescription(vec![text_field(b"a")])].into();
            for i in 0..n {
                input.push_back(Backend::DataRow(vec![Some(i.to_string().into_bytes())]));
            }
            input.push_back(Backend::CommandComplete(b"SELECT 3".to_vec()));
            input.push_back(ready(TransactionStatus::Idle));
            input
        };

        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        assert!(state.set_single_row_mode());
        let mut input = rows(3);
        let mut seen = Vec::new();
        while let Some(status) = get(&mut state, &mut input) {
            seen.push(status);
        }
        assert_eq!(
            seen,
            [
                ExecStatus::SingleTuple,
                ExecStatus::SingleTuple,
                ExecStatus::SingleTuple,
                ExecStatus::TuplesOk
            ]
        );

        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        assert!(!state.set_chunked_rows_mode(0), "a chunk of no rows");
        assert!(state.set_chunked_rows_mode(2));
        let mut input = rows(3);
        let mut seen = Vec::new();
        loop {
            parse(&mut state, &mut input);
            match state.next_result() {
                Next::Result(result) => seen.push((result.status(), result.ntuples())),
                Next::Null => break,
                Next::Block => {}
            }
        }
        assert_eq!(
            seen,
            [
                (ExecStatus::TuplesChunk, 2),
                (ExecStatus::TuplesChunk, 1),
                (ExecStatus::TuplesOk, 0)
            ]
        );
    }

    /// `canChangeResultMode`, `fe-exec.c:1942`: only with a query launched
    /// and none of its results yet, and only for a query that returns rows.
    #[test]
    fn the_row_mode_can_change_only_before_the_first_result() {
        let mut state = PipelineState::new();
        assert!(!state.set_single_row_mode(), "nothing launched");
        state.begin_send(QueryClass::Prepare).unwrap();
        state.append(QueryClass::Prepare);
        assert!(!state.set_single_row_mode(), "a Parse returns no rows");

        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        state
            .apply(Backend::RowDescription(vec![text_field(b"a")]))
            .unwrap();
        assert!(!state.set_single_row_mode(), "a result is pending");

        // A new command outside pipeline mode resets the mode (`fe-exec.c:1756`).
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        assert!(state.set_single_row_mode());
        let mut input: VecDeque<Backend> = vec![
            Backend::CommandComplete(b"SET".to_vec()),
            ready(TransactionStatus::Idle),
        ]
        .into();
        assert_eq!(get(&mut state, &mut input), Some(ExecStatus::CommandOk));
        assert_eq!(get(&mut state, &mut input), None);
        state.begin_send(QueryClass::Simple).unwrap();
        assert_eq!(state.row_mode, RowMode::All);
    }

    /// `pqPipelineFlush`, `fe-exec.c:4047`: in pipeline mode the output is
    /// held until the threshold; in any other state it goes out at once.
    #[test]
    fn a_pipeline_holds_its_output_until_the_threshold() {
        let mut state = PipelineState::new();
        assert!(state.flushes_now(1));
        assert!(state.sends_own_sync());
        state.enter_pipeline_mode().unwrap();
        assert!(!state.sends_own_sync());
        assert!(!state.flushes_now(OUTBUFFER_THRESHOLD - 1));
        assert!(state.flushes_now(OUTBUFFER_THRESHOLD));
        state.pipeline = PipelineStatus::Aborted;
        assert!(state.flushes_now(1), "only PQ_PIPELINE_ON holds back");
    }

    /// `QueryRunner` folds a command's replies the way the `PQgetResult`
    /// loop collects them: a result per statement, the notices aside, and
    /// `Flow::Done` at ReadyForQuery.
    #[test]
    fn a_query_runner_collects_what_pq_exec_would() {
        let mut runner = QueryRunner::new();
        for message in [
            Backend::RowDescription(vec![text_field(b"a")]),
            Backend::NoticeResponse(error(b"01000")),
            Backend::DataRow(vec![Some(b"x".to_vec())]),
            Backend::CommandComplete(b"SELECT 1".to_vec()),
            Backend::RowDescription(vec![text_field(b"b")]),
            Backend::CommandComplete(b"SELECT 0".to_vec()),
        ] {
            assert_eq!(runner.push(message), Ok(Flow::Continue));
        }
        assert_eq!(
            runner.push(ready(TransactionStatus::InTransaction)),
            Ok(Flow::Done)
        );
        assert_eq!(
            runner.transaction_status(),
            Some(TransactionStatus::InTransaction)
        );
        assert_eq!(runner.notices().len(), 1);
        let results = runner.into_results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].value(0, 0), Some(&b"x"[..]));
        assert_eq!(results[1].fname(0), Some(&b"b"[..]));
    }

    fn copy_format(columns: &[i16]) -> CopyFormat {
        CopyFormat {
            overall: 0,
            column_formats: columns.to_vec(),
        }
    }

    /// A simple `COPY … TO STDOUT` sent and its CopyOutResponse parsed.
    fn copying_out() -> PipelineState {
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        let mut input = VecDeque::from([Backend::CopyOutResponse(copy_format(&[0, 0]))]);
        assert!(parse(&mut state, &mut input).is_empty());
        state
    }

    /// `fe-protocol3.c:416` and `getCopyResult`, `fe-exec.c:2241`: the COPY
    /// result getCopyStart made comes first, then a fresh one on every call,
    /// and the state stays put until the data has been moved.
    #[test]
    fn a_copy_response_is_a_copy_result_on_every_call() {
        let mut state = copying_out();
        assert_eq!(state.async_status(), AsyncStatus::CopyOut);
        assert!(!state.is_busy(), "PQisBusy is false during COPY");
        let Next::Result(first) = state.next_result() else {
            panic!("a COPY result")
        };
        assert_eq!(first.status(), ExecStatus::CopyOut);
        assert_eq!(first.nfields(), 2, "one column per format code");
        assert_eq!(first.fformat(1), Some(0));
        assert_eq!(
            first.fname(0),
            Some(&b""[..]),
            "zeroed attDescs: an empty name, where C's PQfname is NULL"
        );
        assert!(!first.binary_tuples());
        let Next::Result(again) = state.next_result() else {
            panic!("a COPY result again")
        };
        assert_eq!(again.status(), ExecStatus::CopyOut);
        assert_eq!(again.nfields(), 0, "PQmakeEmptyPGresult, the second time");
        assert_eq!(state.async_status(), AsyncStatus::CopyOut);

        // fe-protocol3.c:166 — nothing but NOTIFY and NOTICE is parsed by
        // pqParseInput3 during COPY.
        assert_eq!(state.admit(b'd'), Admit::Wait);
        assert_eq!(state.admit(b'C'), Admit::Wait);
        assert_eq!(state.admit(b'N'), Admit::Process);
    }

    /// `getCopyStart`'s `binary` (`fe-protocol3.c:1719`) is
    /// `PQbinaryTuples`.
    #[test]
    fn a_binary_copy_result_says_so() {
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        let mut input = VecDeque::from([Backend::CopyInResponse(CopyFormat {
            overall: 1,
            column_formats: vec![1],
        })]);
        parse(&mut state, &mut input);
        let Next::Result(result) = state.next_result() else {
            panic!("a COPY result")
        };
        assert_eq!(result.status(), ExecStatus::CopyIn);
        assert!(result.binary_tuples());
        assert_eq!(result.fformat(0), Some(1));
    }

    /// `getCopyDataMessage`'s switch, `fe-protocol3.c:1846`-`:1882`.
    #[test]
    fn get_copy_data_message_ends_the_copy_at_anything_but_data() {
        let mut state = copying_out();
        for id in [b'A', b'N', b'S'] {
            assert_eq!(state.copy_message(id), CopyStep::Async);
        }
        assert_eq!(state.copy_message(b'd'), CopyStep::Data);
        assert_eq!(state.async_status(), AsyncStatus::CopyOut);
        assert_eq!(state.copy_message(b'c'), CopyStep::End);
        assert_eq!(state.async_status(), AsyncStatus::Busy);

        let mut state = copying_out();
        assert_eq!(state.copy_message(b'E'), CopyStep::End, "an error ends it");
        assert_eq!(state.async_status(), AsyncStatus::Busy);

        // fe-protocol3.c:1869 — CopyDone during COPY BOTH leaves COPY IN.
        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        let mut input = VecDeque::from([Backend::CopyBothResponse(copy_format(&[]))]);
        parse(&mut state, &mut input);
        assert_eq!(state.copy_message(b'c'), CopyStep::End);
        assert_eq!(state.async_status(), AsyncStatus::CopyIn);
    }

    /// Once the COPY is over, what `getCopyDataMessage` left behind is read
    /// by pqParseInput3: the CopyDone and stray CopyData are dropped
    /// (`fe-protocol3.c:428`, `:437`), and the command ends as usual.
    #[test]
    fn a_finished_copy_out_ends_like_any_command() {
        let mut state = copying_out();
        let Next::Result(_) = state.next_result() else {
            panic!("the COPY result")
        };
        assert_eq!(state.copy_message(b'c'), CopyStep::End);
        let mut input = VecDeque::from([
            Backend::CopyData(b"late\n".to_vec()),
            Backend::CopyDone,
            Backend::CommandComplete(b"COPY 2".to_vec()),
            ready(TransactionStatus::Idle),
        ]);
        parse(&mut state, &mut input);
        let Next::Result(done) = state.next_result() else {
            panic!("the command's result")
        };
        assert_eq!(done.status(), ExecStatus::CommandOk);
        assert_eq!(done.command_status(), b"COPY 2");
        parse(&mut state, &mut input);
        assert_eq!(state.next_result(), Next::Null);
        assert!(state.queue().is_empty());
    }

    /// `PQputCopyEnd`, `fe-exec.c:2766`: a Sync follows only a COPY that an
    /// extended-query command started (`:2801`), and COPY BOTH goes on as
    /// COPY OUT (`:2810`).
    #[test]
    fn put_copy_end_syncs_only_an_extended_query_copy() {
        for (class, sync) in [(QueryClass::Simple, false), (QueryClass::Extended, true)] {
            let mut state = PipelineState::new();
            state.begin_send(class).unwrap();
            state.append(class);
            let mut input = VecDeque::from([Backend::CopyInResponse(copy_format(&[0]))]);
            parse(&mut state, &mut input);
            assert_eq!(state.begin_put_copy(), Ok(()));
            assert_eq!(
                state.begin_get_copy(),
                Err(PipelineError::NoCopyInProgress),
                "COPY IN sends, it does not receive"
            );
            assert_eq!(state.put_copy_end(), Ok(sync), "{class:?}");
            assert_eq!(state.async_status(), AsyncStatus::Busy);
            assert_eq!(
                state.put_copy_end(),
                Err(PipelineError::NoCopyInProgress),
                "only once"
            );
        }

        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        let mut input = VecDeque::from([Backend::CopyBothResponse(copy_format(&[]))]);
        parse(&mut state, &mut input);
        assert_eq!(state.begin_get_copy(), Ok(()));
        assert_eq!(state.put_copy_end(), Ok(false));
        assert_eq!(state.async_status(), AsyncStatus::CopyOut);
    }

    /// `fe-exec.c:2719`, `:2773`, `:2841` — no COPY, no COPY calls.
    #[test]
    fn copy_calls_need_a_copy() {
        let mut state = PipelineState::new();
        assert_eq!(state.begin_put_copy(), Err(PipelineError::NoCopyInProgress));
        assert_eq!(state.put_copy_end(), Err(PipelineError::NoCopyInProgress));
        assert_eq!(state.begin_get_copy(), Err(PipelineError::NoCopyInProgress));
        assert_eq!(
            PipelineError::NoCopyInProgress.message(),
            b"no COPY in progress"
        );
    }

    /// A COPY in pipeline mode blocks the queue (`fe-exec.c:1744`), a Sync
    /// (`:3345`) and leaving the mode (`:3135`, and `:3141` after it).
    #[test]
    fn nothing_is_queued_behind_a_copy_in_a_pipeline() {
        let mut state = PipelineState::new();
        state.enter_pipeline_mode().unwrap();
        state.begin_send(QueryClass::Extended).unwrap();
        state.append(QueryClass::Extended);
        let mut input = VecDeque::from([
            Backend::ParseComplete,
            Backend::BindComplete,
            Backend::CopyInResponse(copy_format(&[0])),
        ]);
        parse(&mut state, &mut input);
        assert_eq!(state.async_status(), AsyncStatus::CopyIn);
        assert_eq!(
            state.begin_send(QueryClass::Extended),
            Err(PipelineError::QueueDuringCopy)
        );
        assert_eq!(
            state.begin_pipeline_sync(),
            Err(PipelineError::SyncDuringCopy)
        );
        assert_eq!(
            state.exit_pipeline_mode(),
            Err(PipelineError::ExitDuringCopy)
        );
        assert_eq!(
            PipelineError::ExitDuringCopy.message(),
            b"cannot exit pipeline mode while in COPY\ncannot exit pipeline mode with uncollected results"
        );
        assert_eq!(
            PipelineError::QueueDuringCopy.message(),
            b"cannot queue commands during COPY"
        );
        assert_eq!(
            PipelineError::SyncDuringCopy.message(),
            b"internal error: cannot send pipeline while in COPY"
        );
    }

    /// `PQexecStart` leaves a COPY OUT by going back to BUSY (`fe-exec.c:2399`)
    /// — and nothing else is left that way.
    #[test]
    fn abandoning_a_copy_out_drops_its_data() {
        let mut state = copying_out();
        state.abandon_copy_out();
        assert_eq!(state.async_status(), AsyncStatus::Busy);
        let mut input = VecDeque::from([
            Backend::CopyData(b"1\n".to_vec()),
            Backend::CopyDone,
            Backend::CommandComplete(b"COPY 1".to_vec()),
        ]);
        parse(&mut state, &mut input);
        let Next::Result(result) = state.next_result() else {
            panic!("the command's result")
        };
        assert_eq!(result.command_status(), b"COPY 1");

        let mut state = PipelineState::new();
        state.begin_send(QueryClass::Simple).unwrap();
        state.append(QueryClass::Simple);
        let mut input = VecDeque::from([Backend::CopyInResponse(copy_format(&[0]))]);
        parse(&mut state, &mut input);
        state.abandon_copy_out();
        assert_eq!(state.async_status(), AsyncStatus::CopyIn, "COPY IN stays");
        assert_eq!(
            PipelineError::ExecDuringCopyBoth.message(),
            b"PQexec not allowed during COPY BOTH"
        );
    }

    /// `PQexecFinish` stops at a COPY result (`fe-exec.c:2448`), and so
    /// does the runner, however much follows.
    #[test]
    fn a_query_runner_stops_at_a_copy_result() {
        let mut runner = QueryRunner::new();
        assert_eq!(
            runner
                .push(Backend::CopyOutResponse(copy_format(&[0])))
                .unwrap(),
            Flow::Done
        );
        assert_eq!(
            runner.push(Backend::CopyData(b"1\n".to_vec())).unwrap(),
            Flow::Done
        );
        let results = runner.into_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::CopyOut);
    }
}
