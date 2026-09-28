//! The stack the embedded server runs on (NAT-407).
//!
//! pgrust's README starts its server under `ulimit -s 65520` and
//! `RUST_MIN_STACK=33554432`. Rust frames are several times C's, so pgrust
//! enforces `max_stack_depth` times `STACK_DEPTH_SCALE` (4 optimized, 32 at
//! opt-level 0) and clamps that to the stack each thread really has:
//! `pg_main` clamps its own thread to `RLIMIT_STACK` (it assumes it runs on
//! the process's main thread), backends are spawned with a stack of at least
//! the rlimit, and `max_stack_depth` itself may not exceed the rlimit less
//! `STACK_DEPTH_SLOP` (`src/backend/utils/misc/stack_depth.c:163`), as in C.
//!
//! pgdrop does the README's setup itself, so a bare `pgdrop postgres` needs
//! neither: it raises the soft `RLIMIT_STACK` to [`TARGET_STACK_RLIMIT`] (never
//! lowering it, never past the hard limit), defaults `RUST_MIN_STACK`, and
//! runs the server on a thread whose stack is exactly the resulting rlimit —
//! so `pg_main`'s clamp matches the stack it is on. A spawned thread is what
//! makes that hold on macOS, whose main-thread stack is fixed at exec time.
//!
//! The waiting main thread blocks every signal first, and the server thread
//! restores the mask it inherits. pgrust's handlers pend a signal on the
//! thread they run on (`install_single_user_signal_bridge`), so a SIGINT or
//! SIGTERM delivered to the idle main thread would otherwise be lost.

// getrlimit, setrlimit, pthread_sigmask and the one environment write.
#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::io::Write;
use std::process::ExitCode;

/// The soft `RLIMIT_STACK` pgdrop raises to: the README's `ulimit -s 65520`,
/// in bytes. 65520 KiB fits under macOS's hard limit of 65532 KiB.
pub const TARGET_STACK_RLIMIT: u64 = 65520 * 1024;

/// The variable Rust's standard library reads for a spawned thread's default
/// stack size, which pgrust's pool threads get.
pub const RUST_MIN_STACK: &str = "RUST_MIN_STACK";

/// The README's `RUST_MIN_STACK=33554432` (32 MiB).
pub const DEFAULT_RUST_MIN_STACK: &str = "33554432";

/// An `RLIMIT_STACK` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Bytes(u64),
    Unlimited,
}

/// Where the server runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackPlan {
    /// On the calling thread: the rlimit is unlimited or unknown, which is
    /// also how `pg_main` reads it (no clamp), so the process stack is the
    /// one C's server would have.
    CallingThread,
    /// Raise the soft limit from `current` to `target` (equal when there is
    /// nothing to raise) and run on a thread with a stack of that size.
    Thread { current: u64, target: u64 },
}

/// Pure: the plan for a soft and a hard `RLIMIT_STACK`, `None` if
/// `getrlimit` failed.
#[must_use]
pub fn plan(limits: Option<(Limit, Limit)>) -> StackPlan {
    match limits {
        None | Some((Limit::Unlimited, _)) => StackPlan::CallingThread,
        Some((Limit::Bytes(soft), hard)) => {
            let reachable = match hard {
                Limit::Bytes(hard) => TARGET_STACK_RLIMIT.min(hard),
                Limit::Unlimited => TARGET_STACK_RLIMIT,
            };
            StackPlan::Thread {
                current: soft,
                target: soft.max(reachable),
            }
        }
    }
}

/// Pure: the `RUST_MIN_STACK` to set, given its current value. A value the
/// user set is theirs, even one the standard library would not parse.
#[must_use]
pub fn min_stack_default(current: Option<&OsStr>) -> Option<&'static str> {
    match current {
        None => Some(DEFAULT_RUST_MIN_STACK),
        Some(_) => None,
    }
}

/// Run `server` on the stack this module describes and return its exit
/// status. A panic in `server` is re-raised here.
///
/// Must be called while the process is still single-threaded: it writes
/// the environment.
pub fn run_on_server_stack<F>(server: F) -> ExitCode
where
    F: FnOnce() -> ExitCode + Send,
{
    if let Some(value) = min_stack_default(std::env::var_os(RUST_MIN_STACK).as_deref()) {
        // SAFETY: the caller runs this before any thread exists (the
        // `postgres` applet, straight from `main`), so nothing reads the
        // environment concurrently.
        unsafe { std::env::set_var(RUST_MIN_STACK, value) };
    }
    let size = match plan(stack_rlimit()) {
        StackPlan::CallingThread => return server(),
        StackPlan::Thread { current, target } => {
            if target > current && set_soft_stack_rlimit(target).is_ok() {
                target
            } else {
                current
            }
        }
    };
    let saved = block_all_signals();
    let outcome = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("postgres".to_owned())
            .stack_size(usize::try_from(size).unwrap_or(usize::MAX))
            .spawn_scoped(scope, move || {
                set_signal_mask(&saved);
                server()
            })
            .map(std::thread::ScopedJoinHandle::join)
    });
    set_signal_mask(&saved);
    match outcome {
        Ok(Ok(status)) => status,
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "pgdrop: could not start the server thread with a {size}-byte stack: {error}"
            );
            ExitCode::FAILURE
        }
    }
}

/// Action: the soft and hard `RLIMIT_STACK`, `None` if `getrlimit` fails.
#[must_use]
pub fn stack_rlimit() -> Option<(Limit, Limit)> {
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes only into the struct it is given.
    if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &raw mut rlim) } != 0 {
        return None;
    }
    let limit = |value: libc::rlim_t| {
        if value == libc::RLIM_INFINITY {
            Limit::Unlimited
        } else {
            Limit::Bytes(value)
        }
    };
    Some((limit(rlim.rlim_cur), limit(rlim.rlim_max)))
}

fn set_soft_stack_rlimit(bytes: u64) -> std::io::Result<()> {
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit and setrlimit read or write only the struct given.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_STACK, &raw mut rlim) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        rlim.rlim_cur = bytes;
        if libc::setrlimit(libc::RLIMIT_STACK, &raw const rlim) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Action: block every signal on the calling thread; the previous mask.
fn block_all_signals() -> libc::sigset_t {
    // SAFETY: sigset_t is plain data, initialized by sigfillset/sigemptyset
    // before pthread_sigmask reads it or writes the old mask into it.
    unsafe {
        let mut all: libc::sigset_t = std::mem::zeroed();
        let mut saved: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&raw mut all);
        libc::sigemptyset(&raw mut saved);
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const all, &raw mut saved);
        saved
    }
}

/// Action: set the calling thread's signal mask.
fn set_signal_mask(mask: &libc::sigset_t) {
    // SAFETY: `mask` is an initialized sigset_t; a null old mask is allowed.
    unsafe {
        libc::pthread_sigmask(libc::SIG_SETMASK, mask, std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn a_default_8_mib_limit_is_raised_to_the_target() {
        assert_eq!(
            plan(Some((Limit::Bytes(8 * MIB), Limit::Unlimited))),
            StackPlan::Thread {
                current: 8 * MIB,
                target: TARGET_STACK_RLIMIT
            }
        );
    }

    #[test]
    fn the_hard_limit_caps_the_raise() {
        // macOS: hard 65532 KiB is above the target, so the target stands.
        assert_eq!(
            plan(Some((Limit::Bytes(8 * MIB), Limit::Bytes(65532 * 1024)))),
            StackPlan::Thread {
                current: 8 * MIB,
                target: TARGET_STACK_RLIMIT
            }
        );
        assert_eq!(
            plan(Some((Limit::Bytes(8 * MIB), Limit::Bytes(16 * MIB)))),
            StackPlan::Thread {
                current: 8 * MIB,
                target: 16 * MIB
            }
        );
    }

    #[test]
    fn a_larger_limit_is_never_lowered() {
        assert_eq!(
            plan(Some((Limit::Bytes(256 * MIB), Limit::Unlimited))),
            StackPlan::Thread {
                current: 256 * MIB,
                target: 256 * MIB
            }
        );
    }

    #[test]
    fn an_unlimited_or_unknown_limit_keeps_the_calling_thread() {
        assert_eq!(
            plan(Some((Limit::Unlimited, Limit::Unlimited))),
            StackPlan::CallingThread
        );
        assert_eq!(plan(None), StackPlan::CallingThread);
    }

    #[test]
    fn rust_min_stack_is_defaulted_only_when_unset() {
        assert_eq!(min_stack_default(None), Some("33554432"));
        assert_eq!(min_stack_default(Some(OsStr::new("8388608"))), None);
        assert_eq!(min_stack_default(Some(OsStr::new("junk"))), None);
    }

    #[test]
    fn this_process_has_a_readable_stack_rlimit() {
        assert!(stack_rlimit().is_some());
    }
}
