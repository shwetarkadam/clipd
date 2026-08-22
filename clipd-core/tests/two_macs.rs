//! The whole send path, with a temp directory standing in for the shared
//! iCloud folder: Mac A sends, Mac B collects, the clip lands in B's history.
//!
//! The real transport can't be exercised in a test — it needs two machines and
//! iCloud Drive — so this covers everything on either side of the folder.

use clipd_core::devices::{inbox_dir, write_atomically, Device};
use clipd_core::files::save_files_in;
use clipd_core::sync::{clip_from_envelope, deliver, pending, Envelope, Payload};
use clipd_core::{ClipEntry, ClipStore, ContentType};
use chrono::Utc;

/// Stands in for `sync_root()`, which reads the real iCloud path.
struct Cloud {
    dir: tempfile::TempDir,
}

impl Cloud {
    fn new() -> Self {
        Cloud {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }
    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// Announce a device, the way `register()` does for the local Mac.
    fn announce(&self, id: &str, name: &str) -> Device {
        let d = Device {
            id: id.into(),
            name: name.into(),
            last_seen: Utc::now(),
        };
        write_atomically(
            &self.path().join("devices").join(format!("{id}.json")),
            serde_json::to_string(&d).expect("json").as_bytes(),
        )
        .expect("announce");
        std::fs::create_dir_all(inbox_dir(self.path(), id)).expect("inbox");
        d
    }
}

/// Mac A sends `clip`; Mac B collects everything waiting and stores it.
/// Returns what B ended up with.
fn round_trip(cloud: &Cloud, b: &Device, clip: &ClipEntry, b_blobs: &std::path::Path) -> Vec<ClipEntry> {
    let mut envelope = clipd_core::envelope_from_clip(clip).expect("package");
    envelope.from_name = "MacBook Air".into();
    envelope.from_device = "aaa111".into();
    deliver(cloud.path(), &b.id, &envelope).expect("deliver");

    let store = ClipStore::in_memory().expect("store");
    let mut stored = Vec::new();
    for (path, parsed) in pending(cloud.path(), &b.id) {
        let env = parsed.expect("parses");
        let received = clip_from_envelope(&env, b_blobs).expect("unpack");
        let id = store.insert(&received).expect("insert");
        stored.push(store.get_by_id(id).expect("read back"));
        std::fs::remove_file(&path).expect("envelope consumed");
    }
    // A collected inbox is an empty one — otherwise the next poll replays it.
    assert!(pending(cloud.path(), &b.id).is_empty());
    stored
}

#[test]
fn a_link_lands_in_the_other_macs_history() {
    let cloud = Cloud::new();
    let b = cloud.announce("bbb222", "Mac mini");
    let tmp = tempfile::tempdir().expect("tempdir");

    let sent = ClipEntry::new("https://example.com/article".into(), Some("Safari".into()), None);
    let got = round_trip(&cloud, &b, &sent, tmp.path());

    assert_eq!(got.len(), 1);
    assert_eq!(got[0].content, "https://example.com/article");
    assert_eq!(got[0].content_type, ContentType::Url);
    // The receiving Mac records where it came from, not a Safari that isn't here.
    assert_eq!(got[0].source_app.as_deref(), Some("MacBook Air"));
    assert_eq!(got[0].source_title.as_deref(), Some("Sent from MacBook Air"));
}

#[test]
fn a_file_arrives_with_its_bytes_and_no_sender_paths() {
    let cloud = Cloud::new();
    let b = cloud.announce("bbb222", "Mac mini");
    let a_side = tempfile::tempdir().expect("tempdir");
    let b_blobs = tempfile::tempdir().expect("tempdir");

    // Mac A copies a file from a path that exists only on Mac A.
    let original = a_side.path().join("contract.pdf");
    std::fs::write(&original, b"%PDF signed").expect("write");
    let refs = save_files_in(&[original.clone()], &a_side.path().join("blobs"));
    let sent = ClipEntry::new_files(refs, Some("Finder".into()));

    let got = round_trip(&cloud, &b, &sent, b_blobs.path());
    assert_eq!(got.len(), 1);
    let received = &got[0];
    assert_eq!(received.content_type, ContentType::File);
    assert_eq!(received.files.len(), 1);

    let f = &received.files[0];
    assert_eq!(f.name, "contract.pdf");
    // Nothing from Mac A's filesystem survives — this is the bug that would
    // otherwise produce clips pointing at paths that don't exist here.
    assert!(!f.original_path.contains(a_side.path().to_str().unwrap()));
    assert!(f.blob_path.as_ref().unwrap().starts_with(b_blobs.path().to_str().unwrap()));

    // And the file is genuinely usable on Mac B.
    let resolved = f.resolve().expect("resolves locally");
    assert_eq!(std::fs::read(resolved).unwrap(), b"%PDF signed");
}

#[test]
fn several_sends_arrive_in_the_order_they_were_sent() {
    let cloud = Cloud::new();
    let b = cloud.announce("bbb222", "Mac mini");

    let store = ClipStore::in_memory().expect("store");
    for (i, text) in ["one", "two", "three"].iter().enumerate() {
        let mut env = Envelope {
            id: format!("env{i}"),
            from_device: "aaa111".into(),
            from_name: "MacBook Air".into(),
            sent_at: Utc::now() + chrono::Duration::milliseconds(i as i64 * 10),
            payload: Payload::Text {
                content: (*text).into(),
                content_type: ContentType::Text,
            },
        };
        env.sent_at = Utc::now() + chrono::Duration::seconds(i as i64);
        deliver(cloud.path(), &b.id, &env).expect("deliver");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let received: Vec<String> = pending(cloud.path(), &b.id)
        .into_iter()
        .map(|(_, e)| {
            let clip = clip_from_envelope(&e.expect("parses"), tmp.path()).expect("unpack");
            store.insert(&clip).expect("insert");
            clip.content
        })
        .collect();
    assert_eq!(received, vec!["one", "two", "three"]);
}

#[test]
fn a_send_to_a_mac_that_is_offline_waits_for_it() {
    let cloud = Cloud::new();
    let b = cloud.announce("bbb222", "Mac mini");

    let sent = ClipEntry::new("waiting for you".into(), None, None);
    let env = clipd_core::envelope_from_clip(&sent).expect("package");
    deliver(cloud.path(), &b.id, &env).expect("deliver");

    // B never runs. The envelope is still there, intact, whenever it wakes up.
    let waiting = pending(cloud.path(), &b.id);
    assert_eq!(waiting.len(), 1);
    assert!(waiting[0].1.is_ok());

    // Polling repeatedly must not consume or corrupt it.
    assert_eq!(pending(cloud.path(), &b.id).len(), 1);
}
