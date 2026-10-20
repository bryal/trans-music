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
    let src_canon = fs::canonicalize(src_root)?;
    let dst_canon = fs::canonicalize(dst_root).ok();
    if dst_canon.as_ref() == Some(&src_canon) {
        return Err(io::Error::other("SRC and DST are the same directory"));
    }
    let mut all_exts = BTreeSet::new();
    let mut skipped = Vec::new();
    let mut todo = Vec::new();
    let mut conflicts = Vec::new();
    let mut expected: BTreeSet<PathBuf> = BTreeSet::new();

    // Each walk skips the other root: a DST inside SRC is not read as input,
    // and a SRC inside DST is not pruned.
    for rel in walk(src_root, true, dst_canon.as_deref())? {
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

    // Never follow links in DST: pruning must not reach outside it.
    let orphans = match walk(dst_root, false, Some(&src_canon)) {
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

/// Transcode when the output is missing or older than the source. An
/// unreadable source is attempted so that the error gets reported.
fn needs_transcode(job: &Job) -> bool {
    let Ok(dst_time) = fs::metadata(&job.dst).and_then(|m| m.modified()) else {
        return true;
    };
    fs::metadata(&job.src).and_then(|m| m.modified()).ok().is_none_or(|src_time| dst_time < src_time)
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// All files under `root`, as paths relative to `root`, sorted, leaving out
/// the directory whose canonical path is `skip`. With `follow_links`,
/// symlinked directories are entered once each, so a link to an ancestor
/// cannot loop.
fn walk(root: &Path, follow_links: bool, skip: Option<&Path>) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel_dir) = stack.pop() {
        let dir = root.join(&rel_dir);
        let canon = fs::canonicalize(&dir)?;
        if Some(canon.as_path()) == skip || !seen.insert(canon) {
            continue;
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let rel = rel_dir.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() || (follow_links && kind.is_symlink() && root.join(&rel).is_dir()) {
                stack.push(rel);
            } else if !kind.is_symlink() || follow_links {
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
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            empty &= is_dir && prune(&path) && fs::remove_dir(&path).is_ok();
        }
        empty
    }
    prune(root);
}
