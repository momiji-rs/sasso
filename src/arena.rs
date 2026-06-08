//! A bump-pointer arena for scoped, single-compile allocation.
//!
//! This is the one `unsafe` module in the library. It is the foundation of the
//! scoped allocator (perf #5): a single `compile()` is a flood of short-lived
//! allocations freed all at once, so within a compile every allocation is a
//! pointer bump, and the whole arena is reset (freed) when the compile ends.
//! Because a compile is `!Send` (the evaluator uses `Rc`), each thread keeps its
//! own arena, so this type is deliberately single-threaded (`Cell`, not atomics)
//! — the global-allocator wrapper that owns the thread-local lives separately.
//!
//! ## Safety strategy
//!
//! - The bump arithmetic is factored into the pure [`bump_compute`] function,
//!   unit-tested exhaustively (alignment / boundary / overflow) with no `unsafe`.
//! - Allocated pointers are derived from the backing pointer via `base.add(..)`
//!   (NOT `addr as *mut u8`), so they keep their provenance — required for Miri's
//!   Stacked/Tree-Borrows checks to pass.
//! - The integration tests run under `cargo miri test` to catch out-of-bounds,
//!   misalignment, use-after-free, and aliasing UB.

#![allow(unsafe_code)]
// Wired into the global allocator in step 2; until then the type is unused.
#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// Pure bump arithmetic over absolute addresses: align `cur` up to `align`, add
/// `size`, and check the result fits at or below `end` (exclusive upper bound).
/// Returns `(aligned_start, new_cursor)` or `None` on overflow / no fit. Touches
/// no memory, so it is unit-testable without any `unsafe`. `align` must be a
/// power of two (guaranteed by [`Layout`]).
fn bump_compute(cur: usize, align: usize, size: usize, end: usize) -> Option<(usize, usize)> {
    let aligned = cur.checked_add(align - 1)? & !(align - 1);
    let next = aligned.checked_add(size)?;
    (next <= end).then_some((aligned, next))
}

/// A bump arena over one fixed backing region reserved from the system
/// allocator. Single-threaded: the cursor is a [`Cell`], and the production
/// allocator keeps one `Arena` per thread.
pub(crate) struct Arena {
    /// Backing region start (and the provenance root for every allocation).
    base: *mut u8,
    /// Backing region size in bytes.
    size: usize,
    /// `base as usize + size`, cached (exclusive upper bound).
    end: usize,
    /// Absolute address of the next free byte.
    cursor: Cell<usize>,
}

impl Arena {
    /// Reserve `size` bytes from the system allocator. Returns `None` if the
    /// layout is invalid or the reservation fails (caller then falls back).
    pub(crate) fn with_system_backing(size: usize) -> Option<Arena> {
        let layout = Layout::from_size_align(size, 4096).ok()?;
        // SAFETY: `size` is non-zero and 4096 is a valid power-of-two align.
        let base = unsafe { System.alloc(layout) };
        if base.is_null() {
            return None;
        }
        Some(Arena {
            base,
            size,
            end: base as usize + size,
            cursor: Cell::new(base as usize),
        })
    }

    /// Bump-allocate `layout` from the arena, or `None` if it does not fit.
    pub(crate) fn alloc(&self, layout: Layout) -> Option<*mut u8> {
        let (aligned, next) =
            bump_compute(self.cursor.get(), layout.align(), layout.size(), self.end)?;
        self.cursor.set(next);
        // Derive the pointer from `base` (preserves provenance) rather than
        // casting an integer address.
        // SAFETY: `bump_compute` guarantees `base <= aligned` and
        // `aligned + size <= base + size`, so the offset is within the backing.
        Some(unsafe { self.base.add(aligned - self.base as usize) })
    }

    /// Roll the cursor back to empty — logically frees everything allocated
    /// since construction or the last reset. The caller must ensure nothing
    /// allocated from the arena is still in use.
    pub(crate) fn reset(&self) {
        self.cursor.set(self.base as usize);
    }

    /// Bytes used since the last reset (the arena high-water mark).
    pub(crate) fn used(&self) -> usize {
        self.cursor.get() - self.base as usize
    }

    /// Whether `ptr` points within this arena's backing region.
    pub(crate) fn contains(&self, ptr: *mut u8) -> bool {
        let p = ptr as usize;
        p >= self.base as usize && p < self.end
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        if !self.base.is_null() {
            if let Ok(layout) = Layout::from_size_align(self.size, 4096) {
                // SAFETY: `base` came from `System.alloc` with exactly this layout.
                unsafe { System.dealloc(self.base, layout) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure bump_compute: alignment, boundary, overflow ----

    #[test]
    fn compute_aligns_up() {
        assert_eq!(bump_compute(10, 8, 4, 1000), Some((16, 20)));
        assert_eq!(bump_compute(16, 8, 8, 1000), Some((16, 24))); // already aligned
        assert_eq!(bump_compute(7, 1, 3, 1000), Some((7, 10))); // align 1 = no-op
    }

    #[test]
    fn compute_every_power_of_two_alignment() {
        for align in [1usize, 2, 4, 8, 16, 32, 64, 128, 256, 4096] {
            let (aligned, next) = bump_compute(1, align, 64, usize::MAX).unwrap();
            assert_eq!(aligned % align, 0, "align {align}");
            assert_eq!(next, aligned + 64);
        }
    }

    #[test]
    fn compute_zero_size() {
        assert_eq!(bump_compute(8, 8, 0, 100), Some((8, 8)));
    }

    #[test]
    fn compute_boundary() {
        assert_eq!(bump_compute(0, 1, 100, 100), Some((0, 100))); // next == end OK
        assert_eq!(bump_compute(0, 1, 101, 100), None); // one past the end
        assert_eq!(bump_compute(90, 8, 20, 100), None); // 96 + 20 > 100
    }

    #[test]
    fn compute_overflow_is_none() {
        assert_eq!(bump_compute(usize::MAX, 8, 0, usize::MAX), None); // align overflow
        assert_eq!(bump_compute(usize::MAX - 3, 1, 10, usize::MAX), None); // size overflow
    }

    // ---- integration: real alloc against a small system-backed arena ----
    // These run under `cargo miri test` to catch UB (OOB / align / provenance).

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn alloc_is_aligned_writable_and_in_bounds() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        for align in [1usize, 2, 4, 8, 16, 64, 256] {
            let p = a.alloc(layout(128, align)).unwrap();
            assert_eq!(p as usize % align, 0, "align {align}");
            assert!(a.contains(p));
            // Writable across the whole allocation (Miri checks bounds + provenance).
            unsafe {
                std::ptr::write_bytes(p, 0xAB, 128);
                assert_eq!(*p, 0xAB);
                assert_eq!(*p.add(127), 0xAB);
            }
        }
    }

    #[test]
    fn allocations_do_not_overlap() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        let p1 = a.alloc(layout(64, 8)).unwrap() as usize;
        let p2 = a.alloc(layout(64, 8)).unwrap() as usize;
        assert!(p2 >= p1 + 64, "p1={p1:#x} p2={p2:#x}");
    }

    #[test]
    fn full_arena_returns_none() {
        let a = Arena::with_system_backing(4096).unwrap();
        assert!(a.alloc(layout(8192, 8)).is_none(), "oversized must not fit");
        // Fill and then fail.
        assert!(a.alloc(layout(2048, 8)).is_some());
        assert!(a.alloc(layout(2048, 8)).is_some());
        assert!(a.alloc(layout(1, 1)).is_none(), "arena should be exhausted");
    }

    #[test]
    fn reset_reuses_the_same_region() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        let p1 = a.alloc(layout(1000, 8)).unwrap();
        assert_eq!(a.used(), 1000);
        a.reset();
        assert_eq!(a.used(), 0);
        let p2 = a.alloc(layout(1000, 8)).unwrap();
        assert_eq!(p1, p2, "reset must hand back the same region");
        // Still writable after reset (Miri: not a use-after-free of the backing).
        unsafe { std::ptr::write_bytes(p2, 0xCD, 1000) };
    }

    #[test]
    fn used_tracks_high_water_with_alignment() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        a.alloc(layout(1, 1)).unwrap();
        a.alloc(layout(8, 8)).unwrap(); // forces alignment padding
        assert!(a.used() >= 9 && a.used() <= 16);
    }

    #[test]
    fn contains_rejects_foreign_pointers() {
        let a = Arena::with_system_backing(4096).unwrap();
        let mut local = 0u8;
        assert!(!a.contains(&mut local as *mut u8));
        let p = a.alloc(layout(16, 8)).unwrap();
        assert!(a.contains(p));
    }
}
