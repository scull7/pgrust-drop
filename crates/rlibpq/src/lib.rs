//! rlibpq: the client side of libpq in pure Rust.
//!
//! Tracks PostgreSQL 18.6 `src/interfaces/libpq/` and the requirements in
//! pgrust issue #40: one codebase producing a native Rust crate and a drop-in
//! C-ABI `libpq.so`, with GSSAPI feature-gated and no libc/OpenSSL build-time
//! coupling.
//!
//! What is here so far is the connection-string front end
//! (`PQconninfoOptions[]` and the two parsers that fill a working copy of it,
//! proved against `t/001_uri.pl`) and the protocol version 3 core: the wire
//! messages, the authentication methods a build without TLS or GSSAPI can do
//! (trust, password, md5, SCRAM-SHA-256), and a blocking `Connection` that
//! runs simple queries and the extended-query commands outside pipeline mode
//! (`PQexecParams`, `PQprepare`, `PQexecPrepared`, `PQdescribePrepared`,
//! `PQdescribePortal`, `PQclosePrepared`, `PQclosePortal`). The rest is
//! tracked in Linear NAT-390 … NAT-396.
//!
//! The C ABI layer will need `unsafe`; the pure-Rust core must not, so the
//! crate denies it until that layer exists as its own module.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod auth;
pub mod base64;
pub mod connection;
pub mod conninfo;
mod cstr;
pub mod error;
pub mod extended;
pub mod hmac;
pub mod md5;
pub mod message;
pub mod pg_config;
pub mod regress;
pub mod result;
pub mod scram;
pub mod sha256;
mod text;
pub mod uri;

pub use auth::{AuthError, AuthRequest, AuthStep, Authenticator, ChannelBinding};
pub use connection::{
    Address, Connection, ConnectionError, QueryClass, QueryRunner, Stream, socket_address,
};
pub use conninfo::{
    CONNINFO_OPTIONS, ConnInfo, ConnOption, ConnOptionDef, Dispchar, Env, UnknownKeyword,
    conndefaults, parse_conninfo, parse_keyword_value, recognized_connection_string,
    uri_prefix_length,
};
pub use error::ConnError;
pub use extended::{ArgumentError, Format, PQ_QUERY_PARAM_MAX_LIMIT, Params};
pub use message::{Backend, Frame, Frontend, ProtocolError, Target, TransactionStatus, next_frame};
pub use regress::regress_report;
pub use result::{
    ContextVisibility, ExecStatus, FieldDescription, QueryResult, ResultError, Verbosity,
};
pub use scram::{Mechanism, ScramClient, ScramError};
pub use text::RawText;
pub use uri::{parse_uri, uri_decode};
