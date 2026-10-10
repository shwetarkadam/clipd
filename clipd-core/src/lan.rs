//! Sending a clip straight to another machine over the local network.
//!
//! The folder transport is a dead drop: the sender writes, the receiver reads,
//! and something else moves the bytes. This is the opposite — a direct,
//! authenticated, encrypted TCP connection between two clipd instances, with
//! nothing in the middle. It is roughly a thousand times faster, needs no cloud
//! account or storage, and works between a Mac and a PC.
//!
//! What it carries is the *same* [`Envelope`] the folder transport uses, so
//! file, image and text handling, the receiving side's path rebuilding, and
//! every test written for them apply unchanged. Only the delivery differs.
//!
//! ## The handshake
//!
//! Each side has a long-lived X25519 key ([`crate::lan_identity`]) plus a fresh
//! ephemeral key per connection. Both are mixed into the session key:
//!
//! - **ephemeral × ephemeral** gives forward secrecy — recording today's
//!   traffic and stealing a machine tomorrow does not decrypt it.
//! - **static × static** gives mutual authentication — only the two machines
//!   that paired can derive it, so an impostor cannot read or forge a clip
//!   even if it can talk to the port.
//!
//! Both feed HKDF, salted with a transcript hash over every public value in the
//! exchange, which binds the keys to *this* connection and stops a recorded
//! handshake being replayed against a different one.
//!
//! ## Messages
//!
//! A clip can be hundreds of megabytes, so it is not sealed as one block: it
//! goes as a stream of [`CHUNK_BYTES`] chunks, each sealed on its own. Bytes
//! then flow from the first moment rather than after the whole clip has been
//! encrypted, so `IO_TIMEOUT` measures a silent peer, not a busy one. Each
//! chunk's nonce carries its sequence number and whether it is the last one,
//! so chunks cannot be reordered, dropped or replayed, and a message cannot be
//! cut short at a chunk boundary.

use crate::lan_identity::{decode_key, encode_key, Identity};
use crate::sync::Envelope;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use x25519_dalek::{EphemeralSecret, PublicKey};

/// The service clipd advertises and looks for over mDNS.
pub const SERVICE_TYPE: &str = "_clipd._tcp.local.";

/// Wire protocol version. Bumped when the handshake or message format changes
/// shape; mismatched peers are told to update rather than failing with a
/// decryption error.
///
/// v2: messages are streamed as sealed chunks instead of one sealed frame.
pub const PROTOCOL_VERSION: u32 = 2;

/// Largest frame, or sealed message, accepted, in bytes.
///
/// Generous compared to the folder transport's 25 MB, because a LAN is a real
/// network rather than a sync service — but still bounded, so a hostile or
/// buggy peer cannot make us allocate without limit.
pub const MAX_FRAME_BYTES: u32 = 256 * 1024 * 1024;

/// Plaintext carried by each sealed chunk of a message.
pub const CHUNK_BYTES: usize = 1024 * 1024;

/// Poly1305 tag appended to every sealed chunk.
const TAG_BYTES: usize = 16;

/// How long a peer may go silent before giving up on it.
const IO_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait for a peer to answer at all.
///
/// Much shorter than [`IO_TIMEOUT`]: a machine on the same network answers in
/// milliseconds, and the usual reason it doesn't is that it has gone to sleep
/// since mDNS last saw it. Waiting the full idle timeout for that only delays
/// the fallback to the shared folder, which is the route that will work.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Opening message from whoever dialled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub device_id: String,
    pub name: String,
    /// Long-lived identity key, base64.
    pub static_key: String,
    /// Per-connection key, base64.
    pub ephemeral_key: String,
}

/// The answer. A refusal carries a reason so the sender can say something
/// useful instead of "connection closed".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum HelloAck {
    Ok {
        device_id: String,
        name: String,
        static_key: String,
        ephemeral_key: String,
    },
    Refused {
        reason: String,
    },
}

/// What the receiver says once it has the clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Receipt {
    Accepted { clip_id: i64 },
    Failed { reason: String },
}

/// An established, encrypted channel.
///
/// Keys are per-direction so the two sides never encrypt different plaintexts
/// under the same key and nonce. Within a direction every chunk takes the next
/// sequence number, so no nonce repeats under a key either.
pub struct Session {
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_seq: u64,
    recv_seq: u64,
    /// The other side, as it identified itself. Already checked against the
    /// trust store by the time a `Session` exists.
    pub peer_device_id: String,
    pub peer_name: String,
}

impl Session {
    fn cipher(key: &[u8; 32]) -> Result<ChaCha20Poly1305, String> {
        ChaCha20Poly1305::new_from_slice(key).map_err(|e| format!("Bad session key: {e}"))
    }

    fn new(
        send_key: [u8; 32],
        recv_key: [u8; 32],
        peer_device_id: String,
        peer_name: String,
    ) -> Session {
        Session {
            send_key,
            recv_key,
            send_seq: 0,
            recv_seq: 0,
            peer_device_id,
            peer_name,
        }
    }

    /// The chunk's sequence number, and a flag marking the final chunk of a
    /// message. Sealing the flag into the nonce is what stops a truncated
    /// message from passing as a complete one.
    fn nonce(seq: u64, last: bool) -> Nonce {
        let mut n = [0u8; 12];
        n[..8].copy_from_slice(&seq.to_be_bytes());
        n[11] = last as u8;
        *Nonce::from_slice(&n)
    }

    fn next(seq: &mut u64) -> Result<u64, String> {
        let this = *seq;
        *seq = this
            .checked_add(1)
            .ok_or("This connection has carried too much; reconnect.")?;
        Ok(this)
    }

    /// Seal and send one message, as a stream of chunks.
    pub fn send(&mut self, w: &mut impl Write, plaintext: &[u8]) -> Result<(), String> {
        if plaintext.len() > MAX_FRAME_BYTES as usize {
            return Err("That clip is too large to send.".into());
        }
        let cipher = Self::cipher(&self.send_key)?;
        // An empty message is still one (empty, final) chunk.
        let count = plaintext.len().div_ceil(CHUNK_BYTES).max(1);
        let mut chunks = plaintext.chunks(CHUNK_BYTES);
        for i in 0..count {
            let chunk = chunks.next().unwrap_or_default();
            let last = i + 1 == count;
            let nonce = Self::nonce(Self::next(&mut self.send_seq)?, last);
            let sealed = cipher
                .encrypt(&nonce, chunk)
                .map_err(|_| "Couldn't encrypt the clip.".to_string())?;
            // The flag travels in the clear so the receiver knows which nonce
            // to try; flipping it makes the chunk fail authentication.
            let mut frame = Vec::with_capacity(1 + sealed.len());
            frame.push(last as u8);
            frame.extend_from_slice(&sealed);
            write_frame(w, &frame)?;
        }
        Ok(())
    }

    /// Receive and open one message sent with [`Session::send`].
    pub fn recv(&mut self, r: &mut impl Read) -> Result<Vec<u8>, String> {
        let cipher = Self::cipher(&self.recv_key)?;
        let mut message = Vec::new();
        loop {
            let frame = read_frame_max(r, (1 + CHUNK_BYTES + TAG_BYTES) as u32)?;
            let (last, sealed) = match frame.split_first() {
                Some((0, rest)) => (false, rest),
                Some((1, rest)) => (true, rest),
                _ => return Err(integrity_failure()),
            };
            let nonce = Self::nonce(Self::next(&mut self.recv_seq)?, last);
            let chunk = cipher
                .decrypt(&nonce, sealed)
                .map_err(|_| integrity_failure())?;
            if message.len() + chunk.len() > MAX_FRAME_BYTES as usize {
                return Err("That machine sent a message over the size limit.".into());
            }
            message.extend_from_slice(&chunk);
            if last {
                return Ok(message);
            }
        }
    }
}

/// Authentication failure is not a decoding hiccup: it means the bytes were
/// altered, or the peer is not who it claimed.
fn integrity_failure() -> String {
    "The clip failed its integrity check — it was altered in transit, \
     or that machine isn't who it said it was."
        .to_string()
}

// ── Framing ────────────────────────────────────────────────────────────────

/// Write a length-prefixed frame.
pub fn write_frame(w: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    let len: u32 = bytes
        .len()
        .try_into()
        .map_err(|_| "That clip is too large to send.".to_string())?;
    if len > MAX_FRAME_BYTES {
        return Err("That clip is too large to send.".into());
    }
    w.write_all(&len.to_be_bytes())
        .map_err(|e| format!("Connection lost while sending: {e}"))?;
    w.write_all(bytes)
        .map_err(|e| format!("Connection lost while sending: {e}"))?;
    w.flush()
        .map_err(|e| format!("Connection lost while sending: {e}"))
}

/// Read a length-prefixed frame, refusing anything over [`MAX_FRAME_BYTES`].
pub fn read_frame(r: &mut impl Read) -> Result<Vec<u8>, String> {
    read_frame_max(r, MAX_FRAME_BYTES)
}

fn read_frame_max(r: &mut impl Read, max: u32) -> Result<Vec<u8>, String> {
    let mut len_bytes = [0u8; 4];
    r.read_exact(&mut len_bytes)
        .map_err(|e| format!("Connection lost while receiving: {e}"))?;
    let len = u32::from_be_bytes(len_bytes);
    if len > max {
        // Refuse *before* allocating — otherwise a four-byte lie is a denial of
        // service.
        return Err(format!(
            "That machine announced a {len}-byte message, which is over the limit."
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Connection lost while receiving: {e}"))?;
    Ok(buf)
}

fn write_json<T: Serialize>(w: &mut impl Write, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| format!("Couldn't encode a message: {e}"))?;
    write_frame(w, &bytes)
}

fn read_json<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> Result<T, String> {
    let bytes = read_frame(r)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Couldn't understand that machine: {e}"))
}

// ── Handshake ──────────────────────────────────────────────────────────────

/// Decides whether a peer offering this identity may proceed.
///
/// A parameter rather than a direct call into the trust store so that pairing
/// can accept a new machine, normal operation can refuse one, and tests can do
/// either without touching the user's real files.
pub type TrustCheck<'a> = &'a dyn Fn(&str, &PublicKey) -> bool;

/// Everything public that was exchanged, hashed. Salting the key derivation
/// with this binds the session key to this exact handshake.
#[allow(clippy::too_many_arguments)]
fn transcript(
    initiator_static: &PublicKey,
    initiator_eph: &PublicKey,
    responder_static: &PublicKey,
    responder_eph: &PublicKey,
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"clipd-lan-v1");
    h.update(PROTOCOL_VERSION.to_be_bytes());
    h.update(initiator_static.as_bytes());
    h.update(initiator_eph.as_bytes());
    h.update(responder_static.as_bytes());
    h.update(responder_eph.as_bytes());
    h.finalize().into()
}

/// Derive the two directional keys from the two shared secrets.
fn derive_keys(
    ee: &[u8; 32],
    ss: &[u8; 32],
    transcript: &[u8; 32],
) -> Result<([u8; 32], [u8; 32]), String> {
    let mut ikm = Vec::with_capacity(64);
    ikm.extend_from_slice(ee);
    ikm.extend_from_slice(ss);

    let hk = Hkdf::<Sha256>::new(Some(transcript), &ikm);
    let mut out = [0u8; 64];
    hk.expand(b"clipd-lan-session-v1", &mut out)
        .map_err(|e| format!("Key derivation failed: {e}"))?;

    let mut i2r = [0u8; 32];
    let mut r2i = [0u8; 32];
    i2r.copy_from_slice(&out[..32]);
    r2i.copy_from_slice(&out[32..]);
    Ok((i2r, r2i))
}

/// Dial side of the handshake.
pub fn handshake_initiator(
    stream: &mut TcpStream,
    identity: &Identity,
    trusted: TrustCheck<'_>,
) -> Result<Session, String> {
    let eph_secret = EphemeralSecret::random_from_rng(rand_core::OsRng);
    let eph_public = PublicKey::from(&eph_secret);

    write_json(
        stream,
        &Hello {
            version: PROTOCOL_VERSION,
            device_id: crate::devices::device_id(),
            name: crate::devices::device_name(),
            static_key: identity.public_key_b64(),
            ephemeral_key: encode_key(&eph_public),
        },
    )?;

    let ack: HelloAck = read_json(stream)?;
    let (device_id, name, static_key, ephemeral_key) = match ack {
        HelloAck::Ok {
            device_id,
            name,
            static_key,
            ephemeral_key,
        } => (device_id, name, static_key, ephemeral_key),
        HelloAck::Refused { reason } => return Err(reason),
    };

    let peer_static = decode_key(&static_key)?;
    let peer_eph = decode_key(&ephemeral_key)?;

    // Check the responder too. Without this, only the receiver is authenticated
    // and anyone could impersonate the machine you meant to send to.
    if !trusted(&device_id, &peer_static) {
        return Err(format!(
            "{name} isn't paired with this machine, or its identity has changed. \
             Run `clipd pair` on both."
        ));
    }

    let ee = eph_secret.diffie_hellman(&peer_eph);
    let ss = identity.secret().diffie_hellman(&peer_static);
    let t = transcript(&identity.public_key(), &eph_public, &peer_static, &peer_eph);
    let (i2r, r2i) = derive_keys(ee.as_bytes(), ss.as_bytes(), &t)?;

    Ok(Session::new(i2r, r2i, device_id, name))
}

/// Listen side of the handshake. Returns the session plus the peer's static
/// key, which pairing needs in order to record the machine as trusted.
pub fn handshake_responder(
    stream: &mut TcpStream,
    identity: &Identity,
    trusted: TrustCheck<'_>,
) -> Result<(Session, PublicKey), String> {
    let hello: Hello = read_json(stream)?;

    if hello.version != PROTOCOL_VERSION {
        let reason = format!(
            "That machine speaks clipd protocol v{}, this one speaks v{PROTOCOL_VERSION}. \
             Update clipd on both.",
            hello.version
        );
        let _ = write_json(
            stream,
            &HelloAck::Refused {
                reason: reason.clone(),
            },
        );
        return Err(reason);
    }

    let peer_static = decode_key(&hello.static_key)?;
    let peer_eph = decode_key(&hello.ephemeral_key)?;

    if !trusted(&hello.device_id, &peer_static) {
        let reason = format!(
            "{} isn't paired with this machine. Run `clipd pair` on both.",
            hello.name
        );
        let _ = write_json(
            stream,
            &HelloAck::Refused {
                reason: reason.clone(),
            },
        );
        return Err(reason);
    }

    let eph_secret = EphemeralSecret::random_from_rng(rand_core::OsRng);
    let eph_public = PublicKey::from(&eph_secret);

    write_json(
        stream,
        &HelloAck::Ok {
            device_id: crate::devices::device_id(),
            name: crate::devices::device_name(),
            static_key: identity.public_key_b64(),
            ephemeral_key: encode_key(&eph_public),
        },
    )?;

    let ee = eph_secret.diffie_hellman(&peer_eph);
    let ss = identity.secret().diffie_hellman(&peer_static);
    let t = transcript(&peer_static, &peer_eph, &identity.public_key(), &eph_public);
    let (i2r, r2i) = derive_keys(ee.as_bytes(), ss.as_bytes(), &t)?;

    // Mirrored: what the initiator sends is what we receive.
    Ok((
        Session::new(r2i, i2r, hello.device_id, hello.name),
        peer_static,
    ))
}

// ── Sending and receiving ──────────────────────────────────────────────────

/// Connect to a peer and hand over one envelope. Returns its id on the far side.
pub fn send_envelope(
    addr: std::net::SocketAddr,
    envelope: &Envelope,
    identity: &Identity,
    trusted: TrustCheck<'_>,
) -> Result<i64, String> {
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("Couldn't reach that machine: {e}"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
    // Clips are small and latency is the whole point; don't wait to coalesce.
    stream.set_nodelay(true).ok();

    let mut session = handshake_initiator(&mut stream, identity, trusted)?;

    let plaintext =
        serde_json::to_vec(envelope).map_err(|e| format!("Couldn't package that clip: {e}"))?;
    session.send(&mut stream, &plaintext)?;

    match serde_json::from_slice::<Receipt>(&session.recv(&mut stream)?)
        .map_err(|e| format!("Couldn't understand the reply: {e}"))?
    {
        Receipt::Accepted { clip_id } => Ok(clip_id),
        Receipt::Failed { reason } => Err(reason),
    }
}

/// Serve one inbound connection: handshake, decrypt, hand the envelope to
/// `accept`, and report back what happened.
pub fn serve_connection(
    stream: &mut TcpStream,
    identity: &Identity,
    trusted: TrustCheck<'_>,
    accept: &mut dyn FnMut(Envelope, &str) -> Result<i64, String>,
) -> Result<(), String> {
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();

    let (mut session, _peer_static) = handshake_responder(stream, identity, trusted)?;
    let envelope: Envelope = serde_json::from_slice(&session.recv(stream)?)
        .map_err(|e| format!("Couldn't understand that clip: {e}"))?;

    let receipt = match accept(envelope, &session.peer_name) {
        Ok(clip_id) => Receipt::Accepted { clip_id },
        Err(reason) => Receipt::Failed { reason },
    };
    let bytes =
        serde_json::to_vec(&receipt).map_err(|e| format!("Couldn't encode the reply: {e}"))?;
    session.send(stream, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ContentType;
    use crate::sync::Payload;
    use std::net::{TcpListener, SocketAddr};
    use std::sync::mpsc;

    fn envelope(content: &str) -> Envelope {
        Envelope {
            id: "env-test".into(),
            from_device: "aaa111".into(),
            from_name: "MacBook Air".into(),
            sent_at: chrono::Utc::now(),
            payload: Payload::Text {
                content: content.into(),
                content_type: ContentType::Url,
            },
        }
    }

    fn allow_all() -> impl Fn(&str, &PublicKey) -> bool {
        |_, _| true
    }
    fn allow_none() -> impl Fn(&str, &PublicKey) -> bool {
        |_, _| false
    }

    /// Runs a one-shot server on a loopback port; returns its address and a
    /// channel carrying what it received.
    fn spawn_server(
        trusted: bool,
    ) -> (SocketAddr, mpsc::Receiver<Result<Envelope, String>>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = mpsc::channel();

        let handle = std::thread::spawn(move || {
            let identity = Identity::load_or_create().expect("identity");
            let (mut stream, _) = listener.accept().expect("accept");
            let check: Box<dyn Fn(&str, &PublicKey) -> bool> = if trusted {
                Box::new(allow_all())
            } else {
                Box::new(allow_none())
            };
            let mut got = |env: Envelope, _from: &str| {
                tx.send(Ok(env)).ok();
                Ok(42)
            };
            if let Err(e) = serve_connection(&mut stream, &identity, &*check, &mut got) {
                tx.send(Err(e)).ok();
            }
        });
        (addr, rx, handle)
    }

    #[test]
    fn a_clip_survives_an_encrypted_round_trip() {
        let (addr, rx, handle) = spawn_server(true);
        let identity = Identity::load_or_create().expect("identity");
        let sent = envelope("https://example.com/lan");

        let clip_id = send_envelope(addr, &sent, &identity, &allow_all()).expect("send");
        assert_eq!(clip_id, 42, "the receiver's clip id comes back to the sender");

        let received = rx.recv().expect("received").expect("no error");
        assert_eq!(received.id, sent.id);
        match received.payload {
            Payload::Text { content, .. } => assert_eq!(content, "https://example.com/lan"),
            _ => panic!("wrong payload kind"),
        }
        handle.join().expect("server thread");
    }

    #[test]
    fn an_unpaired_machine_is_refused_with_a_reason() {
        let (addr, rx, handle) = spawn_server(false);
        let identity = Identity::load_or_create().expect("identity");

        let err = send_envelope(addr, &envelope("secret"), &identity, &allow_all())
            .expect_err("must be refused");
        assert!(err.contains("isn't paired"), "{err}");

        // And the server saw no clip — refusal happens before any payload.
        let server_side = rx.recv().expect("server reported");
        assert!(server_side.is_err());
        handle.join().expect("server thread");
    }

    #[test]
    fn the_sender_also_checks_the_receiver() {
        let (addr, _rx, handle) = spawn_server(true);
        let identity = Identity::load_or_create().expect("identity");

        // Server is happy; the *client* refuses because it doesn't trust it.
        // Without this, anyone could impersonate the machine you meant to send to.
        let err = send_envelope(addr, &envelope("x"), &identity, &allow_none())
            .expect_err("client must refuse an untrusted responder");
        assert!(err.contains("isn't paired") || err.contains("identity has changed"), "{err}");
        handle.join().expect("server thread");
    }

    /// Both ends of a session, sharing a key in the sender → receiver direction.
    fn session_pair(key: [u8; 32]) -> (Session, Session) {
        let other = [0xEEu8; 32];
        (
            Session::new(key, other, "x".into(), "x".into()),
            Session::new(other, key, "x".into(), "x".into()),
        )
    }

    /// Split a sent message back into its length-prefixed frames.
    fn frames_of(mut wire: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        while !wire.is_empty() {
            frames.push(read_frame(&mut wire).expect("frame"));
        }
        frames
    }

    fn reframe(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut wire = Vec::new();
        for f in frames {
            write_frame(&mut wire, f).expect("write");
        }
        wire
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let (k1, k2) = derive_keys(&[7u8; 32], &[9u8; 32], &[3u8; 32]).expect("derive");
        let (mut tx, mut rx) = session_pair(k1);

        let mut wire = Vec::new();
        tx.send(&mut wire, b"a link").expect("send");
        assert_eq!(rx.recv(&mut wire.as_slice()).expect("recv"), b"a link");

        // Flip one bit of the ciphertext (past the length prefix and the
        // last-chunk flag) and it must not decrypt.
        let (mut tx, mut rx) = session_pair(k1);
        let mut wire = Vec::new();
        tx.send(&mut wire, b"a link").expect("send");
        wire[5] ^= 0x01;
        let err = rx.recv(&mut wire.as_slice()).expect_err("must reject");
        assert!(err.contains("integrity check"), "{err}");

        // A different key must not open it either.
        let (mut tx, _) = session_pair(k1);
        let (_, mut wrong) = session_pair(k2);
        let mut wire = Vec::new();
        tx.send(&mut wire, b"a link").expect("send");
        assert!(wrong.recv(&mut wire.as_slice()).is_err());
    }

    #[test]
    fn a_large_message_travels_as_many_chunks() {
        let (mut tx, mut rx) = session_pair([5u8; 32]);
        let message: Vec<u8> = (0..CHUNK_BYTES * 3 + 17).map(|i| i as u8).collect();

        let mut wire = Vec::new();
        tx.send(&mut wire, &message).expect("send");
        assert_eq!(
            frames_of(&wire).len(),
            4,
            "streamed, not sealed as one block"
        );
        assert_eq!(rx.recv(&mut wire.as_slice()).expect("recv"), message);

        // The sequence carries on, so a second message on the same session
        // uses fresh nonces and still opens.
        let mut wire = Vec::new();
        tx.send(&mut wire, b"").expect("send empty");
        assert_eq!(rx.recv(&mut wire.as_slice()).expect("recv empty"), b"");
    }

    #[test]
    fn a_message_cut_short_or_reordered_is_rejected() {
        let message = vec![0xABu8; CHUNK_BYTES * 2 + 1];
        let mut wire = Vec::new();
        session_pair([5u8; 32])
            .0
            .send(&mut wire, &message)
            .expect("send");
        let frames = frames_of(&wire);

        // Dropping the final chunk leaves the receiver waiting for it — never
        // handing over a truncated clip as if it were whole.
        let truncated = reframe(&frames[..2]);
        assert!(session_pair([5u8; 32])
            .1
            .recv(&mut truncated.as_slice())
            .is_err());

        // Relabelling an early chunk as the last one fails authentication.
        let mut relabelled = frames[..1].to_vec();
        relabelled[0][0] = 1;
        let err = session_pair([5u8; 32])
            .1
            .recv(&mut reframe(&relabelled).as_slice())
            .expect_err("must reject");
        assert!(err.contains("integrity check"), "{err}");

        // Swapping two chunks fails authentication too.
        let swapped = reframe(&[frames[1].clone(), frames[0].clone(), frames[2].clone()]);
        let err = session_pair([5u8; 32])
            .1
            .recv(&mut swapped.as_slice())
            .expect_err("must reject");
        assert!(err.contains("integrity check"), "{err}");
    }

    #[test]
    fn the_two_directions_use_different_keys() {
        let t = [3u8; 32];
        let (i2r, r2i) = derive_keys(&[1u8; 32], &[2u8; 32], &t).expect("derive");
        assert_ne!(i2r, r2i, "one key for both directions would reuse a nonce");
    }

    #[test]
    fn the_transcript_binds_the_keys_to_this_handshake() {
        let a = PublicKey::from(&x25519_dalek::StaticSecret::from([1u8; 32]));
        let b = PublicKey::from(&x25519_dalek::StaticSecret::from([2u8; 32]));
        let c = PublicKey::from(&x25519_dalek::StaticSecret::from([3u8; 32]));

        let base = transcript(&a, &b, &a, &b);
        assert_eq!(base, transcript(&a, &b, &a, &b), "deterministic");
        // Swap any participant and the derived keys must differ, so a recorded
        // handshake can't be replayed against a different pair of machines.
        assert_ne!(base, transcript(&a, &b, &a, &c));
        assert_ne!(base, transcript(&c, &b, &a, &b));
    }

    #[test]
    fn an_oversized_length_prefix_is_refused_before_allocating() {
        let mut framed: &[u8] = &[0xFF, 0xFF, 0xFF, 0xFF];
        let err = read_frame(&mut framed).expect_err("must refuse");
        assert!(err.contains("over the limit"), "{err}");
    }

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello clipd").expect("write");
        let mut cursor: &[u8] = &buf;
        assert_eq!(read_frame(&mut cursor).expect("read"), b"hello clipd");
    }
}
