#!/usr/bin/env node
// sasso CLI — `npx sasso input.scss [output.css]`. Pure Node, no dependencies
// of its own: it compiles through the native addon when the platform package
// is installed and the wasm build otherwise (see `loadEngine`), and spreads
// independent jobs over `node:worker_threads`.
// A subset of the dart-sass `sass` CLI flags, sharing the package's compiler.
import {
  readFileSync,
  writeFileSync,
  writeSync,
  watch,
  statSync,
  existsSync,
  readdirSync,
  mkdirSync,
  realpathSync,
  rmSync,
  openSync,
  readSync,
  closeSync,
  accessSync,
  constants as fsConstants,
} from "node:fs";
import { basename, dirname, join, resolve, relative, sep, delimiter } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { isMainThread, workerData, parentPort, Worker } from "node:worker_threads";
import { spawnSync } from "node:child_process";
import { constants as osConstants } from "node:os";
// The pool's default size — physical cores rather than SMT threads on Linux,
// where `/proc/cpuinfo` publishes the topology, and the CPU count everywhere
// else. See _jobs.mjs for the measurement and for both fallbacks.
import { defaultJobs } from "./_jobs.mjs";
// The accepted deprecation ids, shared with the JS API so there is one copy.
import { DEPRECATION_IDS } from "./_deprecations.mjs";
// The prebuilt-addon rules, shared with native.mjs: which engine this platform
// is SUPPOSED to run decides whether a wasm fallback is news (see `loadEngine`).
import { nativePackage, platformKey } from "./_addon.mjs";

/**
 * The engine, chosen at startup rather than imported statically.
 *
 * `sasso-native-<platform>` is an `optionalDependency`, so `npm install sasso`
 * already fetched the native addon on the four prebuilt targets — it is the
 * same compiler as the wasm build, byte-identical in output (`napi/test.mjs`
 * asserts that), and about 2.2x the throughput. Everywhere else the wasm build
 * takes over, and the CLI picks the SPEED variant of it: a command line has
 * none of the download-size pressure that makes `sasso.mjs` the right default
 * for a bundled web build.
 *
 * `SASSO_ENGINE=wasm|native` forces one, which is what the tests use to hold
 * both to the same output.
 */
let compile, compileString, Exception, Logger;

/**
 * How this process chose its engine, for `--engine` and for the fallback
 * warning. The choice was completely unobservable before: an install whose
 * addon did not land compiled at roughly half the throughput and said nothing,
 * so answering "which engine am I on?" took bisecting the install
 * (momiji-rs/sasso#24).
 */
const engine = { kind: null, requested: undefined, platform: null, addon: null, error: null, refused: false };

async function loadEngine() {
  const want = process.env.SASSO_ENGINE;
  engine.requested = want;
  engine.platform = platformKey();
  engine.addon = nativePackage();
  let mod;
  let kind = "native";
  if (want !== "wasm") {
    try {
      mod = await import("./native.mjs");
    } catch (e) {
      // Through the same coerced string in both places: a `throw null` or a
      // thrown string from anything native.mjs imports has no `.message`, and
      // reading it would replace the load failure with a TypeError.
      engine.error = e && e.message ? String(e.message) : String(e);
      // `fail` writes synchronously, which matters because it exits at once.
      // A refused addon is a different answer from a missing one: it WAS found
      // and rejected. Kept apart so the report names which happened, and so
      // the generic fallback warning does not repeat what is said just below.
      engine.refused = e?.code === "SASSO_ADDON_VERSION_MISMATCH";
      if (want === "native") fail(`error: SASSO_ENGINE=native but the addon is unavailable: ${engine.error}`);
      // An ABSENT addon is the ordinary case on the platforms with no prebuild,
      // and falling back to wasm is the whole design — it says nothing. An
      // addon that is present but version-skewed is a broken install: wasm
      // keeps the OUTPUT correct, so the build still succeeds, but staying
      // quiet would trade a wrong compile for a slow one with nothing to read.
      if (engine.refused) {
        writeStderrSync(`warning: ${engine.error}\nwarning: falling back to the wasm engine, which is slower.\n`);
      }
    }
  }
  if (!mod) {
    mod = await import("./sasso.speed.mjs");
    kind = "wasm";
  }
  ({ compile, compileString, Exception, Logger } = mod);
  engine.kind = kind;
  return kind;
}

/** Why `engine.kind` and not the other one, in one clause. */
function engineReason() {
  if (engine.requested === "wasm" || engine.requested === "native") return `forced by SASSO_ENGINE=${engine.requested}`;
  // "the native addon", not "the prebuilt addon": `SASSO_NATIVE_BINARY` and a
  // repo checkout both load one that no platform package delivered.
  if (engine.kind === "native") return "the default here: the native addon loaded";
  if (engine.refused) return "FELL BACK: the prebuilt addon was refused (its version does not match sasso)";
  if (engine.addon) return "FELL BACK: a prebuilt addon exists for this platform but did not load";
  return "the default here: no addon is prebuilt for this platform";
}

/** `--engine`: the whole engine decision, in a form an issue can be pasted into. */
function engineReport() {
  const lines = [];
  // The hand-off first, because with a release binary on PATH the honest answer
  // to "which engine am I on?" is "none of them" — and when there is no binary,
  // `handoff.why` says what was in the way, which is the question that follows.
  if (handoff.path) lines.push(`binary:   ${handoff.path} — every compile is handed to it (${handoff.why})`);
  else if (handoff.why) lines.push(`binary:   not used — ${handoff.why}`);
  lines.push(
    `engine:   ${engine.kind === "native" ? "native (Node addon)" : "wasm (speed build)"} — ${engineReason()}` +
      (handoff.path ? " (unused while the binary above is there)" : ""),
    `platform: ${engine.platform}${engine.addon ? ` (prebuilt addon: ${engine.addon})` : " (no prebuilt addon)"}`,
  );
  // Only ever set when loading the addon was TRIED and failed, so this is the
  // one line that separates "never installed" from "installed but unloadable"
  // — the question the silent fallback used to swallow. First line only: a
  // `SASSO_NATIVE_BINARY` miss carries Node's whole require stack, and one
  // `key: value` per line is what makes this output pasteable.
  if (engine.error) lines.push(`addon:    did not load: ${engine.error.split("\n")[0]}`);
  lines.push(`sasso:    ${packageVersion()}`);
  return lines.join("\n") + "\n";
}

/**
 * A two-line warning on stderr — what happened, then what to do about it — when
 * a platform that HAS a prebuilt addon compiled through wasm anyway. That is an
 * install accident (`--omit=optional`, a partial lockfile, an unloadable addon),
 * not a supported configuration, and it costs roughly half the throughput.
 *
 * Four ways it stays quiet, each for its own reason. `SASSO_ENGINE=wasm` states
 * the intent, so a fallback is not news. A platform with no prebuild is RUNNING
 * its supported engine, and a warning nobody can act on is noise. A refused
 * addon has already been reported by `loadEngine`, in more detail and with the
 * fix in it. And `--quiet` means "don't print warnings" — dart's contract,
 * which this CLI keeps to the letter (stderr is empty under `-q`, asserted);
 * `--engine` is then the way to ask, and it answers whatever the flags say.
 *
 * Once per run, from the main thread: every worker loads its own engine, so
 * warning there would print this per core.
 */
function warnIfFellBack(opts) {
  if (opts.quiet) return;
  if (engine.kind !== "wasm" || engine.requested === "wasm" || !engine.addon) return;
  // A version skew already printed the same fact with the fix in it (#115).
  if (engine.refused) return;
  writeStderrSync(
    `sasso: WARNING: ${engine.addon} is prebuilt for this platform but did not load, so this run ` +
      `compiles through wasm — roughly half the throughput.\n` +
      `sasso: run \`sasso --engine\` for the reason, or set SASSO_ENGINE=wasm to choose wasm silently.\n`,
  );
}

/**
 * The release binary, when this CLI should hand it the whole command line
 * rather than compile in-process.
 *
 * Same compiler, same flags, byte-identical output — but this package pays
 * Node's start-up and then moves every file's source and CSS across the napi
 * boundary, and the binary pays neither. Measured on 40 entry points with
 * `--style=compressed --no-source-map`, published artifacts, macOS/arm64, one
 * run for the whole set (2026-09-18): the binary 15.1 ms, this CLI on the
 * native addon 104.1 ms, this CLI delegating 48.2 ms. So a `brew install`ed
 * sasso sitting beside `npm install sasso` was ~7x the throughput of the one
 * npm reached for, which is what momiji-rs/sasso#24 asked us to stop wasting,
 * and handing it the command line recovers 2.2x of it. What is left is Node
 * itself: 35.0 ms of the 48.2 is start-up and the spawn (the same delegated
 * command line on ONE tiny file), which is why this is a hand-off and not a
 * faster engine.
 *
 * `SASSO_BINARY`:
 *   - unset                  auto: a `sasso` on PATH whose version matches
 *                            this package EXACTLY is used, and anything else
 *                            is passed over silently.
 *   - a path                 use that binary, version unchecked — explicit is
 *                            explicit, and it is how you drive an unreleased
 *                            build.
 *   - 0 / off / false / no   never delegate. Empty counts as unset.
 * `SASSO_ENGINE=wasm|native` also turns delegation off: it demands a specific
 * in-process engine, and a subprocess is not one.
 *
 * The version gate is the whole safety story for the automatic case. Without
 * it, a project pinning `sasso` in its devDependencies would silently compile
 * with whatever sasso happens to be on a developer's PATH. That is #114 —
 * a version-skewed addon loaded anyway, quietly dropping options it could not
 * apply — one process further out, where it is harder to see.
 */
const NEVER_DELEGATE = new Set(["0", "off", "false", "no"]);

/** Set on the child, so a `sasso` on PATH that is really this CLI cannot loop. */
const DELEGATE_MARK = "SASSO_CLI_DELEGATED";

/**
 * What `pickBinary` decided, in the same words twice: `SASSO_DEBUG_ENGINE`
 * prints it as it happens and `--engine` reports it afterwards. One string, so
 * the two can never disagree about why a binary was or was not used.
 */
const handoff = { path: null, why: null };

function engineDebug(msg) {
  if (process.env.SASSO_DEBUG_ENGINE) writeStderrSync(`sasso: ${msg}\n`);
}

/** Compile here, and remember what was in the way. */
function compileInProcess(why) {
  handoff.why = why;
  engineDebug(`compiling in-process: ${why}`);
  return undefined;
}

/** Hand `path` the command line, and remember why it was the right one. */
function handTo(path, why) {
  handoff.path = path;
  handoff.why = why;
  engineDebug(`handing the command line to ${path}: ${why}`);
  return path;
}

/**
 * True for a real executable image, false for a script.
 *
 * `npm install -g sasso` puts a `sasso` on PATH that IS this file behind a
 * `#!/usr/bin/env node` line, so a PATH lookup finds it and delegating to it
 * would fork bomb. Refusing anything that is not a native image rules that out
 * structurally rather than by guessing from the path, and also declines shell
 * wrappers, whose exit codes and stdio we would not control.
 * (`DELEGATE_MARK` still backs this up for a wrapper reached some other way.)
 */
function isNativeImage(path) {
  let fd;
  try {
    fd = openSync(path, "r");
    const head = Buffer.alloc(4);
    if (readSync(fd, head, 0, 4, 0) < 4) return false;
    const magic = head.readUInt32BE(0);
    return (
      magic === 0x7f454c46 || // ELF
      magic === 0xcffaedfe || // Mach-O 64, little-endian (arm64/x86_64 macOS)
      magic === 0xcefaedfe || // Mach-O 32
      magic === 0xcafebabe || // Mach-O universal
      magic === 0xcafebabf || // Mach-O universal, 64-bit
      // PE/COFF ("MZ"). Untested: no CI job has ever run on Windows (#85).
      head.readUInt16BE(0) === 0x4d5a
    );
  } catch {
    return false;
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

/**
 * The first executable native `sasso` on PATH, or undefined.
 *
 * An empty `PATH` entry means the current directory to a POSIX shell, and is
 * skipped here on purpose: this lookup decides who receives the project's whole
 * command line, and honouring it would let a checkout with a `sasso` beside its
 * `package.json` be handed it. The divergence only ever finds FEWER binaries
 * than the shell would, and not finding one costs nothing but speed — the
 * in-process engine compiles the same bytes. `SASSO_BINARY=./sasso` is how to
 * ask for one there deliberately.
 */
function sassoOnPath() {
  const names = process.platform === "win32" ? ["sasso.exe", "sasso"] : ["sasso"];
  for (const dir of (process.env.PATH || "").split(delimiter)) {
    if (!dir) continue;
    for (const name of names) {
      const candidate = join(dir, name);
      try {
        accessSync(candidate, fsConstants.X_OK);
      } catch {
        continue;
      }
      if (isNativeImage(candidate)) return candidate;
    }
  }
  return undefined;
}

/**
 * The binary's version, or undefined if it does not identify itself as sasso.
 *
 * `sasso --version` prints exactly `sasso <version>` and nothing else
 * (src/main.rs: one `println!`), where this CLI prints a bare `<version>`,
 * dart's format. The WHOLE output has to be that line — not its last field, and
 * not its first line either: this is the gate that decides whether a stranger
 * gets the project's command line, and something else installed as `sasso` can
 * print a version too. Reading only the last field would accept
 * `some-other-tool 0.16.0`, and reading only the first line would accept
 * anything that leads with a plausible one.
 */
function binaryVersion(path) {
  const r = spawnSync(path, ["--version"], { encoding: "utf8", timeout: 10000 });
  if (r.error || r.status !== 0) return undefined;
  const m = /^sasso (\S+)$/.exec(String(r.stdout || "").trim());
  return m ? m[1] : undefined;
}

/**
 * The binary to hand this command line to, or undefined to compile in-process.
 *
 * `opts` is already parsed, so `--help`/`--version` have exited in `parseArgs`
 * and still answer from this file alone: a metadata question must not start
 * depending on a subprocess any more than it depended on a compiler.
 */
function pickBinary(opts) {
  if (process.env[DELEGATE_MARK]) return compileInProcess(`${DELEGATE_MARK} is set: this process IS the hand-off`);

  const wantEngine = process.env.SASSO_ENGINE;
  if (wantEngine === "wasm" || wantEngine === "native") {
    return compileInProcess(`SASSO_ENGINE=${wantEngine} demands an in-process engine`);
  }

  const want = process.env.SASSO_BINARY;
  if (want !== undefined && want !== "" && NEVER_DELEGATE.has(want.toLowerCase())) {
    return compileInProcess("SASSO_BINARY declines the binary");
  }

  // The binary has no watcher (#86), so `--watch` must stay in-process or
  // delegating would take a working command line and break it.
  if (opts.watch) return compileInProcess("--watch is not in the binary (#86)");
  // `--update` is no longer on that list: the binary has it, and walks the
  // same dependency graph this CLI does. It is still held back from the
  // EXPLICIT `SASSO_BINARY=<path>` hand-off below, which is documented as
  // version-unchecked and may well name a binary from before the flag
  // existed; the version-matched hand-off further down cannot, because a
  // binary of this version has it by construction.
  const updateNeedsMatch = opts.update;

  if (want !== undefined && want !== "") {
    if (!isNativeImage(want)) fail(`error: SASSO_BINARY=${want} is not an executable sasso binary`);
    if (updateNeedsMatch) {
      return compileInProcess(`--update with SASSO_BINARY=${want}, whose version is unchecked`);
    }
    return handTo(want, `SASSO_BINARY=${want}, version unchecked`);
  }

  const found = sassoOnPath();
  if (!found) return compileInProcess("no sasso binary on PATH");
  const theirs = binaryVersion(found);
  const ours = packageVersion();
  // Silent by default: a mismatch is a normal state of the world, not a problem
  // to interrupt a build over. `--engine` is where to go and ask.
  if (theirs === undefined) {
    return compileInProcess(`${found} does not answer --version with \`sasso <version>\``);
  }
  if (theirs !== ours) {
    return compileInProcess(`${found} is ${theirs}, this package is ${ours}`);
  }
  return handTo(found, `the same version as this package, ${theirs}`);
}

/** Run the binary in our place. Never returns. */
function delegate(path) {
  const r = spawnSync(path, process.argv.slice(2), {
    stdio: "inherit",
    env: { ...process.env, [DELEGATE_MARK]: "1" },
  });
  if (r.error) fail(`error: could not run ${path}: ${r.error.message}`);
  if (r.signal) {
    // Report a killed child the way a shell does, rather than flattening every
    // signal into 1: ^C during a big build should read as 130, not as a
    // compile failure.
    const n = osConstants.signals[r.signal];
    process.exit(n ? 128 + n : 1);
  }
  process.exit(r.status === null ? 1 : r.status);
}

const HELP = `sasso — compile SCSS/Sass to CSS

Usage: sasso [options] <input.scss> [output.css]
       sasso [options] <input.scss>:<output.css> [<in>:<out> ...]
       sasso [options] <in-dir>:<out-dir>
       sasso [options] <dir>                 (compiles the tree in place)
       sasso [options] --stdin [output.css]
       cat a.scss | sasso --stdin

Options:
  -s, --style <expanded|compressed>  Output style (default: expanded).
  -I, --load-path <dir>              Add a load path for @use/@import (repeatable).
  -o, --output <file>                Write the CSS to <file> (the same as a
                                     second positional argument).
      --stdin                        Read the stylesheet from standard input.
      --indented                     Parse stdin as the indented .sass syntax.
      --[no-]source-map              Emit a source map (default: on when writing
                                     to a file, off for stdout).
      --embed-sources                Embed source text in the map's sourcesContent.
      --embed-source-map             Embed the source map as a data: URI in the CSS.
      --[no-]charset                 Emit @charset/BOM for non-ASCII output
                                     (default: on).
      --source-map-urls <relative|absolute>
                                     How the map references its sources
                                     (default: relative).
  -q, --[no-]quiet                   Suppress @warn / @debug / deprecation output.
      --silence-deprecation <IDS>    Don't print these deprecations
                                     (comma-separated; repeatable).
      --[no-]quiet-deps              Drop deprecation warnings raised inside
                                     dependencies: stylesheets reached through
                                     a load path, and whatever those load
                                     relatively. Their own @warn/@debug still
                                     prints, as in dart-sass.
      --[no-]stop-on-error           Don't compile more files once an error is
                                     encountered.
      --[no-]error-css               On a compile error, write a stylesheet
                                     describing it. NOT IMPLEMENTED in this CLI:
                                     the flag is accepted, and a failing compile
                                     always behaves as --no-error-css.
      --no-css                       Compile but discard the CSS: no output
                                     file, no stdout, and an existing output is
                                     left exactly as it was.
      --update                       Leave outputs already newer than their input
                                     and every stylesheet it loads.
  -w, --watch                        Recompile when the input or any dependency
                                     changes (requires <input> <output>).
  -j, --jobs <N>                     Compile at most N files at once
                                     (default: one per core, or per CPU
                                     where the core count is unknown).
      --loop <N>                     Recompile in-process N times and report
                                     throughput (stdout inputs only).
  -c, --[no-]color                   Accepted for compatibility (no-op: output is
                                     never colored).
      --[no-]unicode                 Unicode box glyphs in diagnostics
                                     (default: on).
  -h, --help                         Print this help.
      --version                      Print the version.
      --engine                       Print what this install compiles with and
                                     why: a sasso binary it hands the command
                                     line to, or its own engine (native addon
                                     or wasm).

An <in>:<out> pair may name DIRECTORIES: every .scss/.sass/.css file under
<in> that is not a partial compiles to the matching path under <out>.
Symlinked directories are followed, each one only once.

With no output file the CSS is written to stdout. A Sass error is printed to
stderr and exits non-zero.`;

/** The version npm installed: the `version` of the package.json beside this file. */
function packageVersion() {
  try {
    return JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")).version;
  } catch {
    return "unknown";
  }
}

function fail(msg) {
  writeStderrSync(String(msg).replace(/\n?$/, "\n"));
  process.exit(1);
}

// One shared cell, only ever used to sleep a millisecond (see below).
const idle = new Int32Array(new SharedArrayBuffer(4));

/**
 * Write to stderr and do not come back until the OS has it.
 *
 * `process.stderr.write` on a PIPE is asynchronous and `process.exit` throws
 * away whatever has not reached the kernel: a 480 KB error came out of
 * `sasso huge.scss 2>&1 | cat` as exactly 131072 bytes, every run, while the
 * same error redirected to a file was whole (measured 2026-09-17). A caller
 * that exits immediately afterwards therefore cannot use the stream.
 *
 * A full pipe raises EAGAIN rather than blocking, because Node puts stdio
 * pipes in non-blocking mode; that means the reader is behind, so wait a
 * moment and continue. EPIPE means there is no reader left to tell.
 */
function writeStderrSync(text) {
  const bytes = Buffer.from(text, "utf8");
  let at = 0;
  while (at < bytes.length) {
    try {
      at += writeSync(2, bytes, at, bytes.length - at);
    } catch (e) {
      if (e.code === "EAGAIN") {
        Atomics.wait(idle, 0, 0, 1);
        continue;
      }
      if (e.code === "EPIPE") return;
      throw e;
    }
  }
}

function parseArgs(argv) {
  const opts = {
    style: "expanded",
    loadPaths: [],
    stdin: false,
    indented: false,
    sourceMap: undefined, // tri-state: default depends on output target
    embedSources: false,
    embedSourceMap: false,
    charset: true,
    quiet: false,
    quietDeps: false,
    silenceDeprecations: [],
    stopOnError: false,
    noCss: false,
    // Tri-state: dart's default is "relative", but only an EXPLICIT
    // --source-map-urls is rejected when printing to stdout.
    sourceMapUrls: undefined,
    update: false,
    jobs: undefined,
    watch: false,
    unicode: true,
    loop: undefined,
    output: undefined,
    printEngine: false,
    positionals: [],
  };
  for (let i = 0; i < argv.length; i++) {
    let a = argv[i];
    const takeValue = (inline) => {
      if (inline !== undefined) return inline;
      const v = argv[++i];
      if (v === undefined) fail(`error: ${a} requires a value`);
      return v;
    };
    if (a === "--") {
      opts.positionals.push(...argv.slice(i + 1));
      break;
    } else if (a === "-h" || a === "--help") {
      process.stdout.write(HELP + "\n");
      process.exit(0);
    } else if (a === "--version") {
      // The PACKAGE's version, from the package.json beside this file — not
      // parsed out of an engine's `info`, which names the ENGINE crate: the
      // native addon reports `(sasso-native <ver>)`, the old regex missed it,
      // and the fallback printed the second field — dart's compatibility
      // version — as if it were ours.
      process.stdout.write(`${packageVersion()}\n`);
      process.exit(0);
    } else if (a === "--engine") {
      // Not answered here, unlike `--version`: the answer IS which engine loads,
      // so it is the one metadata question that has to load one. `main` prints
      // it after `loadEngine`, so a demanded-but-missing engine still fails
      // loudly there rather than reporting a fallback it did not take.
      opts.printEngine = true;
    } else if (a === "--stdin") {
      opts.stdin = true;
    } else if (a === "--no-stdin") {
      opts.stdin = false;
    } else if (a === "-w" || a === "--watch") {
      opts.watch = true;
    } else if (a === "--indented") {
      opts.indented = true;
    } else if (a === "--no-indented") {
      opts.indented = false;
    } else if (a === "--source-map") {
      opts.sourceMap = true;
    } else if (a === "--no-source-map") {
      opts.sourceMap = false;
    } else if (a === "--embed-sources") {
      opts.embedSources = true;
    } else if (a === "--no-embed-sources") {
      opts.embedSources = false;
    } else if (a === "--embed-source-map") {
      opts.embedSourceMap = true;
    } else if (a === "--no-embed-source-map") {
      opts.embedSourceMap = false;
    } else if (a === "-q" || a === "--quiet") {
      opts.quiet = true;
    } else if (a === "--no-quiet") {
      opts.quiet = false;
    } else if (a === "--quiet-deps") {
      opts.quietDeps = true;
    } else if (a === "--no-quiet-deps") {
      opts.quietDeps = false;
    } else if (a === "--stop-on-error") {
      opts.stopOnError = true;
    } else if (a === "--no-stop-on-error") {
      opts.stopOnError = false;
    } else if (a === "--no-css") {
      opts.noCss = true;
    } else if (a === "--update") {
      opts.update = true;
    } else if (a === "--charset") {
      opts.charset = true;
    } else if (a === "--no-charset") {
      opts.charset = false;
      // Accepted for dart-sass compatibility. `--error-css` is a real dart
      // feature this CLI does not implement (see HELP), and `--color` is a
      // no-op in the native CLI too. (`--jobs` is no longer in this company:
      // it caps the worker pool — see `runJobs`.)
    } else if (a === "--error-css" || a === "--no-error-css") {
      // no-op: a failing compile always behaves as --no-error-css here
    } else if (a === "-c" || a === "--color" || a === "--no-color") {
      // no-op: output is never colored
    } else if (a === "--unicode") {
      opts.unicode = true;
    } else if (a === "--no-unicode") {
      opts.unicode = false;
    } else if (a === "-j" || a === "--jobs" || a.startsWith("--jobs=") || (a.startsWith("-j") && a.length > 2)) {
      let inline;
      if (a.startsWith("--jobs=")) inline = a.slice(7);
      else if (a.startsWith("-j") && a.length > 2) inline = a.slice(2);
      opts.jobs = positiveInt("--jobs", takeValue(inline), USIZE_MAX);
    } else if (a === "--loop" || a.startsWith("--loop=")) {
      opts.loop = positiveInt("--loop", takeValue(a.startsWith("--loop=") ? a.slice(7) : undefined), U32_MAX);
    } else if (a === "--source-map-urls" || a.startsWith("--source-map-urls=")) {
      const inline = a.startsWith("--source-map-urls=") ? a.slice(18) : undefined;
      const v = takeValue(inline);
      if (v !== "relative" && v !== "absolute") fail(`error: unknown --source-map-urls "${v}"`);
      opts.sourceMapUrls = v;
    } else if (a === "-o" || a === "--output" || a.startsWith("--output=")) {
      const inline = a.startsWith("--output=") ? a.slice(9) : undefined;
      // Repeating it is an assignment, as in the native parser: the last one
      // wins. (Naming the output twice in DIFFERENT ways — `-o` plus a second
      // positional — is the error, and `validate` catches that.)
      opts.output = takeValue(inline);
    } else if (a === "-s" || a === "--style" || a.startsWith("--style=")) {
      const inline = a.startsWith("--style=") ? a.slice(8) : undefined;
      const v = takeValue(inline);
      if (v !== "expanded" && v !== "compressed") fail(`error: unknown style "${v}"`);
      opts.style = v;
    } else if (a === "--silence-deprecation" || a.startsWith("--silence-deprecation=")) {
      const inline = a.startsWith("--silence-deprecation=") ? a.slice(22) : undefined;
      const v = takeValue(inline);
      if (!v) fail("error: --silence-deprecation requires a value");
      for (const raw of v.split(",")) {
        const id = raw.trim();
        // dart rejects an unknown id rather than ignoring it, so a typo is
        // caught instead of quietly leaving the warning in place.
        if (!DEPRECATION_IDS.has(id)) fail(`error: Invalid deprecation "${id}".`);
        if (!opts.silenceDeprecations.includes(id)) opts.silenceDeprecations.push(id);
      }
    } else if (a === "-I" || a === "--load-path" || a.startsWith("--load-path=") || a.startsWith("-I")) {
      let inline;
      if (a.startsWith("--load-path=")) inline = a.slice(12);
      else if (a.startsWith("-I") && a.length > 2) inline = a.slice(2);
      opts.loadPaths.push(takeValue(inline));
    } else if (a.startsWith("-") && a !== "-" && !a.startsWith("-:")) {
      // `-` is standard input and `-:out.css` is a pair reading it, so neither
      // is an unknown option (the native CLI carves out the same two).
      fail(`error: unknown option ${a}`);
    } else {
      opts.positionals.push(a);
    }
  }
  validate(opts);
  return opts;
}

/**
 * A positive integer, or the native CLI's rejection of what was passed. The
 * token itself has to be a decimal integer, as Rust's `parse` requires:
 * `Number()` would take `1.0`, `1e3`, `0x2` and whitespace-padded values that
 * the native CLI refuses (`+3` and `03` it accepts, and so does this). `max` is
 * the Rust integer type the native parser uses, so a value that overflows there
 * is rejected here too rather than starting work nobody can wait for —
 * `--loop=4294967296` overflows a `u32` and would otherwise run four billion
 * compiles.
 */
function positiveInt(flag, value, max) {
  const digits = /^\+?[0-9]+$/.test(value);
  const n = digits ? BigInt(value) : 0n;
  if (!digits || n < 1n || n > max) {
    fail(`error: ${flag} expects a positive integer (got ${JSON.stringify(value)})`);
  }
  return Number(n);
}

/** `u32::MAX` and `usize::MAX`, the widths `parse_loop` and `parse_jobs` use. */
const U32_MAX = 4294967295n;
const USIZE_MAX = 18446744073709551615n;

/**
 * The combinations the native CLI rejects before compiling anything (dart-sass
 * rejects the source-map and arity ones with the same wording): the positional
 * grammar is `<input> [output]`, `--output` names ONE output, source-map flags
 * need a source map, and a map printed to stdout can only be an embedded one
 * with absolute sources.
 */
function validate(opts) {
  const operands = opts.positionals;
  const pairs = operands.some((a) => colonIndex(a) >= 0);

  // Checked in the native parser's order, so a command line that trips two
  // rules reports the same one there and here.
  if (opts.sourceMap === false) {
    if (opts.embedSourceMap) fail("error: --embed-source-map isn't allowed with --no-source-map.");
    if (opts.embedSources) fail("error: --embed-sources isn't allowed with --no-source-map.");
    if (opts.sourceMapUrls !== undefined) fail("error: --source-map-urls isn't allowed with --no-source-map.");
  }
  // dart: `--update is not allowed with --stdin.` Standard input has no mtime,
  // so "is the output newer than its input" has no honest answer. This CLI
  // happened to do the safe thing (statting `-` throws, so nothing looked
  // fresh) while the binary did the dangerous one; refusing the pair is what
  // dart does and leaves neither to luck.
  if (opts.update && opts.stdin) fail("error: --update is not allowed with --stdin.");
  if (pairs) {
    if (!operands.every((a) => colonIndex(a) >= 0)) {
      fail('error: Positional and ":" arguments may not both be used.');
    }
    if (opts.stdin) fail('error: --stdin may not be used with ":" arguments.');
    if (opts.output !== undefined) fail('error: --output may not be used with ":" arguments.');
  } else if (opts.stdin) {
    if (operands.length > 1) fail("error: Only one argument is allowed with --stdin.");
    // With --stdin the single positional IS the output, so naming both it and
    // --output is the same mistake as `<input> <output> --output`.
    if (opts.output !== undefined && operands.length > 0) fail("error: --output requires a single input");
  } else {
    if (operands.length > 2) fail("error: Only two positional args may be passed.");
    if (opts.output !== undefined && operands.length > 1) fail("error: --output requires a single input");
  }

  // The output, however it was named: `--output`, the second positional, or
  // the one positional `--stdin` takes.
  const namedOutput = opts.output !== undefined ? opts.output : pairs ? undefined : operands[opts.stdin ? 0 : 1];
  if (opts.loop !== undefined) {
    // --loop measures the compiler, so it compiles to stdout, once per
    // iteration, with no source map to build and no warnings to print.
    if (pairs || namedOutput !== undefined) {
      fail('error: --loop compiles to stdout only (no ":" arguments or --output).');
    }
    if (!opts.stdin && operands.length === 1 && isDirectory(operands[0])) {
      fail('error: --loop compiles to stdout only (no directories, ":" arguments or --output).');
    }
    if (opts.sourceMap === true || opts.embedSourceMap || opts.embedSources) {
      fail(
        "error: --loop does not generate source maps (drop --source-map, --embed-source-map and --embed-sources).",
      );
    }
  }
  // A directory may not be the OUTPUT. (The `--stdin` path does not go through
  // `parseJobs`, so checking there alone left it to fail as an uncaught EISDIR
  // from `writeFileSync`.)
  if (namedOutput !== undefined && isDirectory(namedOutput)) {
    fail(`error: Directory "${namedOutput}" may not be a positional arg.`);
  }
  // A bare directory entry (`sasso src`) compiles to files, not to stdout.
  const toStdout =
    !pairs &&
    namedOutput === undefined &&
    (opts.stdin || operands.length === 0 || !isDirectory(operands[0]));
  if (!toStdout) return;
  if (opts.sourceMapUrls === "relative") {
    fail("error: --source-map-urls=relative isn't allowed when printing to stdout.");
  }
  if (opts.embedSourceMap) return;
  if (opts.sourceMap === true) {
    fail("error: When printing to stdout, --source-map requires --embed-source-map.");
  }
  if (opts.embedSources) {
    fail("error: When printing to stdout, --embed-sources requires --embed-source-map.");
  }
  if (opts.sourceMapUrls !== undefined) {
    fail("error: When printing to stdout, --source-map-urls requires --embed-source-map.");
  }
}

function readStdin() {
  try {
    return readFileSync(0, "utf8"); // fd 0
  } catch {
    return "";
  }
}

/**
 * dart's `--source-map-urls`: how the map's `sources[]` reference the inputs.
 * `relative` (dart's default) is the lexical path from the MAP file's directory
 * to each source, as a URL — each segment percent-encoded, `/` kept as the
 * separator; `absolute` is a `file://` URL. A source that is not a `file:` URL
 * (a custom importer's) passes through untouched. Mirrors `adjust_sources` in
 * ../../src/main.rs.
 */
function adjustSources(sources, mapDir, mode, stdinText) {
  return (sources || []).map((src) => {
    // A stdin entry has no path: dart records its text as a data: URI.
    if (src === "stdin" || src === "-") {
      return stdinText === undefined ? src : `data:;charset=utf-8,${uricEncode(stdinText)}`;
    }
    let path;
    try {
      path = fileURLToPath(src);
    } catch {
      return src;
    }
    if (mode === "absolute") return pathToFileURL(path).href;
    return relative(mapDir, path).split(sep).map(encodeUrlSegment).join("/");
  });
}

/**
 * Percent-encode one URL path segment exactly like dart's `Uri`: keep the
 * unreserved set (`A-Za-z0-9-._~`), the sub-delims (`!$&'()*+,;=`) and `@`.
 * `encodeURIComponent` escapes the sub-delims, which would spell a source
 * named `the+me,1.scss` differently from dart.
 */
function encodeUrlSegment(seg) {
  let out = "";
  for (const byte of new TextEncoder().encode(seg)) {
    const c = String.fromCharCode(byte);
    if (/[A-Za-z0-9\-._~!$&'()*+,;=@]/.test(c)) out += c;
    else out += `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}

/**
 * Percent-encode for a data: URI the way dart's `Uri.dataFromString` does:
 * every byte outside the URI "uric" set becomes uppercase `%XX`. `encodeURI`
 * keeps exactly that set plus `#`, which must be encoded.
 */
function uricEncode(text) {
  return encodeURI(text).replace(/#/g, "%23");
}

/**
 * The map JSON in dart-sass's exact field order:
 * `version, sourceRoot, sources, names, mappings[, file][, sourcesContent]`.
 * `file` is omitted for a map embedded in stdout output, as dart does.
 */
function mapJson(map, sources, file) {
  const out = { version: 3, sourceRoot: "", sources, names: map.names || [], mappings: map.mappings };
  if (file !== undefined) out.file = file;
  if (map.sourcesContent) out.sourcesContent = map.sourcesContent;
  return JSON.stringify(out);
}

/**
 * dart's `sourceMappingURL` footer. The compiled CSS carries no trailing
 * newline, so EXPANDED appends `\n\n/*# … *\/\n` (the line terminator plus
 * dart's blank separator line) and COMPRESSED appends `/*# … *\/\n` with no
 * leading newline. A `*\/` inside the URL is escaped so it cannot end the
 * comment early.
 */
function sourceMapFooter(css, url, style) {
  const safe = url.replace(/\*\//g, "%2A/");
  return style === "compressed"
    ? `${css}/*# sourceMappingURL=${safe} */\n`
    : `${css}\n\n/*# sourceMappingURL=${safe} */\n`;
}

/** dart's inline map URI: percent-encoded JSON, not base64 (`Uri.dataFromString`). */
function dataUri(json) {
  return `data:application/json;charset=utf-8,${uricEncode(json)}`;
}

/**
 * Write a compile result to `outPath` (file) or stdout. The source map is
 * either inlined as a data: URI (`--embed-source-map`) or written as a `.map`
 * sidecar plus a footer. `--no-css` discards everything, output file included.
 */
function emit(result, outPath, wantMap, opts, stdinText) {
  // Writing can fail for reasons the compile cannot see — a destination that
  // is a directory, a read-only tree, a full disk. The native CLI reports
  // `cannot write <path>: …` and moves on to the next job rather than dying
  // mid-batch, so this returns the message instead of throwing.
  const write = (path, data) => {
    try {
      writeFileSync(path, data);
      return undefined;
    } catch (e) {
      return `error: cannot write ${path}: ${e && e.message ? e.message : e}`;
    }
  };
  // --no-css: the compile (and its diagnostics) was all that was wanted — no
  // stdout, no file, and an existing output is left exactly as it was.
  if (opts.noCss) return;
  // A `<dir>:<dir>` job writes into a tree that may not exist yet, and the map
  // goes in before the CSS (dart's order: nothing should point at a map that
  // failed to write), so the directory has to exist before either.
  if (outPath) {
    try {
      mkdirSync(dirname(outPath), { recursive: true });
    } catch (e) {
      return `error: cannot write ${outPath}: ${e && e.message ? e.message : e}`;
    }
  }
  const body = result.css.replace(/\n?$/, "");
  let css;
  if (wantMap && result.sourceMap) {
    // A stdout map can only be embedded, and dart gives it absolute sources
    // and no `file` field; a file's map is adjusted relative to the `.map`,
    // which sits next to the CSS.
    const mode = outPath ? opts.sourceMapUrls || "relative" : "absolute";
    // Lexical on both sides, as `adjust_sources` is natively: the compiler
    // stamps each source as the path it was named by, normalized but with its
    // symlinks intact, so the map mirrors the tree the build actually walked.
    const mapDir = outPath ? resolve(dirname(outPath)) : process.cwd();
    const sources = adjustSources(result.sourceMap.sources, mapDir, mode, stdinText);
    const file = outPath ? encodeUrlSegment(basename(outPath)) : undefined;
    const json = mapJson(result.sourceMap, sources, file);
    if (opts.embedSourceMap || !outPath) {
      css = sourceMapFooter(body, dataUri(json), opts.style);
    } else {
      const mapPath = outPath + ".map";
      css = sourceMapFooter(body, encodeUrlSegment(basename(mapPath)), opts.style);
      const mapError = write(mapPath, json);
      if (mapError) return mapError;
    }
  } else {
    // dart terminates a CSS FILE with exactly one newline, an empty stylesheet
    // included; only stdout gets nothing for empty output.
    css = outPath || body ? `${body}\n` : "";
  }
  if (outPath) return write(outPath, css);
  process.stdout.write(css);
  return undefined;
}

/**
 * A compile failed: this CLI always behaves as `--no-error-css`, and dart then
 * REMOVES a stale output file so nothing keeps consuming the CSS of an earlier
 * successful build (the `.map`, if any, is left alone). `--no-css` means no
 * output-side effects at all, so it leaves the file be.
 */
function discardStaleOutput(outPath, opts) {
  if (!outPath || opts.noCss) return undefined;
  try {
    rmSync(outPath, { force: true });
    return undefined;
  } catch (e) {
    // Returned rather than printed: this belongs to one job's diagnostics, and
    // the caller decides when that job's block reaches stderr.
    return `error: cannot remove ${outPath}: ${e && e.message ? e.message : e}`;
  }
}

/** The `:` index separating `<input>:<output>` (skips a leading drive letter). */
function colonIndex(p) {
  return p.indexOf(":", /^[a-zA-Z]:[\\/]/.test(p) ? 2 : 0);
}
/**
 * The key two paths are compared BY. On Windows the filesystem is
 * case-insensitive and dart lowercases each part, so `Src` and `src` name one
 * path; everywhere else a path is compared as written — again like dart, which
 * case-folds for no other platform, not even on a case-insensitive macOS
 * volume. Lexical either way: no `realpath`, so a symlink is not resolved.
 * (`path_key` in ../../src/main.rs, same rule.)
 */
function pathKey(path) {
  const abs = resolve(path);
  return process.platform === "win32" ? abs.toLowerCase() : abs;
}

/**
 * A path with its symlinks resolved, or its absolute form when it cannot be
 * resolved. Two spellings of one directory answer the same string, which is
 * what the walker's cycle detection needs (the native walker canonicalizes for
 * the same reason). NOT for comparing paths a user named — see `pathKey`.
 */
function realPath(path) {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}

/**
 * Every compilable stylesheet under `dir`, relative to it: `.scss`, `.sass` and
 * `.css` (exact lowercase suffixes) that are not partials, in sorted order.
 * Symlinked directories are followed — dart does — but each directory is
 * visited once by canonical identity, so a symlink cycle cannot loop or
 * duplicate output. Mirrors `expand_dir` in ../../src/main.rs.
 */
function walkStylesheets(dir) {
  const found = [];
  const seen = new Set([realPath(dir)]);
  const stack = [{ abs: dir, rel: "" }];
  while (stack.length > 0) {
    const { abs, rel } = stack.pop();
    let names;
    try {
      names = readdirSync(abs);
    } catch {
      fail(`Error reading ${abs}: Cannot open file.`);
    }
    // readdir order is unspecified; sort so that, of two names for the same
    // directory (symlinks), the same one is mirrored every run.
    names.sort();
    for (const name of names) {
      const child = join(abs, name);
      const childRel = rel ? join(rel, name) : name;
      let isDir = false;
      try {
        isDir = statSync(child).isDirectory(); // follows symlinks, unlike Dirent
      } catch {
        continue; // a broken link or a file that vanished
      }
      if (isDir) {
        const id = realPath(child);
        if (!seen.has(id)) {
          seen.add(id);
          stack.push({ abs: child, rel: childRel });
        }
      } else if (!name.startsWith("_") && /\.(scss|sass|css)$/.test(name)) {
        found.push(childRel);
      }
    }
  }
  found.sort();
  return found;
}

/** Expand a `<dir>:<dir>` pair into one job per stylesheet under it. */
function expandDirPair(input, output) {
  const jobs = [];
  const inAbs = pathKey(input);
  const outAbs = pathKey(output);
  // dart skips every source INSIDE the output directory when that directory is
  // nested in the source tree: `.:css` run twice would otherwise mirror `css/`
  // into `css/css/`. Nesting is strict — a destination EQUAL to the source is
  // not nested, so `dir:dir` still compiles every file.
  const nested = outAbs !== inAbs && (outAbs + sep).startsWith(inAbs + sep);
  for (const rel of walkStylesheets(input)) {
    const from = join(input, rel);
    const to = join(output, rel.replace(/\.(scss|sass|css)$/, ".css"));
    if (nested && (pathKey(from) + sep).startsWith(outAbs + sep)) continue;
    // dart also skips a plain CSS file whose destination is itself (`dir:dir`
    // with a `plain.css` inside): it would only be rewritten in place.
    if (pathKey(to) === pathKey(from)) continue;
    jobs.push({ input: from, output: to });
  }
  return jobs;
}

/** Whether `path` is a directory (a missing path is not). */
function isDirectory(path) {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false; // missing input: let the compile report it
  }
}

/**
 * Split one `<source>:<destination>` operand. Both sides must be non-empty and
 * there may be exactly one separator, as the native CLI's `split_pair` (and
 * dart, for the second rule) requires — `in.scss:out:other.css` is a mistake,
 * not a destination named `out:other.css`.
 */
function splitPair(arg) {
  const i = colonIndex(arg);
  if (i < 0) fail(`error: expected <input>:<output>, got "${arg}"`);
  const source = arg.slice(0, i);
  const destination = arg.slice(i + 1);
  if (!source || !destination) fail(`error: expected <source>:<destination>, got "${arg}"`);
  if (colonIndex(destination) >= 0) fail(`error: "${arg}" may only contain one ":".`);
  return { source, destination };
}

/**
 * dart keeps its sources in a path-keyed map, so the same file named twice —
 * two spellings of one path, or a directory pair plus an explicit pair naming
 * a file inside it — compiles ONCE, to the destination named last. (An exact
 * duplicate is rejected earlier, as dart does.)
 */
function coalesceJobs(jobs) {
  const byKey = new Map();
  for (const job of jobs) {
    const key = job.input === "-" ? "-" : pathKey(job.input);
    const seen = byKey.get(key);
    if (seen) seen.output = job.output;
    else byKey.set(key, { ...job });
  }
  return [...byKey.values()];
}

/**
 * Parse positionals into `{input, output}` jobs: the `<in>:<out>` pair form, or
 * the space form (`<input> [output]`, where `-o` names the same output). An
 * input of `-` is standard input, as in dart.
 */
function parseJobs(positionals, output) {
  if (positionals.some((p) => colonIndex(p) >= 0)) {
    const pairs = positionals.map(splitPair);
    // dart: each source appears once (`-` included) …
    const seen = new Set();
    for (const { source } of pairs) {
      if (seen.has(source)) fail(`error: Duplicate source "${source}".`);
      seen.add(source);
    }
    // … and a directory on the left compiles the whole tree, as dart-sass and
    // the native CLI do.
    const jobs = [];
    for (const { source, destination } of pairs) {
      if (isDirectory(source)) jobs.push(...expandDirPair(source, destination));
      else jobs.push({ input: source, output: destination });
    }
    return coalesceJobs(jobs);
  }
  const [input, second] = positionals;
  if (input === undefined) return [];
  const out = output !== undefined ? output : second;
  // dart: a bare directory compiles in place (`sasso dir` is `dir:dir`); with
  // an output it may not be a positional argument.
  if (isDirectory(input)) {
    if (out !== undefined) fail(`error: Directory "${input}" may not be a positional arg.`);
    return expandDirPair(input, input);
  }
  return [{ input, output: out }];
}
/**
 * `--update`: true when `output` is at least as new as `input` AND every
 * stylesheet the compile loaded.
 *
 * `deps` is a compile's `loadedUrls`. Checking the entry alone is what this
 * did first, and it left stale CSS on disk whenever a partial changed —
 * silently, which is worse than a slow build (#133). dart-sass walks the
 * graph; so does this now.
 *
 * The graph is known only AFTER a compile, which looks like the wrong order
 * for a flag whose job is to avoid compiling. It is the right order here:
 * `[measured]` on Lichess's 147 entry points, dart's `--update` takes 1.18s to
 * decide that nothing changed, while sasso compiles the whole tree from
 * scratch in 0.48s. Skipping the compile is worth less than the walk costs,
 * and reimplementing `@use`/`@import` resolution in JS to get the graph early
 * would be a second copy of rules that already exist in the compiler. So the
 * compile always runs and `--update` decides whether to WRITE — which is what
 * keeps an unchanged output's mtime stable, the property downstream watchers
 * actually key on.
 */
function isFresh(output, input, deps) {
  // `-` is STANDARD INPUT, not a file named `-`. Two separate reasons it can
  // never be fresh, and the first one bites in practice: with a real file
  // called `-` in the working directory — which `sass - out.css` does not
  // create but a shell redirect easily can — `statSync("-")` succeeds, an
  // output newer than that unrelated file reports FRESH, and the run keeps
  // stale CSS. Even without one, standard input has no mtime, so there is no
  // honest comparison to make. Same rule as the binary, where the entry's
  // `source_path()` is `None` and `output_is_fresh` returns false.
  if (input === "-") return false;
  try {
    if (!existsSync(output)) return false;
    const out = statSync(output).mtimeMs;
    if (out < statSync(input).mtimeMs) return false;
    for (const u of deps || []) {
      let f;
      try {
        f = fileURLToPath(u);
      } catch {
        // A non-file URL (a virtual importer) has no mtime to compare; it
        // cannot be shown unchanged, so it is not treated as fresh.
        return false;
      }
      try {
        if (out < statSync(f).mtimeMs) return false;
      } catch {
        // A dependency that has vanished since the compile: not fresh.
        return false;
      }
    }
    return true;
  } catch {
    return false;
  }
}

// `--watch`: recompile `input` -> `output` whenever the input or any of its
// dependencies (the compile's `loadedUrls`) changes. Watches the directories of
// all involved files (so editor atomic-saves are caught) and debounces bursts.
function runWatch(input, output, common, opts) {
  if (!output) fail("error: --watch requires an output file (sasso --watch in.scss out.css)");
  let watchers = [];
  let timer = null;

  const rewatch = (loadedUrls) => {
    for (const w of watchers) w.close();
    watchers = [];
    const files = new Set([resolve(input)]);
    for (const u of loadedUrls || []) {
      try {
        files.add(fileURLToPath(u));
      } catch {
        // non-file URL (a virtual importer) — nothing to watch
      }
    }
    const dirs = new Set([...files].map((f) => dirname(f)));
    for (const d of dirs) {
      try {
        watchers.push(
          watch(d, (_event, fn) => {
            if (!fn || files.has(join(d, fn))) schedule();
          }),
        );
      } catch {
        // directory vanished — ignore
      }
    }
  };

  const recompile = () => {
    try {
      const result = compile(input, common);
      // Watch before emitting: once the output file is visible, dependency
      // watchers are guaranteed live (a change saved right after the output
      // appears must not fall between emit and watcher registration).
      rewatch(result.loadedUrls);
      const writeError = emit(result, output, common.sourceMap, opts);
      if (writeError) process.stderr.write(`${writeError}\n`);
      else if (!opts.noCss) process.stderr.write(`Compiled ${input} to ${output}.\n`);
    } catch (e) {
      const msg = e instanceof Exception ? e.message : `error: ${e && e.message ? e.message : e}`;
      process.stderr.write(msg.replace(/\n?$/, "\n"));
      const removeError = discardStaleOutput(output, opts);
      if (removeError) process.stderr.write(`${removeError}\n`);
      // keep watching at least the entry so a fix re-triggers a compile
      rewatch([pathToFileURL(resolve(input))]);
    }
  };

  const schedule = () => {
    clearTimeout(timer);
    timer = setTimeout(recompile, 50);
  };

  recompile();
  process.stderr.write("Watching for changes... (press Ctrl-C to stop)\n");
}

/**
 * `--indented` for a FILE input, which forces the indented syntax whatever the
 * extension says — dart-sass documents the flag for stdin but applies it to
 * files too (measured 2026-09-17), and so does the native CLI. Without it the
 * extension decides, so the option is left out entirely.
 */
function syntaxOf(opts) {
  return opts.indented ? { syntax: "indented" } : {};
}

/**
 * Whether this job needs a source map: on by default when writing to a file,
 * off for stdout — and never under `--no-css`, which discards the output, so
 * building a map for it would be paid for and thrown away (the native CLI
 * makes the same call).
 */
function wantSourceMap(opts, output) {
  if (opts.noCss) return false;
  return opts.sourceMap === undefined ? !!output || opts.embedSourceMap : opts.sourceMap;
}

/**
 * `--loop N`: compile the same input N times in-process and report throughput
 * on stderr, then print the last CSS (unless `--no-css`). As in the native CLI
 * an untimed WARM pass runs first — it is the one that reports diagnostics and
 * fails early — and only the timed iterations are silent, so the number
 * measures compiling rather than the first-compile costs around it. No source
 * map is built, and it only ever compiles to stdout.
 */
function runLoop(opts, common) {
  const path = opts.stdin ? undefined : opts.positionals[0];
  // No input at all would otherwise block on a terminal's stdin.
  if (path === undefined && !opts.stdin) {
    fail("error: no input file (pass a path, an <in>:<out> pair, or --stdin)");
  }
  const fromStdin = path === undefined || path === "-";
  const source = fromStdin ? readStdin() : undefined;
  // A file keeps its own syntax (`.sass`, `.css`) and its own URL, as it would
  // outside the loop; only stdin takes `--indented`.
  const compileOnce = (options) =>
    fromStdin
      ? compileString(source, { ...options, sourceMap: false, syntax: opts.indented ? "indented" : "scss" })
      : compile(path, { ...options, sourceMap: false, ...syntaxOf(opts) });
  const run = (options) => {
    // Same reason as the `--stdin` path: the warm pass is the one that
    // reports, and `fail` exits before an asynchronous stderr write can drain.
    //
    // The TIMED passes carry `Logger.silent`, so nothing of theirs can reach
    // stderr and there is nothing to capture. They skip it: swapping
    // `process.stderr.write` and allocating a chunk list per iteration is this
    // CLI's bookkeeping, and `--loop` exists to report the compiler's time.
    const timed = options.logger === Logger.silent;
    const attempt = timed ? compileOrError(() => compileOnce(options)) : captureStderr(() => compileOnce(options));
    if (attempt.text) writeStderrSync(attempt.text);
    if (!attempt.error) return attempt.value.css;
    const e = attempt.error;
    const msg =
      e instanceof Exception
        ? e.message
        : e && e.code === "ENOENT"
          ? `Error reading ${path}: Cannot open file.`
          : `error: ${e && e.message ? e.message : e}`;
    fail(msg);
    return "";
  };

  // The warm/correctness pass: diagnostics once, and a failure here never
  // reaches the timer.
  let last = run(common);
  const silent = { ...common, logger: Logger.silent };
  const start = process.hrtime.bigint();
  for (let i = 0; i < opts.loop; i++) {
    last = run(silent);
  }
  const ms = Number(process.hrtime.bigint() - start) / 1e6;
  const per = ms / opts.loop;
  const perSec = per > 0 ? 1000 / per : Infinity;
  // The native CLI prints a Rust `Duration`, which picks its own unit.
  const elapsed =
    ms >= 1000 ? `${(ms / 1000).toFixed(3)}s` : ms >= 1 ? `${ms.toFixed(3)}ms` : `${(ms * 1000).toFixed(3)}µs`;
  process.stderr.write(
    `sasso: ${opts.loop} compiles in ${elapsed} => ${per.toFixed(3)} ms/compile, ${perSec.toFixed(1)} compiles/sec\n`,
  );
  if (!opts.noCss && last) process.stdout.write(`${last.replace(/\n?$/, "")}\n`);
}

/** A worker thread: same compile loop, same code, pulling from the shared index. */
async function runWorker() {
  const { shared, opts, ctl, stdinBytes } = workerData;
  await loadEngine();
  const common = commonOptions(opts);
  // Only the jobs THIS worker took are in the map; the parent merges by index,
  // so the batch reports in command-line order however the threads interleaved.
  const diagnostics = new Map();
  const failed = compileSlice(sharedList(shared), opts, common, ctl, stdinBytes, diagnostics);
  parentPort.postMessage({ failed, diagnostics: [...diagnostics] });
}

/** The compile options every job shares, rebuilt per thread (a logger cannot be cloned). */
function commonOptions(opts) {
  const common = {
    style: opts.style,
    loadPaths: opts.loadPaths,
    sourceMapIncludeSources: opts.embedSources,
    charset: opts.charset,
    // --no-unicode is not a no-op: it selects the ASCII glyph set for every
    // diagnostic the compiler renders (`,`/`|`/`'` for `╷`/`│`/`╵`).
    unicode: opts.unicode,
    // The COMPILER applies --quiet-deps, from how each file was resolved: the
    // only place that knows, and early enough that a silenced warning does not
    // count toward the deprecation repetition cap either. Filtering here by
    // where a file lives would silence the wrong ones and lose the formatted
    // diagnostic for the rest.
    quietDeps: opts.quietDeps,
    // Also the compiler's job, and for the same reason: filtered out here it
    // would silence the warnings but still tally them, and the run would end
    // with "N repetitive deprecation warnings omitted" counting exactly the
    // ones the caller silenced.
    silenceDeprecations: opts.silenceDeprecations,
  };
  if (opts.quiet) common.logger = Logger.silent;
  return common;
}

async function main() {
  // Arguments FIRST: `--help` and `--version` answer from this file alone and
  // exit inside `parseArgs`. Loading the engine before them made a metadata
  // question depend on a compiler — `SASSO_ENGINE=native sasso --version` on a
  // machine without the addon printed the addon error instead of the version.
  const opts = parseArgs(process.argv.slice(2));
  // Before the engine, because the fastest engine here is not one: a
  // version-matched release binary on PATH gets the whole command line and
  // this process ends with its exit code. See `pickBinary`.
  // `--engine` REPORTS the hand-off instead of taking it: the question it asks is
  // what THIS install does, and delegating would answer with the binary's own
  // idea of `--engine`, which it does not have at all.
  const binary = pickBinary(opts);
  if (binary && !opts.printEngine) delegate(binary);
  await loadEngine();
  // `--engine` is the answer to "which one did I get?", so it reports and stops
  // — before the "no input file" check, since it needs no input.
  if (opts.printEngine) {
    process.stdout.write(engineReport());
    return;
  }
  // Once, here: workers load their own engine and would each repeat this.
  warnIfFellBack(opts);
  const common = commonOptions(opts);

  // --loop: recompile in-process and report throughput, never writing a file.
  if (opts.loop !== undefined) {
    runLoop(opts, common);
    return;
  }

  // --stdin: a single job reading source from standard input.
  if (opts.stdin) {
    if (opts.watch) fail("error: --watch cannot be used with --stdin");
    const output = opts.output !== undefined ? opts.output : opts.positionals[0];
    const wantMap = wantSourceMap(opts, output);
    const source = readStdin();
    // Captured and written synchronously, like a job's: the engine's logger
    // writes warnings through the ASYNCHRONOUS stream, and `fail` exits at
    // once, so on a pipe a warning would arrive after the error it preceded —
    // or, past the 64 KB pipe buffer, not at all (measured 2026-09-17: a
    // 1.2 MB warning came out of `--stdin` as 65584 bytes through a pipe and
    // 1200070 to a file).
    const run = captureStderr(() =>
      compileString(source, { ...common, sourceMap: wantMap, syntax: opts.indented ? "indented" : "scss" }),
    );
    if (run.text) writeStderrSync(run.text);
    let result;
    try {
      if (run.error) throw run.error;
      result = run.value;
    } catch (e) {
      const removeError = discardStaleOutput(output, opts);
      if (removeError) writeStderrSync(`${removeError}\n`);
      if (e instanceof Exception) fail(e.message);
      fail(`error: ${e && e.message ? e.message : e}`);
    }
    const writeError = emit(result, output, wantMap, opts, source);
    if (writeError) fail(writeError);
    return;
  }

  const jobs = parseJobs(opts.positionals, opts.output);
  if (jobs.length === 0) {
    // A directory pair that expands to nothing (an empty tree, or one holding
    // only partials) is not an error — dart exits 0. Having no input at all is.
    if (opts.positionals.length === 0) {
      fail("error: no input file (pass a path, an <in>:<out> pair, or --stdin)");
    }
    return;
  }

  if (opts.watch) {
    if (jobs.length !== 1 || !jobs[0].output) fail("error: --watch requires <input> <output>");
    const wantMap = opts.noCss ? false : opts.sourceMap === undefined ? true : opts.sourceMap;
    runWatch(jobs[0].input, jobs[0].output, { ...common, sourceMap: wantMap, ...syntaxOf(opts) }, opts);
    return; // keep the process alive on the watchers
  }

  const failed = await runJobs(jobs, opts, common);
  // `process.exit` here would discard whatever of the diagnostics just flushed
  // has not reached the kernel yet — stderr on a PIPE is asynchronous, and a
  // 400-job batch lost 26 of its warnings that way (measured 2026-09-17).
  // Setting the code and returning lets Node finish the writes and exit on its
  // own; nothing else is keeping the loop alive by this point.
  if (failed > 0) process.exitCode = 1;
}

/**
 * Compile `jobs`, in this thread or across worker threads.
 *
 * The jobs are independent — each reads one input and writes one output — so
 * both CLIs give them one worker per physical core where the topology is
 * known, and one per CPU where it is not — off Linux, and on a Linux that
 * publishes none (see `_jobs.mjs`, and `default_jobs` in `src/main.rs`, which
 * agree on the rule and on why it is not simply the CPU count). Either way it
 * is one worker per job slot, which is what `-j/--jobs` has always claimed.
 * Sequentially, the difference is most of the gap between the two: the 138
 * lila stylesheets that build without npm dependencies take 704 ms through the
 * binary at `-j 1` and 138 ms at its default (measured 2026-09-17, the same
 * corpus and flags as the changelog's table).
 *
 * Workers pull from a SHARED index rather than taking a fixed slice, so one
 * heavy stylesheet cannot leave eleven threads idle. `--stop-on-error` is a
 * second shared cell: whoever fails sets it, and the others stop taking work,
 * which is the native "don't start more files once one fails".
 *
 * Staying in-process is the right answer for one job (a worker costs more than
 * the compile), when `-j 1` asks for it, and when the batch's writes overlap
 * its own paths — dart's last-one-wins, and its write-then-read, are ORDERS,
 * and an order needs a sequence.
 */
async function runJobs(jobs, opts, common) {
  const wanted = opts.jobs ?? defaultJobs();
  // Standard input is read ONCE, here, and handed to whoever needs it — as
  // SHARED bytes, because `workerData` copies what it carries and only the one
  // worker that claims the `-` job ever reads them. (It used to force the whole
  // batch into this thread instead, so one `-` job cost every OTHER job its
  // parallelism.)
  const stdinBytes = jobs.some((j) => j.input === "-") ? shareText(readStdin()) : undefined;

  // Two sources writing to ONE destination have to stay in command-line order:
  // dart compiles both and the LAST one wins — the same file every run
  // (measured against 1.104.1 on 2026-09-17, in both orders). Run them in
  // parallel and the winner is whoever finishes last, which is the race the
  // native CLI has today. A collision is almost always a slip in the command
  // line, so the parallelism given up here costs nothing real.
  //
  // A job writes its CSS *and*, with source maps on, a `<output>.map` beside
  // it — so `a.scss:out.css` and `b.scss:out.css.map` collide on that sidecar
  // even though their `output`s differ. Both count.
  // The same goes for a path one job WRITES and another READS:
  // `a.scss:b.scss b.scss:out.css` compiles a into b.scss and then b.scss into
  // out.css, and dart, being sequential, always reads the new b.scss. In the
  // pool the second job reads whichever version it finds (measured: dart and
  // `-j 1` compile a's output, the pool compiled the original b.scss).
  //
  // `--no-css` is the exception to both: `emit` and `discardStaleOutput`
  // return early under it, so the batch touches no output at all and there is
  // no last-writer to get right.
  //
  // Only ENTRY paths are compared. A job that writes a file some other job
  // `@use`s is the same hazard and cannot be seen from here — the dependency
  // is known only once that stylesheet has been parsed — so it stays a
  // scheduling race, as it is in the native CLI (#87).
  // A job writing over its OWN input is not one of them: `a.scss:a.scss` reads
  // before it writes, inside a single job, so there is no order between
  // threads to get wrong. Only ANOTHER job's input counts, which is why this
  // remembers who owns each one instead of just that it exists.
  const inputOwner = new Map();
  if (!opts.noCss) {
    jobs.forEach((job, i) => {
      if (job.input === "-") return;
      const key = pathKey(job.input);
      if (!inputOwner.has(key)) inputOwner.set(key, i);
    });
  }
  // A path does not have to match exactly to conflict: `a.scss:out` writes the
  // FILE `out` while `b.scss:out/sub.css` needs `out` to be a DIRECTORY, and
  // on a fresh tree whichever job runs first decides which one fails. dart
  // writes the file and then fails the nested job, the same way every run;
  // the pool alternated (measured 2026-09-17: four runs left a directory, two
  // left a file). So an output that is an ancestor or a descendant of another
  // output counts too.
  //
  // Both directions, without comparing every pair: `seenOut` holds the paths
  // written so far and `seenAncestors` every directory above them. A new path
  // conflicts if it IS one already written, if it is a directory some earlier
  // output sits under, or if any directory above it was written as a file.
  // Sharing a parent directory is not a conflict — that is every ordinary
  // batch — because only written paths ever go into `seenOut`.
  const seenOut = new Set();
  const seenAncestors = new Set();
  let collides = false;
  const scan = opts.noCss ? [] : jobs;
  for (let i = 0; i < scan.length && !collides; i++) {
    const job = scan[i];
    if (job.output === undefined) continue;
    const written = [job.output];
    if (wantSourceMap(opts, job.output) && !opts.embedSourceMap) written.push(`${job.output}.map`);
    for (const path of written) {
      const key = pathKey(path);
      const owner = inputOwner.get(key);
      if (seenOut.has(key) || seenAncestors.has(key) || (owner !== undefined && owner !== i)) {
        collides = true;
        break;
      }
      for (const dir of ancestorsOf(key)) {
        if (seenOut.has(dir)) {
          collides = true;
          break;
        }
        // Already recorded means everything above it was too, and was checked
        // against `seenOut` then. A later output that IS one of those
        // directories is still caught, by the `seenAncestors` test above. So
        // the walk can stop here, which is what keeps a directory build from
        // paying for its whole depth once per file.
        if (seenAncestors.has(dir)) break;
        seenAncestors.add(dir);
      }
      if (collides) break;
      seenOut.add(key);
    }
  }

  const workers = Math.min(jobs.length, Math.max(1, wanted));
  // Diagnostics are collected per job and printed in COMMAND-LINE order, never
  // in completion order: the native CLI reports each unit in input order, and
  // two stylesheets' warnings interleaving mid-block would be worse here than
  // there, with a dozen threads writing at once. Sparse — most jobs say
  // nothing, and a directory build can have thousands.
  const diagnostics = new Map();

  if (workers < 2 || collides) {
    const failed = compileSlice(listOf(jobs), opts, common, null, stdinBytes, diagnostics);
    flushDiagnostics(diagnostics, jobs.length);
    return failed;
  }

  // [0] the next job to take, [1] the stop-on-error flag.
  const ctl = new Int32Array(new SharedArrayBuffer(8));
  // The job list goes over SHARED memory, decoded one job at a time as each is
  // claimed. In `workerData` it was structure-cloned per worker instead, which
  // is O(workers x jobs): a 5,000-file directory build at -j 12 paid ~109 MB
  // for twelve copies of a list that never changes (measured 2026-09-17).
  // `positionals` is dropped for the same reason — it is the same paths again,
  // and a worker has no use for them.
  const shared = shareJobs(jobs);
  const { positionals: _unused, ...workerOpts } = opts;
  const results = await Promise.all(
    Array.from({ length: workers }, () => {
      const worker = new Worker(fileURLToPath(import.meta.url), {
        workerData: { sassoWorker: true, shared, opts: workerOpts, ctl, stdinBytes },
        // stdout/stderr are NOT captured here: a job's diagnostics are
        // collected around the compile itself (see `captureStderr`) and come
        // back in the message, while anything else a worker prints — a crash,
        // say — should reach the user rather than a stream nobody reads.
      });
      return new Promise((resolve, reject) => {
        worker.on("message", resolve);
        worker.on("error", reject);
        worker.on("exit", (code) =>
          code === 0 ? resolve({ failed: 0, diagnostics: [] }) : resolve({ failed: 1, diagnostics: [] }),
        );
      });
    }),
  );
  let failed = 0;
  for (const result of results) {
    failed += result?.failed ?? 0;
    for (const [i, text] of result?.diagnostics ?? []) diagnostics.set(i, text);
  }
  flushDiagnostics(diagnostics, jobs.length);
  return failed;
}

/** A string in shared memory, so `workerData` carries a handle, not a copy. */
function shareText(text) {
  const bytes = new TextEncoder().encode(text);
  const shared = new Uint8Array(new SharedArrayBuffer(bytes.length));
  shared.set(bytes);
  return shared;
}

/**
 * The job list as bytes in SHARED memory: every worker reads the same buffer
 * and decodes only the jobs it claims, so the list costs one copy rather than
 * one per thread. `index` holds three ints per job — where its input starts,
 * how long the input is, and how long the output is (-1 for "no output", which
 * is stdout; an empty output is not a thing `parseJobs` produces).
 */
function shareJobs(jobs) {
  const encoder = new TextEncoder();
  const encoded = jobs.map((job) => [
    encoder.encode(job.input),
    job.output === undefined ? undefined : encoder.encode(job.output),
  ]);
  let total = 0;
  for (const [input, output] of encoded) total += input.length + (output ? output.length : 0);
  const bytes = new Uint8Array(new SharedArrayBuffer(total));
  const index = new Int32Array(new SharedArrayBuffer(jobs.length * 12));
  let at = 0;
  encoded.forEach(([input, output], i) => {
    index[i * 3] = at;
    index[i * 3 + 1] = input.length;
    index[i * 3 + 2] = output ? output.length : -1;
    bytes.set(input, at);
    at += input.length;
    if (output) {
      bytes.set(output, at);
      at += output.length;
    }
  });
  return { bytes, index, count: jobs.length };
}

/**
 * Every directory above an absolute path, nearest first, stopping at the root.
 * Used to compare outputs that are not equal but cannot both exist — a file
 * and a directory of the same name.
 */
function* ancestorsOf(key) {
  let at = dirname(key);
  while (at !== dirname(at)) {
    yield at;
    at = dirname(at);
  }
}

/** A `{ length, at(i) }` view over the plain array, for the in-process path. */
function listOf(jobs) {
  return { length: jobs.length, at: (i) => jobs[i] };
}

/** The same view over `shareJobs`'s buffers, decoding a job only when claimed. */
function sharedList(shared) {
  const decoder = new TextDecoder();
  return {
    length: shared.count,
    at(i) {
      const start = shared.index[i * 3];
      const inputLen = shared.index[i * 3 + 1];
      const outputLen = shared.index[i * 3 + 2];
      return {
        input: decoder.decode(shared.bytes.subarray(start, start + inputLen)),
        output:
          outputLen < 0
            ? undefined
            : decoder.decode(shared.bytes.subarray(start + inputLen, start + inputLen + outputLen)),
      };
    },
  };
}

/**
 * Write the collected diagnostics in JOB order, one blank line between one
 * job's block and the next — dart's shape: a warning block already ends in
 * one, an error does not.
 */
function flushDiagnostics(diagnostics, count) {
  let endsBlank = true;
  for (let i = 0; i < count; i++) {
    const text = diagnostics.get(i);
    if (!text) continue;
    if (!endsBlank) process.stderr.write("\n");
    process.stderr.write(text);
    endsBlank = text.endsWith("\n\n");
  }
}

/** `captureStderr`'s shape without the capture, for a pass that cannot report. */
function compileOrError(fn) {
  try {
    return { value: fn() };
  } catch (e) {
    return { error: e };
  }
}

/**
 * Run `fn` with everything it writes to stderr collected instead of printed.
 *
 * The compiler's warnings come from the engine's default logger, which writes
 * the FORMATTED block — location, deprecation label, source snippet — straight
 * to stderr. A `logger` callback would see the message and the span but not
 * that block, so the write is intercepted rather than the logging. A compile
 * is synchronous and a worker runs one at a time, so nothing else of ours can
 * write in between.
 */
function captureStderr(fn) {
  const chunks = [];
  const original = process.stderr.write;
  process.stderr.write = (chunk, encoding, callback) => {
    chunks.push(typeof chunk === "string" ? chunk : Buffer.from(chunk).toString("utf8"));
    if (typeof encoding === "function") encoding();
    else if (typeof callback === "function") callback();
    return true;
  };
  try {
    return { value: fn(), text: chunks.join("") };
  } catch (e) {
    return { error: e, text: chunks.join("") };
  } finally {
    process.stderr.write = original;
  }
}

/**
 * The compile loop itself. With `ctl` it takes jobs from the shared index
 * (worker mode); without it, it walks the list in order (in-process mode).
 * `jobs` is a `{ length, at(i) }` view — a plain array in this thread, shared
 * bytes in a worker. Diagnostics go into the `diagnostics` map under the job's
 * index, not to stderr, so the caller can put them back in job order.
 * Returns the number that failed; it never exits the process, so a worker can
 * report back and the parent can decide.
 */
function compileSlice(jobs, opts, common, ctl, stdinBytes, diagnostics) {
  const note = (i, text) => diagnostics.set(i, (diagnostics.get(i) ?? "") + text);
  // Decoded on first use, so a worker that never claims the `-` job never
  // touches the bytes; there is at most one such job, so at most one decode.
  let stdinText;
  const stdinSource = () => (stdinText ??= stdinBytes ? new TextDecoder().decode(stdinBytes) : "");
  let failed = 0;
  let next = 0;
  for (;;) {
    let i;
    if (ctl) {
      if (Atomics.load(ctl, 1)) break; // another job failed and --stop-on-error is on
      i = Atomics.add(ctl, 0, 1);
      // Re-check AFTER claiming: between the check above and this claim
      // another worker can fail, and starting this job then would be exactly
      // what --stop-on-error forbids. (The native scheduler re-checks in the
      // same place, after its own `next.fetch_add` in ../../src/main.rs.)
      if (Atomics.load(ctl, 1)) break;
    } else {
      i = next++;
    }
    if (i >= jobs.length) break;
    const { input, output } = jobs.at(i);
    const wantMap = wantSourceMap(opts, output);
    // Warnings and deprecations belong to THIS job, wherever it ran.
    const run = captureStderr(() =>
      input === "-"
        ? compileString(stdinSource(), {
            ...common,
            sourceMap: wantMap,
            syntax: opts.indented ? "indented" : "scss",
          })
        : compile(input, { ...common, sourceMap: wantMap, ...syntaxOf(opts) }),
    );
    if (run.text) note(i, run.text);
    let result;
    try {
      if (run.error) throw run.error;
      result = run.value;
    } catch (e) {
      // With several jobs dart keeps going unless --stop-on-error, and exits
      // non-zero at the end.
      const msg =
        e instanceof Exception
          ? e.message
          : e && e.code === "ENOENT"
            ? `Error reading ${input}: Cannot open file.`
            : `error: ${e && e.message ? e.message : e}`;
      note(i, String(msg).replace(/\n?$/, "\n"));
      failed++;
      // This CLI always behaves as --no-error-css, and dart then drops a stale
      // output rather than leaving the last good build in place.
      const removeError = discardStaleOutput(output, opts);
      if (removeError) note(i, `${removeError}\n`);
      if (opts.stopOnError || jobs.length === 1) {
        if (ctl) Atomics.store(ctl, 1, 1);
        break;
      }
      continue;
    }
    // A job with no output file writes its CSS to the terminal the diagnostics
    // are already on, so buffering reverses what the user sees: dart, the
    // native binary and this CLI before the pool all print the warning during
    // the compile, ahead of the CSS. Flush this job's block before `emit`
    // rather than after it. (`parseJobs` only makes an output-less job from a
    // lone positional, so such a job is always the whole batch — there is no
    // other job's block it could jump ahead of.)
    if (output === undefined) {
      const pending = diagnostics.get(i);
      if (pending) {
        // Synchronously, like the `--stdin` and `--loop` paths: `emit` is about
        // to write the CSS to stdout, and under `2>&1` that is the SAME pipe
        // reached through a second stream. Two asynchronous streams on one
        // file descriptor have no defined interleaving, so flushing before
        // `emit` is only an order if this write has actually finished.
        writeStderrSync(pending);
        diagnostics.delete(i);
      }
    }
    // --update: the compile has run, so its `loadedUrls` is what says whether
    // the output on disk is still current. Leave it alone if it is — including
    // its mtime, which is the point.
    if (opts.update && output && isFresh(output, input, result.loadedUrls)) continue;
    const writeError = emit(result, output, wantMap, opts, input === "-" ? stdinSource() : undefined);
    if (writeError) {
      note(i, `${writeError}\n`);
      failed++;
      if (opts.stopOnError) {
        if (ctl) Atomics.store(ctl, 1, 1);
        break;
      }
    }
  }
  return failed;
}

// A worker thread runs the same file, telling itself apart by its workerData.
if (!isMainThread && workerData && workerData.sassoWorker) runWorker();
else main();
