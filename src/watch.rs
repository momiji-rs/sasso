//! `--watch` without a file-watching dependency: what to poll, how often, and
//! when a change becomes a compile.
//!
//! # Why polling
//!
//! Every native watcher — inotify, kqueue, `ReadDirectoryChangesW` — is a
//! syscall this crate cannot make. `[dependencies]` is empty and that is a
//! selling point, and `unsafe_code = "deny"` outside the Miri-verified arena,
//! so the FFI declarations those APIs need are not available either. What is
//! left in `std` is `fs::metadata`, and asking it repeatedly is a watcher.
//!
//! It is not a worse one here than it sounds. Measured on this machine, one
//! sweep of `fs::metadata` costs about 1.3 microseconds per file:
//!
//! | files | per sweep | at a 50 ms interval |
//! |---|---|---|
//! | 10 | 0.014 ms | 0.0% of a core |
//! | 200 | 0.262 ms | 0.5% |
//! | 500 | 0.665 ms | 1.3% |
//! | 2000 | 3.191 ms | 6.4% |
//! | 5000 | 9.138 ms | 18.3% |
//!
//! A stylesheet tree is the small end of that: a `--watch` session follows the
//! files ONE entry actually loaded, not a whole repository. But 18% of a core
//! for a tree that big is a real cost to leave running all afternoon, so the
//! interval is not a constant — see [`next_interval`].
//!
//! # Why the pieces here are pure
//!
//! The same reason `_coalesce.mjs` is its own module on the npm side: how many
//! compiles a burst of saves costs cannot be asserted from a `--watch` test.
//! The obvious check — save eight times, count the compiles — measures the
//! spacing of the writes against the window rather than the rule, and there is
//! no bound that both catches a regression and survives a loaded CI machine
//! stretching those gaps. Driven by a clock the test supplies, N changes
//! inside one window is exactly two runs, on any machine, every time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The floor on the poll interval: fast enough that a save feels immediate,
/// and 50 ms is also the npm CLI's coalescing window, so the two front ends
/// answer a burst the same way.
pub(crate) const MIN_INTERVAL: Duration = Duration::from_millis(50);

/// The ceiling. A tree big enough to reach this is one where a person is
/// waiting on the compile anyway, and half a second of latency beats a
/// watcher that eats a core.
pub(crate) const MAX_INTERVAL: Duration = Duration::from_millis(500);

/// One part in this that the watcher may spend asking the filesystem
/// questions — 2%, which the measured 1.3 us per file turns into a 50 ms
/// interval for anything under about 800 files.
pub(crate) const SWEEP_BUDGET: u32 = 50;

/// How long to coalesce after a run, matching the npm CLI's `windowMs`.
pub(crate) const WINDOW: Duration = Duration::from_millis(50);

/// The next poll interval, from what the last sweep cost.
///
/// The watcher spends at most one part in `budget` of its time asking the
/// filesystem questions — 2% by default, which at the measured 1.3 us per
/// file is 50 ms for anything under about 800 files and stretches from there.
/// A constant interval cannot do that: 50 ms is free for ten files and 18% of
/// a core for five thousand.
pub(crate) fn next_interval(sweep: Duration, budget: u32) -> Duration {
    let want = sweep * budget;
    want.clamp(MIN_INTERVAL, MAX_INTERVAL)
}

/// What the watcher remembers about one file: enough to tell a save from a
/// touch, and cheap enough to take every tick.
///
/// The length is in here beside the modification time because a filesystem
/// with a coarse timestamp — HFS+ and some network mounts keep whole seconds
/// — can leave the mtime unchanged across two saves a person makes in the
/// same second. Two fields disagree less often than one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl Stamp {
    /// A file that cannot be stat'd — deleted, or never there. It compares
    /// equal to itself, so a dependency that is missing and stays missing is
    /// not a change on every tick.
    pub(crate) const MISSING: Stamp = Stamp {
        modified: None,
        len: u64::MAX,
    };

    pub(crate) fn of(path: &Path) -> Stamp {
        match std::fs::metadata(path) {
            Ok(m) => Stamp {
                modified: m.modified().ok(),
                len: m.len(),
            },
            Err(_) => Stamp::MISSING,
        }
    }
}

/// The set of files a watch is following, and what they looked like last time.
///
/// `BTreeMap` rather than a hash map: the sweep order is then stable, which
/// makes a failure reproducible and the cost above predictable.
#[derive(Default, Debug)]
pub(crate) struct Snapshot {
    files: BTreeMap<PathBuf, Stamp>,
}

impl Snapshot {
    /// Replace the watched set. Called after every compile, because the
    /// dependency set changes when an `@use` is added or removed.
    ///
    /// The two groups are treated differently, and the difference is the
    /// whole correctness of this:
    ///
    /// - a `file` already followed KEEPS the stamp it had. Re-stamping it
    ///   here would record whatever is on disk NOW as the baseline for a
    ///   build made from what was there when the compiler read it — so a
    ///   save that lands while a compile is running would be absorbed and
    ///   never compiled. Measured before this: 1 in 4 lost, writing into a
    ///   680 ms compile.
    /// - a `dir` is always re-stamped, because the compile's own output
    ///   moves it. Creating a file changes its directory's mtime, so
    ///   keeping the old stamp would make every first write look like a
    ///   change and the watch would answer itself.
    ///
    /// `stamp` is injected so a test can drive this without a filesystem.
    pub(crate) fn follow<F>(
        &mut self,
        files: impl IntoIterator<Item = PathBuf>,
        dirs: impl IntoIterator<Item = PathBuf>,
        mut stamp: F,
    ) where
        F: FnMut(&Path) -> Stamp,
    {
        let mut next = BTreeMap::new();
        for p in files {
            let known = self.files.get(&p).copied();
            let s = known.unwrap_or_else(|| stamp(&p));
            next.insert(p, s);
        }
        for d in dirs {
            let s = stamp(&d);
            next.insert(d, s);
        }
        self.files = next;
    }

    /// Has any followed file changed? Updates the remembered stamps, so a
    /// change is reported once.
    pub(crate) fn changed<F>(&mut self, mut stamp: F) -> bool
    where
        F: FnMut(&Path) -> Stamp,
    {
        let mut changed = false;
        for (path, known) in self.files.iter_mut() {
            let now = stamp(path);
            if now != *known {
                *known = now;
                changed = true;
            }
        }
        changed
    }

    /// How many files are followed. Only the tests ask — the watcher itself
    /// never needs to count them — so it is not compiled into the binary.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.files.len()
    }
}

/// What the coalescing rule wants done at a given moment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Step {
    /// Nothing to do yet.
    Wait,
    /// Compile. `provisional` marks the run at the head of a burst, where a
    /// failure is likelier to be a half-written file than anything the user
    /// did: such a run reports nothing and removes nothing, and is always
    /// followed by an authoritative one.
    Run { provisional: bool },
}

/// The leading-edge-with-catch-up rule, driven by a clock the caller owns.
///
/// The first change runs immediately — the common case is one save with
/// nothing after it, and making it wait for a window that will stay empty is
/// pure latency. A provisional run ALWAYS gets a catch-up, not only when it
/// fails: it compiles what is on disk the instant the change is seen, which
/// is not always what the save finally leaves there, and a run that succeeds
/// on stale content would otherwise be the last word.
///
/// `now_ms` is any monotonically non-decreasing millisecond count.
#[derive(Debug)]
pub(crate) struct Coalesce {
    window_ms: u64,
    cooling_until: Option<u64>,
    dirty: bool,
}

impl Coalesce {
    pub(crate) fn new(window: Duration) -> Self {
        Coalesce {
            window_ms: window.as_millis() as u64,
            cooling_until: None,
            dirty: false,
        }
    }

    /// A change was seen.
    pub(crate) fn on_change(&mut self, now_ms: u64) -> Step {
        if self.cooling_until.is_some() {
            self.dirty = true;
            return Step::Wait;
        }
        self.start_cooling(now_ms);
        self.dirty = true; // the catch-up a provisional run always earns
        Step::Run { provisional: true }
    }

    /// Time has passed and nothing new was seen.
    pub(crate) fn on_tick(&mut self, now_ms: u64) -> Step {
        match self.cooling_until {
            Some(until) if now_ms >= until => {
                self.cooling_until = None;
                if self.dirty {
                    self.dirty = false;
                    self.start_cooling(now_ms);
                    // Never provisional. When it was, every run in the chain
                    // declined to report and each failure asked for another,
                    // so a genuinely broken file span forever in silence.
                    Step::Run { provisional: false }
                } else {
                    Step::Wait
                }
            }
            _ => Step::Wait,
        }
    }

    /// The outcome of the run this rule asked for. An AUTHORITATIVE failure
    /// must not ask for another, or the error is reported, re-run, reported
    /// again.
    pub(crate) fn finished(&mut self, provisional: bool, ok: bool) {
        if !provisional && !ok {
            self.dirty = false;
        }
    }

    fn start_cooling(&mut self, now_ms: u64) {
        self.cooling_until = Some(now_ms + self.window_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp_of(n: u64) -> Stamp {
        Stamp {
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(n)),
            len: n,
        }
    }

    /// The property a `--watch` test cannot assert: what a BURST costs. With
    /// the clock supplied, N changes inside one window is exactly two runs.
    #[test]
    fn a_burst_inside_one_window_costs_two_runs() {
        let mut c = Coalesce::new(Duration::from_millis(50));
        let mut runs = Vec::new();
        // Eight changes, 5 ms apart, all inside the first window.
        for i in 0..8u64 {
            if let Step::Run { provisional } = c.on_change(i * 5) {
                runs.push(provisional);
                c.finished(provisional, true);
            }
        }
        // The window closes.
        if let Step::Run { provisional } = c.on_tick(60) {
            runs.push(provisional);
            c.finished(provisional, true);
        }
        assert_eq!(runs, vec![true, false], "one provisional head, one catch-up");
    }

    /// The leading edge is the whole point: one save must not wait out a
    /// window that will stay empty.
    #[test]
    fn a_lone_save_compiles_immediately() {
        let mut c = Coalesce::new(Duration::from_millis(50));
        assert_eq!(c.on_change(0), Step::Run { provisional: true });
    }

    /// A provisional run earns a catch-up even when it SUCCEEDS: what it read
    /// is not always what the save finally left there.
    #[test]
    fn a_successful_provisional_run_still_gets_a_catch_up() {
        let mut c = Coalesce::new(Duration::from_millis(50));
        c.on_change(0);
        c.finished(true, true);
        assert_eq!(c.on_tick(49), Step::Wait, "not before the window closes");
        assert_eq!(c.on_tick(50), Step::Run { provisional: false });
    }

    /// …and an authoritative failure does NOT, or a broken file reports its
    /// error forever.
    #[test]
    fn an_authoritative_failure_does_not_ask_for_another() {
        let mut c = Coalesce::new(Duration::from_millis(50));
        c.on_change(0);
        c.finished(true, true);
        let step = c.on_tick(50);
        assert_eq!(step, Step::Run { provisional: false });
        c.finished(false, false);
        assert_eq!(c.on_tick(100), Step::Wait);
        assert_eq!(c.on_tick(1000), Step::Wait, "and stays quiet");
    }

    /// A change during the cool-down is not lost — it is what the catch-up is
    /// for.
    #[test]
    fn a_change_while_cooling_is_picked_up_by_the_catch_up() {
        let mut c = Coalesce::new(Duration::from_millis(50));
        c.on_change(0);
        c.finished(true, true);
        assert_eq!(c.on_change(10), Step::Wait, "coalesced into the window");
        assert_eq!(c.on_tick(50), Step::Run { provisional: false });
    }

    /// A file that is missing and stays missing is not a change on every
    /// tick — otherwise a watch with one unresolved `@use` would recompile
    /// forever.
    #[test]
    fn a_file_that_stays_missing_is_not_a_change() {
        let mut s = Snapshot::default();
        s.follow([PathBuf::from("/gone.scss")], [], |_| Stamp::MISSING);
        assert!(!s.changed(|_| Stamp::MISSING));
        // …and its appearance IS one.
        assert!(s.changed(|_| stamp_of(1)));
        assert!(!s.changed(|_| stamp_of(1)), "reported once");
    }

    /// The same second, twice: a filesystem with a one-second timestamp can
    /// leave the mtime alone across two saves, and the length catches it.
    #[test]
    fn a_same_second_save_of_a_different_length_is_a_change() {
        let mut s = Snapshot::default();
        let coarse = |len: u64| Stamp {
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            len,
        };
        s.follow([PathBuf::from("/a.scss")], [], |_| coarse(10));
        assert!(s.changed(|_| coarse(11)));
    }

    /// Following a new set keeps what is known about the files that stay: a
    /// file that has just been READ has not just changed, and re-stamping the
    /// whole set on every compile would be a recompile on the next tick.
    #[test]
    fn following_again_does_not_invent_a_change() {
        let mut s = Snapshot::default();
        s.follow([PathBuf::from("/a.scss"), PathBuf::from("/b.scss")], [], |_| {
            stamp_of(1)
        });
        s.follow([PathBuf::from("/a.scss"), PathBuf::from("/c.scss")], [], |_| {
            stamp_of(1)
        });
        assert_eq!(s.len(), 2);
        assert!(!s.changed(|_| stamp_of(1)));
    }

    /// The interval is not a constant: measured, a 5000-file sweep is 9 ms,
    /// and repeating that every 50 ms is 18% of a core left running all
    /// afternoon.
    #[test]
    fn the_interval_grows_with_what_a_sweep_costs() {
        // Ten files: 0.014 ms. The floor decides.
        assert_eq!(next_interval(Duration::from_micros(14), 50), MIN_INTERVAL);
        // 500 files: 0.665 ms, 50x is 33 ms — still the floor.
        assert_eq!(next_interval(Duration::from_micros(665), 50), MIN_INTERVAL);
        // 2000 files: 3.2 ms, 50x is 160 ms.
        assert_eq!(
            next_interval(Duration::from_micros(3191), 50),
            Duration::from_micros(159_550)
        );
        // 5000 files: 9.1 ms, 50x is 457 ms.
        assert_eq!(
            next_interval(Duration::from_micros(9138), 50),
            Duration::from_micros(456_900)
        );
        // And a pathological sweep is capped rather than unbounded.
        assert_eq!(next_interval(Duration::from_millis(100), 50), MAX_INTERVAL);
    }

    /// A save that lands WHILE the compile is running must survive being
    /// followed again. Re-stamping the file after the compile records the
    /// new bytes as the baseline for output built from the old ones, and
    /// nothing ever compiles them — measured at 1 in 4, writing into a
    /// 680 ms compile.
    #[test]
    fn a_file_changed_during_the_compile_is_still_a_change() {
        let mut s = Snapshot::default();
        let f = PathBuf::from("/a.scss");
        s.follow([f.clone()], [], |_| stamp_of(1));
        // The compile runs; the file is saved again; the watcher follows the
        // same set afterwards and must NOT adopt the new stamp.
        s.follow([f.clone()], [], |_| stamp_of(2));
        assert!(s.changed(|_| stamp_of(2)), "the mid-compile save was absorbed");
    }

    /// A directory is the opposite: the compile's own output moves it, so
    /// its stamp is taken fresh or the watch answers itself forever.
    #[test]
    fn a_directory_is_restamped_because_our_own_writes_move_it() {
        let mut s = Snapshot::default();
        let d = PathBuf::from("/out");
        s.follow([], [d.clone()], |_| stamp_of(1));
        // The compile created a file in it, moving its mtime.
        s.follow([], [d.clone()], |_| stamp_of(2));
        assert!(!s.changed(|_| stamp_of(2)), "our own write looked like a change");
    }
}
