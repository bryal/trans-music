//! Walk the source library and decide which files need work.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Extensions (lower case, without dot) treated as audio input.
pub const AUDIO_EXTS: &[&str] = &[
    "flac", "m4a", "mp4", "mp3", "ogg", "oga", "opus", "wav", "wave", "aac", "alac", "mka",
];

/// One source file and where its transcode goes.
#[derive(Debug, Clone)]
pub struct Job {
    /// Path relative to the library root, shown in progress output.
    pub rel: PathBuf,
    pub src: PathBuf,
    pub dst: PathBuf,
}

pub struct Plan {
    /// Every extension seen under `src`, for the user's information.
    pub all_exts: BTreeSet<String>,
    /// Audio files that are up to date and will be skipped.
    pub skipped: Vec<Job>,
    /// Audio files to transcode.
    pub todo: Vec<Job>,
    /// Files under `dst` with no corresponding source file.
    pub orphans: Vec<PathBuf>,
    /// Sources skipped because an earlier source maps to the same output,
    /// e.g. `x.flac` next to `x.mp3`.
    pub conflicts: Vec<Job>,
}

pub fn plan(src_root: &Path, dst_root: &Path, force: bool) -> io::Result<Plan> {
    let mut all_exts = BTreeSet::new();
    let mut skipped = Vec::new();
    let mut todo = Vec::new();
    let mut conflicts = Vec::new();
    let mut expected: BTreeSet<PathBuf> = BTreeSet::new();
    // A destination inside the source must not be scanned as input.
    let dst_in_src = fs::canonicalize(dst_root)
        .ok()
        .zip(fs::canonicalize(src_root).ok())
        .and_then(|(d, s)| d.strip_prefix(s).ok().map(Path::to_path_buf));

    for rel in walk(src_root)? {
        if dst_in_src.as_ref().is_some_and(|d| rel.starts_with(d)) {
            continue;
        }
        let ext = extension(&rel);
        all_exts.insert(ext.clone());
        if !AUDIO_EXTS.contains(&ext.as_str()) {
            continue;
        }
        let dst = dst_root.join(rel.with_extension("opus"));
        let job = Job { src: src_root.join(&rel), dst, rel };
        if !expected.insert(job.dst.clone()) {
            conflicts.push(job);
        } else if force || needs_transcode(&job) {
            todo.push(job);
        } else {
            skipped.push(job);
        }
    }

    let orphans = match walk(dst_root) {
        Ok(rels) => rels
            .into_iter()
            .map(|r| dst_root.join(r))
            .filter(|p| !expected.contains(p))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };

    Ok(Plan { all_exts, skipped, todo, orphans, conflicts })
}

/// Transcode when the output is missing or older than the source.
fn needs_transcode(job: &Job) -> bool {
    let Ok(dst_meta) = fs::metadata(&job.dst) else {
        return true;
    };
    match (fs::metadata(&job.src).and_then(|m| m.modified()), dst_meta.modified()) {
        (Ok(src_time), Ok(dst_time)) => dst_time < src_time,
        _ => false,
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// All regular files under `root`, as paths relative to `root`, sorted.
pub fn walk(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel_dir) = stack.pop() {
        for entry in fs::read_dir(root.join(&rel_dir))? {
            let entry = entry?;
            let rel = rel_dir.join(entry.file_name());
            let kind = entry.file_type()?;
            // Follow symlinked directories, like the legacy tool did.
            if kind.is_dir() || (kind.is_symlink() && root.join(&rel).is_dir()) {
                stack.push(rel);
            } else {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Remove directories under `root` left empty by pruning; `root` itself stays.
pub fn remove_empty_dirs(root: &Path) {
    fn prune(dir: &Path) -> bool {
        let Ok(entries) = fs::read_dir(dir) else { return false };
        let mut empty = true;
        for entry in entries.flatten() {
            let path = entry.path();
            let gone = path.is_dir() && !path.is_symlink() && prune(&path) && fs::remove_dir(&path).is_ok();
            empty &= gone;
        }
        empty
    }
    prune(root);
}
