use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering};

/// Counts allocations made on any thread, then defers to divan's profiler,
/// which only sees the thread running the bench.
pub struct EveryThreadAlloc {
    inner: divan::AllocProfiler,
    allocs: AtomicU64,
    bytes: AtomicU64,
}

impl EveryThreadAlloc {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: divan::AllocProfiler::system(),
            allocs: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> AllocCount {
        AllocCount {
            allocs: self.allocs.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
        }
    }

    fn count(&self, size: usize) {
        self.allocs.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(size as u64, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocCount {
    pub allocs: u64,
    pub bytes: u64,
}

impl AllocCount {
    #[must_use]
    pub const fn since(self, earlier: Self) -> Self {
        Self {
            allocs: self.allocs - earlier.allocs,
            bytes: self.bytes - earlier.bytes,
        }
    }
}

// SAFETY: every call forwards unchanged to divan's profiler, itself a
// pass-through to the system allocator; the counters touch no memory the
// caller owns.
unsafe impl GlobalAlloc for EveryThreadAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.count(layout.size());
        // SAFETY: the caller's contract for `layout` is passed through.
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.count(layout.size());
        // SAFETY: as for `alloc`.
        unsafe { self.inner.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        self.count(new_size);
        // SAFETY: `ptr` came from this allocator with `layout`, per the caller.
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator with `layout`, per the caller.
        unsafe { self.inner.dealloc(ptr, layout) }
    }
}
