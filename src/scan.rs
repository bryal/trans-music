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
}

pub fn plan(src_root: &Path, dst_root: &Path, force: bool) -> io::Result<Plan> {
    let mut all_exts = BTreeSet::new();
    let mut skipped = Vec::new();
    let mut todo = Vec::new();
    let mut expected: BTreeSet<PathBuf> = BTreeSet::new();

    for rel in walk(src_root)? {
        let ext = extension(&rel);
        all_exts.insert(ext.clone());
        if !AUDIO_EXTS.contains(&ext.as_str()) {
            continue;
        }
        let dst = dst_root.join(rel.with_extension("opus"));
        expected.insert(dst.clone());
        let job = Job { src: src_root.join(&rel), dst, rel };
        if force || needs_transcode(&job) {
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

    Ok(Plan { all_exts, skipped, todo, orphans })
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
