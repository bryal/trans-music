//! Command line parsing. Hand-written help on top of `lexopt`.

use std::path::PathBuf;
use std::process::exit;

use lexopt::prelude::*;

pub const HELP: &str = "\
trans-music: mirror a music library as loudness-normalized Opus

Usage: trans-music [OPTIONS] <SRC> <DST>

Every audio file under SRC is transcoded to DST/<same path>.opus, unless the
output already exists and is newer than the source.

Options:
  -b, --bitrate <KBPS>     Opus bitrate in kbit/s                  [default: 120]
  -l, --loudness <LUFS>    Integrated loudness target               [default: -16]
  -p, --true-peak <DBTP>   True-peak ceiling; gain is reduced to
                           respect it                               [default: -2]
  -N, --no-normalize       Skip loudness normalization
  -j, --jobs <N>           Files transcoded in parallel     [default: CPU count]
  -f, --force              Re-transcode even if the output is up to date
  -n, --dry-run            List what would be done, change nothing
      --prune              Delete files in DST with no counterpart in SRC
      --strip-art          Do not copy embedded cover art
  -q, --quiet              Only report errors and warnings
  -v, --verbose            Report every file, including skipped ones
  -h, --help               Show this help
  -V, --version            Show version
";

#[derive(Debug, Clone)]
pub struct Opts {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub bitrate_kbps: u32,
    pub loudness_lufs: f64,
    pub true_peak_dbtp: f64,
    pub normalize: bool,
    pub jobs: usize,
    pub force: bool,
    pub dry_run: bool,
    pub prune: bool,
    pub strip_art: bool,
    pub verbosity: Verbosity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

pub fn parse() -> Opts {
    match try_parse() {
        Ok(opts) => opts,
        Err(e) => {
            eprintln!("error: {e}\n\n{HELP}");
            exit(2)
        }
    }
}

fn try_parse() -> Result<Opts, lexopt::Error> {
    let mut bitrate_kbps = 120;
    let mut loudness_lufs = -16.0;
    let mut true_peak_dbtp = -2.0;
    let mut normalize = true;
    let mut jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut force = false;
    let mut dry_run = false;
    let mut prune = false;
    let mut strip_art = false;
    let mut verbosity = Verbosity::Normal;
    let mut positional: Vec<PathBuf> = Vec::new();

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Short('b') | Long("bitrate") => bitrate_kbps = parser.value()?.parse()?,
            Short('l') | Long("loudness") => loudness_lufs = parser.value()?.parse()?,
            Short('p') | Long("true-peak") => true_peak_dbtp = parser.value()?.parse()?,
            Short('N') | Long("no-normalize") => normalize = false,
            Short('j') | Long("jobs") => jobs = parser.value()?.parse()?,
            Short('f') | Long("force") => force = true,
            Short('n') | Long("dry-run") => dry_run = true,
            Long("prune") => prune = true,
            Long("strip-art") => strip_art = true,
            Short('q') | Long("quiet") => verbosity = Verbosity::Quiet,
            Short('v') | Long("verbose") => verbosity = Verbosity::Verbose,
            Short('h') | Long("help") => {
                print!("{HELP}");
                exit(0)
            }
            Short('V') | Long("version") => {
                println!("trans-music {}", env!("CARGO_PKG_VERSION"));
                exit(0)
            }
            Value(v) => positional.push(v.into()),
            _ => return Err(arg.unexpected()),
        }
    }

    let [src, dst]: [PathBuf; 2] = positional
        .try_into()
        .map_err(|_| lexopt::Error::from("expected exactly two paths: <SRC> <DST>"))?;
    if !(6..=510).contains(&bitrate_kbps) {
        return Err("--bitrate must be between 6 and 510 kbit/s".into());
    }
    if jobs == 0 {
        return Err("--jobs must be positive".into());
    }
    Ok(Opts {
        src,
        dst,
        bitrate_kbps,
        loudness_lufs,
        true_peak_dbtp,
        normalize,
        jobs: jobs.max(1),
        force,
        dry_run,
        prune,
        strip_art,
        verbosity,
    })
}
