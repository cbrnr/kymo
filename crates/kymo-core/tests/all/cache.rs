//! The disk cache: its size, and clearing it without touching builds in progress, kept
//! sidecars or other files.

use crate::common::{TempDir, TempFile};
use kymo_core::cache::{self, Cleared, Usage};
use kymo_core::pyramid::Sidecar;
use kymo_core::{GridId, Recording};
use kymo_testgen::Spec;
use std::time::{Duration, SystemTime};

const HASH: &str = "0d5284d4f6dad0bd4380c50b1337de3dbb3982bdc8522b48e7e6ef04bebbaafd";

fn generated(preset: &str) -> (TempFile, Recording) {
    let file = TempFile::written(&Spec::preset(preset).unwrap());
    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    (file, rec)
}

fn write(dir: &TempDir, name: &str, len: usize) {
    std::fs::write(dir.path().join(name), vec![0u8; len]).unwrap();
}

#[test]
fn a_missing_directory_holds_nothing() {
    let dir = TempDir::new();
    let missing = dir.path().join("missing");
    assert_eq!(cache::usage(&missing).unwrap(), Usage::default());
    assert_eq!(cache::clear(&missing, &[]).unwrap(), Cleared::default());
}

#[test]
fn only_kymos_files_count_and_go() {
    let dir = TempDir::new();
    write(&dir, &format!("{HASH}.kymo-pyramid"), 100);
    // Left behind by a build that never finished: no one holds its lock.
    write(&dir, &format!("{HASH}.4242-0.tmp"), 20);
    write(&dir, "times.4242-1.tmp", 3);
    write(&dir, "notes.txt", 1000);
    write(&dir, "abc.kymo-pyramid", 1000);
    std::fs::create_dir(dir.path().join(format!("{}.kymo-pyramid", &HASH[..63]))).unwrap();
    assert_eq!(
        cache::usage(dir.path()).unwrap(),
        Usage {
            bytes: 123,
            files: 3
        }
    );
    let cleared = cache::clear(dir.path(), &[]).unwrap();
    assert_eq!(
        cleared,
        Cleared {
            removed: Usage {
                bytes: 123,
                files: 3
            },
            kept: 0
        }
    );
    assert_eq!(cache::usage(dir.path()).unwrap(), Usage::default());
    // The other files and the directory.
    assert_eq!(dir.files().len(), 3, "{:?}", dir.files());
    assert!(dir.path().join("notes.txt").exists());
    assert!(dir.path().join("abc.kymo-pyramid").exists());
}

#[test]
fn kept_sidecars_stay() {
    let (_a_file, a) = generated("bdf+c-small");
    let (_b_file, b) = generated("edf+d-mixed");
    let dir = TempDir::new();
    let kept = Sidecar::open_or_create(dir.path(), &a).unwrap();
    assert!(kept.build(&a).unwrap());
    let other = Sidecar::open_or_create(dir.path(), &b).unwrap();
    assert!(other.build(&b).unwrap());
    drop(other);
    assert_eq!(cache::usage(dir.path()).unwrap().files, 2);

    let cleared = cache::clear(dir.path(), &[kept.path()]).unwrap();
    assert_eq!(cleared.removed.files, 1);
    assert_eq!(cleared.kept, 1);
    assert!(kept.path().exists());
    // The kept one opens complete again, the other is rebuilt.
    assert!(
        Sidecar::open_or_create(dir.path(), &a)
            .unwrap()
            .is_complete()
    );
    assert!(
        !Sidecar::open_or_create(dir.path(), &b)
            .unwrap()
            .is_complete()
    );
}

#[test]
fn a_build_in_progress_survives_and_finishes() {
    let (_file, rec) = generated("bdf+c-small");
    let dir = TempDir::new();
    let sidecar = Sidecar::open_or_create(dir.path(), &rec).unwrap();
    let mut builder = sidecar
        .builder(&rec, GridId(0))
        .unwrap()
        .with_chunk_len(1000);
    builder.step().unwrap();
    let building = sidecar.path();

    let cleared = cache::clear(dir.path(), &[]).unwrap();
    assert_eq!(cleared.removed, Usage::default());
    assert_eq!(cleared.kept, 1);
    assert!(building.exists());

    builder.finish().unwrap();
    assert!(sidecar.finish().unwrap());
    assert!(sidecar.is_complete());
    // Its lock is gone with the build.
    let file = std::fs::File::open(sidecar.path()).unwrap();
    assert!(file.try_lock().is_ok());
    drop(file);
    drop(sidecar);
    assert!(
        Sidecar::open_or_create(dir.path(), &rec)
            .unwrap()
            .is_complete()
    );

    // Complete, it is a sidecar like any other.
    assert_eq!(cache::clear(dir.path(), &[]).unwrap().removed.files, 1);
    assert!(dir.files().is_empty(), "{:?}", dir.files());
}

/// A file of `len` bytes named `name`, last modified `age_s` seconds ago.
fn write_aged(dir: &TempDir, name: &str, len: usize, age_s: u64) -> std::path::PathBuf {
    write(dir, name, len);
    let path = dir.path().join(name);
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(age_s))
        .unwrap();
    path
}

fn sidecar_name(n: u8) -> String {
    format!("{}{n:02x}.kymo-pyramid", &HASH[..62])
}

#[test]
fn trimming_removes_the_least_recently_used_first() {
    let dir = TempDir::new();
    let oldest = write_aged(&dir, &sidecar_name(0), 100, 300);
    let old = write_aged(&dir, &sidecar_name(1), 100, 200);
    let recent = write_aged(&dir, &sidecar_name(2), 100, 100);
    write(&dir, "notes.txt", 1000);

    // Within the limit, nothing goes.
    assert_eq!(
        cache::trim(dir.path(), 300, &[]).unwrap(),
        Cleared::default()
    );
    let cleared = cache::trim(dir.path(), 150, &[]).unwrap();
    assert_eq!(
        cleared,
        Cleared {
            removed: Usage {
                bytes: 200,
                files: 2
            },
            kept: 0
        }
    );
    assert!(!oldest.exists() && !old.exists());
    assert!(recent.exists());
    assert!(dir.path().join("notes.txt").exists());
}

#[test]
fn trimming_keeps_open_sidecars_builds_and_time_tables() {
    let dir = TempDir::new();
    let open = write_aged(&dir, &sidecar_name(0), 100, 300);
    let closed = write_aged(&dir, &sidecar_name(1), 100, 100);
    // A build that never finished goes first, however recent.
    let abandoned = write_aged(&dir, &format!("{HASH}.4242-0.tmp"), 10, 0);
    write(&dir, "times.4242-1.tmp", 5);

    let cleared = cache::trim(dir.path(), 0, std::slice::from_ref(&open)).unwrap();
    assert_eq!(
        cleared.removed,
        Usage {
            bytes: 110,
            files: 2
        }
    );
    assert!(open.exists());
    assert!(!closed.exists() && !abandoned.exists());
    assert_eq!(
        cache::usage(dir.path()).unwrap(),
        Usage {
            bytes: 105,
            files: 2
        }
    );
}

#[test]
fn opening_a_sidecar_counts_as_a_use() {
    let (_a_file, a) = generated("bdf+c-small");
    let (_b_file, b) = generated("edf+d-mixed");
    let dir = TempDir::new();
    let [a_path, b_path] = [(&a, 2000), (&b, 1000)].map(|(rec, age_s)| {
        let sidecar = Sidecar::open_or_create(dir.path(), rec).unwrap();
        assert!(sidecar.build(rec).unwrap());
        let path = sidecar.path();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(age_s))
            .unwrap();
        path
    });
    // `a` was used longer ago than `b`, until it opens again.
    assert!(
        Sidecar::open_or_create(dir.path(), &a)
            .unwrap()
            .is_complete()
    );

    let a_len = std::fs::metadata(&a_path).unwrap().len();
    let cleared = cache::trim(dir.path(), a_len, &[]).unwrap();
    assert_eq!(cleared.removed.files, 1);
    assert!(a_path.exists());
    assert!(!b_path.exists());
}

/// What the pyramids of a recording take in memory is what its sidecar holds, but for its
/// header and the stim channels' changes.
#[test]
fn the_pyramids_size_is_the_sidecars() {
    for preset in ["bdf+c-small", "edf+d-mixed"] {
        let (_file, rec) = generated(preset);
        let dir = TempDir::new();
        let sidecar = Sidecar::open_or_create(dir.path(), &rec).unwrap();
        assert!(sidecar.build(&rec).unwrap());
        let len = std::fs::metadata(sidecar.path()).unwrap().len();
        let bytes = kymo_core::pyramid::pyramid_bytes(&rec);
        assert!(bytes > 0 && bytes < len, "{preset}: {bytes} of {len}");
        assert!(len - bytes < 64 << 10, "{preset}: {bytes} of {len}");
    }
}
