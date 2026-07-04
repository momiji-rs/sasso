// Real-corpus measurement: apps/rails tailwind.scss through sasso wasm,
// sync module vs asyncify module (same built-in FS chain).
// Usage: node real-corpus-bench.mjs <sync|async>
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { compileString, compileStringAsync } from '/Users/linyiru/Projects/rust-sass/wasm/npm/sasso.mjs';

const RAILS = '/Users/linyiru/Projects/manekineko/.claude/worktrees/swift-soaring-stroustrup/apps/rails';
const entry = `${RAILS}/app/javascript/stylesheets/tailwind.scss`;
const src = readFileSync(entry, 'utf8');
const opts = {
  url: pathToFileURL(entry),
  loadPaths: [`${RAILS}/node_modules`],
  style: 'expanded',
};

const mode = process.argv[2];
const run = mode === 'sync'
  ? () => compileString(src, opts)
  : () => compileStringAsync(src, opts);

const first = await run();
if (!first.css.includes('@keyframes typingDots')) { console.error('SANITY FAIL'); process.exit(1); }
console.log(`${mode}: css=${first.css.length}B loadedUrls=${first.loadedUrls.length}`);

const WARMUP = 5, REPS = 20;
for (let i = 0; i < WARMUP; i++) await run();
const samples = [];
for (let i = 0; i < REPS; i++) {
  const t0 = performance.now();
  await run();
  samples.push(performance.now() - t0);
}
samples.sort((a, b) => a - b);
const q = (p) => samples[Math.min(samples.length - 1, Math.floor(p * samples.length))];
console.log(`${mode}: median=${q(0.5).toFixed(1)} p10=${q(0.1).toFixed(1)} p90=${q(0.9).toFixed(1)} ms/compile (n=${REPS})`);
