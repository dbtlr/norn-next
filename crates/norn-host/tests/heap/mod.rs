//! A counting global allocator: the live heap bytes this process holds and
//! the highest they have reached since a mark.
//!
//! **Installed only in the binary that declares it.** The `memory` test target
//! is its own binary, so the `#[global_allocator]` it names wraps that binary's
//! allocations and no other target's. Every allocation still goes to the
//! system allocator unchanged; the wrapper adds two relaxed atomic updates, so
//! the resident set the kernel accounts to the process is what it would be
//! without it.
//!
//! **What it sees is the Rust heap.** An allocation made through the global
//! allocator is counted; memory a linked C library takes from `malloc`
//! directly, such as SQLite's page cache, is not. That is the part of a read
//! a row lives in once it is hydrated, which is the part a read that held the
//! vault's rows would grow.
//!
//! A reading is a count of bytes asked for, not of pages the kernel mapped: it
//! moves with what the code holds and not with page size or allocator slack.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The system allocator, counting what passes through it.
pub struct Counting;

/// Bytes currently allocated and not yet freed.
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// The highest [`LIVE`] has reached since the last [`Mark::set`].
static HIGH_WATER: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    HIGH_WATER.fetch_max(now, Ordering::Relaxed);
}

fn shrank(by: usize) {
    LIVE.fetch_sub(by, Ordering::Relaxed);
}

// SAFETY: every method forwards its arguments unchanged to `System`, which
// upholds the `GlobalAlloc` contract, and hands back what `System` returned.
// The counters are side effects on atomics and allocate nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc` is `System.alloc`'s.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            grew(layout.size());
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc_zeroed` is `System.alloc_zeroed`'s.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            grew(layout.size());
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        // SAFETY: `block` came from this allocator, which is `System`'s, under `layout`.
        unsafe { System.dealloc(block, layout) };
        shrank(layout.size());
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `block` came from this allocator, which is `System`'s, under `layout`.
        let moved = unsafe { System.realloc(block, layout, new_size) };
        if !moved.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                shrank(layout.size() - new_size);
            }
        }
        moved
    }
}

/// The live heap at one instant, from which the high-water mark is read.
pub struct Mark {
    baseline: usize,
}

impl Mark {
    /// Reset the high-water mark to the live heap now, and remember that
    /// count as the baseline a later reading is taken above.
    pub fn set() -> Mark {
        let baseline = LIVE.load(Ordering::SeqCst);
        HIGH_WATER.store(baseline, Ordering::SeqCst);
        Mark { baseline }
    }

    /// The most the live heap has stood above this mark's baseline since it
    /// was set.
    pub fn peak_above(&self) -> usize {
        HIGH_WATER
            .load(Ordering::SeqCst)
            .saturating_sub(self.baseline)
    }
}
