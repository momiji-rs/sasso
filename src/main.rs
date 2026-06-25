//! `sasso` command-line interface.
//!
//! A small, dependency-free CLI over the `sasso` library:
//!
//! ```text
//! sasso [options] <input.scss>
//! sasso [options] <input.scss> -o <output.css>
//! sasso --stdin [options] < input.scss
//!
//!   -s, --style <expanded|compressed>   output style (default: expanded)
//!   -I, --load-path <dir>               add an @import search path (repeatable)
//!   -o, --output <file>                 write CSS to <file> (else stdout)
//!       --source-map                    also write <output>.map (requires -o)
//!       --embed-sources                 inline source text in the map
//!       --source-map-urls <relative|absolute>
//!                                       how the map references sources (default: relative)
//!       --stdin                         read SCSS from standard input
//!       --loop <N>                      recompile in-process N times (throughput)
//!   -q, --quiet                         suppress CSS on stdout (timing only)
//!       --version                       print version and exit
//!   -h, --help                          print this help and exit
//! ```
//!
//! `--embed-source-map` (inline `data:` URI in the CSS) is not yet supported.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use sasso::{compile, compile_with_source_map, FsImporter, Options, OutputStyle, Syntax};

// Install the scoped bump-arena allocator (perf #5). Inside each `compile`
// scope every allocation is a pointer bump from a per-thread arena that is
// freed wholesale when the scope ends; outside a scope (startup, arg parsing,
// I/O) requests forward to the system allocator. `compile` copies its result
// out to the system allocator before resetting the arena, so values that
// escape a compile never point into it.
#[global_allocator]
static GLOBAL: sasso::ScopedAlloc = sasso::ScopedAlloc;

const USAGE: &str = "\
sasso — a pure-Rust SCSS to CSS compiler

USAGE:
    sasso [options] <input.scss>
    sasso --stdin [options] < input.scss

OPTIONS:
    -s, --style <expanded|compressed>   output style (default: expanded)
    -I, --load-path <dir>               add an @import search path (repeatable)
    -o, --output <file>                 write CSS to <file> instead of stdout
        --source-map                    also write <output>.map and append a
                                        sourceMappingURL footer (requires -o)
        --embed-sources                 embed full source text in the map's
                                        sourcesContent
        --source-map-urls <relative|absolute>
                                        how the map references its sources
                                        (default: relative)
        --stdin                         read SCSS from standard input
        --indented                      parse the indented .sass syntax
        --no-unicode                    ASCII-only diagnostics (no box glyphs)
        --loop <N>                      recompile in-process N times (throughput)
    -q, --quiet                         suppress CSS on stdout (timing only)
        --version                       print version and exit
    -h, --help                          print this help and exit

NOTE: --embed-source-map (inline data: URI) is not yet supported.
";

struct Cli {
    inputs: Vec<PathBuf>,
    use_stdin: bool,
    style: OutputStyle,
    load_paths: Vec<PathBuf>,
    /// Force the indented `.sass` syntax (otherwise inferred from the input
    /// path's extension; `--stdin` defaults to SCSS).
    indented: bool,
    /// Suppress CSS on stdout (timing-only runs).
    quiet: bool,
    /// Recompile the input in-process this many times and report throughput.
    loop_n: Option<u32>,
    /// Render diagnostics with the ASCII glyph set (dart-sass `--no-unicode`).
    no_unicode: bool,
    /// Write the compiled CSS to this file instead of stdout (`-o`). Requires a
    /// single input.
    output: Option<PathBuf>,
    /// Also write a `<output>.map` sidecar and append the `sourceMappingURL`
    /// footer to the CSS file (`--source-map`; requires `-o`).
    source_map: bool,
    /// Embed each source's full text in the map's `sourcesContent`
    /// (`--embed-sources`).
    embed_sources: bool,
    /// How the map's `sources[]` reference the inputs (`--source-map-urls`).
    source_map_urls: SourceMapUrls,
}

/// How the source map's `sources[]` entries reference the input files.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceMapUrls {
    /// Path relative to the `.map` file's directory (dart-sass default).
    Relative,
    /// Absolute `file://` URL.
    Absolute,
}

fn main() -> ExitCode {
    // Touch stdout/stderr once before any compile scope: std lazily heap-
    // allocates their lock (a boxed pthread_mutex_t on macOS) on first use,
    // and if that first use happened inside an arena scope (e.g. a deprecation
    // warning mid-compile) the allocation would be swept by the scope reset,
    // leaving the static stdio locks dangling for every later print.
    {
        use std::io::Write;
        let _ = std::io::stdout().lock().flush();
        let _ = std::io::stderr().lock().flush();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Ok(Action::Run(cli)) => run(cli),
        Ok(Action::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Action::Version) => {
            println!("sasso {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("error: {msg}\n");
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

enum Action {
    Run(Cli),
    Help,
    Version,
}

fn parse_args(args: &[String]) -> Result<Action, String> {
    let mut cli = Cli {
        inputs: Vec::new(),
        use_stdin: false,
        style: OutputStyle::Expanded,
        load_paths: Vec::new(),
        indented: false,
        quiet: false,
        loop_n: None,
        no_unicode: false,
        output: None,
        source_map: false,
        embed_sources: false,
        source_map_urls: SourceMapUrls::Relative,
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "--version" => return Ok(Action::Version),
            "--stdin" => cli.use_stdin = true,
            "--indented" => cli.indented = true,
            "--no-unicode" => cli.no_unicode = true,
            "--source-map" => cli.source_map = true,
            "--embed-sources" => cli.embed_sources = true,
            "-q" | "--quiet" => cli.quiet = true,
            "-o" | "--output" => {
                i += 1;
                let v = args.get(i).ok_or("--output requires a value")?;
                cli.output = Some(PathBuf::from(v));
            }
            "--source-map-urls" => {
                i += 1;
                let v = args.get(i).ok_or("--source-map-urls requires a value")?;
                cli.source_map_urls = parse_source_map_urls(v)?;
            }
            "--loop" => {
                i += 1;
                let v = args.get(i).ok_or("--loop requires a value")?;
                cli.loop_n = Some(parse_loop(v)?);
            }
            "-s" | "--style" => {
                i += 1;
                let v = args.get(i).ok_or("--style requires a value")?;
                cli.style = parse_style(v)?;
            }
            "-I" | "--load-path" => {
                i += 1;
                let v = args.get(i).ok_or("--load-path requires a value")?;
                cli.load_paths.push(PathBuf::from(v));
            }
            other => {
                if let Some(v) = other.strip_prefix("--style=") {
                    cli.style = parse_style(v)?;
                } else if let Some(v) = other.strip_prefix("--load-path=") {
                    cli.load_paths.push(PathBuf::from(v));
                } else if let Some(v) = other.strip_prefix("--loop=") {
                    cli.loop_n = Some(parse_loop(v)?);
                } else if let Some(v) = other.strip_prefix("--output=") {
                    cli.output = Some(PathBuf::from(v));
                } else if let Some(v) = other.strip_prefix("--source-map-urls=") {
                    cli.source_map_urls = parse_source_map_urls(v)?;
                } else if other.starts_with('-') && other != "-" {
                    return Err(format!("unknown option {other}"));
                } else {
                    cli.inputs.push(PathBuf::from(other));
                }
            }
        }
        i += 1;
    }
    // `--source-map` (the sidecar + footer) only makes sense with a real output
    // file; `--output` accepts exactly one input.
    if cli.source_map && cli.output.is_none() {
        return Err("--source-map requires --output".to_string());
    }
    if cli.output.is_some() && cli.inputs.len() + usize::from(cli.use_stdin) > 1 {
        return Err("--output requires a single input".to_string());
    }
    Ok(Action::Run(cli))
}

fn parse_source_map_urls(s: &str) -> Result<SourceMapUrls, String> {
    match s {
        "relative" => Ok(SourceMapUrls::Relative),
        "absolute" => Ok(SourceMapUrls::Absolute),
        other => Err(format!(
            "unknown --source-map-urls {other:?} (expected relative or absolute)"
        )),
    }
}

fn parse_loop(s: &str) -> Result<u32, String> {
    match s.parse::<u32>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!("--loop expects a positive integer (got {s:?})")),
    }
}

fn parse_style(s: &str) -> Result<OutputStyle, String> {
    match s {
        "expanded" => Ok(OutputStyle::Expanded),
        "compressed" => Ok(OutputStyle::Compressed),
        other => Err(format!(
            "unknown style {other:?} (expected expanded or compressed)"
        )),
    }
}

fn run(cli: Cli) -> ExitCode {
    // Gather the input units to compile, each paired with its syntax. `--stdin`
    // is a single unit (SCSS unless `--indented`); otherwise every path on the
    // command line is a unit, with its syntax inferred from the extension
    // (`.sass` -> indented) unless `--indented` forces it. Multiple file inputs
    // are compiled in one process so per-invocation startup is shared.
    // Each unit is (source, syntax, diagnostic-url). The URL is the path as it
    // should appear in stderr diagnostics (`-` for stdin, matching dart-sass).
    let mut units: Vec<(String, Syntax, String)> = Vec::new();
    if cli.use_stdin {
        let syntax = if cli.indented { Syntax::Sass } else { Syntax::Scss };
        match read_stdin() {
            Ok(s) => units.push((s, syntax, "-".to_string())),
            Err(e) => {
                if is_invalid_utf8(&e) {
                    eprintln!("Error: Invalid UTF-8.");
                    return ExitCode::from(65);
                }
                eprintln!("error: failed to read stdin: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        if cli.inputs.is_empty() {
            eprintln!("error: no input file (pass a path or --stdin)\n");
            eprint!("{USAGE}");
            return ExitCode::FAILURE;
        }
        for path in &cli.inputs {
            // Relative imports resolve against the CONTAINING file's
            // directory (the evaluator's current_file_dir), like dart — the
            // input's directory is deliberately NOT an implicit load path
            // (a file in a subdirectory must not see the entry's siblings).
            let ext_is_sass = path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("sass"))
                .unwrap_or(false);
            let syntax = if cli.indented || ext_is_sass {
                Syntax::Sass
            } else {
                Syntax::Scss
            };
            match std::fs::read_to_string(path) {
                Ok(s) => units.push((s, syntax, path.to_string_lossy().into_owned())),
                Err(e) => {
                    if is_invalid_utf8(&e) {
                        eprintln!("Error: Invalid UTF-8.");
                        return ExitCode::from(65);
                    }
                    eprintln!("error: cannot read {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    let importer = FsImporter::new(cli.load_paths);
    let style = cli.style;
    let unicode = !cli.no_unicode;
    // Build the per-unit options. Declared as a helper fn (not a closure) so the
    // returned `Options` can borrow `url`/`importer` for the caller's lifetime.
    fn opts_for<'a>(
        style: OutputStyle,
        importer: &'a FsImporter,
        unicode: bool,
        syntax: Syntax,
        url: &'a str,
    ) -> Options<'a> {
        Options::default()
            .with_style(style)
            .with_syntax(syntax)
            .with_importer(importer)
            .with_url(url)
            .with_unicode(unicode)
    }

    // File-output mode (`-o`): write the compiled CSS to a file (and, with
    // `--source-map`, a `<output>.map` sidecar + footer) instead of stdout.
    // `parse_args` guarantees exactly one input here. This is a distinct,
    // dart-byte-compatible path; the stdout path below is left untouched.
    if let Some(output) = &cli.output {
        let (source, syntax, url) = &units[0];
        let opts = opts_for(style, &importer, unicode, *syntax, url)
            .with_source_map_include_sources(cli.embed_sources);
        return write_output(
            &opts,
            source,
            output,
            url,
            cli.source_map,
            cli.style,
            cli.source_map_urls,
        );
    }

    // Throughput mode: recompile the whole input set in-process N times, timing
    // only the compile calls (sources are read once), and report ms/compile +
    // compiles/sec to stderr. The CSS is still emitted once unless `--quiet`.
    if let Some(n) = cli.loop_n {
        // Warm + correctness pass (also catches compile errors before timing).
        for (source, syntax, url) in &units {
            if let Err(e) = compile(source, &opts_for(style, &importer, unicode, *syntax, url)) {
                eprintln!("{e}");
                return ExitCode::from(65);
            }
        }
        let mut last = String::new();
        let start = Instant::now();
        for _ in 0..n {
            for (source, syntax, url) in &units {
                match compile(source, &opts_for(style, &importer, unicode, *syntax, url)) {
                    Ok(css) => last = css,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::from(65);
                    }
                }
            }
        }
        let elapsed = start.elapsed();
        let per = elapsed.as_secs_f64() * 1000.0 / f64::from(n);
        let per_sec = if per > 0.0 { 1000.0 / per } else { f64::INFINITY };
        eprintln!("sasso: {n} compiles in {elapsed:.3?} => {per:.3} ms/compile, {per_sec:.1} compiles/sec");
        if !cli.quiet && !last.is_empty() {
            // Match the CLI's single trailing newline (the library API omits it;
            // dart-sass emits nothing at all for empty output).
            println!("{last}");
        }
        return ExitCode::SUCCESS;
    }

    // One-shot: compile each input unit and stream the CSS to stdout in order.
    // dart-sass terminates each NON-empty compiled stylesheet with a single
    // newline that the library API omits (empty output stays empty), so append
    // one per non-empty unit.
    let mut out = String::new();
    for (source, syntax, url) in &units {
        match compile(source, &opts_for(style, &importer, unicode, *syntax, url)) {
            Ok(css) => {
                if !css.is_empty() {
                    out.push_str(&css);
                    out.push('\n');
                }
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(65);
            }
        }
    }
    if !cli.quiet {
        print!("{out}");
    }
    ExitCode::SUCCESS
}

fn read_stdin() -> std::io::Result<String> {
    use std::io::Read as _;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

fn is_invalid_utf8(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::InvalidData
}

/// Compile `source` and write the CSS to `output` (with `--source-map`, also a
/// `<output>.map` sidecar and a `sourceMappingURL` footer), matching dart-sass
/// byte-for-byte. `input_url` is the input path as given on the command line.
fn write_output(
    opts: &Options<'_>,
    source: &str,
    output: &Path,
    input_url: &str,
    source_map: bool,
    style: OutputStyle,
    source_map_urls: SourceMapUrls,
) -> ExitCode {
    if source_map {
        let result = match compile_with_source_map(source, opts) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(65);
            }
        };
        // The sidecar lives next to the CSS as `<output>.map`; the footer URL is
        // its basename (dart writes e.g. `out.css.map` and footers `out.css.map`).
        let map_path = append_ext(output, "map");
        let map_url = path_basename(&map_path);
        let css = append_source_map_footer(&result.css, &map_url, style);
        // Build the map JSON in dart's field order, rewriting `file`/`sources`.
        let file = path_basename(output);
        let sources = adjust_sources(&result.source_map.sources, input_url, &map_path, source_map_urls);
        let map_json = dart_map_json(
            &file,
            &sources,
            result.source_map.sources_content.as_deref(),
            &result.source_map.mappings,
        );
        if let Err(e) = std::fs::write(output, css.as_bytes()) {
            eprintln!("error: cannot write {}: {e}", output.display());
            return ExitCode::FAILURE;
        }
        if let Err(e) = std::fs::write(&map_path, map_json.as_bytes()) {
            eprintln!("error: cannot write {}: {e}", map_path.display());
            return ExitCode::FAILURE;
        }
    } else {
        let mut css = match compile(source, opts) {
            Ok(css) => css,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(65);
            }
        };
        // The library API omits the trailing newline dart-sass's CLI writes to
        // non-empty output (empty output stays empty).
        if !css.is_empty() {
            css.push('\n');
        }
        if let Err(e) = std::fs::write(output, css.as_bytes()) {
            eprintln!("error: cannot write {}: {e}", output.display());
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// dart's `sourceMappingURL` footer. The library CSS has no trailing newline,
/// so EXPANDED appends `\n\n/*# … */\n` (the line terminator plus dart's blank
/// separator line, yielding `…}\n\n/*# … */\n`); COMPRESSED appends
/// `/*# … */\n` with no leading newline. Any `*/` in the URL is escaped as
/// `%2A/` so it cannot terminate the comment early.
fn append_source_map_footer(css: &str, url: &str, style: OutputStyle) -> String {
    let url = url.replace("*/", "%2A/");
    let mut out = String::with_capacity(css.len() + url.len() + 32);
    out.push_str(css);
    match style {
        OutputStyle::Expanded => out.push_str(&format!("\n\n/*# sourceMappingURL={url} */\n")),
        OutputStyle::Compressed => out.push_str(&format!("/*# sourceMappingURL={url} */\n")),
    }
    out
}

/// Serialize the map JSON in dart-sass's exact field order:
/// `version, sourceRoot:"", sources, names:[], mappings, file[, sourcesContent]`.
/// Hand-built (zero-dep) with the same string escaping dart uses.
fn dart_map_json(file: &str, sources: &[String], contents: Option<&[String]>, mappings: &str) -> String {
    let mut s = String::from("{\"version\":3,\"sourceRoot\":\"\",\"sources\":[");
    for (i, src) in sources.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json_str(src, &mut s);
    }
    s.push_str("],\"names\":[],\"mappings\":");
    json_str(mappings, &mut s);
    s.push_str(",\"file\":");
    json_str(file, &mut s);
    if let Some(contents) = contents {
        s.push_str(",\"sourcesContent\":[");
        for (i, c) in contents.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            json_str(c, &mut s);
        }
        s.push(']');
    }
    s.push('}');
    s
}

/// Append a JSON string literal (quotes + escaping) to `out`, matching the
/// library's `sourcemap::json_str` so map fields are byte-identical to dart.
fn json_str(value: &str, out: &mut String) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The basename (last path component) of `p` as a lossy string.
fn path_basename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// `<p>.<ext>` — append a new extension component (e.g. `out.css` -> `out.css.map`).
fn append_ext(p: &Path, ext: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Rewrite each source URL to dart's form for the chosen `--source-map-urls`:
/// `relative` = the lexically-normalized path from the `.map` file's directory
/// to the source (URL-encoded); `absolute` = a canonicalized `file://` URL.
/// The library hands us the input path(s) as stamped during eval (the entry is
/// `input_url`); we adjust each one the same way dart does.
fn adjust_sources(sources: &[String], input_url: &str, map_path: &Path, mode: SourceMapUrls) -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    sources
        .iter()
        .map(|src| {
            // The entry source equals `input_url`; imports carry their own paths.
            // Treat each as a filesystem path relative to cwd.
            let raw: &str = if src == "stdin" { input_url } else { src.as_str() };
            let abs = normalize_path(&cwd.join(raw));
            match mode {
                SourceMapUrls::Absolute => file_url(&abs),
                SourceMapUrls::Relative => {
                    let map_dir = normalize_path(&cwd.join(map_path.parent().unwrap_or(Path::new(""))));
                    let rel = relative_path(&map_dir, &abs);
                    encode_url_path(&rel)
                }
            }
        })
        .collect()
}

/// Lexically normalize a path: resolve `.`/`..` components without touching the
/// filesystem (so it works for paths that may not exist yet), like dart's URL
/// normalization. Keeps it absolute if it started absolute.
fn normalize_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component<'_>> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !matches!(out.last(), Some(Component::RootDir | Component::Prefix(_))) {
                    out.push(comp);
                }
            }
            c => out.push(c),
        }
    }
    out.iter().collect()
}

/// The relative path from directory `base` to `target`, as forward-slash
/// segments (dart emits `/`-separated source URLs on every platform). Both must
/// be normalized absolute paths.
fn relative_path(base: &Path, target: &Path) -> String {
    use std::path::Component;
    let base: Vec<Component<'_>> = base.components().collect();
    let target: Vec<Component<'_>> = target.components().collect();
    let common = base.iter().zip(target.iter()).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..base.len() {
        parts.push("..".to_string());
    }
    for c in &target[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    parts.join("/")
}

/// A `file://` URL for an absolute path, percent-encoding each segment the way
/// dart's `Uri` does (and forward-slash separated).
fn file_url(abs: &Path) -> String {
    use std::path::Component;
    let mut s = String::from("file://");
    for comp in abs.components() {
        match comp {
            Component::RootDir | Component::Prefix(_) => {}
            c => {
                s.push('/');
                s.push_str(&encode_url_segment(&c.as_os_str().to_string_lossy()));
            }
        }
    }
    s
}

/// Percent-encode a forward-slash-separated relative URL path (keeping the `/`).
fn encode_url_path(path: &str) -> String {
    path.split('/')
        .map(encode_url_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Percent-encode one URL path segment exactly like dart's `Uri`: keep the
/// unreserved set (`A-Za-z0-9-._~`), the sub-delims (`!$&'()*+,;=`) and `@`;
/// percent-encode every other byte (UTF-8) as uppercase `%XX`.
fn encode_url_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b'@'
            );
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_upper(b >> 4));
            out.push(hex_upper(b & 0xf));
        }
    }
    out
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}
