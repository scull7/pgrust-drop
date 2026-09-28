//! `PQsocketPoll` (`fe-misc.c:1285`) and `pqSocketCheck` (`:1229`): wait
//! until a socket can be read or written, or a deadline passes, over
//! `poll(2)` — the call upstream makes wherever `HAVE_POLL` is defined
//! (`:1288`), which is every target this crate ships to (ADR-0007).
//!
//! The standard library has no readiness wait, so the call is
//! `rustix::event::poll`, rustix's safe binding of `poll(2)` (owner,
//! 2026-09-27; ADR-0010). This crate declares no foreign function and has
//! no `unsafe` code.

use std::io;
use std::os::fd::BorrowedFd;
use std::time::Instant;

use rustix::event::{PollFd, PollFlags, Timespec};

/// What one wait reported, per direction asked about: `select()`'s input and
/// output masks after the call, which is what `libpq_pipeline.c`'s loops test
/// with `FD_ISSET` (`:1104`, `:1182`, `:2119`, `:2126`).
///
/// A hang-up or an error reads as readable, as `select()` reports it, so that
/// the read that follows is the call that finds out what happened — "the
/// actual error condition will be detected and reported when the caller tries
/// to read or write the socket" (`fe-misc.c:1161`). An error also reads as
/// writable. A direction not asked about is never reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ready {
    pub read: bool,
    pub write: bool,
}

impl Ready {
    /// `PQsocketPoll`'s `> 0`: some condition asked about is met. `false` is
    /// its `0`, the deadline passed.
    #[must_use]
    pub fn any(self) -> bool {
        self.read || self.write
    }

    /// Calculation: the directions `revents` reports, among those asked
    /// about.
    fn from_revents(revents: PollFlags, for_read: bool, for_write: bool) -> Self {
        let errored = revents.contains(PollFlags::ERR);
        Ready {
            read: for_read && (revents.intersects(PollFlags::IN | PollFlags::HUP) || errored),
            write: for_write && (revents.contains(PollFlags::OUT) || errored),
        }
    }
}

/// Calculation: `PQsocketPoll`'s timeout (`fe-misc.c:1305`-`:1317`) —
/// infinite (`-1`) without a deadline, else the whole milliseconds left,
/// `0` once it has passed. A wait longer than `poll` can express (about 24
/// days) is clamped to the longest it can.
#[must_use]
pub fn timeout_ms(end_time: Option<Instant>, now: Instant) -> i32 {
    match end_time {
        None => -1,
        Some(end) => {
            let millis = end.saturating_duration_since(now).as_millis();
            i32::try_from(millis).unwrap_or(i32::MAX)
        }
    }
}

/// Calculation: [`timeout_ms`] in the form rustix 1.x's `poll` takes it,
/// `Option<&Timespec>` (`rustix::event::poll`, rustix 1.1.5
/// `src/event/poll.rs:45`), where `None` waits forever. Where there is no
/// `ppoll` (Darwin) rustix hands `poll(2)` whole milliseconds and refuses
/// more than `c_int::MAX` of them, which the clamp in [`timeout_ms`] keeps
/// it from seeing.
fn timespec(timeout_ms: i32) -> Option<Timespec> {
    let ms = i64::from(timeout_ms);
    (ms >= 0).then(|| Timespec {
        tv_sec: ms / 1000,
        tv_nsec: (ms % 1000) * 1_000_000,
    })
}

/// `PQsocketPoll`, `fe-misc.c:1285`: wait on `sock` for input, output or
/// both until `end_time` (`None` for no deadline; a deadline already passed
/// polls without waiting), and say which conditions are met. Asked about
/// nothing, it answers at once that none is (`:1292`).
///
/// # Errors
/// `poll` failed — interrupted by a signal among other causes, which
/// `pqSocketCheck` retries and this does not — or `sock` is not an open
/// descriptor (`POLLNVAL`, reported as `EBADF`, what `select()` would say).
pub fn socket_poll(
    sock: BorrowedFd<'_>,
    for_read: bool,
    for_write: bool,
    end_time: Option<Instant>,
) -> io::Result<Ready> {
    if !for_read && !for_write {
        return Ok(Ready::default());
    }
    let mut events = PollFlags::ERR;
    if for_read {
        events |= PollFlags::IN;
    }
    if for_write {
        events |= PollFlags::OUT;
    }
    let mut fds = [PollFd::from_borrowed_fd(sock, events)];
    let timeout = timespec(timeout_ms(end_time, Instant::now()));
    rustix::event::poll(&mut fds, timeout.as_ref())?;
    let revents = fds[0].revents();
    if revents.contains(PollFlags::NVAL) {
        return Err(rustix::io::Errno::BADF.into());
    }
    Ok(Ready::from_revents(revents, for_read, for_write))
}

/// `pqSocketCheck`, `fe-misc.c:1229`: [`socket_poll`], retried for as long
/// as a signal interrupts it (`:1259`): rustix reports that as
/// `Errno::INTR`, which is `io::ErrorKind::Interrupted` here.
///
/// # Errors
/// `poll` failed for any reason but `EINTR`.
pub fn socket_check(
    sock: BorrowedFd<'_>,
    for_read: bool,
    for_write: bool,
    end_time: Option<Instant>,
) -> io::Result<Ready> {
    loop {
        match socket_poll(sock, for_read, for_write, end_time) {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{ErrorKind, Read, Write};
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    fn pair() -> (UnixStream, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        (a, b)
    }

    /// A fresh socket can be written and has nothing to read: `POLLOUT` is
    /// the bit the kernel sets for it, and `POLLIN` is not.
    #[test]
    fn a_fresh_socket_is_writable_and_not_readable() {
        let (a, _b) = pair();
        let ready = socket_poll(a.as_fd(), true, true, Some(Instant::now())).unwrap();
        assert_eq!(
            ready,
            Ready {
                read: false,
                write: true
            }
        );
    }

    /// Bytes from the peer set `POLLIN`.
    #[test]
    fn input_from_the_peer_is_readable() {
        let (a, mut b) = pair();
        b.write_all(b"x").unwrap();
        let ready = socket_poll(a.as_fd(), true, false, Some(Instant::now())).unwrap();
        assert_eq!(
            ready,
            Ready {
                read: true,
                write: false
            }
        );
    }

    /// The peer hanging up reads as readable, so the read that follows sees
    /// the end of the stream.
    #[test]
    fn a_peer_that_hung_up_is_readable() {
        let (mut a, b) = pair();
        drop(b);
        assert!(
            socket_poll(a.as_fd(), true, false, Some(Instant::now()))
                .unwrap()
                .read
        );
        assert_eq!(a.read(&mut [0; 1]).unwrap(), 0);
    }

    /// A socket whose send buffer is full is not writable; this is the
    /// state `pqSendSome` waits out.
    #[test]
    fn a_full_socket_is_not_writable_until_the_peer_reads() {
        let (mut a, mut b) = pair();
        let chunk = [0u8; 4096];
        let mut sent = 0usize;
        loop {
            match a.write(&chunk) {
                Ok(n) => sent += n,
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => panic!("{err}"),
            }
        }
        assert!(
            !socket_poll(a.as_fd(), false, true, Some(Instant::now()))
                .unwrap()
                .any()
        );

        let mut sink = vec![0u8; sent];
        b.read_exact(&mut sink).unwrap();
        let deadline = Some(Instant::now() + Duration::from_secs(10));
        assert!(socket_poll(a.as_fd(), false, true, deadline).unwrap().write);
    }

    /// Asked about neither direction, `PQsocketPoll` returns 0 at once
    /// (`fe-misc.c:1292`), even with no deadline.
    #[test]
    fn asking_about_nothing_answers_at_once() {
        let (a, _b) = pair();
        assert_eq!(
            socket_check(a.as_fd(), false, false, None).unwrap(),
            Ready::default()
        );
    }

    /// A deadline passes: the poll comes back with nothing ready, having
    /// waited for it — to within the millisecond the timeout is truncated
    /// to.
    #[test]
    fn a_deadline_that_passes_is_a_timeout() {
        let (a, _b) = pair();
        let start = Instant::now();
        let end = start + Duration::from_millis(20);
        let ready = socket_check(a.as_fd(), true, false, Some(end)).unwrap();
        assert!(!ready.any());
        assert!(start.elapsed() >= Duration::from_millis(19));
    }

    #[test]
    fn the_timeout_is_upstreams() {
        let now = Instant::now();
        assert_eq!(timeout_ms(None, now), -1);
        assert_eq!(timeout_ms(Some(now), now), 0);
        // fe-misc.c:1316 — a deadline already passed polls without waiting.
        assert_eq!(timeout_ms(Some(now), now + Duration::from_secs(1)), 0);
        // :1314 — whole milliseconds, truncated.
        assert_eq!(timeout_ms(Some(now + Duration::from_micros(2_999)), now), 2);
        assert_eq!(
            timeout_ms(Some(now + Duration::from_hours(100 * 24)), now),
            i32::MAX
        );
    }

    #[test]
    fn the_timespec_is_the_timeout_in_milliseconds() {
        assert_eq!(timespec(-1), None);
        assert_eq!(
            timespec(0),
            Some(Timespec {
                tv_sec: 0,
                tv_nsec: 0
            })
        );
        assert_eq!(
            timespec(2_999),
            Some(Timespec {
                tv_sec: 2,
                tv_nsec: 999_000_000
            })
        );
    }

    #[test]
    fn hang_up_and_error_are_reported_only_in_the_directions_asked() {
        let hup = PollFlags::HUP;
        let err = PollFlags::ERR;
        assert_eq!(
            Ready::from_revents(hup, true, true),
            Ready {
                read: true,
                write: false
            }
        );
        assert_eq!(Ready::from_revents(hup, false, true), Ready::default());
        assert_eq!(
            Ready::from_revents(err, true, true),
            Ready {
                read: true,
                write: true
            }
        );
        assert_eq!(
            Ready::from_revents(err, false, true),
            Ready {
                read: false,
                write: true
            }
        );
    }
}
