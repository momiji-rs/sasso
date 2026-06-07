# @momiji-rs/sasso

[sasso](https://github.com/momiji-rs/sasso) — a pure-Rust SCSS → CSS compiler
(a dart-sass alternative) — as a tiny, **dependency-free** WebAssembly module.
No wasm-bindgen, no native add-ons: one small `.wasm` plus a hand-written
loader, so it runs the same in Node and the browser.

```bash
npm install @momiji-rs/sasso
```

```js
import { compile } from "@momiji-rs/sasso";

const css = compile(`
  $brand: #2a7ae2;
  .button {
    color: $brand;
    &:hover { color: darken($brand, 10%); }
  }
`);
console.log(css);

// compressed output
compile("a { color: #ffffff }", { style: "compressed" }); // a{color:#fff}
```

`compile(scss, options?)` returns the CSS string, or throws an `Error` with the
compiler's message on a Sass error. Options: `{ style?: "expanded" | "compressed" }`.

For the CLI and the Rust library, see the
[main repository](https://github.com/momiji-rs/sasso).

Licensed under MIT OR Apache-2.0.
