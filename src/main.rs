mod cli;
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

fn main() {
    let opts = cli::parse();
    let plan = match scan::plan(&opts.src, &opts.dst, opts.force) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: scanning {}: {e}", opts.src.display());
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
        eprintln!("warning: {}: skipped, another source maps to {}", job.rel.display(), job.dst.display());
    }
    if verbose {
        for job in &plan.skipped {
            println!("= {}", job.rel.display());
        }
    }
    if opts.dry_run {
        for job in &plan.todo {
            println!("+ {}", job.rel.display());
        }
        if opts.prune {
            for p in &plan.orphans {
                println!("- {}", p.display());
            }
        }
        return;
    }

    let settings = Settings {
        bitrate_kbps: opts.bitrate,
        normalize: !opts.no_normalize,
        loudness_lufs: opts.loudness.unwrap_or(-16.0),
        true_peak_dbtp: opts.true_peak.unwrap_or(-2.0),
        keep_art: !opts.strip_art,
    };
    let jobs = opts.jobs.or(std::thread::available_parallelism().ok()).map_or(1, NonZeroUsize::get);
    let total = plan.todo.len();
    let next = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    let started = Instant::now();

    std::thread::scope(|scope| {
        for _ in 0..jobs.min(total) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = plan.todo.get(i) else { break };
                    if !quiet {
                        println!("[{}/{total}] + {}", i + 1, job.rel.display());
                    }
                    if let Err(e) = transcode::transcode(job, &settings) {
                        eprintln!("error: {}: {e}", job.rel.display());
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
        scan::remove_empty_dirs(&opts.dst);
    }
    if !quiet {
        println!(
            "done: {} transcoded, {} failed, {:.0?}",
            total - failures,
            failures,
            started.elapsed()
        );
    }
    if failures > 0 {
        exit(1)
    }
}
