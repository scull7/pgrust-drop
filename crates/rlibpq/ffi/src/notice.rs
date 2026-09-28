//! The notice hooks of `fe-connect.c` and `fe-exec.c`: `PQsetNoticeReceiver`,
//! `PQsetNoticeProcessor`, the two defaults behind them, and
//! `pqInternalNotice`, which raises a notice libpq makes itself.
//!
//! A `PGconn` holds its hooks (`conn->noticeHooks`, `libpq-int.h:452`) and
//! every result made on it copies them (`PQmakeEmptyPGresult`,
//! `fe-exec.c:192`), so a result reports its own notices to the hooks that
//! were set when it was made. A notice is a `PGRES_NONFATAL_ERROR` result
//! handed to the receiver; the default receiver passes its message to the
//! processor, and the default processor prints it to stderr.

use std::ffi::{CStr, c_char, c_void};
use std::io::Write as _;

use crate::conn::PGconn;
use crate::result::{PGresult, PQresultErrorMessage};

/// `PQnoticeReceiver`, `libpq-fe.h:241`.
pub type PQnoticeReceiver = unsafe extern "C" fn(arg: *mut c_void, res: *const PGresult);

/// `PQnoticeProcessor`, `libpq-fe.h:242`.
pub type PQnoticeProcessor = unsafe extern "C" fn(arg: *mut c_void, message: *const c_char);

/// `PGNoticeHooks`, `libpq-int.h:147`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NoticeHooks {
    receiver: Option<PQnoticeReceiver>,
    receiver_arg: *mut c_void,
    processor: Option<PQnoticeProcessor>,
    processor_arg: *mut c_void,
}

impl NoticeHooks {
    /// What `pqMakeEmptyPGconn` installs (`fe-connect.c:4976`-`:4977`).
    pub(crate) const DEFAULT: NoticeHooks = NoticeHooks {
        receiver: Some(default_notice_receiver),
        receiver_arg: std::ptr::null_mut(),
        processor: Some(default_notice_processor),
        processor_arg: std::ptr::null_mut(),
    };

    /// A result made with no connection: `PQmakeEmptyPGresult` zeroes the
    /// hooks (`fe-exec.c:228`-`:231`), so its notices go nowhere. Only the
    /// tests make one: every result the shims make has a connection.
    #[cfg(test)]
    pub(crate) const NONE: NoticeHooks = NoticeHooks {
        receiver: None,
        receiver_arg: std::ptr::null_mut(),
        processor: None,
        processor_arg: std::ptr::null_mut(),
    };
}

/// Action: hand `notice` to the receiver it carries, if it has one — the
/// call `pqGetErrorNotice3` (`fe-protocol3.c:1012`) and `pqInternalNotice`
/// (`fe-exec.c:986`) make.
pub(crate) fn receive(notice: &PGresult) {
    let hooks = notice.hooks;
    if let Some(receiver) = hooks.receiver {
        // SAFETY: the receiver and its argument are the pair
        // `PQsetNoticeReceiver` was given, whose caller promised the call,
        // or the default.
        unsafe { receiver(hooks.receiver_arg, notice) };
    }
}

/// Action: `pqInternalNotice`, `fe-exec.c:944`: raise `text`, a primary
/// message without its newline, through `hooks`. Nothing when there is no
/// receiver (`:950`).
pub(crate) fn internal_notice(hooks: &NoticeHooks, text: &[u8]) {
    if hooks.receiver.is_some() {
        receive(&PGresult::internal_notice(text, *hooks));
    }
}

/// `defaultNoticeReceiver`, `fe-connect.c:7842`: pass the notice's message
/// to the processor the notice carries.
///
/// # Safety
///
/// `res` is a live `PGresult` from this library.
unsafe extern "C" fn default_notice_receiver(_arg: *mut c_void, res: *const PGresult) {
    // SAFETY: the caller's contract.
    let Some(notice) = (unsafe { res.as_ref() }) else {
        return;
    };
    if let Some(processor) = notice.hooks.processor {
        // SAFETY: as in `receive`, for the processor; the message is the
        // notice's own, NUL-terminated.
        unsafe { processor(notice.hooks.processor_arg, PQresultErrorMessage(res)) };
    }
}

/// `defaultNoticeProcessor`, `fe-connect.c:7857`: print the message, which
/// ends in its own newline, to stderr.
///
/// # Safety
///
/// `message` is null or a NUL-terminated string.
unsafe extern "C" fn default_notice_processor(_arg: *mut c_void, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: the caller's contract.
    let message = unsafe { CStr::from_ptr(message) };
    let _ = std::io::stderr().lock().write_all(message.to_bytes());
}

/// `PQsetNoticeReceiver`, `fe-connect.c:7802`: install `proc` and its `arg`
/// unless `proc` is NULL, and return the receiver that was installed; NULL
/// for a NULL `conn`.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `proc` is null or a
/// function that may be called with `arg` and a notice for as long as it is
/// installed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsetNoticeReceiver(
    conn: *mut PGconn,
    proc: Option<PQnoticeReceiver>,
    arg: *mut c_void,
) -> Option<PQnoticeReceiver> {
    // SAFETY: the caller's contract.
    let conn = unsafe { conn.as_mut() }?;
    let old = conn.hooks.receiver;
    if proc.is_some() {
        conn.hooks.receiver = proc;
        conn.hooks.receiver_arg = arg;
    }
    old
}

/// `PQsetNoticeProcessor`, `fe-connect.c:7819`: as
/// [`PQsetNoticeReceiver`], for the processor.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `proc` is null or a
/// function that may be called with `arg` and a message for as long as it
/// is installed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsetNoticeProcessor(
    conn: *mut PGconn,
    proc: Option<PQnoticeProcessor>,
    arg: *mut c_void,
) -> Option<PQnoticeProcessor> {
    // SAFETY: the caller's contract.
    let conn = unsafe { conn.as_mut() }?;
    let old = conn.hooks.processor;
    if proc.is_some() {
        conn.hooks.processor = proc;
        conn.hooks.processor_arg = arg;
    }
    old
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;

    thread_local! {
        static SEEN: RefCell<Vec<(usize, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn record(arg: *mut c_void, message: *const c_char) {
        // SAFETY: the processor is called with the message, NUL-terminated.
        let message = unsafe { CStr::from_ptr(message) }.to_bytes().to_vec();
        SEEN.with(|seen| seen.borrow_mut().push((arg.addr(), message)));
    }

    /// `pqInternalNotice`: the primary message plus a newline, through the
    /// default receiver to the processor with its argument; nothing with no
    /// receiver.
    #[test]
    fn an_internal_notice_reaches_the_processor_through_the_default_receiver() {
        let hooks = NoticeHooks {
            processor: Some(record),
            processor_arg: std::ptr::without_provenance_mut(7),
            ..NoticeHooks::DEFAULT
        };
        internal_notice(&hooks, b"row number 5 is out of range 0..0");
        internal_notice(&NoticeHooks::NONE, b"nobody home");
        SEEN.with(|seen| {
            assert_eq!(
                *seen.borrow(),
                [(7, b"row number 5 is out of range 0..0\n".to_vec())]
            );
        });
    }
}
