mod cli;
mod decode;
mod loudness;
mod scan;

use std::process::exit;

use cli::Verbosity;

fn main() {
    let opts = cli::parse();
    let plan = match scan::plan(&opts.src, &opts.dst, opts.force) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: scanning {}: {e}", opts.src.display());
            exit(1)
        }
    };

    if opts.verbosity >= Verbosity::Normal {
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
    if opts.verbosity >= Verbosity::Verbose {
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
    // Temporary: measure loudness only, to validate decoding against ffmpeg.
    for job in &plan.todo {
        let mut reader = decode::AudioReader::open(&job.src).unwrap();
        let mut meter = loudness::Meter::new(reader.channels, reader.rate).unwrap();
        let (rate, channels) = (reader.rate, reader.channels);
        let mut frames = 0usize;
        while let Some(chunk) = reader.next_chunk().unwrap() {
            frames += chunk.len() / channels;
            meter.add(chunk).unwrap();
        }
        let m = meter.finish().unwrap();
        let g = m.gain_to(opts.loudness_lufs, opts.true_peak_dbtp);
        println!(
            "{}: {} Hz {} ch {:.2}s  I={:.1} LUFS  TP={:.1} dBTP  gain={:+.1} dB{}",
            job.rel.display(), rate, channels, frames as f64 / rate as f64,
            m.integrated_lufs, m.true_peak_dbtp, g.db, if g.limited { " (peak-limited)" } else { "" }
        );
    }
}
