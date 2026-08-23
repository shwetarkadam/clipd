//! End-to-end file clips: a Finder-style copy, through capture and storage,
//! back onto the pasteboard as real files.
//!
//! These drive the *real* system pasteboard, so they are opt-in via
//! `CLIPD_CLIPBOARD_TESTS=1` and must not run in parallel with each other.

#![cfg(target_os = "macos")]

use clipd_core::{
    clipboard_read_file_urls, clipboard_read_text, clipboard_write_file_urls, files::save_files_in,
    ClipEntry, ClipStore, ContentType,
};
use std::path::PathBuf;

fn enabled() -> bool {
    std::env::var("CLIPD_CLIPBOARD_TESTS").is_ok()
}

fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p
}

#[test]
fn a_copied_file_survives_the_original_being_deleted() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let blobs = tmp.path().join("blobs");
    let doc = write(tmp.path(), "quarterly report.pdf", b"%PDF-1.4 pretend");

    // 1. Finder puts the file on the pasteboard.
    clipboard_write_file_urls(&[doc.clone()]).expect("put on pasteboard");

    // 2. clipd sees a file copy, not text.
    let seen = clipboard_read_file_urls();
    assert_eq!(seen, vec![doc.clone()]);

    // 3. Capture: bytes are copied into the blob store and filed as a clip.
    let files = save_files_in(&seen, &blobs);
    let entry = ClipEntry::new_files(files, Some("Finder".into()));
    let store = ClipStore::in_memory().expect("store");
    let id = store.insert(&entry).expect("insert");

    // 4. The user deletes the original. This is the case a reference-only
    //    clipboard manager loses, and the whole reason clipd takes a copy.
    std::fs::remove_file(&doc).expect("remove original");

    // 5. Paste it back anyway.
    let back = store.get_by_id(id).expect("load clip");
    assert_eq!(back.content_type, ContentType::File);
    let paths: Vec<PathBuf> = back.files.iter().filter_map(|f| f.resolve()).collect();
    assert_eq!(paths.len(), 1, "the blob still resolves");
    assert_ne!(paths[0], doc, "resolved to clipd's copy, not the dead path");

    clipboard_write_file_urls(&paths).expect("put back on pasteboard");
    assert_eq!(clipboard_read_file_urls(), paths);
    assert_eq!(
        std::fs::read(&paths[0]).expect("read blob"),
        b"%PDF-1.4 pretend"
    );
}

#[test]
fn a_multi_file_copy_round_trips_in_order() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let blobs = tmp.path().join("blobs");
    let a = write(tmp.path(), "a.txt", b"first");
    let b = write(tmp.path(), "b.txt", b"second");
    let c = write(tmp.path(), "c.txt", b"third");

    clipboard_write_file_urls(&[a.clone(), b.clone(), c.clone()]).expect("put");
    let seen = clipboard_read_file_urls();
    assert_eq!(seen, vec![a.clone(), b, c], "order is preserved");

    let entry = ClipEntry::new_files(save_files_in(&seen, &blobs), Some("Finder".into()));
    assert!(entry.preview.contains("3 files"), "{}", entry.preview);

    // Every original path stays searchable in the clip body.
    for p in &seen {
        assert!(entry.content.contains(p.to_str().unwrap()));
    }

    // The first file's path is readable as text, so pasting into a terminal
    // gives one usable path rather than nothing.
    assert_eq!(clipboard_read_text().as_deref(), a.to_str());
}

#[test]
fn an_oversized_file_is_referenced_rather_than_copied() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let blobs = tmp.path().join("blobs");
    let big = tmp.path().join("huge.bin");
    // One byte over the cap.
    std::fs::write(&big, vec![7u8; (clipd_core::MAX_BLOB_BYTES + 1) as usize]).expect("write big");

    let files = save_files_in(&[big.clone()], &blobs);
    assert_eq!(files.len(), 1);
    assert!(
        files[0].blob_path.is_none(),
        "clipd must not duplicate a file this large"
    );
    // It is still pasteable while the original is where it was left.
    assert_eq!(files[0].resolve(), Some(big));
}
