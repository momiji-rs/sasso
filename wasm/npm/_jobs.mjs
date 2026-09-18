// How many files to compile at once when `-j` is not given.
//
// `availableParallelism()` counts SMT threads. A compile is pure computation,
// so two hyperthreads on one core contend for the same execution units rather
// than overlapping stalls: on a Ryzen 7 8745HS (8 cores / 16 threads) over 138
// Lichess stylesheets, `-j 8` beat `-j 16` — 366 ms against 425 ms through this
// CLI, 208 against 235 through the native binary — and defaulting to the core
// count took that corpus from 444 ms to 364 ms. (Times rather than percentages
// on purpose: "faster by" reads differently depending on which of the two you
// divide by, and both readings appear in this file's history.) On a machine without SMT the two counts are equal and
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

/**
 * Logical CPUs, the number `-j` used to default to.
 *
 * `os.cpus().length` is the fallback and is NOT the same question: it counts
 * the host's CPUs and ignores the affinity mask, so under `taskset -c 0,1,8,9`
 * on a 16-thread machine it answers 16 where `availableParallelism()` answers
 * 4 (measured 2026-09-17). The package supports Node >= 16 and
 * `availableParallelism` arrived in 18.14, so on part of that range this is
 * the host count — `allowedCpusFromStatus` is what puts the floor back.
 */
export function logicalCpus() {
  return os.availableParallelism ? os.availableParallelism() : os.cpus().length;
}

/**
 * How many logical CPUs this process may run on, from `/proc/self/status`'s
 * `Cpus_allowed_list` (`0-1,8-9`), or `undefined` if it does not parse.
 *
 * Read on every Node version rather than only the ones missing
 * `availableParallelism`: it is one small file at start-up, and it means the
 * cap does not depend on which Node the user happens to run.
 */
export function allowedCpusFromStatus(text) {
  const line = /^Cpus_allowed_list:\s*(\S+)/m.exec(text);
  if (line === null) return undefined;
  let count = 0;
  for (const part of line[1].split(",")) {
    const [lo, hi] = part.split("-");
    const first = Number(lo);
    const last = hi === undefined ? first : Number(hi);
    if (!Number.isInteger(first) || !Number.isInteger(last) || last < first) return undefined;
    count += last - first + 1;
  }
  return count > 0 ? count : undefined;
}

/**
 * Physical cores from a Linux `/proc/cpuinfo`, or `undefined` when it does not
 * say — containers and VMs often publish no topology at all, and a number
 * derived from nothing is worse than the kernel's own count.
 *
 * A core is a `(physical id, core id)` pair: `core id` alone repeats across
 * sockets.
 *
 * The socket resets at each `processor` record rather than carrying over. On
 * x86 Linux both fields are printed together for every processor, so this
 * changes nothing there; it matters for a file that reports the socket for
 * some processors and not others, where carrying the previous value would
 * file a core under a socket the kernel never claimed. A file that names no
 * socket at all is left alone deliberately — every core then lands under `""`,
 * which is the right answer for the single-socket VMs that report `core id`
 * by itself, and giving up on those would lose them the fix.
 */
export function physicalCoresFromCpuinfo(text) {
  const cores = new Set();
  let pkg = "";
  for (const line of text.split("\n")) {
    const at = line.indexOf(":");
    if (at < 0) continue;
    const key = line.slice(0, at).trim();
    const value = line.slice(at + 1).trim();
    if (key === "processor") pkg = "";
    else if (key === "physical id") pkg = value;
    else if (key === "core id") cores.add(`${pkg}/${value}`);
  }
  return cores.size > 0 ? cores.size : undefined;
}

/**
 * CPUs' worth of cgroup CPU *quota*, or `undefined` when there is no limit.
 *
 * A quota is not an affinity mask: `docker run --cpus=2` leaves
 * `Cpus_allowed_list` at the whole machine and writes `200000 100000` to
 * `cpu.max` instead, so the mask says nothing about it. Node >= 18.14 already
 * accounts for both (`availableParallelism()` answers 2 in that container,
 * measured 2026-09-17) — this is for the older fallback, which answers 16.
 *
 * Deliberately only the cgroup at the root of this process's namespace, which
 * is the container case: a quota applied to a slice deeper in a host's
 * hierarchy is not walked, exactly as before.
 */
export function quotaCpusFromCgroup({ v2, v1Quota, v1Period }) {
  const cpus = (quota, period) => {
    const q = Number(quota);
    const p = Number(period);
    if (!Number.isFinite(q) || !Number.isFinite(p) || q <= 0 || p <= 0) return undefined;
    // Round up: half a CPU of quota is still a reason to run one worker, and
    // rounding down could reach zero.
    return Math.max(1, Math.ceil(q / p));
  };
  if (v2 !== undefined) {
    const [quota, period] = v2.trim().split(/\s+/);
    return quota === "max" ? undefined : cpus(quota, period);
  }
  if (v1Quota !== undefined && v1Period !== undefined) {
    // cgroup v1 writes -1 for "no limit", which the `q <= 0` rejection above
    // already turns into `undefined` — no separate branch for it, because a
    // branch no input can distinguish is a branch no test can hold honest.
    return cpus(v1Quota, v1Period);
  }
  return undefined;
}

/** The default for `-j`, given a way to read `/proc/cpuinfo` and a platform. */
export function defaultJobs({
  platform = process.platform,
  readCpuinfo = defaultReadCpuinfo,
  readStatus = defaultReadStatus,
  readCgroup = defaultReadCgroup,
} = {}) {
  const reported = logicalCpus();
  if (platform !== "linux") return reported;
  const status = readStatus();
  const allowed = status === undefined ? undefined : allowedCpusFromStatus(status);
  const quota = quotaCpusFromCgroup(readCgroup());
  // The smallest of the three, because they answer different questions and
  // only the first knows all of them: on Node >= 18.14 `availableParallelism`
  // covers both the affinity mask and the cgroup quota, and below it neither.
  const logical = Math.min(reported, allowed ?? reported, quota ?? reported);
  const text = readCpuinfo();
  if (text === undefined) return logical;
  const physical = physicalCoresFromCpuinfo(text);
  // The host's core count, capped by what this process may actually use.
  // Three different limits, because no single number covers them on every
  // supported Node: the affinity mask (`taskset`, a cpuset), a cgroup CPU
  // quota (`docker --cpus`, a Kubernetes CPU limit), and whatever the runtime
  // itself reports. Node >= 18.14 folds the first two into
  // `availableParallelism()`; below it neither, which is why they are read
  // here.
  //
  // It is deliberately NOT the number of physical cores inside the affinity
  // mask, which looks more correct and measures much worse. SMT only stops
  // paying once enough cores are in play; restrict the process and the second
  // thread on each core goes back to being worth having. Same corpus and
  // machine as above, best of five, `taskset` masks of whole cores:
  //
  //     cores allowed   1     2     4     6     7     8
  //     SMT is worth  +55%  +50%  +33%   -2%   -6%  -11%
  //
  // so counting cores within the mask would pick 2 where 4 is 50% faster, and
  // 4 where 8 is 33% faster. This rule picked the best of the measured options
  // at every mask size (measured 2026-09-17).
  return physical === undefined ? logical : Math.max(1, Math.min(physical, logical));
}

function defaultReadCpuinfo() {
  try {
    return readFileSync("/proc/cpuinfo", "utf8");
  } catch {
    return undefined;
  }
}

function defaultReadStatus() {
  try {
    return readFileSync("/proc/self/status", "utf8");
  } catch {
    return undefined;
  }
}

/**
 * Which cgroup files to look in, given a way to read one.
 *
 * cgroup v1 mounts the cpu controller under either name depending on the
 * distribution, and `cpu,cpuacct` is the more common of the two — a layout
 * this missed until review pointed it out, which is why the choice is a
 * function that a test can drive rather than a path buried in an fs call.
 *
 * Neither name is resolved from `/proc/self/mountinfo`: that is the container
 * case only, as documented on `quotaCpusFromCgroup`.
 */
export function cgroupFiles(read) {
  const v1 = ["/sys/fs/cgroup/cpu", "/sys/fs/cgroup/cpu,cpuacct"];
  const first = (name) => v1.map((dir) => read(`${dir}/${name}`)).find((t) => t !== undefined);
  return {
    v2: read("/sys/fs/cgroup/cpu.max"),
    v1Quota: first("cpu.cfs_quota_us"),
    v1Period: first("cpu.cfs_period_us"),
  };
}

function defaultReadCgroup() {
  return cgroupFiles((path) => {
    try {
      return readFileSync(path, "utf8");
    } catch {
      return undefined;
    }
  });
}
