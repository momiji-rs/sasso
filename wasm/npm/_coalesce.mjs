/**
 * The rule that decides how many compiles a burst of saves costs.
 *
 * Its own module, and injectable timers, because the property it
 * guarantees cannot be asserted from a `--watch` test. The obvious
 * integration check — save eight times, count the compiles — measures the
 * spacing of the writes against the window, not the rule:
 *
 *   8 saves, 10ms apart, WITH this   ->  3 compiles
 *   8 saves, 10ms apart, WITHOUT it  ->  6 or 7
 *   8 saves, 0ms apart, either way   ->  1 (the OS coalesces the events)
 *
 * So there is no bound that both catches the regression and survives a
 * loaded CI machine stretching those gaps. With a fake clock there is no
 * gap to stretch: N calls inside one window is exactly two runs, on any
 * machine, every time.
 *
 * The shape is a leading edge with a trailing catch-up. The first event
 * runs immediately — the common case is one save with nothing after it,
 * and making it wait for a window that will stay empty is where the old
 * trailing debounce spent 50ms of every edit.
 *
 * @param {object} o
 * @param {number} o.windowMs      how long to coalesce after a run
 * @param {(provisional: boolean) => boolean} o.run  does the work;
 *   returns whether it succeeded. Called with `true` at the head of a
 *   burst, where a failure is likelier to be a half-written file than
 *   anything the user did, and `false` for the catch-up, which is
 *   authoritative.
 * @param {typeof setTimeout} [o.setTimer]
 * @returns {() => void} call once per filesystem event
 */
export function coalesce({ windowMs, run, setTimer = setTimeout }) {
  let cooling = null;
  let dirty = false;

  const fire = (provisional) => {
    cooling = setTimer(() => {
      cooling = null;
      if (dirty) {
        dirty = false;
        // Never provisional. When it was, every run in the chain declined
        // to report and each failure asked for another, so a genuinely
        // broken file spin forever in silence.
        fire(false);
      }
    }, windowMs);
    // Re-run only after a PROVISIONAL failure — "that was probably a
    // half-written file, try again properly". An authoritative failure has
    // been reported already, and asking for another reports it again.
    if (!run(provisional) && provisional) dirty = true;
  };

  return () => {
    if (cooling !== null) {
      dirty = true;
      return;
    }
    fire(true);
  };
}
