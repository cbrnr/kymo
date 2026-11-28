//! Opening a recording: the format comes from the file's first bytes, never its extension.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{OpenError, Opened, brainvision, bvrf, edf, eeglab, fiff, gdf, xdf};

/// Bytes read to recognize a format: enough for MATLAB's 116-byte text header.
const HEAD: u64 = 128;

/// A format recognized from the first bytes of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    /// EDF, EDF+, BDF or BDF+.
    Edf,
    /// GDF 1.x or 2.x.
    Gdf,
    /// GDF of another version.
    OtherGdf,
    Fiff,
    Xdf,
    BrainVision,
    /// A JSON file, as BVRF's header (`.bvrh`) is; the reader checks that it is one.
    Json,
    /// A MATLAB file in the v5 format (MATLAB's v6 and v7 files), as EEGLAB writes `.set`.
    Mat,
    /// A MATLAB v7.3 file, which is HDF5.
    MatHdf5,
    /// A gzip-compressed file (`.fif.gz`, `.xdf.gz`).
    Gzip,
}

/// The format whose file starts with `head`, if any.
fn detect(head: &[u8]) -> Option<Format> {
    // FIFF: the file-id tag first (kind 100, type 31 = ID struct), big-endian.
    const FIFF: [u8; 8] = [0, 0, 0, 100, 0, 0, 0, 31];
    let format = if head.starts_with(b"XDF:") {
        Format::Xdf
    } else if head.starts_with(b"GDF 1.") || head.starts_with(b"GDF 2.") {
        Format::Gdf
    } else if head.starts_with(b"GDF ") {
        Format::OtherGdf
    } else if head.starts_with(&FIFF) {
        Format::Fiff
    } else if head.starts_with(b"MATLAB 7.3 MAT-file") {
        Format::MatHdf5
    } else if head.starts_with(b"MATLAB 5.0 MAT-file") {
        Format::Mat
    } else if head.starts_with(&[0x1f, 0x8b]) {
        Format::Gzip
    } else if is_brainvision(head) {
        Format::BrainVision
    } else if is_json(head) {
        Format::Json
    } else if let Some(b'0' | 0xff) = head.first() {
        // EDF's version field is "0", BDF's starts with 0xFF ("\xffBIOSEMI").
        Format::Edf
    } else {
        return None;
    };
    Some(format)
}

/// BrainVision's first line, as MNE accepts it: "Brain Vision" or "BrainVision", optionally
/// "Core" or "V-Amp", "Data", optionally "Exchange", "Header File", a version; after an
/// optional UTF-8 byte-order mark.
fn is_brainvision(head: &[u8]) -> bool {
    let text = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let line = text
        .split(|&b| b == b'\n' || b == b'\r')
        .next()
        .unwrap_or(text);
    (line.starts_with(b"Brain Vision ") || line.starts_with(b"BrainVision "))
        && line.windows(11).any(|w| w == b"Header File")
}

/// A JSON object's opening brace first, after an optional UTF-8 byte-order mark and
/// whitespace. No other format read starts with one.
fn is_json(head: &[u8]) -> bool {
    let text = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    text.trim_ascii_start().first() == Some(&b'{')
}

/// Why a recognized format can't be opened (yet).
fn unsupported(format: Format) -> &'static str {
    match format {
        Format::Edf
        | Format::BrainVision
        | Format::Json
        | Format::Mat
        | Format::Gdf
        | Format::Fiff
        | Format::Xdf => {
            unreachable!("EDF, BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF are read")
        }
        Format::OtherGdf => "only GDF 1 and 2 files are supported",
        Format::Gzip => "compressed (gzip) files are not read; decompress it first",
        Format::MatHdf5 => {
            "MATLAB v7.3 (HDF5) files are not supported; if this is an EEGLAB dataset, save it \
             again from EEGLAB with \"Save as Matlab v6.5\" turned on in File > Preferences \
             (option_saveversion6)"
        }
    }
}

/// The first bytes of `path`, up to [`HEAD`].
fn head(path: &Path) -> Result<Vec<u8>, OpenError> {
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(HEAD).read_to_end(&mut head))
        .map_err(|source| OpenError::Io {
            path: path.to_owned(),
            source,
        })?;
    Ok(head)
}

/// For a data or marker file, the header file beside it that describes it, if it exists:
/// BrainVision's `.vhdr` for `.eeg` and `.vmrk`, EEGLAB's `.set` for `.fdt`, BVRF's `.bvrh`
/// for `.bvrd`, `.bvrm` and `.bvri`.
fn header_sibling(path: &Path) -> Option<PathBuf> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let candidates: &[&str] = match extension.as_str() {
        "eeg" | "vmrk" => &["vhdr", "VHDR"],
        "fdt" => &["set", "SET"],
        "bvrd" | "bvrm" | "bvri" => &["bvrh", "BVRH"],
        _ => return None,
    };
    candidates
        .iter()
        .map(|e| path.with_extension(e))
        .find(|p| p.is_file())
}

/// Opens a recording file, choosing the reader from the file's first bytes.
///
/// A data or marker file whose content is not recognized (BrainVision's `.eeg` and `.vmrk`,
/// EEGLAB's `.fdt`, BVRF's `.bvrd`, `.bvrm` and `.bvri`) opens the header file beside it, if
/// there is one. EDF and BDF (with their + variants), BrainVision, EEGLAB, GDF 1 and 2, FIFF,
/// XDF and BVRF are read for now. Scratch files (the time tables of XDF streams on their own
/// time stamps) go into kymo's cache directory.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let dir = crate::cache::default_dir().unwrap_or_else(std::env::temp_dir);
    open_with(path, Some(&dir))
}

/// [`open`], with scratch files in `scratch`, or in memory without one, so that nothing is
/// written to disk.
pub fn open_with(path: &Path, scratch: Option<&Path>) -> Result<Opened, OpenError> {
    let (path, format) = match detect(&head(path)?) {
        Some(format) => (path.to_owned(), format),
        None => {
            let sibling = header_sibling(path)
                .map(|sibling| Ok::<_, OpenError>((detect(&head(&sibling)?), sibling)))
                .transpose()?;
            match sibling {
                Some((Some(format), sibling)) => (sibling, format),
                _ => {
                    return Err(OpenError::Unsupported {
                        path: path.to_owned(),
                        reason: "not a supported file format (EDF, BDF, BrainVision, EEGLAB, \
                                 GDF, FIFF, XDF and BVRF are read)"
                            .into(),
                    });
                }
            }
        }
    };
    match format {
        Format::Edf => edf::open(&path),
        Format::BrainVision => brainvision::open(&path),
        Format::Json => bvrf::open(&path),
        Format::Mat => eeglab::open(&path),
        Format::Gdf => gdf::open(&path),
        Format::Fiff => fiff::open(&path),
        Format::Xdf => xdf::open_with(&path, scratch),
        other => Err(OpenError::Unsupported {
            path,
            reason: unsupported(other).into(),
        }),
    }
}

/// The file `name` (as the header writes it) in `dir`; failing that, its last path
/// component there (headers written on another machine may hold absolute paths), then
/// `{base}.{extension}`, with a warning. `None` if none of them exists.
pub(crate) fn companion(
    dir: &Path,
    base: &str,
    name: &str,
    extension: &str,
    what: &str,
    warnings: &mut Vec<String>,
) -> Option<PathBuf> {
    let named = dir.join(name);
    if named.is_file() {
        return Some(named);
    }
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let candidates = [dir.join(last), dir.join(format!("{base}.{extension}"))];
    let found = candidates.into_iter().find(|p| p.is_file())?;
    warnings.push(format!(
        "{what} file {name:?} not found; using {:?}",
        found.file_name().unwrap_or_default()
    ));
    Some(found)
}
