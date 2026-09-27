//! The front-end large object interface: `lo_open` … `lo_export`, each a
//! fast-path function call ([`Connection::fn_call`]) to the server function
//! of the same name.
//!
//! Ported from `src/interfaces/libpq/fe-lobj.c`. The function OIDs are looked
//! up once per connection with `lo_initialize`'s query (`:843`); turning its
//! rows into a [`LoFuncs`] is a pure function, and so is every message a
//! failure leaves ([`LoError::message`]).
//!
//! C returns -1 (or `InvalidOid`) and leaves the reason in
//! `conn->errorMessage`; here each call returns a `Result`, and
//! [`LoError::message`] is the text `PQerrorMessage` would then hold.
//! `lo_export` cannot see an error from closing its file, which `close(2)`
//! reports to C (`:822`): a Rust `File` closes on drop. Every write error
//! is still reported, from the write itself.

use std::io::{self, Read, Write};
use std::path::Path;

use crate::connection::{Connection, ConnectionError, FnResult};
use crate::message::ProtocolError;
use crate::result::{ExecStatus, QueryResult};

/// `INV_WRITE`, `src/include/libpq/libpq-fs.h:21`.
pub const INV_WRITE: i32 = 0x0002_0000;
/// `INV_READ`, `src/include/libpq/libpq-fs.h:22`.
pub const INV_READ: i32 = 0x0004_0000;
/// `SEEK_SET`, the `whence` of `lo_lseek` and `lo_lseek64`.
pub const SEEK_SET: i32 = 0;
/// `SEEK_CUR`.
pub const SEEK_CUR: i32 = 1;
/// `SEEK_END`.
pub const SEEK_END: i32 = 2;
/// `LO_BUFSIZE`, `fe-lobj.c:42`: the chunk `lo_import` and `lo_export` move.
pub const LO_BUFSIZE: usize = 8192;

/// `lo_initialize`'s query, `fe-lobj.c:879`-`:895`.
pub const LO_INITIALIZE_QUERY: &[u8] = b"select proname, oid from pg_catalog.pg_proc \
where proname in (\
'lo_open', \
'lo_close', \
'lo_creat', \
'lo_create', \
'lo_unlink', \
'lo_lseek', \
'lo_lseek64', \
'lo_tell', \
'lo_tell64', \
'lo_truncate', \
'lo_truncate64', \
'loread', \
'lowrite') \
and pronamespace = (select oid from pg_catalog.pg_namespace \
where nspname = 'pg_catalog')";

/// `PGlobjfuncs`, `libpq-int.h:278`: the OIDs of the server functions.
/// The ones every server since "the stone age" has are required
/// (`fe-lobj.c:951`); the later ones are `None` when missing and checked
/// only when used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoFuncs {
    pub lo_open: u32,
    pub lo_close: u32,
    pub lo_creat: u32,
    pub lo_create: Option<u32>,
    pub lo_unlink: u32,
    pub lo_lseek: u32,
    pub lo_lseek64: Option<u32>,
    pub lo_tell: u32,
    pub lo_tell64: Option<u32>,
    pub lo_truncate: Option<u32>,
    pub lo_truncate64: Option<u32>,
    /// `loread`.
    pub lo_read: u32,
    /// `lowrite`.
    pub lo_write: u32,
}

impl LoFuncs {
    /// `lo_initialize`'s loop and checks, `fe-lobj.c:915`-`:1009`: each
    /// `(proname, oid)` row fills its slot, then the required functions are
    /// checked in upstream's order, the first missing one named.
    ///
    /// # Errors
    /// A required function is missing.
    pub fn from_rows<'a>(
        rows: impl IntoIterator<Item = (&'a [u8], &'a [u8])>,
    ) -> Result<Self, LoError> {
        let mut found: [(&[u8], u32); 13] = [
            (b"lo_open", 0),
            (b"lo_close", 0),
            (b"lo_creat", 0),
            (b"lo_create", 0),
            (b"lo_unlink", 0),
            (b"lo_lseek", 0),
            (b"lo_lseek64", 0),
            (b"lo_tell", 0),
            (b"lo_tell64", 0),
            (b"lo_truncate", 0),
            (b"lo_truncate64", 0),
            (b"loread", 0),
            (b"lowrite", 0),
        ];
        for (name, oid) in rows {
            if let Some(slot) = found.iter_mut().find(|(n, _)| *n == name) {
                slot.1 = atoi_oid(oid);
            }
        }
        let [
            lo_open,
            lo_close,
            lo_creat,
            lo_create,
            lo_unlink,
            lo_lseek,
            lo_lseek64,
            lo_tell,
            lo_tell64,
            lo_truncate,
            lo_truncate64,
            lo_read,
            lo_write,
        ] = found.map(|(_, oid)| oid);
        // fe-lobj.c:954-:1009 — "loread" and "lowrite" by their SQL names.
        for (oid, name) in [
            (lo_open, "lo_open"),
            (lo_close, "lo_close"),
            (lo_creat, "lo_creat"),
            (lo_unlink, "lo_unlink"),
            (lo_lseek, "lo_lseek"),
            (lo_tell, "lo_tell"),
            (lo_read, "loread"),
            (lo_write, "lowrite"),
        ] {
            if oid == 0 {
                return Err(LoError::NoFunction(name));
            }
        }
        let optional = |oid: u32| (oid != 0).then_some(oid);
        Ok(LoFuncs {
            lo_open,
            lo_close,
            lo_creat,
            lo_create: optional(lo_create),
            lo_unlink,
            lo_lseek,
            lo_lseek64: optional(lo_lseek64),
            lo_tell,
            lo_tell64: optional(lo_tell64),
            lo_truncate: optional(lo_truncate),
            lo_truncate64: optional(lo_truncate64),
            lo_read,
            lo_write,
        })
    }
}

/// `(Oid) atoi(PQgetvalue(res, n, 1))`, `fe-lobj.c:918`, for the digits an
/// `oid` column prints as: leading digits, anything else 0.
fn atoi_oid(value: &[u8]) -> u32 {
    let digits = value.iter().take_while(|b| b.is_ascii_digit()).count();
    std::str::from_utf8(&value[..digits])
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

/// Why an `lo_*` call failed: what C leaves in `conn->errorMessage` when it
/// returns -1 or `InvalidOid`.
#[derive(Debug)]
pub enum LoError {
    /// The connection or the protocol failed under the call.
    Connection(ConnectionError),
    /// The server refused the function call: the FATAL_ERROR result `PQfn`
    /// returned.
    Server(Box<QueryResult>),
    /// `lo_initialize`'s query did not return rows (`fe-lobj.c:904`); the
    /// result is its error, which C leaves ahead of its own message.
    InitQuery(Box<QueryResult>),
    /// `fe-lobj.c:954`-`:1009` and the on-the-fly checks: "cannot determine
    /// OID of function %s".
    NoFunction(&'static str),
    /// `fe-lobj.c:160`, `:262`, `:313`: a length beyond the int32 the server
    /// function takes, "argument of %s exceeds integer range".
    ExceedsIntegerRange(&'static str),
    /// `fe-lobj.c:669`, `:780`.
    OpenFile { filename: Vec<u8>, err: io::Error },
    /// `fe-lobj.c:725`.
    ReadFile { filename: Vec<u8>, err: io::Error },
    /// `fe-lobj.c:801`.
    WriteFile { filename: Vec<u8>, err: io::Error },
    /// A failure C reports with nothing in `conn->errorMessage`: the server
    /// accepted fewer bytes than `lo_import` gave it (`fe-lobj.c:702`), or
    /// its `lo_close` returned nonzero without an error (`:733`, `:815`).
    NoMessage,
}

impl LoError {
    /// `PQerrorMessage` after the failed call, trailing newline included.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        let mut msg = match self {
            LoError::Connection(err) => err.message(),
            LoError::Server(result) => return result.error_message(),
            LoError::InitQuery(result) => {
                let mut msg = result.error_message();
                msg.extend_from_slice(
                    b"query to initialize large object functions did not return data",
                );
                msg
            }
            LoError::NoFunction(name) => {
                format!("cannot determine OID of function {name}").into_bytes()
            }
            LoError::ExceedsIntegerRange(name) => {
                format!("argument of {name} exceeds integer range").into_bytes()
            }
            LoError::OpenFile { filename, err } => file_message(b"open file", filename, err),
            LoError::ReadFile { filename, err } => file_message(b"read from file", filename, err),
            LoError::WriteFile { filename, err } => file_message(b"write to file", filename, err),
            LoError::NoMessage => return Vec::new(),
        };
        msg.push(b'\n');
        msg
    }
}

/// `"could not %s \"%s\": %s"`, the last `strerror_r(errno)`.
fn file_message(what: &[u8], filename: &[u8], err: &io::Error) -> Vec<u8> {
    let mut msg = b"could not ".to_vec();
    msg.extend_from_slice(what);
    msg.extend_from_slice(b" \"");
    msg.extend_from_slice(filename);
    msg.extend_from_slice(b"\": ");
    msg.extend_from_slice(strerror(err).as_bytes());
    msg
}

/// `strerror(errno)`: `std::io::Error`'s text without the ` (os error N)`
/// Rust appends.
fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    match err.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .unwrap_or(&text)
            .to_owned(),
        None => text,
    }
}

impl std::fmt::Display for LoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for LoError {}

impl From<ConnectionError> for LoError {
    fn from(err: ConnectionError) -> Self {
        LoError::Connection(err)
    }
}

/// A 4-byte integer argument, as `pqPutInt(value, 4, conn)` puts it.
fn int4(value: i32) -> [u8; 4] {
    value.to_be_bytes()
}

/// An Oid argument: C passes it in `u.integer`, the same four bytes.
fn oid4(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

/// An `int32` length argument, refused past `INT_MAX` as C refuses it.
fn int32_len(len: usize, function: &'static str) -> Result<[u8; 4], LoError> {
    i32::try_from(len)
        .map(int4)
        .map_err(|_| LoError::ExceedsIntegerRange(function))
}

/// `PQresultStatus(res) == PGRES_COMMAND_OK`, else the result is the error.
fn command_ok(call: FnResult) -> Result<Option<Vec<u8>>, LoError> {
    if call.result.status() == ExecStatus::CommandOk {
        Ok(call.value)
    } else {
        Err(LoError::Server(Box::new(call.result)))
    }
}

/// `pqGetInt(result_buf, 4, conn)` on the value.
fn int4_value(value: Option<Vec<u8>>) -> Result<i32, LoError> {
    value
        .and_then(|v| <[u8; 4]>::try_from(v).ok())
        .map(i32::from_be_bytes)
        .ok_or_else(|| ConnectionError::from(ProtocolError::InsufficientData(b'V')).into())
}

/// `result_len == 8` and `lo_ntoh64` (`fe-lobj.c:1048`).
fn int8_value(value: Option<Vec<u8>>) -> Result<i64, LoError> {
    value
        .and_then(|v| <[u8; 8]>::try_from(v).ok())
        .map(i64::from_be_bytes)
        .ok_or_else(|| ConnectionError::from(ProtocolError::InsufficientData(b'V')).into())
}

fn filename_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_encoded_bytes().to_vec()
}

impl<S: Read + Write> Connection<S> {
    /// `lo_initialize`, `fe-lobj.c:843`: the function OIDs, looked up on the
    /// first call and kept for the connection's life.
    fn lo_initialize(&mut self) -> Result<LoFuncs, LoError> {
        if let Some(funcs) = self.lobjfuncs {
            return Ok(funcs);
        }
        let res = self
            .exec(LO_INITIALIZE_QUERY)?
            .pop()
            .unwrap_or_else(|| QueryResult::new(ExecStatus::FatalError));
        if res.status() != ExecStatus::TuplesOk {
            return Err(LoError::InitQuery(Box::new(res)));
        }
        let rows = (0..res.ntuples()).map(|row| {
            (
                res.value(row, 0).unwrap_or_default(),
                res.value(row, 1).unwrap_or_default(),
            )
        });
        let funcs = LoFuncs::from_rows(rows)?;
        self.lobjfuncs = Some(funcs);
        Ok(funcs)
    }

    /// `PQfn` with an integer result.
    fn lo_call_int(&mut self, fnid: u32, args: &[&[u8]]) -> Result<i32, LoError> {
        let args: Vec<Option<&[u8]>> = args.iter().copied().map(Some).collect();
        int4_value(command_ok(self.fn_call(fnid, &args)?)?)
    }

    /// `PQnfn` with an 8-byte result buffer.
    fn lo_call_int8(&mut self, fnid: u32, args: &[&[u8]]) -> Result<i64, LoError> {
        let args: Vec<Option<&[u8]>> = args.iter().copied().map(Some).collect();
        int8_value(command_ok(self.nfn(fnid, &args, Some(8))?)?)
    }

    /// `lo_open`, `fe-lobj.c:57`: a descriptor for large object `lobj_id`,
    /// opened with `mode` ([`INV_READ`], [`INV_WRITE`] or both).
    ///
    /// # Errors
    /// As every `lo_*` call: the lookup failed, the server refused, or the
    /// connection broke.
    pub fn lo_open(&mut self, lobj_id: u32, mode: i32) -> Result<i32, LoError> {
        let funcs = self.lo_initialize()?;
        self.lo_call_int(funcs.lo_open, &[&oid4(lobj_id), &int4(mode)])
    }

    /// `lo_close`, `fe-lobj.c:96`.
    ///
    /// # Errors
    /// As [`Connection::lo_open`].
    pub fn lo_close(&mut self, fd: i32) -> Result<i32, LoError> {
        let funcs = self.lo_initialize()?;
        self.lo_call_int(funcs.lo_close, &[&int4(fd)])
    }

    /// `lo_truncate`, `fe-lobj.c:131`.
    ///
    /// # Errors
    /// As [`Connection::lo_open`]; a `len` past `INT_MAX`, or a server
    /// without `lo_truncate`, is refused before anything is sent.
    pub fn lo_truncate(&mut self, fd: i32, len: usize) -> Result<i32, LoError> {
        let funcs = self.lo_initialize()?;
        let fnid = funcs
            .lo_truncate
            .ok_or(LoError::NoFunction("lo_truncate"))?;
        let len = int32_len(len, "lo_truncate")?;
        self.lo_call_int(fnid, &[&int4(fd), &len])
    }

    /// `lo_truncate64`, `fe-lobj.c:195`.
    ///
    /// # Errors
    /// As [`Connection::lo_open`], or a server without `lo_truncate64`.
    pub fn lo_truncate64(&mut self, fd: i32, len: i64) -> Result<i32, LoError> {
        let funcs = self.lo_initialize()?;
        let fnid = funcs
            .lo_truncate64
            .ok_or(LoError::NoFunction("lo_truncate64"))?;
        self.lo_call_int(fnid, &[&int4(fd), &len.to_be_bytes()])
    }

    /// `lo_read`, `fe-lobj.c:245`: up to `buf.len()` bytes into `buf`; the
    /// number read, 0 at the end.
    ///
    /// # Errors
    /// As [`Connection::lo_open`]; a `buf` longer than `INT_MAX` is refused
    /// before anything is sent, and a server that returns more than asked
    /// for breaks the connection ("server returned too much data").
    pub fn lo_read(&mut self, fd: i32, buf: &mut [u8]) -> Result<usize, LoError> {
        let funcs = self.lo_initialize()?;
        let len = int32_len(buf.len(), "lo_read")?;
        let args: [Option<&[u8]>; 2] = [Some(&int4(fd)), Some(&len)];
        let call = self.nfn(funcs.lo_read, &args, Some(buf.len()))?;
        Ok(read_into(buf, command_ok(call)?))
    }

    /// `lo_write`, `fe-lobj.c:295`: the number of bytes written.
    ///
    /// # Errors
    /// As [`Connection::lo_read`].
    pub fn lo_write(&mut self, fd: i32, buf: &[u8]) -> Result<i32, LoError> {
        let funcs = self.lo_initialize()?;
        int32_len(buf.len(), "lo_write")?;
        self.lo_call_int(funcs.lo_write, &[&int4(fd), buf])
    }

    /// `lo_lseek`, `fe-lobj.c:344`: the new position.
    ///
    /// # Errors
    /// As [`Connection::lo_open`].
    pub fn lo_lseek(&mut self, fd: i32, offset: i32, whence: i32) -> Result<i32, LoError> {
        let funcs = self.lo_initialize()?;
        self.lo_call_int(funcs.lo_lseek, &[&int4(fd), &int4(offset), &int4(whence)])
    }

    /// `lo_lseek64`, `fe-lobj.c:385`.
    ///
    /// # Errors
    /// As [`Connection::lo_open`], or a server without `lo_lseek64`.
    pub fn lo_lseek64(&mut self, fd: i32, offset: i64, whence: i32) -> Result<i64, LoError> {
        let funcs = self.lo_initialize()?;
        let fnid = funcs.lo_lseek64.ok_or(LoError::NoFunction("lo_lseek64"))?;
        self.lo_call_int8(fnid, &[&int4(fd), &offset.to_be_bytes(), &int4(whence)])
    }

    /// `lo_creat`, `fe-lobj.c:438`: a new large object's OID. `mode` is
    /// ignored by the server, "once upon a time it had a use".
    ///
    /// # Errors
    /// As [`Connection::lo_open`].
    pub fn lo_creat(&mut self, mode: i32) -> Result<u32, LoError> {
        let funcs = self.lo_initialize()?;
        self.lo_call_int(funcs.lo_creat, &[&int4(mode)])
            .map(i32::cast_unsigned)
    }

    /// `lo_create`, `fe-lobj.c:474`: a new large object, with the OID
    /// `lobj_id` unless that is 0.
    ///
    /// # Errors
    /// As [`Connection::lo_open`], or a server without `lo_create`.
    pub fn lo_create(&mut self, lobj_id: u32) -> Result<u32, LoError> {
        let funcs = self.lo_initialize()?;
        let fnid = funcs.lo_create.ok_or(LoError::NoFunction("lo_create"))?;
        self.lo_call_int(fnid, &[&oid4(lobj_id)])
            .map(i32::cast_unsigned)
    }

    /// `lo_tell`, `fe-lobj.c:515`.
    ///
    /// # Errors
    /// As [`Connection::lo_open`].
    pub fn lo_tell(&mut self, fd: i32) -> Result<i32, LoError> {
        let funcs = self.lo_initialize()?;
        self.lo_call_int(funcs.lo_tell, &[&int4(fd)])
    }

    /// `lo_tell64`, `fe-lobj.c:548`.
    ///
    /// # Errors
    /// As [`Connection::lo_open`], or a server without `lo_tell64`.
    pub fn lo_tell64(&mut self, fd: i32) -> Result<i64, LoError> {
        let funcs = self.lo_initialize()?;
        let fnid = funcs.lo_tell64.ok_or(LoError::NoFunction("lo_tell64"))?;
        self.lo_call_int8(fnid, &[&int4(fd)])
    }

    /// `lo_unlink`, `fe-lobj.c:589`.
    ///
    /// # Errors
    /// As [`Connection::lo_open`].
    pub fn lo_unlink(&mut self, lobj_id: u32) -> Result<i32, LoError> {
        let funcs = self.lo_initialize()?;
        self.lo_call_int(funcs.lo_unlink, &[&oid4(lobj_id)])
    }

    /// `lo_import`, `fe-lobj.c:626`: a new large object holding the file's
    /// bytes, and its OID.
    ///
    /// # Errors
    /// The file could not be opened or read, or as [`Connection::lo_open`].
    pub fn lo_import(&mut self, filename: &Path) -> Result<u32, LoError> {
        self.lo_import_internal(filename, 0)
    }

    /// `lo_import_with_oid`, `fe-lobj.c:641`: [`Connection::lo_import`]
    /// into the OID `lobj_id`, unless that is 0.
    ///
    /// # Errors
    /// As [`Connection::lo_import`].
    pub fn lo_import_with_oid(&mut self, filename: &Path, lobj_id: u32) -> Result<u32, LoError> {
        self.lo_import_internal(filename, lobj_id)
    }

    /// `lo_import_internal`, `fe-lobj.c:647`.
    fn lo_import_internal(&mut self, filename: &Path, oid: u32) -> Result<u32, LoError> {
        // fe-lobj.c:666 — the file is opened before anything is sent.
        let mut file = std::fs::File::open(filename).map_err(|err| LoError::OpenFile {
            filename: filename_bytes(filename),
            err,
        })?;
        let lobj_oid = if oid == 0 {
            self.lo_creat(INV_READ | INV_WRITE)?
        } else {
            self.lo_create(oid)?
        };
        let lobj = self.lo_open(lobj_oid, INV_WRITE)?;

        let mut buf = vec![0u8; LO_BUFSIZE];
        loop {
            let nbytes = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    // fe-lobj.c:718 — close first, then deliberately
                    // overwrite any error from lo_close.
                    let _ = self.lo_close(lobj);
                    return Err(LoError::ReadFile {
                        filename: filename_bytes(filename),
                        err,
                    });
                }
            };
            // fe-lobj.c:706 — a failed lo_write leaves the transaction
            // aborted, so there is no lo_close.
            let written = self.lo_write(lobj, &buf[..nbytes])?;
            if usize::try_from(written).ok() != Some(nbytes) {
                return Err(LoError::NoMessage);
            }
        }
        drop(file);

        if self.lo_close(lobj)? != 0 {
            // fe-lobj.c:733 — a nonzero lo_close is failure with whatever
            // message it left, none.
            return Err(LoError::NoMessage);
        }
        Ok(lobj_oid)
    }

    /// `lo_export`, `fe-lobj.c:748`: write large object `lobj_id` to the
    /// file, created or truncated.
    ///
    /// # Errors
    /// The file could not be created or written, or as
    /// [`Connection::lo_open`].
    pub fn lo_export(&mut self, lobj_id: u32, filename: &Path) -> Result<(), LoError> {
        let lobj = self.lo_open(lobj_id, INV_READ)?;
        let mut file = match std::fs::File::create(filename) {
            Ok(file) => file,
            Err(err) => {
                let _ = self.lo_close(lobj);
                return Err(LoError::OpenFile {
                    filename: filename_bytes(filename),
                    err,
                });
            }
        };
        let mut buf = vec![0u8; LO_BUFSIZE];
        loop {
            // fe-lobj.c:809 — a failed lo_read leaves the transaction
            // aborted, so there is no lo_close.
            let nbytes = self.lo_read(lobj, &mut buf)?;
            if nbytes == 0 {
                break;
            }
            if let Err(err) = file.write_all(&buf[..nbytes]) {
                let _ = self.lo_close(lobj);
                return Err(LoError::WriteFile {
                    filename: filename_bytes(filename),
                    err,
                });
            }
        }
        if self.lo_close(lobj)? != 0 {
            return Err(LoError::NoMessage);
        }
        Ok(())
    }
}

/// `lo_read`'s value copied into `buf`: its length, and 0 for a NULL, where
/// C returns `PQfn`'s `result_len` of -1 (`fe-protocol3.c:2291`,
/// `fe-lobj.c:279`); see `docs/divergences.md`. `nfn` has already refused
/// a value longer than `buf`.
fn read_into(buf: &mut [u8], value: Option<Vec<u8>>) -> usize {
    let value = value.unwrap_or_default();
    buf[..value.len()].copy_from_slice(&value);
    value.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_function() -> Vec<(&'static [u8], &'static [u8])> {
        vec![
            (b"lo_open", b"952"),
            (b"lo_close", b"953"),
            (b"lo_creat", b"957"),
            (b"lo_create", b"715"),
            (b"lo_unlink", b"964"),
            (b"lo_lseek", b"956"),
            (b"lo_lseek64", b"3170"),
            (b"lo_tell", b"958"),
            (b"lo_tell64", b"3171"),
            (b"lo_truncate", b"1004"),
            (b"lo_truncate64", b"3172"),
            (b"loread", b"954"),
            (b"lowrite", b"955"),
        ]
    }

    /// `lo_initialize`'s loop, `fe-lobj.c:915`: each row fills the slot of
    /// its name, whatever the order.
    #[test]
    fn every_row_fills_its_slot() {
        let mut rows = every_function();
        rows.reverse();
        let funcs = LoFuncs::from_rows(rows).unwrap();
        assert_eq!(funcs.lo_open, 952);
        assert_eq!(funcs.lo_read, 954);
        assert_eq!(funcs.lo_write, 955);
        assert_eq!(funcs.lo_create, Some(715));
        assert_eq!(funcs.lo_lseek64, Some(3170));
        assert_eq!(funcs.lo_truncate64, Some(3172));
    }

    /// `fe-lobj.c:954`-`:1009`: the required functions are checked in
    /// upstream's order, and the first missing one is named — `loread` and
    /// `lowrite` by their SQL names.
    #[test]
    fn the_first_missing_required_function_is_named() {
        for (missing, named) in [
            (&b"lo_open"[..], "lo_open"),
            (b"lo_tell", "lo_tell"),
            (b"loread", "loread"),
            (b"lowrite", "lowrite"),
        ] {
            let rows = every_function().into_iter().filter(|(n, _)| *n != missing);
            let err = LoFuncs::from_rows(rows).unwrap_err();
            assert_eq!(
                err.message(),
                format!("cannot determine OID of function {named}\n").into_bytes()
            );
        }
        let rows = every_function()
            .into_iter()
            .filter(|(n, _)| !matches!(*n, b"lo_close" | b"lo_unlink"));
        assert_eq!(
            LoFuncs::from_rows(rows).unwrap_err().message(),
            b"cannot determine OID of function lo_close\n"
        );
    }

    /// The later functions are only checked when used (`fe-lobj.c:141`).
    #[test]
    fn a_missing_later_function_is_none() {
        let rows = every_function()
            .into_iter()
            .filter(|(n, _)| !n.ends_with(b"64") && *n != b"lo_create");
        let funcs = LoFuncs::from_rows(rows).unwrap();
        assert_eq!(funcs.lo_create, None);
        assert_eq!(funcs.lo_lseek64, None);
        assert_eq!(funcs.lo_tell64, None);
        assert_eq!(funcs.lo_truncate64, None);
        assert_eq!(funcs.lo_truncate, Some(1004));
    }

    /// The client-side messages, each with the newline
    /// `libpq_append_conn_error` adds.
    #[test]
    fn the_client_side_messages_are_upstreams() {
        assert_eq!(
            LoError::ExceedsIntegerRange("lo_read").message(),
            b"argument of lo_read exceeds integer range\n"
        );
        assert_eq!(
            LoError::OpenFile {
                filename: b"/nonexistent/x".to_vec(),
                err: io::Error::from_raw_os_error(2),
            }
            .message(),
            b"could not open file \"/nonexistent/x\": No such file or directory\n"
        );
        assert_eq!(
            LoError::InitQuery(Box::new(QueryResult::new(ExecStatus::CommandOk))).message(),
            b"query to initialize large object functions did not return data\n"
        );
        assert!(LoError::NoMessage.message().is_empty());
    }

    /// `lo_truncate`'s and `lo_read`'s guard: the server function takes a
    /// signed int32 length.
    #[test]
    fn a_length_past_int_max_is_refused() {
        assert_eq!(
            int32_len(0x7fff_ffff, "lo_read").unwrap(),
            [0x7f, 0xff, 0xff, 0xff]
        );
        let too_long = usize::try_from(i64::from(i32::MAX) + 1).unwrap();
        assert!(matches!(
            int32_len(too_long, "lo_truncate"),
            Err(LoError::ExceedsIntegerRange("lo_truncate"))
        ));
    }

    /// `lo_hton64` / `lo_ntoh64`, `fe-lobj.c:1023`: most significant half
    /// first, which is plain network order.
    #[test]
    fn a_64_bit_value_is_sent_most_significant_half_first() {
        assert_eq!(
            4_294_967_000i64.to_be_bytes(),
            [0, 0, 0, 0, 0xff, 0xff, 0xfe, 0xd8]
        );
        assert_eq!(
            int8_value(Some(vec![0, 0, 0, 1, 0, 0, 0, 2])).unwrap(),
            (1i64 << 32) + 2
        );
        assert!(int8_value(Some(vec![0; 4])).is_err());
        assert_eq!(int4_value(Some(vec![0xff; 4])).unwrap(), -1);
    }

    #[test]
    fn a_null_lo_read_value_reads_nothing() {
        let mut buf = [7u8; 4];
        assert_eq!(read_into(&mut buf, None), 0);
        assert_eq!(buf, [7; 4]);
        assert_eq!(read_into(&mut buf, Some(b"ab".to_vec())), 2);
        assert_eq!(buf, [b'a', b'b', 7, 7]);
    }

    #[test]
    fn atoi_reads_the_leading_digits() {
        assert_eq!(atoi_oid(b"952"), 952);
        assert_eq!(atoi_oid(b"4294967295"), u32::MAX);
        assert_eq!(atoi_oid(b"x"), 0);
    }
}
