//! The disk cache as Settings shows it: its size, measured, cleared and trimmed to its limit
//! on a background thread, so the UI thread never waits on the disk.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use kymo_core::cache::{self, Usage};

use crate::document::plural;

/// The cache in a directory, what is known of it, and the measuring or clearing under way.
pub struct DiskCache {
    dir: Option<PathBuf>,
    status: Status,
    pending: Option<Receiver<Outcome>>,
    /// A clearing asked for while something else was under way, with what it keeps.
    clear_next: Option<Vec<PathBuf>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    /// Not measured since Settings last opened.
    Unknown,
    /// Being measured for the first time since Settings opened. Measured again, or trimmed
    /// to its limit (which measures it too), it stays [`Status::Known`] meanwhile, so its
    /// size doesn't flicker.
    Measuring,
    Clearing,
    /// Its size.
    Known(Usage),
    Failed(String),
}

type Outcome = Result<Usage, String>;

/// What a background thread does to the cache before measuring it.
#[derive(Clone, Copy)]
enum Work {
    Measure,
    Clear,
    /// Trims it to this many bytes.
    Trim(u64),
}

impl DiskCache {
    /// The cache in `dir`; `None` keeps pyramids in memory, with no cache to show.
    pub fn new(dir: Option<PathBuf>) -> DiskCache {
        DiskCache {
            dir,
            status: Status::Unknown,
            pending: None,
            clear_next: None,
        }
    }

    /// Whether there is a cache directory.
    pub fn exists(&self) -> bool {
        self.dir.is_some()
    }

    /// Follows the Settings window: measures when it opens (and nothing is under way), and
    /// forgets the size when it closes, so the next opening measures again. Picks up a
    /// result.
    pub fn follow(&mut self, settings_open: bool, repaint: impl Fn() + Send + 'static) {
        if let Some(outcome) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.pending = None;
            self.status = match outcome {
                Ok(usage) => Status::Known(usage),
                Err(error) => Status::Failed(error),
            };
        }
        if self.pending.is_some() {
            return;
        }
        if let Some(keep) = self.clear_next.take() {
            self.run(Work::Clear, keep, repaint);
            return;
        }
        if !settings_open {
            self.status = Status::Unknown;
        } else if self.status == Status::Unknown {
            self.run(Work::Measure, Vec::new(), repaint);
        }
    }

    /// Clears the cache but for `keep` (the open recording's sidecar), then measures it; once
    /// what is under way has finished, if anything is.
    pub fn clear(&mut self, keep: Vec<PathBuf>, repaint: impl Fn() + Send + 'static) {
        if self.busy() {
            self.clear_next = Some(keep);
        } else {
            self.run(Work::Clear, keep, repaint);
        }
    }

    /// Trims the cache to `limit` bytes but for `keep` (the open recording's sidecar; see
    /// [`cache::trim`]), then measures it; returns whether it started, as it waits for
    /// what is under way.
    pub fn trim(
        &mut self,
        limit: u64,
        keep: Vec<PathBuf>,
        repaint: impl Fn() + Send + 'static,
    ) -> bool {
        if self.busy() {
            return false;
        }
        self.run(Work::Trim(limit), keep, repaint);
        true
    }

    fn run(&mut self, work: Work, keep: Vec<PathBuf>, repaint: impl Fn() + Send + 'static) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let run = || -> std::io::Result<Usage> {
                match work {
                    Work::Measure => {}
                    Work::Clear => {
                        cache::clear(&dir, &keep)?;
                    }
                    Work::Trim(limit) => {
                        cache::trim(&dir, limit, &keep)?;
                    }
                }
                cache::usage(&dir)
            };
            let _ = tx.send(run().map_err(|e| format!("{}: {e}", dir.display())));
            repaint();
        });
        self.status = match (work, &self.status) {
            (Work::Clear, _) => Status::Clearing,
            (_, Status::Known(_)) => self.status.clone(),
            _ => Status::Measuring,
        };
        self.pending = Some(rx);
    }

    /// Whether it is being measured or cleared.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// What Settings shows: "788 MB in 107 files".
    pub fn text(&self) -> String {
        match &self.status {
            Status::Unknown | Status::Measuring => "Measuring\u{2026}".to_owned(),
            Status::Clearing => "Clearing\u{2026}".to_owned(),
            Status::Failed(error) => error.clone(),
            Status::Known(usage) => describe(*usage),
        }
    }

    /// Whether Clear would remove anything: the cache was measured and holds files, and no
    /// clearing is under way or asked for.
    pub fn clearable(&self) -> bool {
        self.clear_next.is_none() && matches!(self.status, Status::Known(usage) if usage.files > 0)
    }
}

/// "788 MB in 107 files"; "Empty" for none.
fn describe(usage: Usage) -> String {
    if usage.files == 0 {
        return "Empty".to_owned();
    }
    format!(
        "{} in {} file{}",
        format_bytes(usage.bytes),
        usage.files,
        plural(usage.files)
    )
}

/// Bytes in the units Settings counts memory in (1 MB = 2²⁰ bytes): whole KB below 1 MB,
/// whole MB below 1 GB, GB with one decimal from then on.
pub fn format_bytes(bytes: u64) -> String {
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    if bytes < MB {
        format!("{} KB", bytes.div_ceil(1 << 10))
    } else if bytes < GB {
        format!("{} MB", (bytes + MB / 2) / MB)
    } else {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_bytes(0), "0 KB");
        assert_eq!(format_bytes(1), "1 KB");
        assert_eq!(format_bytes(788 << 20), "788 MB");
        assert_eq!(format_bytes(5_100 << 20), "5.0 GB");
        assert_eq!(format_bytes(51 << 29), "25.5 GB");
        assert_eq!(
            describe(Usage {
                bytes: 3 << 20,
                files: 1
            }),
            "3 MB in 1 file"
        );
        assert_eq!(describe(Usage::default()), "Empty");
    }

    #[test]
    fn texts() {
        let known = DiskCache {
            dir: None,
            status: Status::Known(Usage {
                bytes: 5 << 20,
                files: 1,
            }),
            pending: None,
            clear_next: None,
        };
        assert_eq!(known.text(), "5 MB in 1 file");
    }
}
