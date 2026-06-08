//! The scoped bump allocator (perf #5) — the library's one `unsafe` module.
//!
//! A single `compile()` is a flood of short-lived allocations freed all at once,
//! so within a compile scope every allocation is a pointer bump from a
//! per-thread arena, and the whole arena is reset when the scope ends. Outside a
//! scope, allocations forward to the system allocator. Because a compile is
//! `!Send` (the evaluator uses `Rc`), each thread keeps its own arena, so the
//! state is thread-local and single-threaded (`Cell`, not atomics).
//!
//! ## Safety strategy
//!
//! - Bump arithmetic is the pure [`bump_compute`] (exhaustively unit-tested:
//!   alignment / boundary / overflow, no `unsafe`).
//! - Pointers are derived via `base.add(..)` (never `addr as *mut u8`) so they
//!   keep provenance — required for Miri's Stacked/Tree-Borrows checks.
//! - The thread-local [`ThreadState`] is POD (no `Drop`): the first TLS access
//!   must not register a destructor, because a destructor would allocate and
//!   re-enter the allocator. Its backing region is therefore leaked at thread
//!   exit (virtual, lazily committed; compile threads are few).
//! - [`Arena`] (test-only) is a standalone, `Drop`-ing twin of the same bump +
//!   provenance logic, run under `cargo miri test` for UB detection without
//!   leaking (Miri does not execute `#[global_allocator]`, so the live
//!   [`ScopedAlloc`] path is covered by AddressSanitizer + the full sass-spec
//!   suite run under the arena instead).

#![allow(unsafe_code)]
// Some scope primitives are wired up in later steps (Importer boundary).
#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// Pure bump arithmetic over absolute addresses: align `cur` up to `align`, add
/// `size`, and check the result fits at or below `end` (exclusive). Returns
/// `(aligned_start, new_cursor)` or `None` on overflow / no fit. Touches no
/// memory. `align` must be a power of two (guaranteed by [`Layout`]).
fn bump_compute(cur: usize, align: usize, size: usize, end: usize) -> Option<(usize, usize)> {
    let aligned = cur.checked_add(align - 1)? & !(align - 1);
    let next = aligned.checked_add(size)?;
    (next <= end).then_some((aligned, next))
}

/// Per-thread arena size: virtual address space reserved up front. Physical
/// pages commit lazily on first touch, so an unused reservation costs ~nothing.
const THREAD_ARENA_SIZE: usize = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Per-thread bump state. POD only (no `Drop`) — see the module-level safety
/// note. The backing region leaks at thread exit (virtual + lazily committed).
struct ThreadState {
    base: Cell<*mut u8>,
    end: Cell<usize>,
    cursor: Cell<usize>,
    /// Scope nesting depth. `0` = inactive: allocations pass through to System.
    depth: Cell<u32>,
}

impl ThreadState {
    const fn new() -> ThreadState {
        ThreadState {
            base: Cell::new(std::ptr::null_mut()),
            end: Cell::new(0),
            cursor: Cell::new(0),
            depth: Cell::new(0),
        }
    }

    /// Reserve the backing region on first use. Returns `false` on failure (the
    /// caller then forwards the request to the system allocator).
    #[cold]
    fn reserve(&self) -> bool {
        let Ok(layout) = Layout::from_size_align(THREAD_ARENA_SIZE, 4096) else {
            return false;
        };
        // SAFETY: non-zero size, 4096 is a valid power-of-two alignment.
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            return false;
        }
        self.base.set(p);
        self.end.set(p as usize + THREAD_ARENA_SIZE);
        self.cursor.set(p as usize);
        true
    }
}

thread_local! {
    // `const {}` init: no lazy allocation, and POD means no TLS destructor — so
    // accessing this from inside the global allocator cannot re-enter it.
    static TL: ThreadState = const { ThreadState::new() };
}

/// A scoped bump global allocator. Inside a `compile()` scope it bump-allocates
/// from a per-thread arena that is reset when the scope ends; outside any scope
/// it forwards to the system allocator. Install it in a binary or wasm wrapper:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOC: sasso::ScopedAlloc = sasso::ScopedAlloc;
/// ```
///
/// It is safe to install even if `compile` is never called: with no active scope
/// every request goes straight to the system allocator.
pub struct ScopedAlloc;

unsafe impl GlobalAlloc for ScopedAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TL.with(|tl| {
            if tl.depth.get() == 0 {
                // SAFETY: forwarding an unchanged layout to the system allocator.
                return unsafe { System.alloc(layout) };
            }
            if tl.base.get().is_null() && !tl.reserve() {
                return unsafe { System.alloc(layout) };
            }
            match bump_compute(tl.cursor.get(), layout.align(), layout.size(), tl.end.get()) {
                Some((aligned, next)) => {
                    tl.cursor.set(next);
                    let base = tl.base.get();
                    // SAFETY: bump_compute guarantees base <= aligned and
                    // aligned + size <= base + size, so the offset is in-bounds.
                    unsafe { base.add(aligned - base as usize) }
                }
                // Arena exhausted → fall back to the system allocator.
                // SAFETY: forwarding an unchanged layout to the system allocator.
                None => unsafe { System.alloc(layout) },
            }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        TL.with(|tl| {
            let p = ptr as usize;
            let base = tl.base.get() as usize;
            let in_arena = base != 0 && p >= base && p < tl.end.get();
            if !in_arena {
                // SAFETY: not from the arena, so it came from the system
                // allocator with this same layout.
                unsafe { System.dealloc(ptr, layout) };
            }
            // in-arena: no-op (reclaimed wholesale on scope reset)
        });
    }
}

/// An RAII scope marker. Construct it on entering a compile; on `drop` (the
/// panic / early-exit path) it leaves the scope and resets the arena. The
/// success path in `compile` finishes manually — leaving, copying the result
/// out, then resetting — and `mem::forget`s the guard.
pub(crate) struct Scope;

impl Scope {
    pub(crate) fn enter() -> Scope {
        TL.with(|tl| tl.depth.set(tl.depth.get() + 1));
        Scope
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        // Panic / early-exit path: leave and, if outermost, reset.
        if leave_no_reset() {
            reset();
        }
    }
}

/// Leave the current scope WITHOUT resetting, returning whether this was the
/// outermost scope. The success path copies the result out before [`reset`].
pub(crate) fn leave_no_reset() -> bool {
    TL.with(|tl| {
        let d = tl.depth.get().saturating_sub(1);
        tl.depth.set(d);
        d == 0
    })
}

/// Reset the arena to empty. Only resets when no scope is active (so a nested
/// scope can't free an outer scope's allocations).
pub(crate) fn reset() {
    TL.with(|tl| {
        if tl.depth.get() == 0 {
            tl.cursor.set(tl.base.get() as usize);
        }
    });
}

/// Suspend the scope (allocations go to System) around a caller callback whose
/// allocations may outlive the arena — e.g. an `Importer`. Returns the saved
/// depth to restore with [`resume`].
pub(crate) fn pause() -> u32 {
    TL.with(|tl| {
        let d = tl.depth.get();
        tl.depth.set(0);
        d
    })
}

/// Restore the depth saved by [`pause`].
pub(crate) fn resume(saved: u32) {
    TL.with(|tl| tl.depth.set(saved));
}

// =========================================================================
// Test-only standalone arena: a `Drop`-ing twin of the bump + provenance logic
// above, used to exercise it under `cargo miri test` without leaking.
// =========================================================================

#[cfg(test)]
struct Arena {
    base: *mut u8,
    size: usize,
    end: usize,
    cursor: Cell<usize>,
}

#[cfg(test)]
impl Arena {
    fn with_system_backing(size: usize) -> Option<Arena> {
        let layout = Layout::from_size_align(size, 4096).ok()?;
        // SAFETY: non-zero size, valid align.
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

    fn alloc(&self, layout: Layout) -> Option<*mut u8> {
        let (aligned, next) =
            bump_compute(self.cursor.get(), layout.align(), layout.size(), self.end)?;
        self.cursor.set(next);
        // SAFETY: in-bounds offset (see bump_compute).
        Some(unsafe { self.base.add(aligned - self.base as usize) })
    }

    fn reset(&self) {
        self.cursor.set(self.base as usize);
    }

    fn used(&self) -> usize {
        self.cursor.get() - self.base as usize
    }

    fn contains(&self, ptr: *mut u8) -> bool {
        let p = ptr as usize;
        p >= self.base as usize && p < self.end
    }
}

#[cfg(test)]
impl Drop for Arena {
    fn drop(&mut self) {
        if let Ok(layout) = Layout::from_size_align(self.size, 4096) {
            // SAFETY: base came from System.alloc with this layout.
            unsafe { System.dealloc(self.base, layout) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure bump_compute (also covered by Miri) ----

    #[test]
    fn compute_aligns_up() {
        assert_eq!(bump_compute(10, 8, 4, 1000), Some((16, 20)));
        assert_eq!(bump_compute(16, 8, 8, 1000), Some((16, 24)));
        assert_eq!(bump_compute(7, 1, 3, 1000), Some((7, 10)));
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
        assert_eq!(bump_compute(0, 1, 100, 100), Some((0, 100)));
        assert_eq!(bump_compute(0, 1, 101, 100), None);
        assert_eq!(bump_compute(90, 8, 20, 100), None);
    }

    #[test]
    fn compute_overflow_is_none() {
        assert_eq!(bump_compute(usize::MAX, 8, 0, usize::MAX), None);
        assert_eq!(bump_compute(usize::MAX - 3, 1, 10, usize::MAX), None);
    }

    // ---- standalone Arena (run under Miri for UB) ----

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn arena_alloc_is_aligned_writable_and_in_bounds() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        for align in [1usize, 2, 4, 8, 16, 64, 256] {
            let p = a.alloc(layout(128, align)).unwrap();
            assert_eq!(p as usize % align, 0, "align {align}");
            assert!(a.contains(p));
            unsafe {
                std::ptr::write_bytes(p, 0xAB, 128);
                assert_eq!(*p, 0xAB);
                assert_eq!(*p.add(127), 0xAB);
            }
        }
    }

    #[test]
    fn arena_allocations_do_not_overlap() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        let p1 = a.alloc(layout(64, 8)).unwrap() as usize;
        let p2 = a.alloc(layout(64, 8)).unwrap() as usize;
        assert!(p2 >= p1 + 64);
    }

    #[test]
    fn arena_full_returns_none() {
        let a = Arena::with_system_backing(4096).unwrap();
        assert!(a.alloc(layout(8192, 8)).is_none());
        assert!(a.alloc(layout(2048, 8)).is_some());
        assert!(a.alloc(layout(2048, 8)).is_some());
        assert!(a.alloc(layout(1, 1)).is_none());
    }

    #[test]
    fn arena_reset_reuses_region() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        let p1 = a.alloc(layout(1000, 8)).unwrap();
        assert_eq!(a.used(), 1000);
        a.reset();
        assert_eq!(a.used(), 0);
        let p2 = a.alloc(layout(1000, 8)).unwrap();
        assert_eq!(p1, p2);
        unsafe { std::ptr::write_bytes(p2, 0xCD, 1000) };
    }

    // ---- ScopedAlloc routing (NOT under Miri: it doesn't run a
    // #[global_allocator], and the thread-local backing intentionally leaks).
    // These call ScopedAlloc directly; the test thread's own allocations go to
    // the real (system) global allocator, so they don't perturb the arena. ----

    #[test]
    #[cfg_attr(miri, ignore)]
    fn scoped_routes_to_system_when_inactive() {
        // No scope entered: depth 0 → System. dealloc must round-trip.
        let l = layout(64, 8);
        let p = unsafe { ScopedAlloc.alloc(l) };
        assert!(!p.is_null());
        assert!(!TL.with(|tl| {
            let b = tl.base.get() as usize;
            (p as usize) >= b && (p as usize) < tl.end.get() && b != 0
        }));
        unsafe { ScopedAlloc.dealloc(p, l) };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn scoped_bumps_inside_scope_and_resets() {
        let l = layout(128, 16);
        let scope = Scope::enter();
        let p1 = unsafe { ScopedAlloc.alloc(l) };
        let p2 = unsafe { ScopedAlloc.alloc(l) };
        let in_arena = |p: *mut u8| {
            TL.with(|tl| {
                let b = tl.base.get() as usize;
                b != 0 && (p as usize) >= b && (p as usize) < tl.end.get()
            })
        };
        assert!(in_arena(p1) && in_arena(p2), "in-scope allocs come from the arena");
        assert!(p2 as usize >= p1 as usize + 128, "no overlap");
        assert_eq!(p1 as usize % 16, 0);
        // dealloc of an in-arena pointer is a no-op (doesn't free / crash).
        unsafe { ScopedAlloc.dealloc(p1, l) };
        // Finish the scope manually (as compile() does) and reset.
        let outer = leave_no_reset();
        assert!(outer);
        reset();
        // After reset the next in-scope alloc reuses the region.
        let scope2 = Scope::enter();
        let p3 = unsafe { ScopedAlloc.alloc(l) };
        assert_eq!(p3, p1, "reset hands back the same region");
        let _ = leave_no_reset();
        reset();
        drop(scope2);
        std::mem::forget(scope);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn pause_routes_to_system_then_resumes() {
        let l = layout(64, 8);
        let scope = Scope::enter();
        let saved = pause(); // depth → 0
        let p_sys = unsafe { ScopedAlloc.alloc(l) }; // goes to System
        let in_arena = |p: *mut u8| {
            TL.with(|tl| {
                let b = tl.base.get() as usize;
                b != 0 && (p as usize) >= b && (p as usize) < tl.end.get()
            })
        };
        assert!(!in_arena(p_sys), "paused scope routes to System");
        unsafe { ScopedAlloc.dealloc(p_sys, l) };
        resume(saved); // depth restored
        let p_arena = unsafe { ScopedAlloc.alloc(l) };
        assert!(in_arena(p_arena), "resumed scope bumps from the arena again");
        let _ = leave_no_reset();
        reset();
        std::mem::forget(scope);
    }
}
