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
use std::sync::atomic::{AtomicUsize, Ordering};

// =========================================================================
// Process-global arena-region registry.
//
// `dealloc` only needs to answer "is this pointer inside SOME thread's arena
// region?" — an in-arena free is a no-op (reclaimed wholesale on scope reset),
// anything else forwards to System. Arena regions are never freed (they leak
// at thread exit by design), so the set of regions only grows, and a global
// table of `[base, end)` ranges can answer that question with two atomic loads
// per registered region — no thread-local access. This halves the macOS
// `_tlv_get_addr` dynamic-TLS traffic, which `alloc` (which genuinely needs
// the per-thread cursor) still pays.
//
// Memory ordering: everything is Relaxed, and that is sufficient because the
// slots are WRITE-ONCE. A false "in arena" needs `base != 0 && p >= base &&
// p < end` — both loads nonzero — and a nonzero load of a write-once slot is
// its final value, so the containment is real. A false "not in arena" (a
// stale 0) could only misroute a pointer that genuinely lives in the
// unobserved region — but a thread always sees its own claim/publication, and
// any pointer legitimately handed to another thread rides that channel's
// happens-before edge, which makes the registry writes visible to relaxed
// loads too. A thread with no such edge cannot legitimately hold the pointer.
// (The old per-thread check misclassified cross-thread frees of arena
// pointers as System allocations — the registry handles them correctly.)
// =========================================================================

/// Max registered arena regions (one per thread that ever compiles). A thread
/// past the cap simply runs without an arena.
const MAX_ARENAS: usize = 128;

/// A `[base, end)` region; the pair sits in one cache line per slot.
struct Region {
    base: AtomicUsize,
    end: AtomicUsize,
}

#[allow(clippy::declare_interior_mutable_const)] // repeated-element array init (MSRV < 1.79)
const ZERO_REGION: Region = Region {
    base: AtomicUsize::new(0),
    end: AtomicUsize::new(0),
};
/// Claim counter: slots `0..REGION_SLOTS` are claimed (possibly unpublished).
static REGION_SLOTS: AtomicUsize = AtomicUsize::new(0);
static REGIONS: [Region; MAX_ARENAS] = [ZERO_REGION; MAX_ARENAS];

/// Claim a slot and publish `[base, end)`. Returns `false` when the registry
/// is full (the caller must then NOT use the region as an arena: `dealloc`
/// would misroute its pointers to `System`).
fn register_region(base: usize, end: usize) -> bool {
    let idx = REGION_SLOTS.fetch_add(1, Ordering::Relaxed);
    if idx >= MAX_ARENAS {
        return false;
    }
    REGIONS[idx].base.store(base, Ordering::Relaxed);
    REGIONS[idx].end.store(end, Ordering::Relaxed);
    true
}

/// Whether `p` lies inside any registered arena region.
#[inline]
fn in_any_arena(p: usize) -> bool {
    let n = REGION_SLOTS.load(Ordering::Relaxed).min(MAX_ARENAS);
    for r in &REGIONS[..n] {
        let base = r.base.load(Ordering::Relaxed);
        if base != 0 && p >= base && p < r.end.load(Ordering::Relaxed) {
            return true;
        }
    }
    false
}

/// Pure bump arithmetic over absolute addresses: align `cur` up to `align`, add
/// `size`, and check the result fits at or below `end` (exclusive). Returns
/// `(aligned_start, new_cursor)` or `None` on overflow / no fit. Touches no
/// memory. `align` must be a power of two (guaranteed by [`Layout`]).
fn bump_compute(cur: usize, align: usize, size: usize, end: usize) -> Option<(usize, usize)> {
    let aligned = cur.checked_add(align - 1)? & !(align - 1);
    let next = aligned.checked_add(size)?;
    (next <= end).then_some((aligned, next))
}

// ── Arena reservation size ──────────────────────────────────────────────
//
// The region is reserved up front on first use. On a 64-bit host this is
// virtual — physical pages commit lazily on first touch, so a huge unused
// reservation costs ~nothing, and the size is a fixed 2 GiB. On wasm32 there
// is no lazy commit (`memory.grow` zero-fills and commits every page
// immediately) and the address space is only 4 GiB, so the reservation must
// be a realistic peak working-set bound: a single large-stylesheet compile
// peaks around 25 MiB, and the region grows the wasm heap ONCE on the first
// compile and is then reused (reset, not freed) — a fixed footprint, not
// per-compile growth. Anything that overflows the region spills to the system
// allocator with no loss of correctness.
//
// The wasm size has two layers of developer control:
//   • compile-time default — `SASSO_WASM_ARENA_MB` at build time (default 32),
//   • runtime override — [`set_arena_bytes`] before the first compile (0
//     disables the arena entirely: every allocation forwards to System).
// Native ignores both and always uses its 2 GiB virtual reservation.

/// Const decimal parser for the `SASSO_WASM_ARENA_MB` build-time value.
#[cfg(target_arch = "wasm32")]
const fn parse_mb(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut n = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        assert!(
            b >= b'0' && b <= b'9',
            "SASSO_WASM_ARENA_MB must be decimal digits"
        );
        n = n * 10 + (b - b'0') as usize;
        i += 1;
    }
    n
}

/// Compile-time default arena size (wasm): `SASSO_WASM_ARENA_MB` MiB, else 32.
#[cfg(target_arch = "wasm32")]
const WASM_DEFAULT_ARENA_SIZE: usize = match option_env!("SASSO_WASM_ARENA_MB") {
    Some(s) => parse_mb(s) * 1024 * 1024,
    None => 32 * 1024 * 1024,
};

/// Runtime override of the wasm arena size: `0` = unset (use the compile-time
/// default), `usize::MAX` = explicitly disabled, anything else = that many
/// bytes. Read only on wasm; native's [`effective_arena_size`] ignores it.
static ARENA_CONFIG: AtomicUsize = AtomicUsize::new(0);

/// Override the wasm arena reservation size, in **bytes**. Must be called
/// BEFORE the first `compile()` — the region is reserved on first use and then
/// fixed, so a later call has no effect. `0` disables the arena entirely
/// (every allocation forwards to the system allocator: lower memory, slower).
/// No effect on native targets (they always use the 2 GiB virtual reservation).
pub fn set_arena_bytes(bytes: usize) {
    ARENA_CONFIG.store(if bytes == 0 { usize::MAX } else { bytes }, Ordering::Relaxed);
}

/// The arena size to reserve, resolving the runtime override against the
/// compile-time default. `0` means "disabled" (the caller forwards to System).
#[cfg(target_arch = "wasm32")]
#[inline]
fn effective_arena_size() -> usize {
    match ARENA_CONFIG.load(Ordering::Relaxed) {
        0 => WASM_DEFAULT_ARENA_SIZE,
        usize::MAX => 0,
        n => n,
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn effective_arena_size() -> usize {
    2 * 1024 * 1024 * 1024 // 2 GiB virtual; the runtime override is wasm-only
}

/// Per-thread bump state. POD only (no `Drop`) — see the module-level safety
/// note. The backing region leaks at thread exit (virtual + lazily committed).
struct ThreadState {
    base: Cell<*mut u8>,
    end: Cell<usize>,
    cursor: Cell<usize>,
    /// Scope nesting depth. `0` = inactive: allocations pass through to System.
    depth: Cell<u32>,
    /// Set once if [`Self::reserve`] fails (OOM, registry full, or the arena
    /// is disabled): the alloc path then forwards straight to System without
    /// retrying the `#[cold]` reservation on every allocation.
    reserve_failed: Cell<bool>,
}

impl ThreadState {
    const fn new() -> ThreadState {
        ThreadState {
            base: Cell::new(std::ptr::null_mut()),
            end: Cell::new(0),
            cursor: Cell::new(0),
            depth: Cell::new(0),
            reserve_failed: Cell::new(false),
        }
    }

    /// Reserve the backing region on first use. Returns `false` on failure (the
    /// caller then forwards the request to the system allocator) — including
    /// when the arena is disabled (size 0) by the runtime override.
    #[cold]
    fn reserve(&self) -> bool {
        let size = effective_arena_size();
        if size == 0 {
            return false; // disabled: run on the system allocator
        }
        let Ok(layout) = Layout::from_size_align(size, 4096) else {
            return false;
        };
        // SAFETY: non-zero size, 4096 is a valid power-of-two alignment.
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            return false;
        }
        if !register_region(p as usize, p as usize + size) {
            // Registry full: this region must not serve as an arena (dealloc
            // wouldn't recognize its pointers). Hand it back and run without.
            // SAFETY: p came from System.alloc with this same layout.
            unsafe { System.dealloc(p, layout) };
            return false;
        }
        self.base.set(p);
        self.end.set(p as usize + size);
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
            if tl.base.get().is_null() {
                // Reserve once; if it fails (OOM / registry full / disabled),
                // remember that and forward to System on every later alloc
                // instead of re-running the cold reservation.
                if tl.reserve_failed.get() {
                    return unsafe { System.alloc(layout) };
                }
                if !tl.reserve() {
                    tl.reserve_failed.set(true);
                    return unsafe { System.alloc(layout) };
                }
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
        // The global region registry answers "arena or System?" without a
        // thread-local lookup (see its module section above).
        if !in_any_arena(ptr as usize) {
            // SAFETY: not from any arena, so it came from the system
            // allocator with this same layout.
            unsafe { System.dealloc(ptr, layout) };
        }
        // in-arena: no-op (reclaimed wholesale on scope reset)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Bump-arena fast path: if `ptr` is the MOST RECENT allocation in this
        // thread's arena (its end sits exactly at the cursor), resize it in
        // place by moving the cursor — no copy, and no dead intermediate buffer
        // left behind. A naive realloc (alloc-new + copy + no-op dealloc) is
        // what makes a growing `Vec` leak arena space on every doubling
        // (4→8→16→…); this reclaims it for the common "grow the value just
        // allocated" pattern, the dominant case in the parser/evaluator.
        let resized = TL.with(|tl| {
            if tl.depth.get() == 0 {
                return false;
            }
            let base = tl.base.get();
            if base.is_null() {
                return false;
            }
            let addr = ptr as usize;
            // `ptr` must lie in THIS arena (≥ base) AND be the last bump
            // (`addr + old_size == cursor`). A system pointer or an earlier
            // (non-tail) arena block fails this and takes the copy fallback.
            if addr < base as usize || addr + layout.size() != tl.cursor.get() {
                return false;
            }
            match addr.checked_add(new_size) {
                Some(new_end) if new_end <= tl.end.get() => {
                    tl.cursor.set(new_end);
                    true
                }
                // A grow past the arena end (or overflow) → copy fallback.
                _ => false,
            }
        });
        if resized {
            return ptr;
        }
        // Fallback: the stock `GlobalAlloc::realloc` (alloc new, copy the
        // overlap, free old). `self.alloc`/`self.dealloc` route arena-vs-system
        // themselves; the old block, if in-arena, is reclaimed at scope reset.
        // SAFETY: same contract and aliasing as the default impl.
        unsafe {
            let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
            let new_ptr = self.alloc(new_layout);
            if !new_ptr.is_null() {
                core::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
            new_ptr
        }
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
        let (aligned, next) = bump_compute(self.cursor.get(), layout.align(), layout.size(), self.end)?;
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

    /// Twin of [`ScopedAlloc::realloc`]'s logic: extend the last bump in place,
    /// else copy to a fresh allocation.
    fn realloc(&self, ptr: *mut u8, old: Layout, new_size: usize) -> Option<*mut u8> {
        let addr = ptr as usize;
        if addr >= self.base as usize && addr + old.size() == self.cursor.get() {
            let new_end = addr.checked_add(new_size)?;
            if new_end <= self.end {
                self.cursor.set(new_end);
                return Some(ptr);
            }
        }
        let np = self.alloc(Layout::from_size_align(new_size, old.align()).ok()?)?;
        // SAFETY: np is a fresh, non-overlapping allocation of >= copy length.
        unsafe { core::ptr::copy_nonoverlapping(ptr, np, old.size().min(new_size)) };
        Some(np)
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
    fn realloc_extends_tail_in_place_else_copies() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        let p = a.alloc(layout(8, 8)).unwrap();
        // SAFETY: p is a live 8-byte allocation.
        unsafe { std::ptr::write_bytes(p, 0xCD, 8) };
        let used = a.used();
        // Tail grow: same pointer, only the delta is bumped (no dead buffer).
        let p2 = a.realloc(p, layout(8, 8), 16).unwrap();
        assert_eq!(p, p2, "tail realloc grows in place");
        assert_eq!(a.used(), used + 8, "only the +8 delta is consumed");
        // SAFETY: p2 still points at the (now larger) live block.
        unsafe { assert_eq!(*p2, 0xCD, "data preserved in place") };
        // Intervening allocation makes p2 no longer the tail → copy fallback.
        let _q = a.alloc(layout(8, 8)).unwrap();
        let used_mid = a.used();
        let p3 = a.realloc(p2, layout(16, 8), 32).unwrap();
        assert_ne!(p2, p3, "non-tail realloc copies to a fresh block");
        assert!(a.used() > used_mid, "fallback allocates fresh");
        // SAFETY: p3 is the fresh block holding the copied bytes.
        unsafe { assert_eq!(*p3, 0xCD, "data copied to the new block") };
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
        assert!(
            in_arena(p1) && in_arena(p2),
            "in-scope allocs come from the arena"
        );
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
