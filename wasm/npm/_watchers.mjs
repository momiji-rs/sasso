/**
 * One live `fs.watch` handle per directory, and what to do when one dies.
 *
 * Its own module with an injectable `watch` because every way this goes
 * wrong is unreachable from a `--watch` test on a healthy machine. The
 * failures are real, though, and they are silent:
 *
 *   - Tearing every watcher down and rebuilding it on each compile
 *     leaves a gap in which a save is simply not seen. Measured at 1 in
 *     30 going stale FOREVER, and it got worse when a change doubled the
 *     number of rewatch cycles. So `sync` closes only what is no longer
 *     wanted and opens only what is new.
 *   - A watcher that fails takes the whole session with it: an `error`
 *     event with nobody listening throws, out of a callback with nothing
 *     to catch it.
 *   - A watcher that fails and is merely forgotten is worse than a crash
 *     in one way — nothing reopens it, nothing says so, and every save in
 *     that directory is missed for the life of the session.
 *
 * Two facts from node's own `internal/fs/watchers` source (v22.22.3)
 * shape this, and both are load-bearing:
 *
 *   - Before emitting `error`, node closes and nulls the handle, and
 *     deliberately does NOT fire `close` ("We don't use this.close()
 *     here to avoid firing the close event"). So `error` cannot be left
 *     to the `close` listener, the slot is genuinely uncovered when it
 *     arrives, and the dead handle cannot come back as a duplicate.
 *   - A watch that cannot be STARTED throws synchronously instead —
 *     including `ENOSPC`, "System limit for number of file watchers
 *     reached". That one must not be swallowed with the directory that
 *     merely does not exist.
 *
 * @param {object} o
 * @param {(dir: string, cb: (event: string, filename: string | null) => void) => object} o.watch
 * @param {(dir: string, event: string, filename: string | null) => void} o.onEvent
 * @param {(line: string) => void} o.report  a warning for the user, one line
 * @param {number} [o.retries]  re-arms allowed per directory before giving up
 */
export function makeWatchers({ watch, onEvent, report, retries = 3 }) {
  /** directory -> the single live handle watching it. */
  const live = new Map();
  /** Consecutive failures since this directory last delivered an event. */
  const failures = new Map();
  /** Directories already complained about, so one fault is one line. */
  const said = new Set();

  const sayOnce = (d, line) => {
    if (said.has(d)) return;
    said.add(d);
    report(line);
  };

  const open = (d) => {
    let handle;
    try {
      handle = watch(d, (event, filename) => {
        // A delivered event is the only proof the watcher WORKS —
        // opening one is not, which is why neither of these is reset
        // where the handle is created. The budget is for a directory
        // failing in a burst, not for one that failed an hour ago, and
        // a fault after a working stretch is worth saying again.
        failures.delete(d);
        said.delete(d);
        onEvent(d, event, filename);
      });
    } catch (e) {
      // A directory that is not there is not a fault — the probe waits
      // for it and the next rewatch tries again. Anything else means
      // this directory is NOT watched and nobody would ever know.
      if (e?.code !== "ENOENT") {
        sayOnce(d, `sasso: cannot watch ${d}: ${e?.message ?? e}`);
      }
      return;
    }

    // Only ever drop OUR handle. `sync` already deletes what it closes,
    // so by the time a `close` arrives the slot may hold a newer handle
    // for the same directory; evicting that one would reopen the
    // teardown gap this module exists to avoid.
    handle.on("close", () => {
      if (live.get(d) === handle) live.delete(d);
    });
    handle.on("error", (e) => {
      if (live.get(d) === handle) live.delete(d);
      const n = (failures.get(d) ?? 0) + 1;
      failures.set(d, n);
      if (n > retries) {
        sayOnce(
          d,
          `sasso: gave up watching ${d} after ${n} errors (${e?.message ?? e}) — ` +
            `changes there will be missed until the next compile`,
        );
        return;
      }
      open(d);
    });

    live.set(d, handle);
  };

  return {
    /**
     * Make the live set exactly `dirs`, touching nothing that is already
     * right. In the common case the set does not change between compiles
     * and this does nothing at all.
     *
     * @param {Set<string>} dirs
     */
    sync(dirs) {
      for (const [d, handle] of live) {
        if (!dirs.has(d)) {
          live.delete(d);
          handle.close();
        }
      }
      for (const d of dirs) {
        if (!live.has(d)) open(d);
      }
    },
    /** How many directories are actually being watched. */
    get size() {
      return live.size;
    },
  };
}
