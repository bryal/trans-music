mod cli;
mod decode;
mod encode;
mod loudness;
mod resample;
mod scan;
mod transcode;

use std::process::exit;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use cli::Verbosity;
use transcode::{Settings, Target};

fn main() {
    let opts = cli::parse();
    let plan = match scan::plan(&opts.src, &opts.dst, opts.force) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: scanning {}: {e}", opts.src.display());
            exit(1)
        }
    };
    let quiet = opts.verbosity == Verbosity::Quiet;
    let verbose = opts.verbosity == Verbosity::Verbose;

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
        bitrate_kbps: opts.bitrate_kbps,
        target: opts.normalize.then_some(Target {
            loudness_lufs: opts.loudness_lufs,
            true_peak_dbtp: opts.true_peak_dbtp,
        }),
        keep_art: !opts.strip_art,
    };
    let total = plan.todo.len();
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let started = Instant::now();

    std::thread::scope(|scope| {
        for _ in 0..opts.jobs.min(total.max(1)) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = plan.todo.get(i) else { break };
                    if !quiet {
                        println!("[{}/{total}] + {}", i + 1, job.rel.display());
                    }
                    match transcode::transcode(&job.src, &job.dst, &settings) {
                        Ok(report) => {
                            if let Some(w) = report.warning {
                                eprintln!("warning: {}: {w}", job.rel.display());
                            }
                            if let Some(g) = report.gain {
                                if g.limited {
                                    eprintln!(
                                        "warning: {}: gain capped at {:+.1} dB by the true-peak ceiling",
                                        job.rel.display(),
                                        g.db
                                    );
                                } else if verbose {
                                    println!("  {}: gain {:+.1} dB", job.rel.display(), g.db);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("error: {}: {e}", job.rel.display());
                            failures.lock().unwrap().push(job.rel.clone());
                        }
                    }
                }
            });
        }
    });

    let failures = failures.into_inner().unwrap();
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
            total - failures.len(),
            failures.len(),
            started.elapsed()
        );
    }
    if !failures.is_empty() {
        exit(1)
    }
}
