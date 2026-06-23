// sasso — types for the dart-sass *modern* JS API (drop-in for `sass`).

export type OutputStyle = "expanded" | "compressed";

export interface Options {
  /** Output style. Defaults to `"expanded"`. */
  style?: OutputStyle;
  /** Also produce a Source Map v3 (populates {@link CompileResult.sourceMap}). */
  sourceMap?: boolean;
  /** Embed each source's full text in the map's `sourcesContent`. Requires `sourceMap`. */
  sourceMapIncludeSources?: boolean;
  /** Filesystem directories searched (in order) when resolving `@use`/`@import`. */
  loadPaths?: string[];
  /** Custom importers, tried (in order) before the filesystem. **Synchronous only.** */
  importers?: (Importer | FileImporter)[];
  /**
   * Host-defined Sass functions, keyed by signature (`"pow($base, $exponent)"`).
   * Each callback receives the bound {@link Value} arguments and returns a
   * {@link Value}. They override built-in global functions but lose to user
   * `@function`s. A callback may be async, but only under the async compile APIs
   * (`compileStringAsync`/`compileAsync`); the sync APIs throw on a Promise.
   */
  functions?: Record<string, CustomFunction>;
}

/** A host-defined Sass function. */
export type CustomFunction = (args: Value[]) => Value | Promise<Value>;

export interface StringOptions extends Options {
  /**
   * The canonical URL of `source`. Included in {@link CompileResult.loadedUrls}
   * and used as the base for the source's relative imports. Accepts a `file:`
   * (or other-scheme) URL, as a string or `URL`.
   */
  url?: string | URL;
  /** The syntax of `source`. Defaults to `"scss"`. */
  syntax?: "scss" | "indented" | "css";
}

/** Context passed to importer callbacks (dart-sass `CanonicalizeContext`). */
export interface CanonicalizeContext {
  /** `true` for `@import` (which also considers `*.import` files), else `false`. */
  fromImport: boolean;
  /** The canonical URL of the stylesheet containing the rule, if any. */
  containingUrl: URL | undefined;
}

/** What an {@link Importer.load} returns for a canonical URL. */
export interface ImporterResult {
  contents: string;
  /** Defaults to `"scss"`. */
  syntax?: "scss" | "indented" | "css";
  sourceMapUrl?: URL | string;
}

/**
 * A dart-sass *modern* importer. The callbacks may return a value or a
 * `Promise` — but the **synchronous** APIs (`compileString`/`compile`) require
 * the synchronous form (a `Promise` throws there); the **async** APIs
 * (`compileStringAsync`/`compileAsync`/the Compiler API) accept either.
 */
export interface Importer {
  canonicalize(url: string, context: CanonicalizeContext): URL | string | null | Promise<URL | string | null>;
  load(canonicalUrl: URL): ImporterResult | null | Promise<ImporterResult | null>;
}

/**
 * A dart-sass *modern* FileImporter (resolved on disk). Like {@link Importer},
 * `findFileUrl` may be async — but only under the async compile APIs.
 */
export interface FileImporter {
  findFileUrl(url: string, context: CanonicalizeContext): URL | string | null | Promise<URL | string | null>;
}

/** A Source Map v3 (the parsed JSON object). */
export interface RawSourceMap {
  version: 3;
  file?: string;
  sources: string[];
  sourcesContent?: string[];
  names: string[];
  mappings: string;
}

/** The dart-sass `CompileResult` returned by every `compile*` entry point. */
export interface CompileResult {
  css: string;
  /** Canonical URLs of all loaded stylesheets (the entry plus every import). */
  loadedUrls: URL[];
  /** Present only when `options.sourceMap` is `true`. */
  sourceMap?: RawSourceMap;
}

export interface ConfigureOptions {
  /**
   * Bump-arena reservation in MiB (default 32). `0` disables the arena, so
   * every allocation forwards to the system allocator — a lower memory
   * footprint at a lower throughput. Must be set before the first compile.
   */
  arenaMiB?: number;
}

/**
 * A Sass compilation error. Approximates the dart-sass `Exception`: an `Error`
 * subclass with `name === "Exception"` and a `sassMessage` (the message without
 * the leading `Error: `).
 */
export class Exception extends Error {
  readonly name: "Exception";
  readonly sassMessage: string;
}

/** dart-sass-style implementation string: `"sasso\t<version>"`. */
export const info: string;

/** Compile an SCSS source string. dart-sass `compileString`. */
export function compileString(source: string, options?: StringOptions): CompileResult;

/** Async `compileString` — resolves the synchronous result. */
export function compileStringAsync(source: string, options?: StringOptions): Promise<CompileResult>;

/**
 * Compile an SCSS file by path. dart-sass `compile` — **Node only** (reads the
 * file from disk). For an in-memory string, use {@link compileString}.
 */
export function compile(path: string | URL, options?: Options): CompileResult;

/** Async `compile` — resolves the synchronous result. */
export function compileAsync(path: string | URL, options?: Options): Promise<CompileResult>;

/** A reusable synchronous compiler (dart-sass Compiler API). */
export interface Compiler {
  compile(path: string | URL, options?: Options): CompileResult;
  compileString(source: string, options?: StringOptions): CompileResult;
  dispose(): void;
}

/** A reusable async compiler (dart-sass AsyncCompiler API; used by Vite). */
export interface AsyncCompiler {
  compileAsync(path: string | URL, options?: Options): Promise<CompileResult>;
  compileStringAsync(source: string, options?: StringOptions): Promise<CompileResult>;
  dispose(): Promise<void>;
}

/** Create a reusable synchronous compiler. dart-sass `initCompiler`. */
export function initCompiler(): Compiler;

/** Create a reusable async compiler. dart-sass `initAsyncCompiler` (Vite calls this). */
export function initAsyncCompiler(): Promise<AsyncCompiler>;

/**
 * Configure the bump-arena allocator. MUST be called before the first compile —
 * the arena region is reserved on first use and then fixed.
 */
export function configure(options?: ConfigureOptions): void;

// ----- the dart-sass `Value` type system (custom-function arguments/returns) -----

export type ColorSpace =
  | "rgb" | "srgb" | "srgb-linear" | "display-p3" | "a98-rgb" | "prophoto-rgb"
  | "rec2020" | "hsl" | "hwb" | "lab" | "oklab" | "lch" | "oklch"
  | "xyz" | "xyz-d50" | "xyz-d65";

/** Base class for every Sass value. */
export abstract class Value {
  readonly isTruthy: boolean;
  readonly realNull: Value | null;
  readonly asList: Value[];
  readonly hasBrackets: boolean;
  readonly separator: string | null;
  assertNumber(name?: string): SassNumber;
  assertString(name?: string): SassString;
  assertColor(name?: string): SassColor;
  assertMap(name?: string): SassMap;
  assertBoolean(name?: string): SassBoolean;
  equals(other: Value): boolean;
}

export class SassBoolean extends Value {
  constructor(value: boolean);
  readonly value: boolean;
}
export const sassTrue: SassBoolean;
export const sassFalse: SassBoolean;
export const sassNull: Value;

export class SassString extends Value {
  constructor(text?: string, options?: { quotes?: boolean });
  readonly text: string;
  readonly hasQuotes: boolean;
  readonly sassLength: number;
}

export class SassNumber extends Value {
  constructor(value: number, unit?: string);
  constructor(value: number, options: { numeratorUnits?: string[]; denominatorUnits?: string[] });
  readonly value: number;
  readonly numeratorUnits: string[];
  readonly denominatorUnits: string[];
  readonly hasUnits: boolean;
  readonly isInt: boolean;
  readonly asInt: number | null;
  hasUnit(unit: string): boolean;
  assertInt(name?: string): number;
  assertUnit(unit: string, name?: string): SassNumber;
}

export class SassColor extends Value {
  constructor(options: {
    space?: ColorSpace;
    alpha?: number;
    /** Channel values by name (e.g. red/green/blue, lightness/chroma/hue); `null` = missing. */
    [channel: string]: ColorSpace | number | null | undefined;
  });
  readonly space: ColorSpace;
  readonly channels: number[];
  readonly channelsOrNull: (number | null)[];
  readonly alpha: number;
  readonly red: number;
  readonly green: number;
  readonly blue: number;
  channel(name: string): number;
  isChannelMissing(name: string): boolean;
}

export class SassList extends Value {
  constructor(contents?: Value[], options?: { separator?: string | null; brackets?: boolean });
  /** 0-based element access. */
  get(index: number): Value | undefined;
}

export class SassArgumentList extends SassList {
  constructor(contents?: Value[], keywords?: Map<string, Value>, options?: { separator?: string | null; brackets?: boolean });
  readonly keywords: Map<string, Value>;
}

export class SassMap extends Value {
  constructor(contents?: Map<Value, Value>);
  readonly contents: Map<Value, Value>;
  get(key: Value): Value | undefined;
}

declare const _default: {
  compile: typeof compile;
  compileAsync: typeof compileAsync;
  compileString: typeof compileString;
  compileStringAsync: typeof compileStringAsync;
  initCompiler: typeof initCompiler;
  initAsyncCompiler: typeof initAsyncCompiler;
  configure: typeof configure;
  info: typeof info;
  Exception: typeof Exception;
  Value: typeof Value;
  SassBoolean: typeof SassBoolean;
  SassString: typeof SassString;
  SassNumber: typeof SassNumber;
  SassColor: typeof SassColor;
  SassList: typeof SassList;
  SassArgumentList: typeof SassArgumentList;
  SassMap: typeof SassMap;
  sassTrue: typeof sassTrue;
  sassFalse: typeof sassFalse;
  sassNull: typeof sassNull;
};
export default _default;
