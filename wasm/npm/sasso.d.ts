export interface Options {
  /** Output style. Defaults to `"expanded"`. */
  style?: "expanded" | "compressed";
}

/**
 * Compile an SCSS string to CSS. Throws an `Error` (carrying the compiler's
 * message) on a Sass error.
 */
export function compile(scss: string, options?: Options): string;

declare const _default: { compile: typeof compile };
export default _default;
