#!/usr/bin/env node
// sasso CLI — `npx sasso input.scss [output.css]`. Pure Node + wasm, no deps.
// A subset of the dart-sass `sass` CLI flags, sharing the package's compiler.
import { readFileSync, writeFileSync, watch, statSync, existsSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { compile, compileString, info, Exception, Logger } from "./sasso.mjs";

const HELP = `sasso — compile SCSS/Sass to CSS

Usage: sasso [options] <input.scss> [output.css]
       sasso [options] <input.scss>:<output.css> [<in>:<out> ...]
       sasso [options] --stdin [output.css]
       cat a.scss | sasso --stdin

Options:
  -s, --style <expanded|compressed>  Output style (default: expanded).
  -I, --load-path <dir>              Add a load path for @use/@import (repeatable).
      --stdin                        Read the stylesheet from standard input.
      --indented                     Parse stdin as the indented .sass syntax.
      --[no-]source-map              Emit a source map (default: on when writing
                                     to a file, off for stdout).
      --embed-sources                Embed source text in the map's sourcesContent.
      --embed-source-map             Embed the source map as a data: URI in the CSS.
      --[no-]charset                 Emit @charset/BOM for non-ASCII output
                                     (default: on).
  -q, --quiet                        Suppress @warn / @debug / deprecation output.
      --update                       Skip outputs already newer than their input.
  -w, --watch                        Recompile when the input or any dependency
                                     changes (requires <input> <output>).
  -h, --help                         Print this help.
      --version                      Print the version.

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
    update: false,
    watch: false,
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
    } else if (a === "-w" || a === "--watch") {
      opts.watch = true;
    } else if (a === "--indented") {
      opts.indented = true;
    } else if (a === "--source-map") {
      opts.sourceMap = true;
    } else if (a === "--no-source-map") {
      opts.sourceMap = false;
    } else if (a === "--embed-sources") {
      opts.embedSources = true;
    } else if (a === "--embed-source-map") {
      opts.embedSourceMap = true;
    } else if (a === "-q" || a === "--quiet") {
      opts.quiet = true;
    } else if (a === "--update") {
      opts.update = true;
    } else if (a === "--charset") {
      opts.charset = true;
    } else if (a === "--no-charset") {
      opts.charset = false;
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
    } else if (a.startsWith("-") && a !== "-") {
      fail(`error: unknown option "${a}" (try --help)`);
    } else {
      opts.positionals.push(a);
    }
  }
  return opts;
}

function readStdin() {
  try {
    return readFileSync(0, "utf8"); // fd 0
  } catch {
    return "";
  }
}

// Write a compile result to `outPath` (file) or stdout. The source map is either
// inlined as a data: URI (`embedMap`) or written as a `.map` sidecar + footer.
function emit(result, outPath, wantMap, embedMap) {
  let css = result.css;
  if (wantMap && embedMap) {
    const map = { ...result.sourceMap, file: outPath ? basename(outPath) : undefined };
    const b64 = Buffer.from(JSON.stringify(map), "utf8").toString("base64");
    css = css.replace(/\n?$/, "") + `\n/*# sourceMappingURL=data:application/json;base64,${b64} */\n`;
  } else if (wantMap && outPath) {
    const mapPath = outPath + ".map";
    const map = { ...result.sourceMap, file: basename(outPath) };
    css = css.replace(/\n?$/, "") + `\n/*# sourceMappingURL=${basename(mapPath)} */\n`;
    writeFileSync(mapPath, JSON.stringify(map));
  }
  if (outPath) writeFileSync(outPath, css);
  else process.stdout.write(css);
}

/** The `:` index separating `<input>:<output>` (skips a leading drive letter). */
function colonIndex(p) {
  return p.indexOf(":", /^[a-zA-Z]:[\\/]/.test(p) ? 2 : 0);
}
/** Parse positionals into `{input, output}` jobs (colon-pair form or space form). */
function parseJobs(positionals) {
  if (positionals.some((p) => colonIndex(p) >= 0)) {
    return positionals.map((p) => {
      const i = colonIndex(p);
      if (i < 0) fail(`error: expected <input>:<output>, got "${p}"`);
      return { input: p.slice(0, i), output: p.slice(i + 1) };
    });
  }
  const [input, output] = positionals;
  return input === undefined ? [] : [{ input, output }];
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
function runWatch(input, output, common, embedMap) {
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
      emit(result, output, common.sourceMap, embedMap);
      process.stderr.write(`Compiled ${input} to ${output}.\n`);
      rewatch(result.loadedUrls);
    } catch (e) {
      const msg = e instanceof Exception ? e.message : `error: ${e && e.message ? e.message : e}`;
      process.stderr.write(msg.replace(/\n?$/, "\n"));
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

function main() {
  const opts = parseArgs(process.argv.slice(2));
  const common = {
    style: opts.style,
    loadPaths: opts.loadPaths,
    sourceMapIncludeSources: opts.embedSources,
    charset: opts.charset,
  };
  if (opts.quiet) common.logger = Logger.silent;

  // --stdin: a single job reading source from standard input.
  if (opts.stdin) {
    if (opts.watch) fail("error: --watch cannot be used with --stdin");
    const output = opts.positionals[0];
    const wantMap = opts.sourceMap === undefined ? !!output || opts.embedSourceMap : opts.sourceMap;
    let result;
    try {
      result = compileString(readStdin(), { ...common, sourceMap: wantMap, syntax: opts.indented ? "indented" : "scss" });
    } catch (e) {
      if (e instanceof Exception) fail(e.message);
      fail(`error: ${e && e.message ? e.message : e}`);
    }
    emit(result, output, wantMap, opts.embedSourceMap);
    return;
  }

  const jobs = parseJobs(opts.positionals);
  if (jobs.length === 0) fail("error: no input file (pass a path, or --stdin). Try --help.");

  if (opts.watch) {
    if (jobs.length !== 1 || !jobs[0].output) fail("error: --watch requires <input> <output>");
    const wantMap = opts.sourceMap === undefined ? true : opts.sourceMap;
    runWatch(jobs[0].input, jobs[0].output, { ...common, sourceMap: wantMap }, opts.embedSourceMap);
    return; // keep the process alive on the watchers
  }

  for (const { input, output } of jobs) {
    const wantMap = opts.sourceMap === undefined ? !!output || opts.embedSourceMap : opts.sourceMap;
    // --update: leave outputs that are already newer than their input untouched.
    if (opts.update && output && isFresh(output, input)) continue;
    let result;
    try {
      result = compile(input, { ...common, sourceMap: wantMap });
    } catch (e) {
      if (e instanceof Exception) fail(e.message);
      if (e && e.code === "ENOENT") fail(`error: cannot read "${input}": no such file`);
      fail(`error: ${e && e.message ? e.message : e}`);
    }
    emit(result, output, wantMap, opts.embedSourceMap);
  }
}

main();
