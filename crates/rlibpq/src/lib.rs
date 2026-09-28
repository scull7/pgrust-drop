//! rlibpq: the client side of libpq in pure Rust.
//!
//! Tracks PostgreSQL 18.6 `src/interfaces/libpq/` and the requirements in
//! pgrust issue #40: one codebase producing a native Rust crate and a drop-in
//! C-ABI `libpq.so`, with GSSAPI feature-gated and no libc/OpenSSL build-time
//! coupling.
//!
//! What is here so far is the connection-string front end
//! (`PQconninfoOptions[]` and the two parsers that fill a working copy of it,
//! proved against `t/001_uri.pl`, and the connection service files that fill
//! in its defaults, proved against `t/006_service.pl`) and the protocol version 3 core: the wire
//! messages, the authentication methods a build without TLS or GSSAPI can do
//! (trust, password, md5, SCRAM-SHA-256), and a `Connection` that runs simple
//! queries and the extended-query commands, blocking (`PQexec`,
//! `PQexecParams`, `PQprepare`, `PQexecPrepared`, `PQdescribePrepared`,
//! `PQdescribePortal`, `PQclosePrepared`, `PQclosePortal`) or asynchronously
//! (`PQsendQuery` and its siblings, `PQgetResult`, single-row and chunked
//! modes) and in pipeline mode (`PQenterPipelineMode` … `PQpipelineSync`),
//! over the pure state machine in `pipeline`; the COPY data transfer in both
//! directions (`PQputCopyData`, `PQputCopyEnd`, `PQgetCopyData`); the
//! blocking query cancel calls (`PQgetCancel`, `PQcancel`, `PQrequestCancel`,
//! `PQcancelCreate`, `PQcancelBlocking`); and the
//! `fe-trace.c` protocol trace behind `PQtrace` and `PQsetTraceFlags`; and
//! the encryption negotiation (`sslmode`, `sslnegotiation`, `gssencmode`) as
//! a pure state machine, run today with no TLS backend, so every connection
//! is plaintext and a mode that needs TLS is refused as a C libpq built
//! without SSL refuses it; and the encoding-aware escaping of `fe-exec.c`
//! (`PQescapeLiteral`, `PQescapeIdentifier`, `PQescapeStringConn`,
//! `PQescapeByteaConn`, `PQunescapeBytea`) over the client encoding the
//! server reports, proved against `test_escape.c`; and the fast-path
//! function call (`PQfn`) with the large object interface of `fe-lobj.c`
//! over it (`lo_open` … `lo_import`, `lo_export`), proved against
//! `src/test/examples/testlo.c` and `testlo64.c`; the host list —
//! `host`, `hostaddr` and `port` lists walked in order or shuffled by
//! `load_balance_hosts=random` over `pg_prng`'s generator, each server
//! checked against `target_session_attrs`; and the socket I/O of
//! `fe-misc.c` — a flush that reads while it waits to write, non-blocking
//! mode (`PQsetnonblocking`) and `PQsocketPoll` — proved against
//! `test_pipelined_insert` and `test_uniqviol`. The
//! rest is tracked in Linear NAT-390 … NAT-396.
//!
//! The C ABI layer will need `unsafe`; the pure-Rust core must not, so the
//! crate denies it until that layer exists as its own module, with no
//! exception: the readiness wait the standard library does not offer is
//! `rustix::event::poll`, a safe API (ADR-0010).

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod auth;
pub mod base64;
pub mod cancel;
pub mod connection;
pub mod conninfo;
mod cstr;
pub mod encoding;
pub mod error;
pub mod escape;
pub mod extended;
pub mod hmac;
pub mod hosts;
pub mod lobj;
pub mod md5;
pub mod message;
pub mod negotiate;
pub mod pg_config;
pub mod pipeline;
pub mod poll;
pub mod print;
pub mod regress;
pub mod result;
pub mod scram;
pub mod service;
pub mod sha256;
pub mod target;
mod text;
pub mod trace;
pub mod uri;
pub mod wcwidth;

pub use auth::{AuthError, AuthRequest, AuthStep, Authenticator, ChannelBinding};
pub use cancel::{Cancel, CancelConn, CancelError, CancelStatus, CancelStep, Peer};
pub use connection::{
    Address, Connection, ConnectionError, CopyRead, Flush, FnResult, Socket, Stream, Tracer,
    socket_address,
};
pub use conninfo::{
    CONNINFO_OPTIONS, ConnInfo, ConnOption, ConnOptionDef, Dispchar, Env, UnknownKeyword,
    conndefaults, parse_conninfo, parse_keyword_value, recognized_connection_string,
    uri_prefix_length,
};
pub use encoding::Encoding;
pub use error::ConnError;
pub use escape::{EscapeError, EscapedString, escape_bytea, escape_string, unescape_bytea};
pub use extended::{ArgumentError, Format, PQ_QUERY_PARAM_MAX_LIMIT, Params};
pub use hosts::{ConnHost, HostType, LoadBalance, Prng, TargetServerType, conn_hosts};
pub use lobj::{LoError, LoFuncs};
pub use message::{
    Backend, CopyFormat, Frame, Frontend, ProtocolError, Target, TransactionStatus,
    next_copy_frame, next_frame,
};
pub use negotiate::{
    AfterRefusal, Build, EncMethod, EncryptionOptions, GssEncMode, Negotiation, SslMode,
    SslNegotiation,
};
pub use pipeline::{
    AsyncStatus, CopyStep, Flow, PipelineError, PipelineState, PipelineStatus, QueryClass,
    QueryRunner, RowMode,
};
pub use print::{PrintOpt, display_tuples, print, print_tuples};
pub use regress::regress_report;
pub use result::{
    ContextVisibility, ExecStatus, FieldDescription, QueryResult, ResultError, Verbosity,
};
pub use scram::{Mechanism, ScramClient, ScramError};
pub use service::{Files, Filesystem, parse_service_file, parse_service_info};
pub use target::{CheckQuery, PgBool, ServerState, TargetCheck, TargetRejection, check_target};
pub use text::RawText;
pub use trace::{AuthResponse, Origin, TraceFlags};
pub use uri::{parse_uri, uri_decode};
