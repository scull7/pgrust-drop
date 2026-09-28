//! mimalloc, the allocator the embedded server is built for (NAT-407).
//!
//! `main_main`'s `bin/postgres.rs` makes mimalloc the global allocator and
//! hands pgrust three hooks into it. pgdrop does the same: `main.rs` declares
//! [`Global`] the `#[global_allocator]`, and the `postgres` applet calls
//! [`install_hooks`] before `pg_main`, in `bin/postgres.rs`'s order.
//!
//! Not carried over: `bin/postgres.rs`'s debug-build allocation tracker
//! (`PGRUST_ALLOC_TRACK`) and memory-context census (`PGRUST_MCXT_CENSUS`),
//! pgrust's own leak-hunting instruments, so a debug pgdrop allocates
//! through the bare mimalloc a release one does.

// The hooks call mimalloc's C API; each call is annotated below.
#![allow(unsafe_code)]

/// The global allocator's type, as `bin/postgres.rs` declares it.
pub type Global = mimalloc::MiMalloc;

/// Action: install what `bin/postgres.rs`'s `run` installs next to the
/// allocator, after the seams and before `pg_main`:
///
/// * `mcx::set_allocator_release`: `mi_collect(true)`, which returns freed
///   but retained segments at pgrust's alloc-churn boundaries (hash
///   aggregate spill batches);
/// * `memwatchdog::set_allocator_stats`: mimalloc's resident and committed
///   bytes, for the memory watchdog's ledger;
/// * `runtime::install_qos_mem_probe`: its resident bytes, the QoS memory
///   governor's usage basis where `/proc` is not.
///
/// All three read mimalloc's process-wide state, so they answer for
/// whatever allocator is global; under anything but [`Global`] they report
/// mimalloc's own (empty) heap.
pub fn install_hooks() {
    mcx::set_allocator_release(release);
    memwatchdog::set_allocator_stats(|| {
        let info = process_info();
        memwatchdog::AllocatorStats {
            current_rss: info.current_rss,
            current_commit: info.current_commit,
        }
    });
    runtime::install_qos_mem_probe(|| process_info().current_rss);
}

fn release() {
    // SAFETY: `mi_collect` takes no pointers and may be called from any
    // thread at any time.
    unsafe { libmimalloc_sys::mi_collect(true) };
}

/// The two figures of `mi_process_info` the hooks report.
struct ProcessInfo {
    current_rss: usize,
    current_commit: usize,
}

fn process_info() -> ProcessInfo {
    let mut info = ProcessInfo {
        current_rss: 0,
        current_commit: 0,
    };
    // SAFETY: every out-parameter of `mi_process_info` is nullable; the two
    // non-null ones point at live `usize`s for the duration of the call.
    unsafe {
        libmimalloc_sys::mi_process_info(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut info.current_rss,
            std::ptr::null_mut(),
            &raw mut info.current_commit,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit tests run under the system allocator, so the block comes from
    /// [`Global`] directly; the statistics must then see mimalloc's heap.
    #[test]
    fn the_statistics_see_what_mimalloc_committed() {
        use std::alloc::{GlobalAlloc, Layout};
        let layout = Layout::from_size_align(1 << 20, 64).expect("a layout");
        // SAFETY: a non-zero-sized layout; the block is freed with it.
        unsafe {
            let block = Global {}.alloc(layout);
            assert!(!block.is_null());
            assert_eq!(block.align_offset(64), 0);
            assert!(process_info().current_commit >= layout.size());
            Global {}.dealloc(block, layout);
        }
        release();
    }
}
