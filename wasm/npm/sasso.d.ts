export interface Options {
  /** Output style. Defaults to `"expanded"`. */
  style?: "expanded" | "compressed";
  /** Also produce a Source Map v3. When `true`, `compile` returns a {@link CompileResult}. */
  sourceMap?: boolean;
  /** Embed each source's full text in the map's `sourcesContent`. Requires `sourceMap`. */
  sourceMapIncludeSources?: boolean;
}

/** A Source Map v3 (the parsed JSON object). */
export interface SourceMap {
  version: 3;
  file?: string;
  sources: string[];
  sourcesContent?: string[];
  names: string[];
  mappings: string;
}

/** Returned by `compile` when `sourceMap: true`. */
export interface CompileResult {
  css: string;
  sourceMap: SourceMap;
}

export interface ConfigureOptions {
  /**
   * Bump-arena reservation in MiB (default 32). `0` disables the arena, so
   * every allocation forwards to the system allocator — a lower memory
   * footprint at a lower throughput. Must be set before the first `compile`.
   */
  arenaMiB?: number;
}

/**
 * Compile an SCSS string to CSS. Throws an `Error` (carrying the compiler's
 * message) on a Sass error. With `sourceMap: true` it returns a
 * {@link CompileResult} (`{ css, sourceMap }`) instead of a bare CSS string.
 */
export function compile(scss: string, options: Options & { sourceMap: true }): CompileResult;
export function compile(scss: string, options?: Options): string;

/**
 * Configure the bump-arena allocator. MUST be called before the first
 * `compile()` — the arena region is reserved on first use and then fixed.
 */
export function configure(options?: ConfigureOptions): void;

declare const _default: { compile: typeof compile; configure: typeof configure };
export default _default;
