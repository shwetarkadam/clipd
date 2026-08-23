//! The LAN transport carrying real payloads end to end, over loopback TCP.
//!
//! The unit tests cover the handshake in isolation; this covers the whole
//! stack — clip → envelope → encrypted frame → decrypt → clip written into a
//! store — with the payload types that actually matter.

use clipd_core::files::save_files_in;
use clipd_core::lan::{send_envelope, serve_connection};
use clipd_core::lan_identity::Identity;
use clipd_core::{ClipEntry, ClipStore, ContentType, Envelope};
use std::net::TcpListener;
use std::sync::mpsc;
use x25519_dalek::PublicKey;

fn allow_all(_: &str, _: &PublicKey) -> bool {
    true
}

/// One-shot receiver that materialises whatever arrives into `blob_dir` and a
/// fresh in-memory store, then reports the clip back.
fn spawn_receiver(
    blob_dir: std::path::PathBuf,
) -> (
    std::net::SocketAddr,
    mpsc::Receiver<Result<ClipEntry, String>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();

    let handle = std::thread::spawn(move || {
        let identity = Identity::load_or_create().expect("identity");
        let (mut stream, _) = listener.accept().expect("accept");
        let store = ClipStore::in_memory().expect("store");

        let mut accept = |env: Envelope, _from: &str| -> Result<i64, String> {
            let clip = clipd_core::clip_from_envelope(&env, &blob_dir)?;
            let id = store.insert(&clip).map_err(|e| e.to_string())?;
            let stored = store.get_by_id(id).map_err(|e| e.to_string())?;
            tx.send(Ok(stored)).ok();
            Ok(id)
        };

        if let Err(e) = serve_connection(&mut stream, &identity, &allow_all, &mut accept) {
            tx.send(Err(e)).ok();
        }
    });
    (addr, rx, handle)
}

#[test]
fn a_link_goes_straight_to_the_other_machine() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (addr, rx, handle) = spawn_receiver(tmp.path().join("blobs"));

    let clip = ClipEntry::new("https://example.com/lan-link".into(), Some("Safari".into()), None);
    let envelope = clipd_core::envelope_from_clip(&clip).expect("package");

    let identity = Identity::load_or_create().expect("identity");
    let clip_id = send_envelope(addr, &envelope, &identity, &allow_all).expect("send");
    assert!(clip_id > 0, "the receiver's clip id comes back");

    let received = rx.recv().expect("received").expect("no error");
    assert_eq!(received.content, "https://example.com/lan-link");
    assert_eq!(received.content_type, ContentType::Url);
    handle.join().expect("receiver thread");
}

#[test]
fn a_file_arrives_with_its_bytes_over_the_network() {
    let sender_side = tempfile::tempdir().expect("tempdir");
    let receiver_side = tempfile::tempdir().expect("tempdir");
    let blobs = receiver_side.path().join("blobs");
    let (addr, rx, handle) = spawn_receiver(blobs.clone());

    // A file that exists only on the sending machine.
    let original = sender_side.path().join("contract.pdf");
    std::fs::write(&original, b"%PDF over the wire").expect("write");
    let refs = save_files_in(&[original.clone()], &sender_side.path().join("blobs"));
    let clip = ClipEntry::new_files(refs, Some("Finder".into()));
    let envelope = clipd_core::envelope_from_clip(&clip).expect("package");

    let identity = Identity::load_or_create().expect("identity");
    send_envelope(addr, &envelope, &identity, &allow_all).expect("send");

    let received = rx.recv().expect("received").expect("no error");
    assert_eq!(received.content_type, ContentType::File);
    assert_eq!(received.files.len(), 1);

    let f = &received.files[0];
    assert_eq!(f.name, "contract.pdf");
    // Nothing from the sender's filesystem survives the trip.
    assert!(!f.original_path.contains(sender_side.path().to_str().unwrap()));
    assert_eq!(
        std::fs::read(f.resolve().expect("resolves locally")).unwrap(),
        b"%PDF over the wire"
    );
    handle.join().expect("receiver thread");
}

#[test]
fn a_multi_megabyte_file_goes_over_lan_that_the_folder_would_refuse() {
    let sender_side = tempfile::tempdir().expect("tempdir");
    let receiver_side = tempfile::tempdir().expect("tempdir");
    let (addr, rx, handle) = spawn_receiver(receiver_side.path().join("blobs"));

    // Comfortably over the folder transport's 25 MB envelope cap. The LAN path
    // has no business inheriting a limit that exists because iCloud is slow.
    let big = sender_side.path().join("video.bin");
    let size = (clipd_core::MAX_ENVELOPE_BYTES + 8 * 1024 * 1024) as usize;
    std::fs::write(&big, vec![0xAB; size]).expect("write big");

    let refs = save_files_in(&[big], &sender_side.path().join("blobs"));
    let clip = ClipEntry::new_files(refs, None);
    let envelope = clipd_core::envelope_from_clip(&clip).expect("package");

    // The folder route refuses it...
    assert!(clipd_core::encode_envelope(&envelope).is_err());

    // ...and the LAN route carries it anyway.
    let identity = Identity::load_or_create().expect("identity");
    send_envelope(addr, &envelope, &identity, &allow_all).expect("send over lan");

    let received = rx.recv().expect("received").expect("no error");
    assert_eq!(received.files.len(), 1);
    assert_eq!(received.files[0].size as usize, size);
    handle.join().expect("receiver thread");
}
