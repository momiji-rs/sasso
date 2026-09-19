/**
 * Whether one filesystem event should provoke a recompile.
 *
 * Its own module, and pure, for one reason: the hardest branch cannot be
 * reached from a test on the machines we develop on. `fs.watch` is allowed
 * to call back without a filename, and some filesystems do — but macOS and
 * Linux name their events, so the nameless path never executes there no
 * matter what a `--watch` test does to the disk. Left inside `runWatch` it
 * would be a closure nothing could call; out here it is eight lines with a
 * table of cases against it.
 *
 * That branch is not hypothetical. Scheduling blindly on a nameless event
 * bypasses the self-output check, and then the compile that writes
 * `out.css` triggers the compile that writes `out.css`.
 *
 * @param {object} event
 * @param {string|null} event.path   the changed file, or null when the
 *   platform did not say which one
 * @param {Set<string>} event.known  files the last successful compile
 *   loaded, entry included
 * @param {Set<string>} event.ours   the output and its source map: written
 *   BY this watch, so never a reason to run it again
 * @param {boolean} event.failing    the last compile failed, so the output
 *   has already been removed and the fix may be a file that did not exist
 *   when `known` was taken
 * @param {() => boolean} event.anyKnownMoved  did any file in `known`
 *   change mtime since the last compile? Called only when there is no
 *   filename, because it stats every dependency.
 */
export function triggersRecompile({ path, known, ours, failing, anyKnownMoved }) {
  if (path === null) {
    // No name to judge by, so ask the files. While failing there is
    // nothing of ours on disk to cause a loop, and the fix may be a
    // brand-new file with no earlier mtime to differ from — so anything
    // is worth a try.
    return failing || anyKnownMoved();
  }
  if (ours.has(path)) return false;
  return failing || known.has(path);
}
