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
//!       --version                       print version and exit
//!   -h, --help                          print this help and exit
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use sasso::{compile, FsImporter, Options, OutputStyle};

const USAGE: &str = "\
sasso — a pure-Rust SCSS to CSS compiler

USAGE:
    sasso [options] <input.scss>
    sasso --stdin [options] < input.scss

OPTIONS:
    -s, --style <expanded|compressed>   output style (default: expanded)
    -I, --load-path <dir>               add an @import search path (repeatable)
        --stdin                         read SCSS from standard input
        --version                       print version and exit
    -h, --help                          print this help and exit
";

struct Cli {
    input: Option<PathBuf>,
    use_stdin: bool,
    style: OutputStyle,
    load_paths: Vec<PathBuf>,
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
        input: None,
        use_stdin: false,
        style: OutputStyle::Expanded,
        load_paths: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "--version" => return Ok(Action::Version),
            "--stdin" => cli.use_stdin = true,
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
                } else if other.starts_with('-') && other != "-" {
                    return Err(format!("unknown option {other}"));
                } else {
                    cli.input = Some(PathBuf::from(other));
                }
            }
        }
        i += 1;
    }
    Ok(Action::Run(cli))
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
    let source = if cli.use_stdin {
        match read_stdin() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: failed to read stdin: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match &cli.input {
            Some(path) => {
                // Make the input file's directory an implicit load path so
                // sibling partials resolve, like dart-sass.
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        cli.load_paths.insert(0, parent.to_path_buf());
                    }
                }
                match std::fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("error: cannot read {}: {e}", path.display());
                        return ExitCode::FAILURE;
                    }
                }
            }
            None => {
                eprintln!("error: no input file (pass a path or --stdin)\n");
                eprint!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    };

    if cli.load_paths.is_empty() {
        cli.load_paths.push(PathBuf::from("."));
    }
    let importer = FsImporter::new(cli.load_paths);
    let options = Options::default().with_style(cli.style).with_importer(&importer);

    match compile(&source, &options) {
        Ok(css) => {
            print!("{css}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn read_stdin() -> std::io::Result<String> {
    use std::io::Read as _;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}
