//! Ctrl-C: `src/fe_utils/cancel.c` and psql's callback into it,
//! `psql_cancel_callback` (`src/bin/psql/common.c:311`).
//!
//! Upstream installs `handle_sigint` for SIGINT (`cancel.c:153`, through
//! `pqsignal`, so with `SA_RESTART`) once the connection is up
//! (`startup.c:314`). The handler sets `cancel_pressed` (`common.c:323`) and,
//! when a query is running, sends the cancel request from inside the handler
//! with `PQcancel` and reports on stderr with a bare `write()`:
//! `Cancel request sent` or `Could not send cancel request: ` and `PQcancel`'s
//! message (`cancel.c:163`-`:173`). `SendQuery` and `PSQLexec` bracket every
//! query with `SetCancelConn` / `ResetCancelConn` (`common.c:1173`, `:1309`,
//! `:686`, `:690`) so that the handler knows which backend to cancel.
//!
//! `PQcancel` is written to be async-signal-safe; [`rlibpq::Cancel::cancel`]
//! is not — `std`'s socket calls allocate. So the handler here does only what
//! is safe in a handler — it stores `cancel_pressed` and writes one byte to a
//! non-blocking socket — and a thread of its own, woken by that byte, sends
//! the request and writes the report, as the Windows arm of `cancel.c` does
//! from its console-handler thread (`cancel.c:195`-`:224`, holding
//! `cancelConnLock` as [`CANCEL_CONN`]'s mutex is held here). ADR-0009
//! records why the handler needs the crate's only `unsafe` code, and
//! `docs/divergences.md` the window the thread opens.
//!
//! Data and calculations: [`CANCEL_PRESSED`], [`cancel_report`]. Actions:
//! [`setup_cancel_handler`], [`set_cancel_conn`], [`reset_cancel_conn`].

use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rlibpq::{Cancel, CancelError};

/// `cancel_pressed` (`common.c:323`): set by every SIGINT, read and cleared
/// by `MainLoop` (`mainloop.c:88`-`:100`).
pub static CANCEL_PRESSED: AtomicBool = AtomicBool::new(false);

/// `cancelConn` (`cancel.c:43`): what the handler's thread cancels, `None`
/// while no query is running.
static CANCEL_CONN: Mutex<Option<Cancel>> = Mutex::new(None);

fn cancel_conn() -> MutexGuard<'static, Option<Cancel>> {
    // The value is replaced whole, never left half-written, so a panic while
    // it was held cannot have broken it.
    CANCEL_CONN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `SetCancelConn(pset.db)` (`cancel.c:77`): from now on a SIGINT cancels
/// through `cancel`. `None` is a connection with no socket, for which
/// `PQgetCancel` returns NULL (`fe-cancel.c:377`).
pub fn set_cancel_conn(cancel: Option<Cancel>) {
    *cancel_conn() = cancel;
}

/// `ResetCancelConn()` (`cancel.c:107`): no query is running any more.
pub fn reset_cancel_conn() {
    *cancel_conn() = None;
}

/// Calculation: what `handle_sigint` writes to stderr after `PQcancel`
/// (`cancel.c:165`-`:173`, with the messages of `:186`-`:187`).
#[must_use]
pub fn cancel_report(outcome: &Result<(), CancelError>) -> Vec<u8> {
    match outcome {
        Ok(()) => b"Cancel request sent\n".to_vec(),
        Err(err) => {
            let mut report = b"Could not send cancel request: ".to_vec();
            report.extend_from_slice(&err.message());
            report
        }
    }
}

/// Action: the body of `handle_sigint` after the callback — cancel the
/// running query, if there is one, and say how that went.
#[cfg(unix)]
fn send_cancel() {
    // Held across the request, as `cancelConnLock` is (`cancel.c:208`-`:222`):
    // the query cannot be replaced by the next one while it is cancelled.
    let conn = cancel_conn();
    if let Some(cancel) = conn.as_ref() {
        sys::write_stderr(&cancel_report(&cancel.cancel()));
    }
}

/// `psql_setup_cancel_handler()` (`common.c:327`): install the SIGINT
/// handler. Only the first call does anything. A failure to install leaves
/// SIGINT's default action in place, as a failed `pqsignal` does upstream.
pub fn setup_cancel_handler() {
    #[cfg(unix)]
    {
        static SETUP: std::sync::Once = std::sync::Once::new();
        SETUP.call_once(unix::setup);
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::c_int;
    use std::os::fd::IntoRawFd as _;
    use std::os::unix::net::UnixDatagram;
    use std::sync::atomic::{AtomicI32, Ordering};

    use super::{CANCEL_PRESSED, send_cancel, sys};

    /// The write end of the wake-up socket, for the handler; `-1` until set.
    static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

    /// `handle_sigint` (`cancel.c:153`) up to the point where it would call
    /// `PQcancel`: `psql_cancel_callback`'s `cancel_pressed = true`, then one
    /// byte to wake the thread that sends the request. Atomics and `write()`
    /// are all it touches; `errno` is saved and restored around it, as
    /// `pqsignal`'s `wrapper_handler` does (`src/port/pqsignal.c:88`, `:112`).
    extern "C" fn handle_sigint(_signo: c_int) {
        sys::preserving_errno(|| {
            CANCEL_PRESSED.store(true, Ordering::SeqCst);
            // The socket is non-blocking, so a full buffer — a pile of
            // unanswered SIGINTs — drops this byte rather than hanging here.
            sys::write(WAKE_FD.load(Ordering::SeqCst), &[0]);
        });
    }

    pub(super) fn setup() {
        let Ok((wake, woken)) = UnixDatagram::pair() else {
            return;
        };
        if wake.set_nonblocking(true).is_err() {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("psql-cancel".to_string())
            .spawn(move || {
                let mut byte = [0u8; 1];
                loop {
                    match woken.recv(&mut byte) {
                        Ok(_) => send_cancel(),
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => return,
                    }
                }
            });
        if spawned.is_err() {
            return;
        }
        // The descriptor stays open for the life of the process: the handler
        // may run at any moment until it exits.
        WAKE_FD.store(wake.into_raw_fd(), Ordering::SeqCst);
        sys::install_sigint(handle_sigint);
    }
}

/// The C library calls the handler needs and `std` does not offer: installing
/// a signal handler, a `write()` safe to make inside one, and `errno`.
///
/// Declared here rather than taken from the `libc` crate, which is not an
/// approved dependency (AGENTS.md); each signature is the same on glibc, musl
/// and Darwin, the three targets rpsql ships to (ADR-0007), and so is
/// `SIGINT`'s number. ADR-0009.
#[cfg(unix)]
#[allow(unsafe_code)]
mod sys {
    use std::ffi::{c_int, c_void};

    /// `SIGINT` on Linux and Darwin alike.
    const SIGINT: c_int = 2;
    /// `SIG_ERR`, `(void (*)(int)) -1`.
    const SIG_ERR: usize = usize::MAX;

    unsafe extern "C" {
        // `sighandler_t signal(int, sighandler_t)`: a function pointer in, a
        // function pointer (or `SIG_ERR`) out; `usize` has its size.
        fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> usize;
        #[link_name = "write"]
        fn c_write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        #[cfg_attr(
            any(target_os = "linux", target_os = "android"),
            link_name = "__errno_location"
        )]
        #[cfg_attr(
            any(target_os = "macos", target_os = "ios", target_os = "freebsd"),
            link_name = "__error"
        )]
        fn errno_location() -> *mut c_int;
    }

    /// `pqsignal(SIGINT, handler)` (`cancel.c:189`). `signal()` has BSD
    /// semantics on all three C libraries — the handler stays installed and
    /// interrupted system calls restart — which is what `pqsignal` asks
    /// `sigaction` for with `SA_RESTART` (`pqsignal.c:141`).
    pub(super) fn install_sigint(handler: extern "C" fn(c_int)) -> bool {
        // SAFETY: `handler` is an `extern "C"` function that touches only
        // atomics, `write()` and `errno`, all async-signal-safe.
        unsafe { signal(SIGINT, handler) != SIG_ERR }
    }

    /// One `write()`, its result ignored, as `write_stderr` does
    /// (`cancel.c:31`-`:36`).
    pub(super) fn write(fd: c_int, bytes: &[u8]) {
        // SAFETY: the pointer and length describe `bytes`, which outlives the
        // call; a bad descriptor is an `EBADF`, not undefined behaviour.
        let _ = unsafe { c_write(fd, bytes.as_ptr().cast(), bytes.len()) };
    }

    /// `write_stderr` (`cancel.c:31`): straight to descriptor 2, past `std`'s
    /// `Stderr` lock, which the main thread may be holding.
    pub(super) fn write_stderr(bytes: &[u8]) {
        write(2, bytes);
    }

    /// Run `f` and put `errno` back as it was.
    pub(super) fn preserving_errno(f: impl FnOnce()) {
        // SAFETY: `errno_location` returns the calling thread's `errno`,
        // valid for the life of the thread.
        let saved = unsafe { *errno_location() };
        f();
        // SAFETY: as above.
        unsafe { *errno_location() = saved };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_delivered_request_is_reported_as_sent() {
        // `cancel.c:167`, with the text of `:186`.
        assert_eq!(cancel_report(&Ok(())), b"Cancel request sent\n");
    }

    #[test]
    fn a_failed_request_is_reported_with_pqcancels_message() {
        // `cancel.c:171`-`:172`: the fixed prefix, then `errbuf` as it is.
        assert_eq!(
            cancel_report(&Err(CancelError::NoCancelKey)),
            b"Could not send cancel request: PQcancel() -- no cancellation key received"
        );
    }

    /// The whole chain in one process: a SIGINT to this process sets
    /// `cancel_pressed`, does not end the process, and makes the handler's
    /// thread deliver the CancelRequest to the peer in `cancelConn` — here a
    /// listener standing in for the postmaster.
    #[cfg(unix)]
    #[test]
    fn sigint_sets_cancel_pressed_and_delivers_the_cancel_request() {
        use std::io::Read as _;
        use std::os::unix::net::UnixListener;
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("rpsql-sigint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join(".s.PGSQL.5432");
        let postmaster = UnixListener::bind(&socket).unwrap();
        let key = [7u8, 8, 9, 10];
        let cancel = Cancel::new(rlibpq::Peer::Unix(socket), 4242, &key);
        let expected = cancel.packet().unwrap().to_vec();

        setup_cancel_handler();
        set_cancel_conn(Some(cancel));
        CANCEL_PRESSED.store(false, Ordering::SeqCst);
        let status = std::process::Command::new("kill")
            .args(["-INT", &std::process::id().to_string()])
            .status()
            .expect("kill(1) runs");
        assert!(status.success());

        let (mut request, _) = postmaster.accept().expect("the cancel connection");
        // The client sends the packet and then waits for the postmaster to
        // close (`fe-cancel.c:690`), so read exactly the packet, then close.
        let mut packet = vec![0u8; expected.len()];
        request.read_exact(&mut packet).unwrap();
        drop(request);
        assert_eq!(packet, expected, "the CancelRequest packet for pid 4242");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !CANCEL_PRESSED.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "cancel_pressed was never set");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Let the thread finish its report before `cancelConn` goes away.
        reset_cancel_conn();
        CANCEL_PRESSED.store(false, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
