mod decode;
mod encode;
mod loudness;
mod resample;
mod scan;
mod transcode;

use std::num::NonZeroUsize;
use std::process::exit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use transcode::Settings;

mod flags {
    use std::num::NonZeroUsize;
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
            /// Worker threads, each transcoding one file at a time [default: CPU count]
            optional -t,--threads n: NonZeroUsize
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

fn main() {
    // Checked before parsing so it works without the required arguments.
    if std::env::args().skip(1).any(|a| a == "-V" || a == "--version") {
        println!("trans-music {}", env!("CARGO_PKG_VERSION"));
        exit(0)
    }
    let opts = flags::TransMusic::from_env_or_exit();
    let settings = Settings {
        src: opts.src,
        dst: opts.dst,
        bitrate_kbps: opts.bitrate,
        normalize: !opts.no_normalize,
        loudness_lufs: opts.loudness.unwrap_or(-16.0),
        true_peak_dbtp: opts.true_peak.unwrap_or(-2.0),
        keep_art: !opts.strip_art,
    };
    let plan = match scan::plan(&settings, opts.force) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: scanning {}: {e}", settings.src.display());
            exit(1)
        }
    };
    let quiet = opts.quiet;
    let verbose = opts.verbose && !quiet;

    if !quiet {
        let exts: Vec<_> = plan.all_exts.iter().map(String::as_str).collect();
        println!("extensions in source: {}", exts.join(" "));
        println!(
            "{} audio files, {} up to date, {} to transcode, {} orphans in destination",
            plan.skipped.len() + plan.todo.len(),
            plan.skipped.len(),
            plan.todo.len(),
            plan.orphans.len()
        );
    }
    for job in &plan.conflicts {
        eprintln!("warning: {}: skipped, another source maps to {}", job.display(), settings.output(job).display());
    }
    if verbose {
        for job in &plan.skipped {
            println!("= {}", job.display());
        }
    }
    if opts.dry_run {
        for job in &plan.todo {
            println!("+ {}", job.display());
        }
        if opts.prune {
            for p in &plan.orphans {
                println!("- {}", p.display());
            }
        }
        return;
    }

    let threads = opts.threads.or(std::thread::available_parallelism().ok()).map_or(1, NonZeroUsize::get);
    let total = plan.todo.len();
    let next = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    let started = Instant::now();

    std::thread::scope(|scope| {
        for _ in 0..threads.min(total) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = plan.todo.get(i) else { break };
                    if !quiet {
                        println!("[{}/{total}] + {}", i + 1, job.display());
                    }
                    if let Err(e) = transcode::transcode(job, &settings) {
                        eprintln!("error: {}: {e}", job.display());
                        failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    let failures = failures.into_inner();
    if opts.prune {
        for p in &plan.orphans {
            if !quiet {
                println!("- {}", p.display());
            }
            if let Err(e) = std::fs::remove_file(p) {
                eprintln!("error: removing {}: {e}", p.display());
            }
        }
        scan::remove_empty_dirs(&settings.dst);
    }
    if !quiet {
        println!("done: {} transcoded, {} failed, {:.0?}", total - failures, failures, started.elapsed());
    }
    if failures > 0 {
        exit(1)
    }
}
