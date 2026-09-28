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
//! is not — `std`'s socket calls allocate. So the handler here, installed
//! with `signal-hook` (ADR-0009), does only what is safe in a handler: it
//! stores `cancel_pressed` ([`signal_hook::flag::register`]) and wakes a
//! thread of its own ([`signal_hook::iterator::Signals`]), which sends the
//! request and writes the report, as the Windows arm of `cancel.c` does from
//! its console-handler thread (`cancel.c:195`-`:224`, holding
//! `cancelConnLock` as [`CANCEL_CONN`]'s mutex is held here).
//! `docs/divergences.md` records the window the thread opens.
//!
//! Data and calculations: [`CANCEL_PRESSED`], [`cancel_report`]. Actions:
//! [`setup_cancel_handler`], [`set_cancel_conn`], [`reset_cancel_conn`].

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use rlibpq::{Cancel, CancelError};

/// `cancel_pressed` (`common.c:323`): set by every SIGINT, read and cleared
/// by `MainLoop` (`mainloop.c:88`-`:100`).
/// An `Arc` because the handler keeps a reference of its own.
pub static CANCEL_PRESSED: LazyLock<Arc<AtomicBool>> =
    LazyLock::new(|| Arc::new(AtomicBool::new(false)));

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
/// running query, if there is one, and say how that went on `stderr`.
#[cfg(unix)]
fn send_cancel(stderr: &mut impl std::io::Write) {
    // Held across the request, as `cancelConnLock` is (`cancel.c:208`-`:222`):
    // the query cannot be replaced by the next one while it is cancelled.
    let conn = cancel_conn();
    if let Some(cancel) = conn.as_ref() {
        // One write, its result ignored, as `write_stderr` does
        // (`cancel.c:31`-`:36`).
        let _ = stderr.write_all(&cancel_report(&cancel.cancel()));
    }
}

/// `psql_setup_cancel_handler()` (`common.c:327`): install the SIGINT
/// handler. Only the first call does anything. A failure to duplicate
/// stderr or to open `signal-hook`'s wake-up pipe leaves SIGINT's default
/// action in place, as a failed `pqsignal` does upstream; a failure to start
/// the thread after that leaves SIGINT caught and ignored.
pub fn setup_cancel_handler() {
    #[cfg(unix)]
    {
        static SETUP: std::sync::Once = std::sync::Once::new();
        SETUP.call_once(unix::setup);
    }
}

#[cfg(unix)]
mod unix {
    use std::os::fd::AsFd as _;
    use std::sync::Arc;

    use signal_hook::consts::SIGINT;
    use signal_hook::iterator::Signals;

    use super::{CANCEL_PRESSED, send_cancel};

    /// `handle_sigint` (`cancel.c:153`), split in two. In the handler,
    /// `psql_cancel_callback`'s `cancel_pressed = true` (`common.c:323`) and a
    /// wake-up for the `psql-cancel` thread; on that thread, the `PQcancel`
    /// and the report. `signal-hook` installs its handler with `SA_RESTART`,
    /// as `pqsignal` does (`src/port/pqsignal.c:141`), and saves and restores
    /// `errno` around it, as `pqsignal`'s `wrapper_handler` does
    /// (`pqsignal.c:88`, `:112`).
    pub(super) fn setup() {
        // `write_stderr` writes to descriptor 2 directly (`cancel.c:31`),
        // past `std`'s `Stderr` lock, which the main thread holds for the
        // whole session: so does a duplicate of it.
        let Ok(stderr) = std::io::stderr().as_fd().try_clone_to_owned() else {
            return;
        };
        let mut stderr = std::fs::File::from(stderr);
        // The handler does nothing but wake the thread until the flag is
        // registered below, and `send_cancel` does nothing outside a query,
        // so the order of the two registrations is not observable.
        let Ok(mut signals) = Signals::new([SIGINT]) else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("psql-cancel".to_string())
            .spawn(move || {
                for _ in signals.forever() {
                    send_cancel(&mut stderr);
                }
            });
        if spawned.is_ok() {
            let _ = signal_hook::flag::register(SIGINT, Arc::clone(&CANCEL_PRESSED));
        }
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
