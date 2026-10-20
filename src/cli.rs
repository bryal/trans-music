//! Command line parsing. The `xflags!` grammar generates `flags::TransMusic`.

use std::path::PathBuf;
use std::process::exit;

mod flags {
    use std::path::PathBuf;

    xflags::xflags! {
        /// Mirror a music library as loudness-normalized Opus.
        ///
        /// Every audio file under <src> is transcoded to <dst>/<same path>.opus,
        /// unless the output already exists and is newer than the source.
        cmd trans-music {
            /// Library to read
            required src: PathBuf
            /// Mirror to write
            required dst: PathBuf
            /// Opus bitrate in kbit/s
            required -b,--bitrate kbps: u32
            /// Integrated loudness target in LUFS [default: -16]
            optional -l,--loudness lufs: f64
            /// True-peak ceiling in dBTP; the gain is reduced to respect it [default: -2]
            optional -p,--true-peak dbtp: f64
            /// Skip loudness normalization
            optional -N,--no-normalize
            /// Files transcoded in parallel [default: CPU count]
            optional -j,--jobs n: usize
            /// Re-transcode even if the output is up to date
            optional -f,--force
            /// List what would be done, change nothing
            optional -n,--dry-run
            /// Delete files in <dst> with no counterpart in <src>
            optional --prune
            /// Do not copy embedded cover art
            optional --strip-art
            /// Only report errors and warnings
            optional -q,--quiet
            /// Report every file, including skipped ones
            optional -v,--verbose
        }
    }

}

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

/// Parses the command line, applying defaults and range checks. Exits with
/// a message on bad usage.
pub fn parse() -> Opts {
    // Checked before parsing so it works without the required paths.
    if std::env::args().skip(1).any(|a| a == "-V" || a == "--version") {
        println!("trans-music {}", env!("CARGO_PKG_VERSION"));
        exit(0)
    }
    let f = flags::TransMusic::from_env_or_exit();
    if !(6..=510).contains(&f.bitrate) {
        usage_error("--bitrate must be between 6 and 510 kbit/s");
    }
    let jobs = f.jobs.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    if jobs == 0 {
        usage_error("--jobs must be positive");
    }
    Opts {
        src: f.src,
        dst: f.dst,
        bitrate_kbps: f.bitrate,
        loudness_lufs: f.loudness.unwrap_or(-16.0),
        true_peak_dbtp: f.true_peak.unwrap_or(-2.0),
        normalize: !f.no_normalize,
        jobs,
        force: f.force,
        dry_run: f.dry_run,
        prune: f.prune,
        strip_art: f.strip_art,
        verbosity: match (f.quiet, f.verbose) {
            (true, _) => Verbosity::Quiet,
            (_, true) => Verbosity::Verbose,
            _ => Verbosity::Normal,
        },
    }
}

fn usage_error(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(2)
}
