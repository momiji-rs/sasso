export interface Options {
  /** Output style. Defaults to `"expanded"`. */
  style?: "expanded" | "compressed";
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
 * message) on a Sass error.
 */
export function compile(scss: string, options?: Options): string;

/**
 * Configure the bump-arena allocator. MUST be called before the first
 * `compile()` — the arena region is reserved on first use and then fixed.
 */
export function configure(options?: ConfigureOptions): void;

declare const _default: { compile: typeof compile; configure: typeof configure };
export default _default;
