mod cli;
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
    todo!("transcode")
}
