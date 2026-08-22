//! The wire format for sending a clip to another Mac.
//!
//! An envelope is a self-contained JSON file dropped into the target Mac's
//! inbox under [`crate::devices::sync_root`]. Self-contained matters: paths are
//! meaningless across machines, so the bytes travel inside the envelope and the
//! receiver writes them into *its own* store. A clip that arrives never carries
//! a path from the machine that sent it.
//!
//! Envelopes are capped at [`MAX_ENVELOPE_BYTES`]. iCloud Drive is a fine
//! courier for a link, a screenshot, or a PDF, and a bad one for a disc image —
//! oversized sends are refused with an explanation rather than queued forever.

use crate::files::{format_size, FileRef};
use crate::models::{ClipEntry, ContentType};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Largest envelope that may be handed to iCloud Drive.
///
/// Sized so ordinary sends — links, snippets, screenshots, documents — always
/// work, while something that would sit uploading for minutes is refused at the
/// point of sending, when the user can still do something about it.
pub const MAX_ENVELOPE_BYTES: u64 = 25 * 1024 * 1024;

/// File extension for envelopes waiting in an inbox.
pub const ENVELOPE_EXT: &str = "clipdenv";

/// What a clip becomes on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    /// Text, links, code — anything whose content *is* the clip.
    Text {
        content: String,
        content_type: ContentType,
    },
    /// A screenshot or copied graphic, as PNG bytes.
    Image {
        #[serde(with = "b64")]
        png: Vec<u8>,
        /// OCR text from the sending Mac, so the clip stays searchable here
        /// without re-running Vision on arrival.
        #[serde(default)]
        ocr_text: Option<String>,
    },
    /// Files copied in Finder, with their bytes.
    Files { files: Vec<InlineFile> },
}

/// One file, inlined into an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineFile {
    pub name: String,
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
}

/// A clip in transit between two Macs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Unique per send, and the filename it lands under.
    pub id: String,
    /// Device id of the sending Mac.
    pub from_device: String,
    /// Display name of the sending Mac, so the receiver can say who sent it
    /// without a lookup that might fail.
    pub from_name: String,
    pub sent_at: DateTime<Utc>,
    pub payload: Payload,
}

impl Envelope {
    /// A one-line description for notifications: "Link from MacBook Air".
    pub fn summary(&self) -> String {
        let what = match &self.payload {
            Payload::Text { content_type, .. } => match content_type {
                ContentType::Url => "Link".to_string(),
                ContentType::Code => "Code".to_string(),
                _ => "Text".to_string(),
            },
            Payload::Image { .. } => "Image".to_string(),
            Payload::Files { files } if files.len() == 1 => files[0].name.clone(),
            Payload::Files { files } => format!("{} files", files.len()),
        };
        format!("{what} from {}", self.from_name)
    }
}

/// Build an envelope from a clip that is about to be sent.
///
/// Reads file and image bytes off disk here, on the sending side, so the
/// envelope is complete the moment it is written.
pub fn envelope_from_clip(clip: &ClipEntry) -> Result<Envelope, String> {
    let payload = match clip.content_type {
        ContentType::Image => {
            let path = clip
                .image_path
                .as_deref()
                .ok_or("That image clip has no file backing it any more.")?;
            let png = std::fs::read(path)
                .map_err(|e| format!("Couldn't read the image for sending: {e}"))?;
            Payload::Image {
                png,
                ocr_text: clip.ocr_text.clone(),
            }
        }
        ContentType::File => {
            if clip.files.is_empty() {
                return Err("That file clip has no files in it.".into());
            }
            let mut files = Vec::with_capacity(clip.files.len());
            for f in &clip.files {
                let path = f.resolve().ok_or_else(|| {
                    format!("\"{}\" isn't on this Mac any more, so it can't be sent.", f.name)
                })?;
                if path.is_dir() {
                    return Err(format!(
                        "\"{}\" is a folder. Sending folders isn't supported yet — zip it first.",
                        f.name
                    ));
                }
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("Couldn't read \"{}\" for sending: {e}", f.name))?;
                files.push(InlineFile {
                    name: f.name.clone(),
                    bytes,
                });
            }
            Payload::Files { files }
        }
        _ => {
            if clip.content.trim().is_empty() {
                return Err("There's nothing in that clip to send.".into());
            }
            Payload::Text {
                content: clip.content.clone(),
                content_type: clip.content_type.clone(),
            }
        }
    };

    Ok(Envelope {
        id: new_envelope_id(),
        from_device: crate::devices::device_id(),
        from_name: crate::devices::device_name(),
        sent_at: Utc::now(),
        payload,
    })
}

/// Serialize an envelope and check it against the transport's size limit.
pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(envelope)
        .map_err(|e| format!("Couldn't package that clip for sending: {e}"))?;
    if json.len() as u64 > MAX_ENVELOPE_BYTES {
        return Err(format!(
            "That's {} once packaged, over the {} limit for sending through iCloud. \
             Share it another way, or send a link to it instead.",
            format_size(json.len() as u64),
            format_size(MAX_ENVELOPE_BYTES)
        ));
    }
    Ok(json)
}

/// Drop an envelope into a target Mac's inbox.
///
/// The write is atomic, so the receiving daemon never picks up a partial file.
pub fn deliver(root: &Path, target_device_id: &str, envelope: &Envelope) -> Result<(), String> {
    let bytes = encode(envelope)?;
    let path = crate::devices::inbox_dir(root, target_device_id)
        .join(format!("{}.{ENVELOPE_EXT}", envelope.id));
    crate::devices::write_atomically(&path, &bytes)
}

/// Every envelope waiting in this Mac's inbox, oldest first.
///
/// Unreadable envelopes are reported to the caller rather than silently
/// dropped, so a bad file can be cleaned up instead of retried forever.
pub fn pending(root: &Path, device_id: &str) -> Vec<(std::path::PathBuf, Result<Envelope, String>)> {
    let dir = crate::devices::inbox_dir(root, device_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut found: Vec<(std::path::PathBuf, Result<Envelope, String>)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == ENVELOPE_EXT))
        .map(|path| {
            let parsed = std::fs::read(&path)
                .map_err(|e| format!("Couldn't read {}: {e}", path.display()))
                .and_then(|bytes| {
                    serde_json::from_slice::<Envelope>(&bytes)
                        .map_err(|e| format!("Couldn't understand {}: {e}", path.display()))
                });
            (path, parsed)
        })
        .collect();

    // Oldest first, so a burst of sends arrives in the order it was sent.
    found.sort_by(|a, b| match (&a.1, &b.1) {
        (Ok(x), Ok(y)) => x.sent_at.cmp(&y.sent_at),
        _ => a.0.cmp(&b.0),
    });
    found
}

/// Send a clip to another Mac. The one call the hotkey, the HUD and the CLI
/// all go through.
///
/// `target` is whatever the user typed, or `None` to mean "the other Mac" —
/// which is unambiguous, and therefore decision-free, whenever there is exactly
/// one. Returns the device it went to, so the caller can say where.
pub fn send_clip(clip: &ClipEntry, target: Option<&str>) -> Result<crate::devices::Device, String> {
    let (device, how) = send_clip_via(clip, target)?;
    let _ = how;
    Ok(device)
}

/// How a clip actually got there. Worth surfacing: the two routes have very
/// different latency, and "why was that slow" is otherwise unanswerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Straight to the machine over the local network.
    Lan,
    /// Left in the shared folder for it to pick up.
    Folder,
}

/// A machine we could send to, and how we could reach it.
#[derive(Debug, Clone)]
pub struct Reachable {
    pub device_id: String,
    pub name: String,
    /// Present when the machine is on this network right now.
    pub lan: Option<std::net::SocketAddr>,
    /// True when it also has an inbox in the shared folder.
    pub via_folder: bool,
}

/// Every machine reachable by either route, LAN-visible ones first.
///
/// The two transports discover peers independently — one over mDNS, one from
/// the shared folder — and a machine may appear in both. Merging by device id
/// means it is one entry with two possible routes rather than a confusing
/// duplicate.
pub fn reachable_devices() -> Vec<Reachable> {
    let mut by_id: std::collections::BTreeMap<String, Reachable> = Default::default();

    for peer in crate::lan_discovery::cached_peers() {
        by_id.insert(
            peer.device_id.clone(),
            Reachable {
                device_id: peer.device_id,
                name: peer.name,
                lan: Some(peer.addr),
                via_folder: false,
            },
        );
    }

    if let Some(root) = crate::devices::sync_root() {
        for device in crate::devices::peers(&root) {
            by_id
                .entry(device.id.clone())
                .and_modify(|r| r.via_folder = true)
                .or_insert(Reachable {
                    device_id: device.id,
                    name: device.name,
                    lan: None,
                    via_folder: true,
                });
        }
    }

    let mut out: Vec<Reachable> = by_id.into_values().collect();
    // LAN-reachable machines first: they are the fast route, and when the user
    // has to pick, the better option should be the one they see.
    out.sort_by_key(|r| (r.lan.is_none(), r.name.to_lowercase()));
    out
}

/// Resolve what the user typed against everything reachable.
///
/// Same rules as the folder-only resolver: one candidate needs no naming, an
/// exact name beats a substring, and ambiguity is reported rather than guessed.
pub fn resolve_reachable(target: Option<&str>) -> Result<Reachable, String> {
    let candidates = reachable_devices();
    if candidates.is_empty() {
        return Err(
            "No other machines found. Run clipd on the other machine — on the same \
             network, or sharing the same sync folder — then try again."
                .into(),
        );
    }

    let Some(query) = target.map(str::trim).filter(|q| !q.is_empty()) else {
        return match candidates.len() {
            1 => Ok(candidates.into_iter().next().expect("checked len")),
            _ => Err(format!(
                "More than one machine to send to — name one of: {}",
                candidates
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        };
    };

    let q = query.to_lowercase();
    let exact: Vec<&Reachable> = candidates
        .iter()
        .filter(|c| c.name.to_lowercase() == q || c.device_id == query)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }

    let partial: Vec<&Reachable> = candidates
        .iter()
        .filter(|c| c.name.to_lowercase().contains(&q) || c.device_id.starts_with(query))
        .collect();
    match partial.len() {
        1 => Ok(partial[0].clone()),
        0 => Err(format!("No machine matching \"{query}\".")),
        _ => Err(format!(
            "\"{query}\" matches more than one machine: {}",
            partial
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Send, reporting which route carried it.
///
/// LAN is tried first when the machine is on the network: it is roughly a
/// thousand times faster and involves no third party. The folder is the
/// fallback, which also covers the machine being asleep or elsewhere.
pub fn send_clip_via(
    clip: &ClipEntry,
    target: Option<&str>,
) -> Result<(crate::devices::Device, Route), String> {
    let dest = resolve_reachable(target)?;
    let envelope = envelope_from_clip(clip)?;
    let device = crate::devices::Device {
        id: dest.device_id.clone(),
        name: dest.name.clone(),
        last_seen: Utc::now(),
    };

    let mut lan_error = None;
    if let Some(addr) = dest.lan {
        match send_over_lan(addr, &envelope) {
            Ok(()) => {
                record_last_send(&device, &envelope.id);
                return Ok((device, Route::Lan));
            }
            // Don't give up yet — the machine may have just slept, and the
            // folder still gets the clip there eventually.
            Err(e) => lan_error = Some(e),
        }
    }

    if dest.via_folder {
        if let Some(root) = crate::devices::sync_root() {
            deliver(&root, &dest.device_id, &envelope)?;
            record_last_send(&device, &envelope.id);
            return Ok((device, Route::Folder));
        }
    }

    Err(match lan_error {
        // Surface the real reason — usually "not paired", which is actionable.
        Some(e) => e,
        None => format!(
            "{} isn't reachable right now. Put both machines on the same network, \
             or set a shared folder with `clipd sync-root`.",
            dest.name
        ),
    })
}

fn send_over_lan(addr: std::net::SocketAddr, envelope: &Envelope) -> Result<(), String> {
    let identity = crate::lan_identity::Identity::load_or_create()?;
    let trusted = |device_id: &str, key: &x25519_dalek::PublicKey| {
        crate::lan_identity::is_trusted(device_id, key)
    };
    crate::lan::send_envelope(addr, envelope, &identity, &trusted).map(|_| ())
}

/// The most recent send, kept so it can be taken back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSend {
    pub device_id: String,
    pub device_name: String,
    pub envelope_id: String,
    pub sent_at: DateTime<Utc>,
}

fn last_send_path() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clipd")
        .join("last-send.json")
}

fn record_last_send(device: &crate::devices::Device, envelope_id: &str) {
    let record = LastSend {
        device_id: device.id.clone(),
        device_name: device.name.clone(),
        envelope_id: envelope_id.to_string(),
        sent_at: Utc::now(),
    };
    match serde_json::to_vec(&record) {
        Ok(bytes) => {
            if let Err(e) = crate::devices::write_atomically(&last_send_path(), &bytes) {
                log::debug!("couldn't record the last send: {e}");
            }
        }
        Err(e) => log::debug!("couldn't encode the last send: {e}"),
    }
}

/// What was sent last, if anything.
pub fn last_send() -> Option<LastSend> {
    let bytes = std::fs::read(last_send_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Take back an envelope that hasn't been collected yet.
///
/// This is what makes "no confirmation dialog" defensible: a send costs one
/// keystroke and a mistake costs one more, so the common case pays nothing.
/// Returns `false` when the other Mac already picked it up — at that point it
/// is in their history and no longer ours to withdraw.
pub fn recall(root: &Path, target_device_id: &str, envelope_id: &str) -> Result<bool, String> {
    let path = crate::devices::inbox_dir(root, target_device_id)
        .join(format!("{envelope_id}.{ENVELOPE_EXT}"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Couldn't take that send back: {e}")),
    }
}

/// Take back the most recent send. Returns the device it was headed to.
pub fn recall_last() -> Result<(LastSend, bool), String> {
    let root = crate::devices::sync_root().ok_or("iCloud Drive isn't set up on this Mac.")?;
    let last = last_send().ok_or("Nothing has been sent from this Mac yet.")?;
    let recalled = recall(&root, &last.device_id, &last.envelope_id)?;
    if recalled {
        let _ = std::fs::remove_file(last_send_path());
    }
    Ok((last, recalled))
}

/// Ask iCloud to materialise any envelopes it has evicted from local storage.
///
/// iCloud Drive replaces the contents of files it considers cold with a
/// `.name.icloud` placeholder. An inbox that has sat unread across a restart
/// can be full of them, and they are invisible to a plain directory read — so
/// nudge them back before looking.
pub fn request_downloads(root: &Path, device_id: &str) {
    let dir = crate::devices::inbox_dir(root, device_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for path in entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "icloud"))
    {
        // brctl ships with macOS and is the supported way to force a download
        // from a command line. Best-effort: if it is missing or fails, the
        // envelope simply stays pending until iCloud fetches it on its own.
        match std::process::Command::new("brctl")
            .arg("download")
            .arg(&path)
            .output()
        {
            Ok(o) if o.status.success() => {
                log::debug!("requested iCloud download of {}", path.display())
            }
            Ok(o) => log::debug!(
                "brctl download failed for {}: {}",
                path.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => log::debug!("couldn't run brctl: {e}"),
        }
    }
}

/// Turn a received envelope into a clip belonging to *this* Mac.
///
/// `blob_dir` is where inlined file bytes are written. Nothing from the sending
/// machine's filesystem survives this step: paths are rebuilt locally, because
/// a path from another Mac names nothing here.
pub fn clip_from_envelope(envelope: &Envelope, blob_dir: &Path) -> Result<ClipEntry, String> {
    let mut clip = match &envelope.payload {
        Payload::Text {
            content,
            content_type,
        } => {
            let mut c = ClipEntry::new(content.clone(), Some(envelope.from_name.clone()), None);
            // Trust the sender's classification over re-detecting: it had the
            // original context, and re-running detect() can disagree.
            c.content_type = content_type.clone();
            c
        }
        Payload::Image { png, ocr_text } => {
            let (w, h, rgba) = decode_png(png)?;
            let saved = crate::images::save_rgba_image(w as usize, h as usize, &rgba)
                .map_err(|e| format!("Couldn't save the received image: {e}"))?;
            ClipEntry::new_image(
                saved.hash,
                saved.full_path.to_string_lossy().into_owned(),
                saved.thumb_path.to_string_lossy().into_owned(),
                ocr_text.clone(),
                Some(envelope.from_name.clone()),
                saved.width,
                saved.height,
            )
        }
        Payload::Files { files } => {
            if files.is_empty() {
                return Err("That envelope had no files in it.".into());
            }
            std::fs::create_dir_all(blob_dir)
                .map_err(|e| format!("Couldn't create the file store: {e}"))?;

            let mut refs = Vec::with_capacity(files.len());
            for f in files {
                let name = sanitize_name(&f.name);
                let hash = {
                    use sha2::{Digest, Sha256};
                    let mut h = Sha256::new();
                    h.update(&f.bytes);
                    format!("{:x}", h.finalize())
                };
                let blob = match Path::new(&name).extension().and_then(|e| e.to_str()) {
                    Some(ext) if !ext.is_empty() => blob_dir.join(format!("{hash}.{ext}")),
                    _ => blob_dir.join(&hash),
                };
                if !blob.exists() {
                    crate::devices::write_atomically(&blob, &f.bytes)?;
                }
                refs.push(FileRef {
                    name: name.clone(),
                    // The blob *is* the original as far as this Mac is
                    // concerned — there is no earlier local path to point at.
                    original_path: blob.to_string_lossy().into_owned(),
                    blob_path: Some(blob.to_string_lossy().into_owned()),
                    size: f.bytes.len() as u64,
                });
            }
            ClipEntry::new_files(refs, Some(envelope.from_name.clone()))
        }
    };

    // Provenance: where it came from is the sending Mac, not an app here.
    clip.source_title = Some(format!("Sent from {}", envelope.from_name));
    clip.timestamp = envelope.sent_at;
    Ok(clip)
}

/// Strip anything from a sender-supplied filename that could escape the blob
/// directory. The name arrives over a channel we do not control, so it is only
/// ever a display string plus an extension — never a path.
fn sanitize_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .trim_start_matches('.');
    if base.is_empty() {
        "received-file".to_string()
    } else {
        base.to_string()
    }
}

fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| format!("Couldn't decode the received image: {e}"))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Ok((w, h, img.into_raw()))
}

fn new_envelope_id() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    h.update(std::process::id().to_le_bytes());
    h.update(crate::devices::device_id().as_bytes());
    format!("{:x}", h.finalize())[..24].to_string()
}

/// base64 for the byte fields, so an envelope stays valid JSON.
mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::inbox_dir;

    fn text_envelope(content: &str) -> Envelope {
        Envelope {
            id: "env1".into(),
            from_device: "aaa111".into(),
            from_name: "MacBook Air".into(),
            sent_at: Utc::now(),
            payload: Payload::Text {
                content: content.into(),
                content_type: ContentType::Url,
            },
        }
    }

    #[test]
    fn a_link_survives_the_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = text_envelope("https://example.com/thing");
        deliver(tmp.path(), "target-mac", &env).expect("deliver");

        let waiting = pending(tmp.path(), "target-mac");
        assert_eq!(waiting.len(), 1);
        let received = waiting[0].1.as_ref().expect("parsed");
        assert_eq!(received, &env);

        let clip = clip_from_envelope(received, &tmp.path().join("blobs")).expect("materialize");
        assert_eq!(clip.content, "https://example.com/thing");
        assert_eq!(clip.content_type, ContentType::Url);
        assert_eq!(clip.source_app.as_deref(), Some("MacBook Air"));
        assert_eq!(clip.source_title.as_deref(), Some("Sent from MacBook Air"));
    }

    #[test]
    fn files_are_rebuilt_locally_with_no_trace_of_the_sender_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blobs = tmp.path().join("blobs");
        let env = Envelope {
            payload: Payload::Files {
                files: vec![InlineFile {
                    name: "report.pdf".into(),
                    bytes: b"%PDF pretend".to_vec(),
                }],
            },
            ..text_envelope("")
        };

        let clip = clip_from_envelope(&env, &blobs).expect("materialize");
        assert_eq!(clip.content_type, ContentType::File);
        assert_eq!(clip.files.len(), 1);

        let f = &clip.files[0];
        assert_eq!(f.name, "report.pdf");
        // Every recorded path must be inside *our* blob dir.
        assert!(f.original_path.starts_with(blobs.to_str().unwrap()), "{f:?}");
        assert!(f.blob_path.as_ref().unwrap().starts_with(blobs.to_str().unwrap()));
        assert_eq!(std::fs::read(f.resolve().unwrap()).unwrap(), b"%PDF pretend");
    }

    #[test]
    fn a_malicious_filename_cannot_escape_the_blob_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blobs = tmp.path().join("blobs");
        let env = Envelope {
            payload: Payload::Files {
                files: vec![InlineFile {
                    name: "../../../../../../tmp/pwned.sh".into(),
                    bytes: b"rm -rf /".to_vec(),
                }],
            },
            ..text_envelope("")
        };

        let clip = clip_from_envelope(&env, &blobs).expect("materialize");
        let written = clip.files[0].resolve().expect("blob written");
        assert_eq!(
            written.parent(),
            Some(blobs.as_path()),
            "sender-supplied names must not choose the directory"
        );
        assert!(!Path::new("/tmp/pwned.sh").exists());
    }

    #[test]
    fn an_oversized_envelope_is_refused_before_it_is_sent() {
        let big = Envelope {
            payload: Payload::Files {
                files: vec![InlineFile {
                    name: "huge.bin".into(),
                    bytes: vec![0u8; (MAX_ENVELOPE_BYTES + 1024) as usize],
                }],
            },
            ..text_envelope("")
        };
        let err = encode(&big).unwrap_err();
        assert!(err.contains("limit for sending"), "{err}");

        // And nothing is left in the inbox when a send is refused.
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(deliver(tmp.path(), "target-mac", &big).is_err());
        assert!(pending(tmp.path(), "target-mac").is_empty());
    }

    #[test]
    fn envelopes_are_delivered_oldest_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for (i, content) in ["first", "second", "third"].iter().enumerate() {
            let mut env = text_envelope(content);
            env.id = format!("env{i}");
            env.sent_at = Utc::now() + chrono::Duration::seconds(i as i64);
            deliver(tmp.path(), "target-mac", &env).expect("deliver");
        }
        let order: Vec<String> = pending(tmp.path(), "target-mac")
            .into_iter()
            .map(|(_, e)| match e.unwrap().payload {
                Payload::Text { content, .. } => content,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(order, vec!["first", "second", "third"]);
    }

    #[test]
    fn a_corrupt_envelope_is_surfaced_not_swallowed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = inbox_dir(tmp.path(), "target-mac");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(format!("bad.{ENVELOPE_EXT}")), b"{ truncated").expect("write");

        let waiting = pending(tmp.path(), "target-mac");
        assert_eq!(waiting.len(), 1);
        assert!(waiting[0].1.is_err(), "a bad envelope must be reported");
    }

    #[test]
    fn unrelated_files_in_an_inbox_are_ignored() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = inbox_dir(tmp.path(), "target-mac");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // iCloud sprinkles these around; they are not ours to read.
        std::fs::write(dir.join(".DS_Store"), b"junk").expect("write");
        std::fs::write(dir.join("notes.txt"), b"junk").expect("write");
        assert!(pending(tmp.path(), "target-mac").is_empty());
    }

    #[test]
    fn an_image_clip_becomes_a_local_png() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A 2x2 red PNG, encoded here so the test owns its fixture.
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).expect("encode");

        let env = Envelope {
            payload: Payload::Image {
                png: png.into_inner(),
                ocr_text: Some("hello".into()),
            },
            ..text_envelope("")
        };
        let clip = clip_from_envelope(&env, &tmp.path().join("blobs")).expect("materialize");
        assert_eq!(clip.content_type, ContentType::Image);
        let path = clip.image_path.expect("image written locally");
        assert!(Path::new(&path).exists());
        assert_eq!(clip.ocr_text.as_deref(), Some("hello"));
        // Clean up: this one legitimately writes into the real image store.
        crate::images::delete_image_files(Some(&path), clip.thumb_path.as_deref());
    }

    #[test]
    fn an_uncollected_send_can_be_taken_back() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = text_envelope("oops wrong Mac");
        deliver(tmp.path(), "target-mac", &env).expect("deliver");
        assert_eq!(pending(tmp.path(), "target-mac").len(), 1);

        assert!(recall(tmp.path(), "target-mac", &env.id).expect("recall"));
        assert!(pending(tmp.path(), "target-mac").is_empty());
    }

    #[test]
    fn recalling_a_collected_send_reports_it_is_too_late() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = text_envelope("already read");
        deliver(tmp.path(), "target-mac", &env).expect("deliver");

        // The other Mac collects it, the way the inbox loop does.
        for (path, _) in pending(tmp.path(), "target-mac") {
            std::fs::remove_file(path).expect("collect");
        }

        assert!(
            !recall(tmp.path(), "target-mac", &env.id).expect("recall"),
            "a collected clip is theirs now, not ours to withdraw"
        );
    }

    #[test]
    fn recall_does_not_disturb_other_pending_sends() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut keep = text_envelope("keep me");
        keep.id = "keep".into();
        let mut drop = text_envelope("drop me");
        drop.id = "drop".into();
        deliver(tmp.path(), "target-mac", &keep).expect("deliver");
        deliver(tmp.path(), "target-mac", &drop).expect("deliver");

        assert!(recall(tmp.path(), "target-mac", "drop").expect("recall"));
        let left = pending(tmp.path(), "target-mac");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].1.as_ref().unwrap().id, "keep");
    }

    #[test]
    fn summaries_name_the_sending_mac() {
        assert_eq!(text_envelope("https://x.com").summary(), "Link from MacBook Air");
        let files = Envelope {
            payload: Payload::Files {
                files: vec![InlineFile {
                    name: "a.pdf".into(),
                    bytes: vec![],
                }],
            },
            ..text_envelope("")
        };
        assert_eq!(files.summary(), "a.pdf from MacBook Air");
    }
}
