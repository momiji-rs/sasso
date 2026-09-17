#!/usr/bin/env node
// sasso CLI — `npx sasso input.scss [output.css]`. Pure Node + wasm, no deps.
// A subset of the dart-sass `sass` CLI flags, sharing the package's compiler.
import {
  readFileSync,
  writeFileSync,
  watch,
  statSync,
  existsSync,
  readdirSync,
  mkdirSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { basename, dirname, join, resolve, relative, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { compile, compileString, info, Exception, Logger } from "./sasso.mjs";

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
      --[no-]quiet-deps              Drop deprecation warnings raised inside
                                     dependencies: stylesheets reached through
                                     a load path, and whatever those load
                                     relatively. Their own @warn/@debug still
                                     prints, as in dart-sass.
      --[no-]stop-on-error           Stop after the first file that fails.
      --[no-]error-css               On a compile error, write a stylesheet
                                     describing it. NOT IMPLEMENTED in this CLI:
                                     the flag is accepted, and a failing compile
                                     always behaves as --no-error-css.
      --no-css                       Compile but discard the CSS: no output
                                     file, no stdout, and an existing output is
                                     left exactly as it was.
      --update                       Skip outputs already newer than their input.
  -w, --watch                        Recompile when the input or any dependency
                                     changes (requires <input> <output>).
  -j, --jobs <N>                     Accepted for dart-sass compatibility; this
                                     CLI compiles sequentially.
      --loop <N>                     Recompile in-process N times and report
                                     throughput (stdout inputs only).
  -c, --[no-]color                   Accepted for compatibility (no-op: output is
                                     never colored).
      --[no-]unicode                 Unicode box glyphs in diagnostics
                                     (default: on).
  -h, --help                         Print this help.
      --version                      Print the version.

An <in>:<out> pair may name DIRECTORIES: every .scss/.sass/.css file under
<in> that is not a partial compiles to the matching path under <out>.
Symlinked directories are followed, each one only once.

With no output file the CSS is written to stdout. A Sass error is printed to
stderr and exits non-zero.`;

function fail(msg) {
  process.stderr.write(String(msg).replace(/\n?$/, "\n"));
  process.exit(1);
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
    stopOnError: false,
    noCss: false,
    // Tri-state: dart's default is "relative", but only an EXPLICIT
    // --source-map-urls is rejected when printing to stdout.
    sourceMapUrls: undefined,
    update: false,
    watch: false,
    unicode: true,
    loop: undefined,
    output: undefined,
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
      // info is "dart-sass\t<ver>\t(sasso <ver>)\t[Rust]" — surface the sasso one.
      const m = /\(sasso ([^)]+)\)/.exec(info);
      process.stdout.write((m ? m[1] : info.split("\t")[1] || "unknown") + "\n");
      process.exit(0);
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
      // feature this CLI does not implement (see HELP); `--color` is a no-op in
      // the native CLI too, and `--jobs` has no meaning without parallelism.
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
      // Consumed and ignored (this CLI is sequential), but validated: the
      // native CLI rejects a non-positive value rather than compiling.
      positiveInt("--jobs", takeValue(inline));
    } else if (a === "--loop" || a.startsWith("--loop=")) {
      opts.loop = positiveInt("--loop", takeValue(a.startsWith("--loop=") ? a.slice(7) : undefined));
    } else if (a === "--source-map-urls" || a.startsWith("--source-map-urls=")) {
      const inline = a.startsWith("--source-map-urls=") ? a.slice(18) : undefined;
      const v = takeValue(inline);
      if (v !== "relative" && v !== "absolute") fail(`error: unknown --source-map-urls "${v}"`);
      opts.sourceMapUrls = v;
    } else if (a === "-o" || a === "--output" || a.startsWith("--output=")) {
      const inline = a.startsWith("--output=") ? a.slice(9) : undefined;
      if (opts.output !== undefined) fail("error: --output requires a single input");
      opts.output = takeValue(inline);
    } else if (a === "-s" || a === "--style" || a.startsWith("--style=")) {
      const inline = a.startsWith("--style=") ? a.slice(8) : undefined;
      const v = takeValue(inline);
      if (v !== "expanded" && v !== "compressed") fail(`error: unknown style "${v}"`);
      opts.style = v;
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
 * token itself has to be a decimal integer, as Rust's `parse::<u32>` requires:
 * `Number()` would take `1.0`, `1e3`, `0x2` and whitespace-padded values that
 * the native CLI refuses (`+3` and `03` it accepts, and so does this).
 */
function positiveInt(flag, value) {
  const n = /^\+?[0-9]+$/.test(value) ? Number(value) : NaN;
  if (!Number.isSafeInteger(n) || n < 1) {
    fail(`error: ${flag} expects a positive integer (got ${JSON.stringify(value)})`);
  }
  return n;
}

/**
 * The combinations the native CLI rejects before compiling anything (dart-sass
 * rejects the source-map and arity ones with the same wording): the positional
 * grammar is `<input> [output]`, `--output` names ONE output, source-map flags
 * need a source map, and a map printed to stdout can only be an embedded one
 * with absolute sources.
 */
function validate(opts) {
  const pairs = opts.positionals.some((a) => colonIndex(a) >= 0);
  if (opts.output !== undefined) {
    if (pairs) fail('error: --output may not be used with ":" arguments.');
    if (opts.positionals.length > 1) fail("error: --output requires a single input");
  }
  if (opts.stdin) {
    if (pairs) fail('error: --stdin may not be used with ":" arguments.');
    if (opts.positionals.length > 1) fail("error: Only one argument is allowed with --stdin.");
  } else if (!pairs && opts.positionals.length > 2) {
    fail("error: Only two positional args may be passed.");
  }
  // A directory may not be the OUTPUT, however it was named — `--output`, the
  // second positional, or the one positional `--stdin` takes. (The `--stdin`
  // path does not go through `parseJobs`, so checking there alone left it to
  // fail as an uncaught EISDIR from `writeFileSync`.)
  if (!pairs) {
    const named = opts.output !== undefined ? opts.output : opts.positionals[opts.stdin ? 0 : 1];
    if (named !== undefined && isDirectory(named)) {
      fail(`error: Directory "${named}" may not be a positional arg.`);
    }
  }
  // Source-map flags need a source map, wherever the CSS goes. (`validate`
  // used to check only the stdout cases, so a file output accepted and then
  // ignored them.)
  if (opts.sourceMap === false) {
    if (opts.embedSourceMap) fail("error: --embed-source-map isn't allowed with --no-source-map.");
    if (opts.embedSources) fail("error: --embed-sources isn't allowed with --no-source-map.");
    if (opts.sourceMapUrls !== undefined) fail("error: --source-map-urls isn't allowed with --no-source-map.");
  }
  if (opts.loop !== undefined) {
    // --loop measures the compiler, so it compiles to stdout, once per
    // iteration, with no source map to build and no warnings to print.
    if (pairs || opts.output !== undefined) {
      fail('error: --loop compiles to stdout only (no ":" arguments or --output).');
    }
    if (!opts.stdin && opts.positionals.length > 1) {
      fail('error: --loop compiles to stdout only (no ":" arguments or --output).');
    }
    if (opts.positionals.length === 1 && !opts.stdin && isDirectory(opts.positionals[0])) {
      fail('error: --loop compiles to stdout only (no directories, ":" arguments or --output).');
    }
    if (opts.sourceMap === true || opts.embedSourceMap || opts.embedSources) {
      fail(
        "error: --loop does not generate source maps (drop --source-map, --embed-source-map and --embed-sources).",
      );
    }
  }
  // A bare directory entry (`sasso src`) compiles to files, not to stdout.
  const toStdout =
    !pairs &&
    opts.output === undefined &&
    (opts.stdin
      ? opts.positionals.length === 0
      : opts.positionals.length < 2 && !isDirectory(opts.positionals[0] ?? ""));
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
    // The compiler stamps each source as its REAL path, so the map directory
    // has to be resolved the same way or `/var` and `/private/var` (macOS)
    // would produce an eleven-step `../` climb instead of dart's `../in.scss`.
    const mapDir = outPath ? realPath(dirname(outPath)) : realPath(process.cwd());
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
  if (!outPath || opts.noCss) return false;
  try {
    rmSync(outPath, { force: true });
    return false;
  } catch (e) {
    process.stderr.write(`error: cannot remove ${outPath}: ${e && e.message ? e.message : e}\n`);
    return true;
  }
}

/** The `:` index separating `<input>:<output>` (skips a leading drive letter). */
function colonIndex(p) {
  return p.indexOf(":", /^[a-zA-Z]:[\\/]/.test(p) ? 2 : 0);
}
/**
 * A path with its symlinks resolved, or its absolute form when it cannot be
 * resolved. Two spellings of one directory answer the same string, which is
 * what both the source-map base and the walker's cycle detection need.
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
  const inAbs = resolve(input);
  const outAbs = resolve(output);
  // dart skips every source INSIDE the output directory when that directory is
  // nested in the source tree: `.:css` run twice would otherwise mirror `css/`
  // into `css/css/`. Nesting is strict — a destination EQUAL to the source is
  // not nested, so `dir:dir` still compiles every file.
  const nested = outAbs !== inAbs && (outAbs + sep).startsWith(inAbs + sep);
  for (const rel of walkStylesheets(input)) {
    const from = join(input, rel);
    const to = join(output, rel.replace(/\.(scss|sass|css)$/, ".css"));
    if (nested && (resolve(from) + sep).startsWith(outAbs + sep)) continue;
    // dart also skips a plain CSS file whose destination is itself (`dir:dir`
    // with a `plain.css` inside): it would only be rewritten in place.
    if (resolve(to) === resolve(from)) continue;
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
    const key = job.input === "-" ? "-" : resolve(job.input);
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
/** `--update`: true when `output` already exists and is newer than `input`. */
function isFresh(output, input) {
  try {
    return existsSync(output) && statSync(output).mtimeMs >= statSync(input).mtimeMs;
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
      discardStaleOutput(output, opts);
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
 * on stderr, then print the last CSS (unless `--no-css`). It measures the
 * compiler, so warnings are silenced and no source map is built — and, as in
 * the native CLI, it only ever compiles to stdout.
 */
function runLoop(opts, common) {
  const path = opts.stdin ? undefined : opts.positionals[0];
  // No input at all would otherwise block on a terminal's stdin.
  if (path === undefined && !opts.stdin) {
    fail("error: no input file (pass a path, an <in>:<out> pair, or --stdin)");
  }
  const fromStdin = path === undefined || path === "-";
  const source = fromStdin ? readStdin() : undefined;
  const options = { ...common, logger: Logger.silent, sourceMap: false };
  let last = "";
  const start = process.hrtime.bigint();
  for (let i = 0; i < opts.loop; i++) {
    try {
      // A file keeps its own syntax (`.sass`, `.css`) and its own URL, as it
      // would outside the loop; only stdin takes `--indented`.
      last = fromStdin
        ? compileString(source, { ...options, syntax: opts.indented ? "indented" : "scss" }).css
        : compile(path, { ...options, ...syntaxOf(opts) }).css;
    } catch (e) {
      const msg =
        e instanceof Exception
          ? e.message
          : e && e.code === "ENOENT"
            ? `Error reading ${path}: Cannot open file.`
            : `error: ${e && e.message ? e.message : e}`;
      fail(msg);
    }
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

function main() {
  const opts = parseArgs(process.argv.slice(2));
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
  };
  if (opts.quiet) common.logger = Logger.silent;

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
    let result;
    try {
      result = compileString(source, { ...common, sourceMap: wantMap, syntax: opts.indented ? "indented" : "scss" });
    } catch (e) {
      discardStaleOutput(output, opts);
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

  // Standard input is read at most once, however many jobs name it.
  let stdinSource;
  let failed = 0;
  for (const { input, output } of jobs) {
    const wantMap = wantSourceMap(opts, output);
    // --update: leave outputs that are already newer than their input untouched.
    if (opts.update && output && isFresh(output, input)) continue;
    let result;
    try {
      if (input === "-") {
        if (stdinSource === undefined) stdinSource = readStdin();
        result = compileString(stdinSource, {
          ...common,
          sourceMap: wantMap,
          syntax: opts.indented ? "indented" : "scss",
        });
      } else {
        result = compile(input, { ...common, sourceMap: wantMap, ...syntaxOf(opts) });
      }
    } catch (e) {
      // With several jobs dart keeps going unless --stop-on-error, and exits
      // non-zero at the end; `fail` would stop at the first one.
      const msg =
        e instanceof Exception
          ? e.message
          : e && e.code === "ENOENT"
            ? `Error reading ${input}: Cannot open file.`
            : `error: ${e && e.message ? e.message : e}`;
      process.stderr.write(String(msg).replace(/\n?$/, "\n"));
      failed++;
      // This CLI always behaves as --no-error-css, and dart then drops a stale
      // output rather than leaving the last good build in place.
      discardStaleOutput(output, opts);
      if (opts.stopOnError || jobs.length === 1) process.exit(1);
      continue;
    }
    const writeError = emit(result, output, wantMap, opts, input === "-" ? stdinSource : undefined);
    if (writeError) {
      process.stderr.write(`${writeError}\n`);
      failed++;
      if (opts.stopOnError) process.exit(1);
    }
  }
  if (failed > 0) process.exit(1);
}

main();
