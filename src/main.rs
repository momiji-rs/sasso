//! `sasso` command-line interface.
//!
//! A small, dependency-free CLI over the `sasso` library, flag-compatible with
//! the dart-sass CLI wherever the two overlap:
//!
//! ```text
//! sasso [options] <input.scss> [<output.css>]     CSS to stdout, or to a file
//! sasso [options] <in.scss>:<out.css>...          one output file per input
//! sasso [options] <in-dir/>:<out-dir/>            compile a whole tree
//! sasso --stdin [options] [<output.css>] < input.scss
//! ```
//!
//! The positional grammar is dart-sass's (`<input> [output]`); several files
//! are compiled through `in:out` pairs, in parallel, one worker per physical
//! core where the topology is known and one per CPU where it is not
//! (`-j/--jobs N` to cap it), with diagnostics still reported in command-line
//! order. Exit codes follow dart-sass too: `64` for a usage error, `65` for a
//! compile error, `66` when an input cannot be read.

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use sasso::{
    compile, compile_with_source_map, FsImporter, Options, OutputStyle, SourceMap, Syntax, WarnEvent,
    WarnHandler,
};

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
    sasso [options] <input.scss> [<output.css>]     CSS to stdout, or to a file
    sasso [options] <in.scss>:<out.css>...          one output file per input
    sasso [options] <in-dir/>:<out-dir/>            compile a whole tree
    sasso --stdin [options] [<output.css>] < input.scss

INPUT AND OUTPUT:
    -s, --style <expanded|compressed>   output style (default: expanded)
    -I, --load-path <dir>               add an @import/@use search path (repeatable)
    -o, --output <file>                 write CSS to <file> (same as a second
                                        positional argument)
        --stdin                         read SCSS from standard input
        --indented                      parse the indented .sass syntax
        --[no-]charset                  emit @charset/BOM for non-ASCII CSS
                                        (default: on)
        --[no-]error-css                on a compile error, write a stylesheet
                                        describing it (default: on when
                                        compiling to a file)

SOURCE MAPS:
        --[no-]source-map               generate source maps (default: on when
                                        compiling to a file, off for stdout)
        --source-map-urls <relative|absolute>
                                        how the map references its sources
                                        (default: relative)
        --[no-]embed-sources            embed the source text in the map's
                                        sourcesContent
        --[no-]embed-source-map         inline the map into the CSS as a
                                        data: URI instead of a .map file

WARNINGS:
    -q, --[no-]quiet                    don't print warnings
        --[no-]quiet-deps               don't print compiler warnings from
                                        dependencies (stylesheets reached
                                        through load paths)

OTHER:
    -j, --jobs <N>                      compile at most N files at once
                                        (default: one per core, or per CPU
                                        where the core count is unknown)
        --[no-]stop-on-error            don't start more files once one fails
    -c, --[no-]color                    accepted for dart-sass compatibility
                                        (no-op: sasso never colors output)
        --[no-]unicode                  Unicode box glyphs in diagnostics
                                        (default: on)
        --loop <N>                      recompile in-process N times and report
                                        throughput (stdout inputs only)
        --no-css                        compile but discard the CSS (timing or
                                        lint runs)
        --version                       print version and exit
    -h, --help                          print this help and exit
    --                                  end of options: what follows are inputs
                                        and outputs, even if they start with -
";

/// Exit codes, as dart-sass (and BSD `sysexits.h`) define them.
const EXIT_USAGE: u8 = 64;
const EXIT_COMPILE: u8 = 65;
const EXIT_IO: u8 = 66;

struct Cli {
    /// Positional arguments in order, a `-` included; resolved into `entry` /
    /// `output` after parsing (dart's `<input> [output]`).
    positionals: Vec<String>,
    /// dart-style `source:destination` pairs (files or directories); a `-`
    /// source is standard input.
    pairs: Vec<(PathBuf, PathBuf)>,
    /// The final value of `--[no-]stdin`.
    stdin_flag: bool,
    /// The single input of stdout / `-o` mode, once positionals are resolved.
    entry: Option<Entry>,
    style: OutputStyle,
    load_paths: Vec<PathBuf>,
    /// Force the indented `.sass` syntax (otherwise inferred from the input
    /// path's extension; `--stdin` defaults to SCSS).
    indented: bool,
    /// Don't print warnings (dart-sass `--quiet`).
    quiet: bool,
    /// Don't print compiler (deprecation) warnings from dependencies —
    /// stylesheets reached through a load path (dart-sass `--quiet-deps`).
    quiet_deps: bool,
    /// Compile but discard the CSS (timing-only runs).
    no_css: bool,
    /// Recompile the input in-process this many times and report throughput.
    loop_n: Option<u32>,
    /// Render diagnostics with the ASCII glyph set (dart-sass `--no-unicode`).
    no_unicode: bool,
    /// Write the compiled CSS to this file instead of stdout (`-o`). Requires a
    /// single input.
    output: Option<PathBuf>,
    /// `--[no-]source-map`; `None` = dart's default (on for file output, off
    /// for stdout).
    source_map: Option<bool>,
    /// Embed each source's full text in the map's `sourcesContent`.
    embed_sources: bool,
    /// Inline the map into the CSS as a `data:` URI instead of a sidecar.
    embed_source_map: bool,
    /// How the map's `sources[]` reference the inputs (`--source-map-urls`);
    /// `None` when not given (dart's default, `relative`).
    source_map_urls: Option<SourceMapUrls>,
    /// `--[no-]error-css`; `None` = dart's default (on for file output).
    error_css: Option<bool>,
    /// Don't start compiling more files once one has failed.
    stop_on_error: bool,
    /// Emit `@charset`/BOM for non-ASCII output (dart-sass `--charset`).
    charset: bool,
    /// Worker-thread cap (`-j`); `None` = `default_jobs` — one per physical
    /// core where the topology is known, one per CPU where it is not.
    jobs: Option<usize>,
}

/// The single input of stdout / `-o` mode.
enum Entry {
    /// `--stdin`, or a positional `-`.
    Stdin,
    File(PathBuf),
}

/// How the source map's `sources[]` entries reference the input files.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceMapUrls {
    /// Path relative to the `.map` file's directory (dart-sass default).
    Relative,
    /// Absolute `file://` URL.
    Absolute,
}

/// How many files to compile at once when `-j` is not given.
///
/// `available_parallelism` counts SMT threads. A compile is pure computation,
/// so two hyperthreads on one core contend for the same execution units rather
/// than overlapping each other's stalls — past the core count, more workers is
/// slower. Measured on a Ryzen 7 8745HS (8 cores / 16 threads) over 138
/// Lichess stylesheets: 208 ms at `-j 8` against 235 ms at `-j 16`.
///
/// Linux publishes the topology in `/proc/cpuinfo`, which is a read rather
/// than a process spawn. Apple silicon has no SMT, so the logical count is
/// already right there; other platforms keep it rather than grow a dependency
/// or an `unsafe` sysctl call for a number that is, at worst, the old default.
fn default_jobs() -> usize {
    let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    if !cfg!(target_os = "linux") {
        return logical;
    }
    jobs_from(logical, std::fs::read_to_string("/proc/cpuinfo").ok().as_deref())
}

/// The decision itself, separated from the two host facts it reads so a test
/// can hand it a machine this one is not.
fn jobs_from(logical: usize, cpuinfo: Option<&str>) -> usize {
    // Never more than the kernel offers this process: a cgroup or `taskset`
    // cap shows up in `available_parallelism`, not in `/proc/cpuinfo`.
    //
    // Both kinds of cap, which is worth stating because it is easy to assume
    // otherwise — measured in one process on a 16-thread host, 2026-09-17:
    //
    //     plain host        mask 0-15      no quota            answers 16
    //     --cpus=2          mask 0-15      cpu.max 200000 …    answers 2
    //     --cpuset-cpus=…   mask 0-1,8-9   no quota            answers 4
    //
    // The middle row is the one that matters: the mask is the whole machine,
    // so the 2 can only have come from the quota. Nothing here needs to read
    // either file. The npm CLI is not so lucky: `availableParallelism()` has
    // only accounted for the quota since Node 22, and below 18.14 there is no
    // such API at all, so `_jobs.mjs` reads the mask and the quota itself.
    //
    // This is the host's core count capped by what the process may use, and
    // deliberately NOT the cores inside the mask, which measures much worse:
    // SMT only stops paying once enough cores are in play. Same corpus, best
    // of five, `taskset` masks of whole cores:
    //
    //     cores allowed   1     2     4     6     7     8
    //     SMT is worth  +55%  +50%  +33%   -2%   -6%  -11%
    //
    // Counting cores within the mask would pick 2 where 4 is 50% faster, and
    // 4 where 8 is 33% faster.
    cpuinfo
        .and_then(physical_cores)
        .map_or(logical, |physical| physical.clamp(1, logical))
}

/// Physical cores in `/proc/cpuinfo` text, or `None` when it reports no
/// topology — containers and VMs often do not, and a number derived from
/// nothing is worse than the kernel's own count.
///
/// A core is a `(physical id, core id)` pair: `core id` alone repeats across
/// sockets, so deduplicating on it halves a dual-socket machine.
fn physical_cores(cpuinfo: &str) -> Option<usize> {
    let mut cores = std::collections::BTreeSet::new();
    // Buffered per record rather than inserted on sight, for two reasons: a
    // file may name the socket for some processors and not others, and nothing
    // promises `physical id` is printed before `core id`. Either way, inserting
    // early files the core under a socket the kernel never gave it.
    let mut package: Option<&str> = None;
    let mut core: Option<&str> = None;
    let mut end_of_record = |package: &mut Option<&str>, core: &mut Option<&str>| {
        if let Some(id) = core.take() {
            cores.insert((package.take().unwrap_or("").to_string(), id.to_string()));
        }
        *package = None;
    };
    for line in cpuinfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "processor" => end_of_record(&mut package, &mut core),
            "physical id" => {
                // Seeing a field this record already has means the previous
                // record ended, whether or not a `processor` line said so.
                if package.is_some() {
                    end_of_record(&mut package, &mut core);
                }
                package = Some(value.trim());
            }
            "core id" => {
                if core.is_some() {
                    end_of_record(&mut package, &mut core);
                }
                core = Some(value.trim());
            }
            _ => {}
        }
    }
    end_of_record(&mut package, &mut core);
    (!cores.is_empty()).then_some(cores.len())
}

#[cfg(test)]
mod default_jobs_tests {
    use super::{jobs_from, physical_cores};

    #[test]
    fn eight_threads_on_four_cores_read_as_four() {
        let text: String = (0..8)
            .map(|i| format!("processor\t: {i}\nphysical id\t: 0\ncore id\t: {}\n\n", i / 2))
            .collect();
        assert_eq!(physical_cores(&text), Some(4));
    }

    #[test]
    fn two_sockets_of_four_read_as_eight_not_four() {
        let mut text = String::new();
        for package in 0..2 {
            for core in 0..4 {
                text.push_str(&format!("physical id\t: {package}\ncore id\t: {core}\n\n"));
            }
        }
        assert_eq!(physical_cores(&text), Some(8));
    }

    #[test]
    fn a_cgroup_cap_below_the_core_count_wins() {
        // 8 physical cores on the host, but this process may use two of them.
        let host: String = (0..16)
            .map(|i| format!("physical id\t: 0\ncore id\t: {}\n\n", i / 2))
            .collect();
        assert_eq!(jobs_from(2, Some(&host)), 2);
        assert_eq!(jobs_from(16, Some(&host)), 8);
        assert_eq!(jobs_from(4, None), 4);
    }

    #[test]
    fn the_socket_resets_at_each_processor_record() {
        // Two sockets' worth of `core id: 0` are two cores, however incomplete
        // the file is about saying so.
        let partial = "processor\t: 0\nphysical id\t: 0\ncore id\t: 0\n\nprocessor\t: 1\ncore id\t: 0\n";
        assert_eq!(physical_cores(partial), Some(2));
        // But a file that names no socket at all is one socket, not a
        // giving-up: that is what a single-socket VM reports.
        let unnamed = "processor\t: 0\ncore id\t: 0\n\nprocessor\t: 1\ncore id\t: 0\n";
        assert_eq!(physical_cores(unnamed), Some(1));
    }

    #[test]
    fn the_fields_may_come_in_either_order() {
        // Nothing promises `physical id` is printed first. Two sockets of two
        // cores, written the other way round, are still four cores.
        let mut text = String::new();
        for package in 0..2 {
            for core in 0..2 {
                text.push_str(&format!(
                    "processor\t: 0\ncore id\t: {core}\nphysical id\t: {package}\n\n"
                ));
            }
        }
        assert_eq!(physical_cores(&text), Some(4));
    }

    #[test]
    fn no_topology_reported_is_no_answer() {
        assert_eq!(physical_cores("processor\t: 0\nmodel name\t: Whatever\n"), None);
        assert_eq!(physical_cores(""), None);
    }
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
            ExitCode::from(EXIT_USAGE)
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
        positionals: Vec::new(),
        pairs: Vec::new(),
        stdin_flag: false,
        entry: None,
        style: OutputStyle::Expanded,
        load_paths: Vec::new(),
        indented: false,
        quiet: false,
        quiet_deps: false,
        no_css: false,
        loop_n: None,
        no_unicode: false,
        output: None,
        source_map: None,
        embed_sources: false,
        embed_source_map: false,
        source_map_urls: None,
        error_css: None,
        stop_on_error: false,
        charset: true,
        jobs: None,
    };
    let mut i = 0;
    // After `--` (end of options) every argument is an operand, so an input
    // or output whose name starts with `-` can be named.
    let mut only_operands = false;
    while i < args.len() {
        let a = &args[i];
        if only_operands {
            push_operand(&mut cli, a)?;
            i += 1;
            continue;
        }
        match a.as_str() {
            "--" => only_operands = true,
            "-h" | "--help" => return Ok(Action::Help),
            "--version" => return Ok(Action::Version),
            "--stdin" => cli.stdin_flag = true,
            "--no-stdin" => cli.stdin_flag = false,
            "--indented" => cli.indented = true,
            "--no-indented" => cli.indented = false,
            "--unicode" => cli.no_unicode = false,
            "--no-unicode" => cli.no_unicode = true,
            "--source-map" => cli.source_map = Some(true),
            "--no-source-map" => cli.source_map = Some(false),
            "--embed-sources" => cli.embed_sources = true,
            "--no-embed-sources" => cli.embed_sources = false,
            "--embed-source-map" => cli.embed_source_map = true,
            "--no-embed-source-map" => cli.embed_source_map = false,
            "--error-css" => cli.error_css = Some(true),
            "--no-error-css" => cli.error_css = Some(false),
            "--charset" => cli.charset = true,
            "--no-charset" => cli.charset = false,
            "-q" | "--quiet" => cli.quiet = true,
            "--no-quiet" => cli.quiet = false,
            "--quiet-deps" => cli.quiet_deps = true,
            "--no-quiet-deps" => cli.quiet_deps = false,
            "--stop-on-error" => cli.stop_on_error = true,
            "--no-stop-on-error" => cli.stop_on_error = false,
            // sasso never emits ANSI colors; accept dart's flags so a build
            // script written for `sass` runs unchanged.
            "-c" | "--color" | "--no-color" => {}
            "--no-css" => cli.no_css = true,
            "-o" | "--output" => {
                i += 1;
                let v = args.get(i).ok_or("--output requires a value")?;
                cli.output = Some(PathBuf::from(v));
            }
            "--source-map-urls" => {
                i += 1;
                let v = args.get(i).ok_or("--source-map-urls requires a value")?;
                cli.source_map_urls = Some(parse_source_map_urls(v)?);
            }
            "--loop" => {
                i += 1;
                let v = args.get(i).ok_or("--loop requires a value")?;
                cli.loop_n = Some(parse_loop(v)?);
            }
            "-j" | "--jobs" => {
                i += 1;
                let v = args.get(i).ok_or("--jobs requires a value")?;
                cli.jobs = Some(parse_jobs(v)?);
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
                } else if let Some(v) = other.strip_prefix("--jobs=") {
                    cli.jobs = Some(parse_jobs(v)?);
                } else if let Some(v) = other.strip_prefix("--output=") {
                    cli.output = Some(PathBuf::from(v));
                } else if let Some(v) = other.strip_prefix("--source-map-urls=") {
                    cli.source_map_urls = Some(parse_source_map_urls(v)?);
                } else if other.starts_with('-') && other != "-" && !other.starts_with("-:") {
                    return Err(format!("unknown option {other}"));
                } else {
                    push_operand(&mut cli, other)?;
                }
            }
        }
        i += 1;
    }
    // The same combinations dart-sass rejects, with its wording.
    if cli.embed_source_map && cli.source_map == Some(false) {
        return Err("--embed-source-map isn't allowed with --no-source-map.".to_string());
    }
    if cli.embed_sources && cli.source_map == Some(false) {
        return Err("--embed-sources isn't allowed with --no-source-map.".to_string());
    }
    if cli.source_map_urls.is_some() && cli.source_map == Some(false) {
        return Err("--source-map-urls isn't allowed with --no-source-map.".to_string());
    }
    if !cli.pairs.is_empty() {
        if !cli.positionals.is_empty() {
            return Err("Positional and \":\" arguments may not both be used.".to_string());
        }
        if cli.stdin_flag {
            return Err("--stdin may not be used with \":\" arguments.".to_string());
        }
        if cli.output.is_some() {
            return Err("--output may not be used with \":\" arguments.".to_string());
        }
        if cli.loop_n.is_some() {
            return Err("--loop compiles to stdout only (no \":\" arguments or --output).".to_string());
        }
        // dart: each source appears once (`-` included) …
        let mut seen: std::collections::HashSet<&Path> = std::collections::HashSet::new();
        for (src, _) in &cli.pairs {
            if !seen.insert(src.as_path()) {
                return Err(format!("Duplicate source {:?}.", src.to_string_lossy()));
            }
        }
        // … and it keeps sources in a path-keyed map, so two spellings of one
        // file (`a.scss` and `./a.scss`, `dir/../a.scss`) coalesce into a
        // single compile whose destination is the later one.
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut keys: Vec<PathBuf> = Vec::new();
        let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (src, dest) in std::mem::take(&mut cli.pairs) {
            let key = if src == Path::new("-") {
                src.clone()
            } else {
                normalize_path(&cwd.join(&src))
            };
            match keys.iter().position(|k| *k == key) {
                Some(i) => kept[i].1 = dest,
                None => {
                    keys.push(key);
                    kept.push((src, dest));
                }
            }
        }
        cli.pairs = kept;
    } else {
        // dart's positional grammar: `<input> [output]`, or just `[output]`
        // with `--stdin`. An INPUT of `-` is standard input, whatever the
        // final `--[no-]stdin` value; a `-` in the output position is a file
        // called `-`, as in dart.
        let max = if cli.stdin_flag { 1 } else { 2 };
        if cli.positionals.len() > max {
            return Err(if cli.stdin_flag {
                "Only one argument is allowed with --stdin.".to_string()
            } else {
                "Only two positional args may be passed.".to_string()
            });
        }
        let mut positionals = cli.positionals.iter();
        cli.entry = if cli.stdin_flag {
            Some(Entry::Stdin)
        } else {
            positionals.next().map(|p| {
                if p == "-" {
                    Entry::Stdin
                } else {
                    Entry::File(PathBuf::from(p))
                }
            })
        };
        if let Some(out) = positionals.next() {
            if cli.output.is_some() {
                return Err("--output requires a single input".to_string());
            }
            cli.output = Some(PathBuf::from(out));
        }
        if cli.output.is_some() && cli.loop_n.is_some() {
            return Err("--loop compiles to stdout only (no \":\" arguments or --output).".to_string());
        }
    }
    // A bare directory positional compiles in place (see `run`): file output,
    // never stdout.
    let entry_is_dir = matches!(&cli.entry, Some(Entry::File(p)) if p.is_dir());
    if cli.loop_n.is_some() && entry_is_dir {
        return Err(
            "--loop compiles to stdout only (no directories, \":\" arguments or --output).".to_string(),
        );
    }
    if cli.loop_n.is_some() && (cli.source_map == Some(true) || cli.embed_source_map || cli.embed_sources) {
        return Err(
            "--loop does not generate source maps (drop --source-map, --embed-source-map and --embed-sources)."
                .to_string(),
        );
    }
    let to_stdout = cli.pairs.is_empty() && cli.output.is_none() && !entry_is_dir;
    if to_stdout {
        // A stdout map can only be embedded, and its sources are always
        // absolute `file://` URLs.
        if cli.source_map_urls == Some(SourceMapUrls::Relative) {
            return Err("--source-map-urls=relative isn't allowed when printing to stdout.".to_string());
        }
        if !cli.embed_source_map {
            if cli.source_map == Some(true) {
                return Err("When printing to stdout, --source-map requires --embed-source-map.".to_string());
            }
            if cli.embed_sources {
                return Err(
                    "When printing to stdout, --embed-sources requires --embed-source-map.".to_string(),
                );
            }
            if cli.source_map_urls.is_some() {
                return Err(
                    "When printing to stdout, --source-map-urls requires --embed-source-map.".to_string(),
                );
            }
        }
    }
    Ok(Action::Run(cli))
}

/// Record an operand: a `source:destination` pair, or a positional argument.
fn push_operand(cli: &mut Cli, arg: &str) -> Result<(), String> {
    match split_pair(arg)? {
        Some(pair) => cli.pairs.push(pair),
        None => cli.positionals.push(arg.to_string()),
    }
    Ok(())
}

/// Split a dart-style `source:destination` argument at its separating colon,
/// or return `Ok(None)` for a plain path. On Windows the colon of a leading
/// drive letter (`C:\in.scss`) is part of the path, not a separator.
fn split_pair(arg: &str) -> Result<Option<(PathBuf, PathBuf)>, String> {
    let mut from = 0;
    while let Some(off) = arg[from..].find(':') {
        let idx = from + off;
        let is_drive_colon = cfg!(windows) && idx == 1 && arg.as_bytes()[0].is_ascii_alphabetic();
        if is_drive_colon {
            from = idx + 1;
            continue;
        }
        let (src, dest) = (&arg[..idx], &arg[idx + 1..]);
        if src.is_empty() || dest.is_empty() {
            return Err(format!("expected <source>:<destination>, got {arg:?}"));
        }
        // dart: exactly one separator; the destination may only carry a drive
        // colon of its own (`C:\out.css`).
        let dest_drive_colon = cfg!(windows)
            && dest.len() > 1
            && dest.as_bytes()[1] == b':'
            && dest.as_bytes()[0].is_ascii_alphabetic();
        let extra = if dest_drive_colon {
            dest[2..].contains(':')
        } else {
            dest.contains(':')
        };
        if extra {
            return Err(format!("{arg:?} may only contain one \":\"."));
        }
        return Ok(Some((PathBuf::from(src), PathBuf::from(dest))));
    }
    Ok(None)
}

/// Report a usage error the way `parse_args` failures are reported.
fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}\n");
    eprint!("{USAGE}");
    ExitCode::from(EXIT_USAGE)
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

fn parse_jobs(s: &str) -> Result<usize, String> {
    match s.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!("--jobs expects a positive integer (got {s:?})")),
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

// ---------------------------------------------------------------------------
// Compile units
// ---------------------------------------------------------------------------

/// Where a unit's source text comes from.
#[derive(Clone)]
enum Source {
    /// Already read (standard input).
    Text(String),
    /// Read from this file when the unit is compiled.
    File(PathBuf),
    /// Standard input was not valid UTF-8: a compile-class failure that gets
    /// the same reporting (error stylesheet, stale-output cleanup) as a file's.
    InvalidUtf8,
}

/// Where a unit's CSS goes.
enum Target {
    Stdout,
    File(PathBuf),
}

/// One stylesheet to compile. `url` is the path as it appears in diagnostics
/// (`-` for stdin, matching dart-sass).
struct Unit {
    source: Source,
    url: String,
    syntax: Syntax,
    target: Target,
}

/// Settings shared by every unit (read-only across worker threads).
struct Shared {
    /// `-I` directories. Each unit builds its own `FsImporter` from them: the
    /// importer's dependency record (`--quiet-deps`) is per compilation, as in
    /// dart — the same file can be a dependency of one entry and not another.
    load_paths: Vec<PathBuf>,
    style: OutputStyle,
    unicode: bool,
    charset: bool,
    quiet: bool,
    quiet_deps: bool,
    no_css: bool,
    embed_sources: bool,
    embed_source_map: bool,
    source_map_urls: SourceMapUrls,
    /// Generate a source map for file targets (dart default: yes).
    file_source_map: bool,
    /// Write an error stylesheet to a file target on failure (dart default: yes).
    file_error_css: bool,
    /// Print an error stylesheet to stdout on failure (`--error-css`, explicit).
    stdout_error_css: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    /// A parse/eval error (or invalid UTF-8 input): exit 65.
    CompileError,
    /// The input could not be read or the output not written: exit 66.
    IoError,
}

/// What one unit produced. Diagnostics are buffered so parallel workers can
/// still be reported in command-line order.
struct Outcome {
    stderr: String,
    stdout: String,
    status: Status,
}

impl Outcome {
    fn failed(status: Status, stderr: String) -> Self {
        Outcome {
            stderr,
            stdout: String::new(),
            status,
        }
    }
}

/// Infer a file's syntax from its extension (`--indented` forces `.sass`).
/// Exact lowercase suffixes, like dart's `Syntax.forPath`: `input.CSS` or
/// `input.SASS` is SCSS.
fn syntax_for(path: &Path, indented: bool) -> Syntax {
    if indented {
        return Syntax::Sass;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("sass") => Syntax::Sass,
        Some("css") => Syntax::Css,
        _ => Syntax::Scss,
    }
}

/// Whether directory mode compiles this file: a `.scss`/`.sass`/`.css` (exact
/// lowercase suffix) whose name does not start with `_` (a partial), like
/// dart-sass.
fn is_compilable(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    // Exact lowercase suffixes, like dart's `_isEntrypoint`: `UPPER.SCSS` is
    // skipped.
    !name.starts_with('_')
        && matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("scss" | "sass" | "css")
        )
}

/// Expand a `dir:outdir` pair into one unit per compilable file under `src`
/// (recursively), mirroring the tree under `dest` with a `.css` extension.
/// Files are visited in sorted order so output is deterministic. Symlinked
/// directories are followed (as dart does), but each directory is visited once
/// by canonical identity, so a symlink cycle cannot loop or duplicate output.
fn expand_dir(src: &Path, dest: &Path, indented: bool, units: &mut Vec<Unit>) -> Result<(), String> {
    let mut files = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    seen.insert(std::fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf()));
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|_| format!("Error reading {}: Cannot open file.", dir.display()))?;
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| format!("Error reading {}: Cannot open file.", dir.display()))?;
            paths.push(entry.path());
        }
        // `read_dir` order is unspecified; sort so that, of two names for the
        // same directory (symlinks), the same one is mirrored every run.
        paths.sort();
        for path in paths {
            if path.is_dir() {
                let identity = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if seen.insert(identity) {
                    stack.push(path);
                }
            } else if is_compilable(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    let cwd = std::env::current_dir().unwrap_or_default();
    let src_abs = path_key(&normalize_path(&cwd.join(src)));
    let dest_abs = path_key(&normalize_path(&cwd.join(dest)));
    // dart-sass 1.104.1 skips every source file INSIDE the output directory
    // when that directory is nested in the source tree: `.:css` run twice
    // would otherwise mirror `css/` into `css/css/`. Nesting is strict — a
    // destination EQUAL to the source is not nested, so `sasso dir` (which is
    // `dir:dir`) still compiles every file — and it is the destination that
    // counts, not an intermediate directory: `.:css/deep` skips only what is
    // under `css/deep`, and still compiles `css/stale.scss`.
    let nested = dest_abs != src_abs && dest_abs.starts_with(&src_abs);
    for path in files {
        let rel = path.strip_prefix(src).unwrap_or(&path).with_extension("css");
        let out = dest.join(rel);
        let path_abs = path_key(&normalize_path(&cwd.join(&path)));
        if nested && path_abs.starts_with(&dest_abs) {
            continue;
        }
        // dart skips a CSS file whose destination is itself (`sasso dir`, or
        // `dir:dir`, with a `plain.css` inside): it would only be rewritten in
        // place.
        if path_key(&normalize_path(&cwd.join(&out))) == path_abs {
            continue;
        }
        units.push(Unit {
            url: path.to_string_lossy().into_owned(),
            syntax: syntax_for(&path, indented),
            source: Source::File(path),
            target: Target::File(out),
        });
    }
    Ok(())
}

fn run(cli: Cli) -> ExitCode {
    // Gather the units. `--stdin` or the positional input is a single stdout
    // unit (redirected to a file by `-o` / a second positional); every `in:out`
    // pair is a file unit (a directory pair expands to one per file). Relative
    // imports resolve against the CONTAINING file's directory (the evaluator's
    // current_file_dir), like dart — the input's directory is deliberately NOT
    // an implicit load path.
    let mut units: Vec<Unit> = Vec::new();
    // Standard input is read at most once, however many units name it.
    let mut stdin_cache: Option<Source> = None;
    let stdin_syntax = if cli.indented { Syntax::Sass } else { Syntax::Scss };
    let mut dir_entry = false;
    if let Some(out) = &cli.output {
        if out.is_dir() {
            return usage_error(&format!(
                "Directory {:?} may not be a positional arg.",
                out.to_string_lossy()
            ));
        }
    }
    match &cli.entry {
        Some(Entry::Stdin) => match stdin_source(&mut stdin_cache) {
            Ok(source) => units.push(Unit {
                source,
                url: "-".to_string(),
                syntax: stdin_syntax,
                target: Target::Stdout,
            }),
            Err(code) => return code,
        },
        // dart: a bare directory compiles in place (`sass dir` is `dir:dir`);
        // with an output it may not be a positional argument.
        Some(Entry::File(path)) if path.is_dir() => {
            if cli.output.is_some() {
                return usage_error(&format!(
                    "Directory {:?} may not be a positional arg.",
                    path.to_string_lossy()
                ));
            }
            if let Err(msg) = expand_dir(path, path, cli.indented, &mut units) {
                eprintln!("{msg}");
                return ExitCode::from(EXIT_IO);
            }
            dir_entry = true;
        }
        Some(Entry::File(path)) => units.push(Unit {
            url: path.to_string_lossy().into_owned(),
            syntax: syntax_for(path, cli.indented),
            source: Source::File(path.clone()),
            target: Target::Stdout,
        }),
        None => {}
    }
    for (src, dest) in &cli.pairs {
        if src == Path::new("-") {
            // `-:out.css`: standard input to a file, alongside other pairs.
            match stdin_source(&mut stdin_cache) {
                Ok(source) => units.push(Unit {
                    source,
                    url: "-".to_string(),
                    syntax: stdin_syntax,
                    target: Target::File(dest.clone()),
                }),
                Err(code) => return code,
            }
        } else if src.is_dir() {
            if let Err(msg) = expand_dir(src, dest, cli.indented, &mut units) {
                eprintln!("{msg}");
                return ExitCode::from(EXIT_IO);
            }
        } else {
            units.push(Unit {
                url: src.to_string_lossy().into_owned(),
                syntax: syntax_for(src, cli.indented),
                source: Source::File(src.clone()),
                target: Target::File(dest.clone()),
            });
        }
    }
    // dart keeps every source — explicit pairs and directory-expanded files
    // alike — in one path-keyed map, so a file named twice (`src:out` plus
    // `src/a.scss:a.css`) compiles once, to the destination named last.
    let units = {
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut keys: Vec<PathBuf> = Vec::new();
        let mut kept: Vec<Unit> = Vec::new();
        for unit in units {
            let key = match &unit.source {
                Source::File(path) => path_key(&normalize_path(&cwd.join(path))),
                Source::Text(_) | Source::InvalidUtf8 => PathBuf::from("-"),
            };
            match keys.iter().position(|k| *k == key) {
                Some(i) => kept[i].target = unit.target,
                None => {
                    keys.push(key);
                    kept.push(unit);
                }
            }
        }
        kept
    };
    let mut units = units;
    if let Some(output) = &cli.output {
        // `parse_args` guarantees a single stdout unit here; redirect it.
        if let Some(unit) = units.first_mut() {
            unit.target = Target::File(output.clone());
        }
    }
    if units.is_empty() {
        if cli.pairs.is_empty() && !dir_entry {
            return usage_error("no input file (pass a path, an <in>:<out> pair, or --stdin)");
        }
        return ExitCode::SUCCESS;
    }

    let shared = Shared {
        load_paths: cli.load_paths.clone(),
        style: cli.style,
        unicode: !cli.no_unicode,
        charset: cli.charset,
        quiet: cli.quiet,
        quiet_deps: cli.quiet_deps,
        no_css: cli.no_css,
        embed_sources: cli.embed_sources,
        embed_source_map: cli.embed_source_map,
        source_map_urls: cli.source_map_urls.unwrap_or(SourceMapUrls::Relative),
        // dart: source maps default ON when writing to a file; an explicit
        // `--embed-source-map` implies one.
        file_source_map: cli.source_map.unwrap_or(true) || cli.embed_source_map,
        file_error_css: cli.error_css.unwrap_or(true),
        stdout_error_css: cli.error_css == Some(true),
    };

    // Throughput mode: recompile the whole input set in-process N times, timing
    // only the compile calls (sources are read once), and report ms/compile +
    // compiles/sec to stderr. The CSS is still emitted once unless `--no-css`.
    if let Some(n) = cli.loop_n {
        return run_loop(&units, &shared, n);
    }

    let jobs = cli.jobs.unwrap_or_else(default_jobs);
    let outcomes = compile_all(&units, &shared, jobs, cli.stop_on_error);

    // Report in command-line order: each unit's diagnostics, then its CSS.
    let mut worst = Status::Ok;
    {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        // dart separates one unit's diagnostics from the next with a blank
        // line; a warning block already ends in one, an error does not.
        let mut stderr_ends_blank = true;
        // A consumer that closed the pipe early is not an error (dart exits 0
        // too, as does every Unix filter); any other stdout failure is.
        let mut stdout_ok = true;
        for outcome in outcomes.into_iter().flatten() {
            worst = worse(worst, outcome.status);
            if !outcome.stderr.is_empty() {
                if !stderr_ends_blank {
                    let _ = stderr.write_all(b"\n");
                }
                let _ = stderr.write_all(outcome.stderr.as_bytes());
                stderr_ends_blank = outcome.stderr.ends_with("\n\n");
            }
            if stdout_ok && !outcome.stdout.is_empty() {
                if let Err(e) = stdout.write_all(outcome.stdout.as_bytes()) {
                    stdout_ok = false;
                    if e.kind() != std::io::ErrorKind::BrokenPipe {
                        let _ = writeln!(stderr, "error: cannot write to stdout: {e}");
                        worst = Status::IoError;
                    }
                }
            }
        }
        if stdout_ok {
            if let Err(e) = stdout.flush() {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    let _ = writeln!(stderr, "error: cannot write to stdout: {e}");
                    worst = Status::IoError;
                }
            }
        }
    }
    match worst {
        Status::Ok => ExitCode::SUCCESS,
        Status::CompileError => ExitCode::from(EXIT_COMPILE),
        Status::IoError => ExitCode::from(EXIT_IO),
    }
}

/// The more severe of two statuses (I/O error > compile error > ok).
fn worse(a: Status, b: Status) -> Status {
    match (a, b) {
        (Status::IoError, _) | (_, Status::IoError) => Status::IoError,
        (Status::CompileError, _) | (_, Status::CompileError) => Status::CompileError,
        _ => Status::Ok,
    }
}

/// Compile every unit, up to `jobs` at a time, returning one slot per unit in
/// input order. A slot is `None` when `stop_on_error` skipped the unit.
fn compile_all(units: &[Unit], shared: &Shared, jobs: usize, stop_on_error: bool) -> Vec<Option<Outcome>> {
    let n = units.len();
    if jobs <= 1 || n <= 1 {
        let mut results = Vec::with_capacity(n);
        for unit in units {
            let outcome = compile_unit(unit, shared);
            let failed = outcome.status != Status::Ok;
            results.push(Some(outcome));
            if failed && stop_on_error {
                break;
            }
        }
        results.resize_with(n, || None);
        return results;
    }
    // A shared counter hands out the next unit; each worker stores its result
    // in that unit's slot, so ordering is recovered without a channel.
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let slots: Vec<Mutex<Option<Outcome>>> = (0..n).map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(n) {
            scope.spawn(|| loop {
                if stop_on_error && failed.load(Ordering::Relaxed) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                // Re-check after claiming: a failure another worker reported
                // between the check above and the claim must not start this
                // unit either (its slot stays `None`, "not run").
                if stop_on_error && failed.load(Ordering::Relaxed) {
                    break;
                }
                let outcome = compile_unit(&units[i], shared);
                if outcome.status != Status::Ok {
                    failed.store(true, Ordering::Relaxed);
                }
                *slots[i].lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| slot.into_inner().unwrap_or_else(|p| p.into_inner()))
        .collect()
}

/// Read a unit's source text, or the outcome that reports why it could not be.
fn read_source<'u>(unit: &'u Unit) -> Result<Cow<'u, str>, Outcome> {
    match &unit.source {
        Source::Text(s) => Ok(Cow::Borrowed(s.as_str())),
        Source::InvalidUtf8 => Err(Outcome::failed(
            Status::CompileError,
            "Error: Invalid UTF-8.\n".to_string(),
        )),
        Source::File(path) => match std::fs::read_to_string(path) {
            Ok(s) => Ok(Cow::Owned(s)),
            Err(e) if is_invalid_utf8(&e) => Err(Outcome::failed(
                Status::CompileError,
                "Error: Invalid UTF-8.\n".to_string(),
            )),
            Err(_) => Err(Outcome::failed(
                Status::IoError,
                format!("Error reading {}: Cannot open file.\n", path.display()),
            )),
        },
    }
}

/// Build the per-unit options. A helper fn (not a closure) so the returned
/// `Options` can borrow `unit`/`shared` for the caller's lifetime.
fn options_for<'a>(
    unit: &'a Unit,
    shared: &'a Shared,
    importer: &'a FsImporter,
    unicode: bool,
    warn: WarnHandler,
) -> Options<'a> {
    let opts = Options::default()
        .with_style(shared.style)
        .with_syntax(unit.syntax)
        .with_importer(importer)
        .with_url(&unit.url)
        .with_unicode(unicode)
        .with_charset(shared.charset)
        .with_source_map_include_sources(shared.embed_sources)
        .with_warn_handler(warn);
    if shared.quiet_deps {
        // dart's `--quiet-deps`: compiler warnings from dependencies —
        // stylesheets the importer reached through a load path, and whatever
        // those load relatively — are dropped inside the compiler, ahead of its
        // repetition cap. A dependency's own `@warn` still prints.
        opts.with_quiet_deps(importer.dependencies())
    } else {
        opts
    }
}

/// A warning handler that appends each diagnostic to `buf` exactly as the
/// library's default logger would print it (the block plus a blank line),
/// honouring `--quiet` and `--quiet-deps`.
fn buffered_warn_handler(shared: &Shared, buf: Rc<RefCell<String>>) -> WarnHandler {
    let quiet = shared.quiet;
    Rc::new(move |ev: &WarnEvent<'_>| {
        // `--quiet` drops everything (`--quiet-deps` is applied inside the
        // compiler, see `options_for`).
        if quiet {
            return;
        }
        let mut b = buf.borrow_mut();
        b.push_str(ev.formatted);
        b.push('\n');
    })
}

fn silent_warn_handler() -> WarnHandler {
    Rc::new(|_: &WarnEvent<'_>| {})
}

/// Compile one unit end to end: read, compile (with a map when wanted), write
/// or buffer the CSS, and on failure render the diagnostic (plus dart's error
/// stylesheet where it applies).
fn compile_unit(unit: &Unit, shared: &Shared) -> Outcome {
    match read_source(unit) {
        Ok(source) => compile_source(unit, &source, shared),
        Err(mut outcome) => {
            // A read failure never reaches the compiler, but dart treats
            // invalid UTF-8 as a compile error all the same: the file target
            // gets the error stylesheet (or, with `--no-error-css`, loses any
            // stale CSS). There is no source span to render here.
            if outcome.status == Status::CompileError {
                let message = outcome.stderr.trim_end_matches('\n').to_string();
                finish_compile_error(unit, shared, &message, &message, &mut outcome);
            }
            outcome
        }
    }
}

/// Wrap up a compile error for `unit`: write dart's error stylesheet where it
/// applies — or, with error CSS off, remove a stale output file so nothing
/// consumes the CSS of an earlier successful build (dart deletes it too; the
/// `.map`, if any, is left alone). `rendered` is the diagnostic as printed to
/// the terminal, `ascii` the same rendered with the ASCII glyph set.
fn finish_compile_error(unit: &Unit, shared: &Shared, rendered: &str, ascii: &str, outcome: &mut Outcome) {
    // `--no-css` means no output-side effects at all: no error stylesheet, and
    // an existing output is left exactly as it was.
    if shared.no_css {
        return;
    }
    match &unit.target {
        Target::Stdout => {
            if shared.stdout_error_css {
                outcome.stdout = error_css(rendered, ascii);
            }
        }
        Target::File(output) => {
            if shared.file_error_css {
                if let Err(msg) = write_file(output, error_css(rendered, ascii).as_bytes()) {
                    outcome.stderr.push_str(&msg);
                    outcome.stderr.push('\n');
                    outcome.status = Status::IoError;
                }
            } else if let Err(e) = std::fs::remove_file(output) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    outcome
                        .stderr
                        .push_str(&format!("error: cannot remove {}: {e}\n", output.display()));
                    outcome.status = Status::IoError;
                }
            }
        }
    }
}

/// [`compile_unit`] for an already-read `source`.
fn compile_source(unit: &Unit, source: &str, shared: &Shared) -> Outcome {
    let importer = FsImporter::new(shared.load_paths.clone());
    let warnings = Rc::new(RefCell::new(String::new()));
    let opts = options_for(
        unit,
        shared,
        &importer,
        shared.unicode,
        buffered_warn_handler(shared, Rc::clone(&warnings)),
    );

    // No map when nothing will be written (`--no-css`): mapping is not free.
    let want_map = !shared.no_css
        && match &unit.target {
            Target::File(_) => shared.file_source_map,
            Target::Stdout => shared.embed_source_map,
        };
    // `(css, map)`: the map is `Some` only when one was requested.
    let compiled: Result<(String, Option<SourceMap>), sasso::Error> = if want_map {
        compile_with_source_map(source, &opts).map(|r| (r.css, Some(r.source_map)))
    } else {
        compile(source, &opts).map(|css| (css, None))
    };
    // A stdin unit's text: dart records it in the map as a `data:` URI.
    let stdin_text = match &unit.source {
        Source::Text(text) => Some(text.as_str()),
        Source::File(_) | Source::InvalidUtf8 => None,
    };

    let mut outcome = Outcome {
        stderr: std::mem::take(&mut *warnings.borrow_mut()),
        stdout: String::new(),
        status: Status::Ok,
    };
    match compiled {
        // `--no-css`: the compile (and its diagnostics) was all that was wanted.
        Ok(_) if shared.no_css => {}
        Ok((css, map)) => match &unit.target {
            Target::Stdout => {
                outcome.stdout = match map {
                    Some(map) => {
                        // dart embeds a stdout map with absolute `file://` sources and
                        // no `file` field.
                        let sources = adjust_sources(
                            &map.sources,
                            &unit.url,
                            stdin_text,
                            Path::new(""),
                            SourceMapUrls::Absolute,
                        );
                        let json =
                            dart_map_json(None, &sources, map.sources_content.as_deref(), &map.mappings);
                        append_source_map_footer(&css, &data_uri(&json), shared.style)
                    }
                    // The library API omits the trailing newline dart-sass's CLI
                    // writes to non-empty output (empty output stays empty).
                    None if css.is_empty() => css,
                    None => format!("{css}\n"),
                };
            }
            Target::File(output) => {
                if let Err(msg) = write_css_file(output, &css, map.as_ref(), &unit.url, stdin_text, shared) {
                    outcome.stderr.push_str(&msg);
                    outcome.stderr.push('\n');
                    outcome.status = Status::IoError;
                }
            }
        },
        Err(err) => {
            let rendered = err.to_string();
            outcome.stderr.push_str(&rendered);
            outcome.stderr.push('\n');
            outcome.status = Status::CompileError;
            let want_error_css = match &unit.target {
                Target::File(_) => shared.file_error_css,
                Target::Stdout => shared.stdout_error_css,
            };
            // The comment block always uses the ASCII glyph set (dart avoids
            // non-ASCII there so the file needs no @charset); re-render the
            // error that way when the terminal rendering was Unicode.
            let ascii = if want_error_css && shared.unicode {
                let ascii_opts = options_for(unit, shared, &importer, false, silent_warn_handler());
                compile(source, &ascii_opts)
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| rendered.clone())
            } else {
                rendered.clone()
            };
            finish_compile_error(unit, shared, &rendered, &ascii, &mut outcome);
        }
    }
    outcome
}

/// Throughput mode (`--loop N`): a warm/correctness pass reports diagnostics
/// once, then the whole set is recompiled N times with a silent logger.
fn run_loop(units: &[Unit], shared: &Shared, n: u32) -> ExitCode {
    let mut sources = Vec::with_capacity(units.len());
    for unit in units {
        // Read once; the warm pass and the timed loop share the text.
        let source = match read_source(unit) {
            Ok(s) => s.into_owned(),
            Err(outcome) => {
                eprint!("{}", outcome.stderr);
                return ExitCode::from(if outcome.status == Status::IoError {
                    EXIT_IO
                } else {
                    EXIT_COMPILE
                });
            }
        };
        let outcome = compile_source(unit, &source, shared);
        eprint!("{}", outcome.stderr);
        if outcome.status != Status::Ok {
            return ExitCode::from(if outcome.status == Status::IoError {
                EXIT_IO
            } else {
                EXIT_COMPILE
            });
        }
        sources.push(source);
    }
    let importer = FsImporter::new(shared.load_paths.clone());
    let mut last = String::new();
    let start = Instant::now();
    for _ in 0..n {
        for (unit, source) in units.iter().zip(&sources) {
            let opts = options_for(unit, shared, &importer, shared.unicode, silent_warn_handler());
            match compile(source, &opts) {
                Ok(css) => last = css,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::from(EXIT_COMPILE);
                }
            }
        }
    }
    let elapsed = start.elapsed();
    let per = elapsed.as_secs_f64() * 1000.0 / f64::from(n);
    let per_sec = if per > 0.0 { 1000.0 / per } else { f64::INFINITY };
    eprintln!("sasso: {n} compiles in {elapsed:.3?} => {per:.3} ms/compile, {per_sec:.1} compiles/sec");
    if !shared.no_css && !last.is_empty() {
        // Match the CLI's single trailing newline (the library API omits it;
        // dart-sass emits nothing at all for empty output).
        println!("{last}");
    }
    ExitCode::SUCCESS
}

/// Standard input as a unit source, read on first use and handed out again
/// afterwards (a `-` entry and a `-:out` pair may both name it). Invalid
/// UTF-8 becomes [`Source::InvalidUtf8`] so the unit fails like a file would
/// (dart itself crashes on this); an I/O failure is reported here and mapped
/// to the exit code the caller should return with.
fn stdin_source(cache: &mut Option<Source>) -> Result<Source, ExitCode> {
    if let Some(source) = cache {
        return Ok(source.clone());
    }
    let source = match read_stdin() {
        Ok(text) => Source::Text(text),
        Err(e) if is_invalid_utf8(&e) => Source::InvalidUtf8,
        Err(e) => {
            eprintln!("error: failed to read stdin: {e}");
            return Err(ExitCode::from(EXIT_IO));
        }
    };
    *cache = Some(source.clone());
    Ok(source)
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

// ---------------------------------------------------------------------------
// File output
// ---------------------------------------------------------------------------

/// Write `bytes` to `path`, creating missing parent directories like dart-sass.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("error: cannot create {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(path, bytes).map_err(|e| format!("error: cannot write {}: {e}", path.display()))
}

/// Write compiled CSS to `output`. With a map: either a `<output>.map` sidecar
/// plus a `sourceMappingURL` footer, or (`--embed-source-map`) the map inlined
/// as a `data:` URI in the footer — byte-for-byte what dart-sass writes.
/// `input_url` is the input path as given on the command line.
fn write_css_file(
    output: &Path,
    css: &str,
    map: Option<&SourceMap>,
    input_url: &str,
    stdin_text: Option<&str>,
    shared: &Shared,
) -> Result<(), String> {
    match map {
        Some(map) => {
            // The sidecar lives next to the CSS as `<output>.map`; the footer URL
            // is its basename (dart writes e.g. `out.css.map` and footers
            // `out.css.map`). An embedded map keeps the same relative sources.
            let map_path = append_ext(output, "map");
            // Both the map's `file` and the footer's URL are URLs, not paths:
            // dart percent-encodes the basename (`out%20file%231.css`).
            let file = encode_url_segment(&path_basename(output));
            let sources = adjust_sources(
                &map.sources,
                input_url,
                stdin_text,
                &map_path,
                shared.source_map_urls,
            );
            let map_json = dart_map_json(
                Some(&file),
                &sources,
                map.sources_content.as_deref(),
                &map.mappings,
            );
            if shared.embed_source_map {
                let css = append_source_map_footer(css, &data_uri(&map_json), shared.style);
                write_file(output, css.as_bytes())
            } else {
                // The map goes first (dart's order too): if it cannot be
                // written, no CSS pointing at a missing map gets published.
                let map_url = encode_url_segment(&path_basename(&map_path));
                let css = append_source_map_footer(css, &map_url, shared.style);
                write_file(&map_path, map_json.as_bytes())?;
                write_file(output, css.as_bytes())
            }
        }
        None => {
            // dart terminates a CSS FILE with exactly one newline, an empty
            // stylesheet included (`\n` alone); only stdout gets nothing for
            // empty output. The library API omits the newline.
            let css = format!("{css}\n");
            write_file(output, css.as_bytes())
        }
    }
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

/// dart's error stylesheet (`--error-css`): the ASCII-rendered diagnostic as a
/// leading comment, then a `body::before` whose `content` shows the full
/// diagnostic in the browser. `rendered` is the error as printed to the
/// terminal (Unicode glyphs unless `--no-unicode`); `ascii` the same error
/// rendered with the ASCII glyph set.
fn error_css(rendered: &str, ascii: &str) -> String {
    let ascii = ascii.trim_end_matches('\n');
    let rendered = rendered.trim_end_matches('\n');
    // `*/` inside the message would close the comment; dart swaps the slash
    // for U+2215 DIVISION SLASH.
    let comment = ascii.replace("*/", "*\u{2215}").replace('\n', "\n * ");
    let mut content = String::with_capacity(rendered.len() + 32);
    for c in rendered.chars() {
        match c {
            '"' => content.push_str("\\\""),
            '\\' => content.push_str("\\\\"),
            '\n' => content.push_str("\\a "),
            c if !c.is_ascii() => content.push_str(&format!("\\{:x} ", c as u32)),
            c => content.push(c),
        }
    }
    format!(
        "/* {comment} */\n\n\
         body::before {{\n  \
         font-family: \"Source Code Pro\", \"SF Mono\", Monaco, Inconsolata, \"Fira Mono\",\n      \
         \"Droid Sans Mono\", monospace, monospace;\n  \
         white-space: pre;\n  \
         display: block;\n  \
         padding: 1em;\n  \
         margin-bottom: 1em;\n  \
         border-bottom: 2px solid black;\n  \
         content: \"{content}\";\n\
         }}\n"
    )
}

/// The inline form of a source map (`--embed-source-map`): dart's
/// `Uri.dataFromString` output, i.e. `data:application/json;charset=utf-8,`
/// followed by the JSON with every byte outside the URI "uric" set
/// (unreserved + sub-delims + `;/?:@&=+$,`) percent-encoded as uppercase `%XX`.
fn data_uri(json: &str) -> String {
    format!("data:application/json;charset=utf-8,{}", uric_encode(json))
}

/// Percent-encode `s` the way dart's `Uri.dataFromString` does: every byte
/// outside the URI "uric" set (unreserved + sub-delims + `;/?:@&=+$,`) becomes
/// uppercase `%XX`.
fn uric_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'*'
                    | b'\''
                    | b'('
                    | b')'
                    | b';'
                    | b'/'
                    | b'?'
                    | b':'
                    | b'@'
                    | b'&'
                    | b'='
                    | b'+'
                    | b'$'
                    | b','
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

/// Serialize the map JSON in dart-sass's exact field order:
/// `version, sourceRoot:"", sources, names:[], mappings[, file][, sourcesContent]`.
/// Hand-built (zero-dep) with the same string escaping dart uses. `file` is
/// omitted for a map embedded in stdout output, as dart does.
fn dart_map_json(
    file: Option<&str>,
    sources: &[String],
    contents: Option<&[String]>,
    mappings: &str,
) -> String {
    let mut s = String::from("{\"version\":3,\"sourceRoot\":\"\",\"sources\":[");
    for (i, src) in sources.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json_str(src, &mut s);
    }
    s.push_str("],\"names\":[],\"mappings\":");
    json_str(mappings, &mut s);
    if let Some(file) = file {
        s.push_str(",\"file\":");
        json_str(file, &mut s);
    }
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
/// `input_url`); we adjust each one the same way dart does. A stdin entry has
/// no path: dart records its text as a `data:;charset=utf-8,…` URI.
fn adjust_sources(
    sources: &[String],
    input_url: &str,
    stdin_text: Option<&str>,
    map_path: &Path,
    mode: SourceMapUrls,
) -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    sources
        .iter()
        .map(|src| {
            // The entry source equals `input_url`; imports carry their own paths.
            let is_entry = src == "stdin" || src == input_url;
            if let (true, Some(text)) = (is_entry, stdin_text) {
                return format!("data:;charset=utf-8,{}", uric_encode(text));
            }
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
/// A path reduced to the key the two directory-relationship tests compare —
/// "is this file inside the destination" and "is its destination itself".
///
/// On Windows the filesystem is case-insensitive and dart canonicalizes each
/// part to lowercase (see `importer::absolute_normalized`), so `Src:src/css`
/// names one directory two ways and the tests have to see that. Everywhere
/// else the path is compared as written, again like dart — which case-folds
/// for no other style, not even on a case-insensitive macOS volume. The key
/// is lexical either way: no `realpath`, so a symlink is not resolved.
#[cfg(windows)]
fn path_key(p: &Path) -> PathBuf {
    PathBuf::from(p.to_string_lossy().to_lowercase())
}

/// See the Windows variant above: elsewhere the path is its own key.
#[cfg(not(windows))]
fn path_key(p: &Path) -> PathBuf {
    p.to_path_buf()
}

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
            Component::RootDir => {}
            // Windows prefixes, spelled as dart's `Uri.file` does: a drive is
            // the first segment (`file:///C:/…`), a UNC share is the authority
            // (`file://server/share/…`); verbatim (`\\?\`) forms drop the marker.
            Component::Prefix(prefix) => {
                use std::path::Prefix;
                match prefix.kind() {
                    Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                        s.push('/');
                        s.push(letter as char);
                        s.push(':');
                    }
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                        s.push_str(&encode_url_segment(&server.to_string_lossy()));
                        s.push('/');
                        s.push_str(&encode_url_segment(&share.to_string_lossy()));
                    }
                    Prefix::Verbatim(rest) | Prefix::DeviceNS(rest) => {
                        s.push('/');
                        s.push_str(&encode_url_segment(&rest.to_string_lossy()));
                    }
                }
            }
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
