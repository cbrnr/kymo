//! Kymo's disk cache: the pyramid sidecars of recordings opened before (see
//! [`Sidecar`](crate::pyramid::Sidecar)), sidecars still being built, and the temporary
//! files of irregular time tables. Everything in it can be rebuilt from the recordings.
//!
//! Only files kymo names are counted or removed; anything else in the directory is left
//! alone, and so is the directory itself. [`trim`] keeps the cache within a limit by
//! removing the sidecars used least recently: a sidecar's modification time is its last use,
//! as opening a complete one sets it to the time of opening.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The default cache directory: `kymo` in the platform's user cache directory.
pub fn default_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("kymo"))
}

/// What the cache holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Bytes on disk, by file length.
    pub bytes: u64,
    pub files: usize,
}

/// What [`clear`] or [`trim`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cleared {
    /// The files removed and their bytes.
    pub removed: Usage,
    /// Files left in place: those asked to be kept, sidecars being built, and files the
    /// system would not remove (on Windows, possibly one another kymo has open).
    pub kept: usize,
}

/// What kind of cache file a name is, if kymo names files so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A complete sidecar.
    Sidecar,
    /// A sidecar being built, or left behind by a build that never finished.
    Building,
    /// An irregular time table's scratch file.
    Times,
}

fn kind(name: &str) -> Option<Kind> {
    let is_hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    // `<pid>-<n>`.
    let is_tag = |s: &str| {
        s.split_once('-').is_some_and(|(pid, n)| {
            [pid, n]
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
    };
    if let Some(hash) = name.strip_suffix(".kymo-pyramid") {
        return is_hash(hash).then_some(Kind::Sidecar);
    }
    let stem = name.strip_suffix(".tmp")?;
    let (head, tag) = stem.split_once('.')?;
    if !is_tag(tag) {
        return None;
    }
    if head == "times" {
        Some(Kind::Times)
    } else {
        is_hash(head).then_some(Kind::Building)
    }
}

/// One of kymo's files in the cache.
struct Entry {
    path: PathBuf,
    len: u64,
    kind: Kind,
    /// When it was last modified; for a complete sidecar, when it was last used.
    modified: SystemTime,
}

/// Kymo's files in `dir`.
fn files(dir: &Path) -> io::Result<Vec<Entry>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // Nothing cached yet.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(kind) = entry.file_name().to_str().and_then(kind) else {
            continue;
        };
        // A file removed meanwhile (a build finishing or failing) is no longer there.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() {
            files.push(Entry {
                path: entry.path(),
                len: meta.len(),
                kind,
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
    Ok(files)
}

/// What the cache in `dir` holds; nothing if there is no such directory.
pub fn usage(dir: &Path) -> io::Result<Usage> {
    Ok(files(dir)?
        .iter()
        .fold(Usage::default(), |usage, entry| Usage {
            bytes: usage.bytes + entry.len,
            files: usage.files + 1,
        }))
}

/// Removes kymo's files from `dir`, except those in `keep` (the open recordings' sidecars)
/// and sidecars being built: a temporary sidecar is removed only if it can be locked, as its
/// builder holds a lock until the file is complete, so only those of builds that ended
/// without finishing (a crash) go. A complete sidecar another kymo has open stays usable to
/// it: macOS and Linux keep a removed file's mapping, and Windows removes such a file only
/// once it is closed, or refuses to.
pub fn clear(dir: &Path, keep: &[PathBuf]) -> io::Result<Cleared> {
    let keep: Vec<PathBuf> = keep.iter().map(|p| canonical(p)).collect();
    let mut cleared = Cleared::default();
    for entry in files(dir)? {
        if keep.contains(&canonical(&entry.path)) {
            cleared.kept += 1;
            continue;
        }
        if remove(&entry) {
            cleared.removed.bytes += entry.len;
            cleared.removed.files += 1;
        } else if entry.path.exists() {
            cleared.kept += 1;
        }
    }
    Ok(cleared)
}

/// Removes files from the cache in `dir` until what it holds is at most `limit` bytes:
/// first sidecars left behind by builds that never finished, then complete sidecars, the
/// one used longest ago first. Those in `keep` (the open recordings' sidecars), sidecars
/// being built and the time tables' scratch files stay, so the cache may stay above the
/// limit; [`Cleared::kept`] counts the files that would have gone but could not be removed.
/// A limit of 0 removes every sidecar of a recording not open.
pub fn trim(dir: &Path, limit: u64, keep: &[PathBuf]) -> io::Result<Cleared> {
    let keep: Vec<PathBuf> = keep.iter().map(|p| canonical(p)).collect();
    let files = files(dir)?;
    let mut bytes: u64 = files.iter().map(|e| e.len).sum();
    let mut candidates: Vec<&Entry> = files
        .iter()
        .filter(|e| matches!(e.kind, Kind::Building | Kind::Sidecar))
        .filter(|e| !keep.contains(&canonical(&e.path)))
        .collect();
    candidates.sort_by_key(|e| (e.kind == Kind::Sidecar, e.modified));
    let mut cleared = Cleared::default();
    for entry in candidates {
        if bytes <= limit {
            break;
        }
        if remove(entry) {
            bytes -= entry.len;
            cleared.removed.bytes += entry.len;
            cleared.removed.files += 1;
        } else if entry.kind == Kind::Sidecar && entry.path.exists() {
            // A build in progress is no candidate, so it isn't counted.
            cleared.kept += 1;
        }
    }
    Ok(cleared)
}

/// Removes a cache file: one being built only if no one is building it.
fn remove(entry: &Entry) -> bool {
    match entry.kind {
        Kind::Sidecar | Kind::Times => fs::remove_file(&entry.path).is_ok(),
        Kind::Building => remove_unlocked(&entry.path),
    }
}

/// Records a use of the complete sidecar at `path` for [`trim`]: sets its modification time
/// to now. Without permission to, it keeps the time it was built at.
pub(crate) fn touch(path: &Path) {
    // Windows sets file times only through a handle with write access.
    if let Ok(file) = File::options().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// Removes `path` if no one holds a lock on it.
fn remove_unlocked(path: &Path) -> bool {
    let Ok(file) = File::options().read(true).write(true).open(path) else {
        return false;
    };
    if file.try_lock().is_err() {
        return false;
    }
    // Unix removes a file that is open; Windows only once the handle is closed, as std
    // opens files sharing deletion, so the file goes when `file` is dropped.
    let removed = fs::remove_file(path).is_ok();
    drop(file);
    removed
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let hash = "0d5284d4f6dad0bd4380c50b1337de3dbb3982bdc8522b48e7e6ef04bebbaafd";
        assert_eq!(kind(&format!("{hash}.kymo-pyramid")), Some(Kind::Sidecar));
        assert_eq!(kind(&format!("{hash}.123-0.tmp")), Some(Kind::Building));
        assert_eq!(kind("times.123-4.tmp"), Some(Kind::Times));
        for other in [
            "notes.txt",
            "abc.kymo-pyramid",
            &format!("{hash}.kymo-pyramid.bak"),
            &format!("{hash}.123.tmp"),
            &format!("{hash}.-1.tmp"),
            "times.a-1.tmp",
            "times.tmp",
            ".DS_Store",
        ] {
            assert_eq!(kind(other), None, "{other}");
        }
    }
}
