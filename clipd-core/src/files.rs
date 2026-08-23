//! On-disk storage for file clips.
//!
//! When you copy a file in Finder, the pasteboard carries a *reference* to it —
//! a `file://` URL, not the bytes. That reference is fragile in a way a
//! clipboard manager cannot live with: move the file, rename it, empty the
//! Downloads folder, and a clip captured last Tuesday silently rots.
//!
//! So clipd takes its own copy, content-addressed by a hash of the bytes, under
//! `<data_local>/clipd/files/`. Copying the same file twice reuses one blob.
//! Very large files ([`MAX_BLOB_BYTES`]) are left where they are and the clip
//! keeps only a reference — duplicating a 4 GB video to make it re-pastable is a
//! worse deal than the occasional dead link.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Largest file clipd will copy into its own store.
///
/// Above this the clip references the original path instead. Chosen so an
/// ordinary working set — documents, images, archives, build outputs — is
/// captured, while disk images and video are not silently duplicated.
pub const MAX_BLOB_BYTES: u64 = 100 * 1024 * 1024;

/// How much of a file is read at a time when hashing.
const HASH_CHUNK: usize = 64 * 1024;

/// One file belonging to a file clip.
///
/// A single Finder copy can carry several files, so a file clip owns a `Vec` of
/// these rather than one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    /// Display name (the last path component at copy time).
    pub name: String,
    /// Where the file lived when it was copied. Kept even when a blob exists:
    /// it is what makes the clip searchable by path, and it is the fallback if
    /// the blob is ever pruned.
    pub original_path: String,
    /// clipd's own copy under [`files_dir`]. `None` means the file was past
    /// [`MAX_BLOB_BYTES`] and only the reference was kept.
    #[serde(default)]
    pub blob_path: Option<String>,
    /// Size in bytes at copy time.
    pub size: u64,
}

impl FileRef {
    /// The path to paste from: clipd's copy when it still exists, otherwise the
    /// original location, otherwise `None` because both are gone.
    pub fn resolve(&self) -> Option<PathBuf> {
        if let Some(blob) = &self.blob_path {
            let p = PathBuf::from(blob);
            if p.exists() {
                return Some(p);
            }
        }
        let original = PathBuf::from(&self.original_path);
        original.exists().then_some(original)
    }

    /// Whether this file can still be pasted.
    pub fn is_available(&self) -> bool {
        self.resolve().is_some()
    }
}

/// Directory where file blobs live: `<data_local>/clipd/files/`.
pub fn files_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("files")
}

/// SHA-256 of a file's contents, streamed so a large file does not have to fit
/// in memory just to be identified.
pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Copy one file into the blob store, returning the reference to record.
///
/// Idempotent: an identical file already in the store is reused rather than
/// written again. Files above [`MAX_BLOB_BYTES`], and directories, come back
/// with `blob_path: None` — referenced, not copied.
pub fn save_file(path: &Path) -> io::Result<FileRef> {
    save_file_in(path, &files_dir())
}

/// [`save_file`], against an explicit blob directory.
///
/// The directory is a parameter so tests can work in a tempdir instead of
/// scattering blobs through the user's real store.
pub fn save_file_in(path: &Path, dir: &Path) -> io::Result<FileRef> {
    let meta = std::fs::metadata(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let original_path = path.to_string_lossy().into_owned();

    // Directories are referenced, never copied: recursively duplicating a
    // folder on every Cmd+C is not a trade a clipboard manager should make.
    if meta.is_dir() || meta.len() > MAX_BLOB_BYTES {
        return Ok(FileRef {
            name,
            original_path,
            blob_path: None,
            size: meta.len(),
        });
    }

    std::fs::create_dir_all(dir)?;

    let hash = hash_file(path)?;
    // Keep the extension on the blob so Finder, Quick Look and the paste path
    // all still recognise the type from the filename alone.
    let blob = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if !ext.is_empty() => dir.join(format!("{hash}.{ext}")),
        _ => dir.join(&hash),
    };

    if !blob.exists() {
        // Write to a temp name first so an interrupted copy can never leave a
        // truncated blob sitting at the content-addressed path, where it would
        // be trusted forever after.
        let staging = dir.join(format!(".{hash}.partial"));
        std::fs::copy(path, &staging)?;
        std::fs::rename(&staging, &blob)?;
    }

    Ok(FileRef {
        name,
        original_path,
        blob_path: Some(blob.to_string_lossy().into_owned()),
        size: meta.len(),
    })
}

/// Copy a set of files into the blob store, skipping any that cannot be read.
///
/// A Finder copy of ten files where one is unreadable should still give you the
/// other nine, so failures are logged and dropped rather than failing the clip.
pub fn save_files(paths: &[PathBuf]) -> Vec<FileRef> {
    save_files_in(paths, &files_dir())
}

/// [`save_files`], against an explicit blob directory.
pub fn save_files_in(paths: &[PathBuf], dir: &Path) -> Vec<FileRef> {
    paths
        .iter()
        .filter_map(|p| match save_file_in(p, dir) {
            Ok(r) => Some(r),
            Err(e) => {
                log::debug!("skipping unreadable file {}: {e}", p.display());
                None
            }
        })
        .collect()
}

/// Content hash for a whole file clip: the hashes of its members, in order.
///
/// Copying the same set of files twice must dedup to one clip, while the same
/// files in a different selection order stay distinct (that is a different copy).
pub fn hash_file_set(files: &[FileRef]) -> String {
    let mut hasher = Sha256::new();
    for f in files {
        hasher.update(f.name.as_bytes());
        hasher.update([0u8]);
        hasher.update(f.size.to_le_bytes());
        // Blob path carries the content hash; fall back to the original path
        // for referenced-only files, which is the best identity available.
        match &f.blob_path {
            Some(b) => hasher.update(b.as_bytes()),
            None => hasher.update(f.original_path.as_bytes()),
        }
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
}

/// Human-readable byte count for previews: `2.4 MB`, `812 KB`.
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Remove blobs backing a file clip (best-effort; ignores missing files).
///
/// Only blobs are touched — the user's original files are never deleted, no
/// matter what happens to the clip that referenced them.
pub fn delete_file_blobs(files: &[FileRef]) {
    delete_file_blobs_in(files, &files_dir())
}

/// [`delete_file_blobs`], against an explicit blob directory.
pub fn delete_file_blobs_in(files: &[FileRef], dir: &Path) {
    for f in files {
        let Some(blob) = &f.blob_path else { continue };
        let path = PathBuf::from(blob);
        // Refuse to unlink anything that escaped the blob directory. A clip
        // row is data, and data should never be able to name an arbitrary path
        // for deletion.
        if path.parent() != Some(dir) {
            log::warn!("refusing to delete blob outside the store: {blob}");
            continue;
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create");
        f.write_all(bytes).expect("write");
        p
    }

    #[test]
    fn save_copies_dedups_and_resolves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = write_temp(tmp.path(), "notes.txt", b"hello clipd");

        let blobs = tmp.path().join("blobs");
        let a = save_file_in(&src, &blobs).expect("save");
        assert_eq!(a.name, "notes.txt");
        assert_eq!(a.size, 11);
        let blob = a.blob_path.clone().expect("small file must be copied");
        assert!(Path::new(&blob).exists());
        assert!(blob.ends_with(".txt"), "blob keeps the extension: {blob}");

        // Same bytes → same blob, written once.
        let b = save_file_in(&src, &blobs).expect("save again");
        assert_eq!(a.blob_path, b.blob_path);

        // The blob outlives the original — that is the entire point.
        std::fs::remove_file(&src).expect("remove original");
        assert_eq!(a.resolve(), Some(PathBuf::from(&blob)));
        assert!(a.is_available());

        delete_file_blobs_in(std::slice::from_ref(&a), &blobs);
        assert!(!Path::new(&blob).exists());
        assert!(!a.is_available(), "both copies gone means unavailable");
    }

    #[test]
    fn resolve_falls_back_to_the_original_when_the_blob_is_gone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = write_temp(tmp.path(), "kept.txt", b"still here");
        let blobs = tmp.path().join("blobs");
        let mut r = save_file_in(&src, &blobs).expect("save");
        delete_file_blobs_in(std::slice::from_ref(&r), &blobs);
        assert_eq!(r.resolve(), Some(src.clone()));

        // And with no blob recorded at all (the oversized-file case).
        r.blob_path = None;
        assert_eq!(r.resolve(), Some(src));
    }

    #[test]
    fn directories_are_referenced_not_copied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sub = tmp.path().join("folder");
        std::fs::create_dir(&sub).expect("mkdir");
        let r = save_file_in(&sub, &tmp.path().join("blobs")).expect("save dir");
        assert!(r.blob_path.is_none());
        assert_eq!(r.name, "folder");
        assert_eq!(r.resolve(), Some(sub));
    }

    #[test]
    fn hash_distinguishes_sets_and_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blobs = tmp.path().join("blobs");
        let a = save_file_in(&write_temp(tmp.path(), "a.txt", b"aaa"), &blobs).expect("a");
        let b = save_file_in(&write_temp(tmp.path(), "b.txt", b"bbb"), &blobs).expect("b");

        let ab = hash_file_set(&[a.clone(), b.clone()]);
        assert_eq!(ab, hash_file_set(&[a.clone(), b.clone()]), "stable");
        assert_ne!(ab, hash_file_set(&[b, a.clone()]), "order matters");
        assert_ne!(ab, hash_file_set(&[a]), "membership matters");
    }

    #[test]
    fn save_files_drops_unreadable_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let good = write_temp(tmp.path(), "good.txt", b"ok");
        let missing = tmp.path().join("nope.txt");
        let refs = save_files_in(&[good, missing], &tmp.path().join("blobs"));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "good.txt");
    }

    #[test]
    fn delete_refuses_paths_outside_the_blob_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = write_temp(tmp.path(), "precious.txt", b"do not delete");
        let forged = FileRef {
            name: "precious.txt".into(),
            original_path: outside.to_string_lossy().into_owned(),
            blob_path: Some(outside.to_string_lossy().into_owned()),
            size: 13,
        };
        delete_file_blobs_in(&[forged], &tmp.path().join("blobs"));
        assert!(outside.exists(), "deletion must stay inside the blob store");
    }

    #[test]
    fn sizes_read_as_humans_expect() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }
}
