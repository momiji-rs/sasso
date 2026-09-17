// How many files to compile at once when `-j` is not given.
//
// `availableParallelism()` counts SMT threads. A compile is pure computation,
// so two hyperthreads on one core contend for the same execution units rather
// than overlapping stalls: on a Ryzen 7 8745HS (8 cores / 16 threads) over 138
// Lichess stylesheets, `-j 8` beat `-j 16` by 16% through this CLI and by 13%
// through the native binary, and defaulting to the core count took that corpus
// from 451 ms to 342 ms. On a machine without SMT the two counts are equal and
// nothing changes.
//
// Linux publishes the topology in `/proc/cpuinfo`, which is a read rather than
// a fork. Apple silicon has no SMT, so the logical count is already right
// there; Intel Macs and Windows keep the logical count rather than pay a
// subprocess at start-up for a number that is, at worst, today's default.
import { readFileSync } from "node:fs";
// Default import, NOT a named one: `availableParallelism` only exists on
// Node >= 18.14, and a missing named export fails ESM *linking* — this module
// is reached from the package's `bin`, so the CLI would not start at all on an
// older Node, before any fallback could run. (`_loader.mjs` carries the same
// note for the library entries.)
import os from "node:os";

/** Logical CPUs, the number `-j` used to default to. */
export function logicalCpus() {
  return os.availableParallelism ? os.availableParallelism() : os.cpus().length;
}

/**
 * Physical cores from a Linux `/proc/cpuinfo`, or `undefined` when it does not
 * say — containers and VMs often publish no topology at all, and a number
 * derived from nothing is worse than the kernel's own count.
 *
 * A core is a `(physical id, core id)` pair: `core id` alone repeats across
 * sockets.
 */
export function physicalCoresFromCpuinfo(text) {
  const cores = new Set();
  let pkg = "";
  for (const line of text.split("\n")) {
    const at = line.indexOf(":");
    if (at < 0) continue;
    const key = line.slice(0, at).trim();
    const value = line.slice(at + 1).trim();
    if (key === "physical id") pkg = value;
    else if (key === "core id") cores.add(`${pkg}/${value}`);
  }
  return cores.size > 0 ? cores.size : undefined;
}

/** The default for `-j`, given a way to read `/proc/cpuinfo` and a platform. */
export function defaultJobs({ platform = process.platform, readCpuinfo = defaultReadCpuinfo } = {}) {
  const logical = logicalCpus();
  if (platform !== "linux") return logical;
  const text = readCpuinfo();
  if (text === undefined) return logical;
  const physical = physicalCoresFromCpuinfo(text);
  // Never more than the kernel offers: a `taskset`-restricted or cgroup-capped
  // process sees fewer logical CPUs than the machine has cores.
  return physical === undefined ? logical : Math.max(1, Math.min(physical, logical));
}

function defaultReadCpuinfo() {
  try {
    return readFileSync("/proc/cpuinfo", "utf8");
  } catch {
    return undefined;
  }
}
