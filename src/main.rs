//! `sasso` command-line interface.
//!
//! A small, dependency-free CLI over the `sasso` library:
//!
//! ```text
//! sasso [options] <input.scss>
//! sasso --stdin [options] < input.scss
//!
//!   -s, --style <expanded|compressed>   output style (default: expanded)
//!   -I, --load-path <dir>               add an @import search path (repeatable)
//!       --stdin                         read SCSS from standard input
//!       --loop <N>                      recompile in-process N times (throughput)
//!   -q, --quiet                         suppress CSS on stdout (timing only)
//!       --version                       print version and exit
//!   -h, --help                          print this help and exit
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use sasso::{compile, FsImporter, Options, OutputStyle, Syntax};

const USAGE: &str = "\
sasso — a pure-Rust SCSS to CSS compiler

USAGE:
    sasso [options] <input.scss>
    sasso --stdin [options] < input.scss

OPTIONS:
    -s, --style <expanded|compressed>   output style (default: expanded)
    -I, --load-path <dir>               add an @import search path (repeatable)
        --stdin                         read SCSS from standard input
        --indented                      parse the indented .sass syntax
        --loop <N>                      recompile in-process N times (throughput)
    -q, --quiet                         suppress CSS on stdout (timing only)
        --version                       print version and exit
    -h, --help                          print this help and exit
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
}

fn main() -> ExitCode {
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
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "--version" => return Ok(Action::Version),
            "--stdin" => cli.use_stdin = true,
            "--indented" => cli.indented = true,
            "-q" | "--quiet" => cli.quiet = true,
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
                } else if other.starts_with('-') && other != "-" {
                    return Err(format!("unknown option {other}"));
                } else {
                    cli.inputs.push(PathBuf::from(other));
                }
            }
        }
        i += 1;
    }
    Ok(Action::Run(cli))
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

fn run(mut cli: Cli) -> ExitCode {
    // Gather the input units to compile, each paired with its syntax. `--stdin`
    // is a single unit (SCSS unless `--indented`); otherwise every path on the
    // command line is a unit, with its syntax inferred from the extension
    // (`.sass` -> indented) unless `--indented` forces it. Multiple file inputs
    // are compiled in one process so per-invocation startup is shared.
    let mut units: Vec<(String, Syntax)> = Vec::new();
    if cli.use_stdin {
        let syntax = if cli.indented { Syntax::Sass } else { Syntax::Scss };
        match read_stdin() {
            Ok(s) => units.push((s, syntax)),
            Err(e) => {
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
            // Make each input file's directory an implicit load path so sibling
            // partials resolve, like dart-sass.
            if let Some(parent) = path.parent() {
                let parent = parent.to_path_buf();
                if !parent.as_os_str().is_empty() && !cli.load_paths.contains(&parent) {
                    cli.load_paths.push(parent);
                }
            }
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
                Ok(s) => units.push((s, syntax)),
                Err(e) => {
                    eprintln!("error: cannot read {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    if cli.load_paths.is_empty() {
        cli.load_paths.push(PathBuf::from("."));
    }
    let importer = FsImporter::new(cli.load_paths);
    let style = cli.style;
    let opts_for = |syntax| {
        Options::default()
            .with_style(style)
            .with_syntax(syntax)
            .with_importer(&importer)
    };

    // Throughput mode: recompile the whole input set in-process N times, timing
    // only the compile calls (sources are read once), and report ms/compile +
    // compiles/sec to stderr. The CSS is still emitted once unless `--quiet`.
    if let Some(n) = cli.loop_n {
        // Warm + correctness pass (also catches compile errors before timing).
        for (source, syntax) in &units {
            if let Err(e) = compile(source, &opts_for(*syntax)) {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
        let mut last = String::new();
        let start = Instant::now();
        for _ in 0..n {
            for (source, syntax) in &units {
                match compile(source, &opts_for(*syntax)) {
                    Ok(css) => last = css,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
        let elapsed = start.elapsed();
        let per = elapsed.as_secs_f64() * 1000.0 / f64::from(n);
        let per_sec = if per > 0.0 { 1000.0 / per } else { f64::INFINITY };
        eprintln!("sasso: {n} compiles in {elapsed:.3?} => {per:.3} ms/compile, {per_sec:.1} compiles/sec");
        if !cli.quiet {
            print!("{last}");
        }
        return ExitCode::SUCCESS;
    }

    // One-shot: compile each input unit and stream the CSS to stdout in order.
    let mut out = String::new();
    for (source, syntax) in &units {
        match compile(source, &opts_for(*syntax)) {
            Ok(css) => out.push_str(&css),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
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
