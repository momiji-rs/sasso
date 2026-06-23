#!/usr/bin/env node
// sasso CLI — `npx sasso input.scss [output.css]`. Pure Node + wasm, no deps.
// A subset of the dart-sass `sass` CLI flags, sharing the package's compiler.
import { readFileSync, writeFileSync } from "node:fs";
import { basename } from "node:path";
import { compile, compileString, info, Exception } from "./sasso.mjs";

const HELP = `sasso — compile SCSS/Sass to CSS

Usage: sasso [options] <input.scss> [output.css]
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
    } else if (a === "--indented") {
      opts.indented = true;
    } else if (a === "--source-map") {
      opts.sourceMap = true;
    } else if (a === "--no-source-map") {
      opts.sourceMap = false;
    } else if (a === "--embed-sources") {
      opts.embedSources = true;
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

function main() {
  const opts = parseArgs(process.argv.slice(2));
  const [input, output] = opts.positionals;

  if (!opts.stdin && input === undefined) {
    fail("error: no input file (pass a path, or --stdin). Try --help.");
  }
  if (opts.stdin && input !== undefined && output === undefined) {
    // `sasso --stdin out.css` — the single positional is the OUTPUT.
  }
  const outPath = opts.stdin ? input : output;
  const wantMap = opts.sourceMap === undefined ? !!outPath : opts.sourceMap;

  const common = {
    style: opts.style,
    loadPaths: opts.loadPaths,
    sourceMap: wantMap,
    sourceMapIncludeSources: opts.embedSources,
  };

  let result;
  try {
    if (opts.stdin) {
      result = compileString(readStdin(), {
        ...common,
        syntax: opts.indented ? "indented" : "scss",
      });
    } else {
      result = compile(input, common);
    }
  } catch (e) {
    if (e instanceof Exception) fail(e.message);
    if (e && e.code === "ENOENT") fail(`error: cannot read "${input}": no such file`);
    fail(`error: ${e && e.message ? e.message : e}`);
  }

  let css = result.css;
  if (wantMap && outPath) {
    // dart-sass appends a sourceMappingURL footer for file output and writes a
    // sidecar map. `file` points at the output basename.
    const mapPath = outPath + ".map";
    const map = { ...result.sourceMap, file: basename(outPath) };
    const footer = `\n/*# sourceMappingURL=${basename(mapPath)} */\n`;
    css = css.replace(/\n?$/, "") + footer;
    writeFileSync(mapPath, JSON.stringify(map));
  }

  if (outPath) writeFileSync(outPath, css);
  else process.stdout.write(css);
}

main();
