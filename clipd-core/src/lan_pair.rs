//! The one-time handshake that lets two machines trust each other.
//!
//! Both machines run `clipd pair` at the same time. Each advertises itself on a
//! short-lived pairing service, finds the other, and they exchange long-lived
//! public keys. Both then display a six-digit code derived from *both* keys —
//! if a machine in the middle swapped either one, the numbers differ.
//!
//! The user comparing those two numbers is the entire security of this step,
//! which is why nothing here auto-confirms. It is deliberately separate from
//! the daemon's listener: pairing needs a human at each end, and the daemon has
//! nobody to ask.

use crate::lan::{read_frame, write_frame};
use crate::lan_identity::{decode_key, encode_key, short_auth_string, Identity, TrustedPeer};
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Service advertised only while a pairing is in progress.
///
/// Separate from the everyday `_clipd._tcp` so that simply being on the network
/// never looks like an invitation to pair.
pub const PAIR_SERVICE: &str = "_clipd-pair._tcp.local.";

/// How long `clipd pair` looks for the other machine before giving up.
pub const DISCOVER_TIMEOUT: Duration = Duration::from_secs(60);

/// What the two machines exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairHello {
    device_id: String,
    name: String,
    static_key: String,
}

/// Name of the lock that keeps two pairing sessions from overlapping.
const PAIRING_LOCK: &str = "pairing";

/// The other machine, plus the code the user must compare.
///
/// Not `Clone`: it owns the machine-wide pairing lock, and a second copy would
/// mean a second session believing it holds one.
pub struct PairingOffer {
    pub device_id: String,
    pub name: String,
    pub static_key: String,
    /// Six digits. Must match on both screens.
    pub confirmation_code: String,
    /// Held until the offer is accepted or dropped, so nothing else on this
    /// machine can start advertising [`PAIR_SERVICE`] in the meantime.
    _lock: Option<crate::lock::ProcessLock>,
}

impl std::fmt::Debug for PairingOffer {
    /// Hand-written because [`crate::lock::ProcessLock`] isn't `Debug`, and
    /// because the lock is an implementation detail nobody debugging a pairing
    /// wants to read.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingOffer")
            .field("device_id", &self.device_id)
            .field("name", &self.name)
            .field("confirmation_code", &self.confirmation_code)
            .finish_non_exhaustive()
    }
}

impl PairingOffer {
    /// Record this machine as trusted. Call only after the user has confirmed
    /// the codes match on both screens.
    pub fn accept(&self) -> Result<(), String> {
        crate::lan_identity::trust_peer(TrustedPeer {
            device_id: self.device_id.clone(),
            name: self.name.clone(),
            public_key: self.static_key.clone(),
            paired_at: chrono::Utc::now(),
        })
    }
}

/// Find the other machine and exchange keys, returning the code to compare.
///
/// Both sides call this. Whichever has the lexicographically smaller device id
/// dials; the other listens. That rule is arbitrary but must be *agreed*, or
/// both would sit waiting or both would dial.
pub fn discover_and_exchange(stop: Arc<AtomicBool>) -> Result<PairingOffer, String> {
    use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
    use std::sync::atomic::Ordering;

    // One pairing at a time. Two sessions would advertise the same service
    // twice and each could match the wrong machine — and with two codes on
    // screen, "do these match?" stops meaning anything.
    let lock = crate::lock::ProcessLock::try_acquire(PAIRING_LOCK).ok_or(
        "A pairing is already in progress on this machine. Finish or cancel it first.",
    )?;

    let identity = Identity::load_or_create()?;
    let me = crate::devices::device_id();
    let my_name = crate::devices::device_name();

    let listener =
        TcpListener::bind("0.0.0.0:0").map_err(|e| format!("Couldn't open a port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Couldn't read the port: {e}"))?
        .port();
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Couldn't configure the port: {e}"))?;

    let mdns = ServiceDaemon::new().map_err(|e| format!("Couldn't start mDNS: {e}"))?;
    let mut props = std::collections::HashMap::new();
    props.insert("device_id".to_string(), me.clone());
    props.insert("name".to_string(), my_name.clone());

    let service = ServiceInfo::new(
        PAIR_SERVICE,
        &me,
        &format!("{me}.local."),
        (),
        port,
        Some(props),
    )
    .map_err(|e| format!("Couldn't describe this machine: {e}"))?
    .enable_addr_auto();

    mdns.register(service)
        .map_err(|e| format!("Couldn't announce this machine: {e}"))?;
    let receiver = mdns
        .browse(PAIR_SERVICE)
        .map_err(|e| format!("Couldn't look for the other machine: {e}"))?;

    let deadline = Instant::now() + DISCOVER_TIMEOUT;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        // Someone dialled us.
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = mdns.shutdown();
                return respond(&mut stream, &identity, &me, &my_name, lock);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => log::debug!("pairing accept failed: {e}"),
        }

        // Or we found someone to dial.
        if let Ok(ServiceEvent::ServiceResolved(info)) =
            receiver.recv_timeout(Duration::from_millis(300))
        {
            let Some(their_id) = info.get_property_val_str("device_id") else {
                continue;
            };
            if their_id == me {
                continue; // our own announcement
            }
            // Only the smaller id dials, so the two never cross.
            if me.as_str() >= their_id {
                continue;
            }
            let Some(ip) = info.get_addresses().iter().next().copied() else {
                continue;
            };
            let addr = SocketAddr::new(ip, info.get_port());
            match TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
                Ok(mut stream) => {
                    let _ = mdns.shutdown();
                    return initiate(&mut stream, &identity, &me, &my_name, lock);
                }
                Err(e) => log::debug!("couldn't dial {addr}: {e}"),
            }
        }
    }

    let _ = mdns.shutdown();
    Err("Couldn't find the other machine. Make sure both are on the same network \
         and running `clipd pair` at the same time."
        .into())
}

fn initiate(
    stream: &mut TcpStream,
    identity: &Identity,
    me: &str,
    my_name: &str,
    lock: crate::lock::ProcessLock,
) -> Result<PairingOffer, String> {
    send_hello(stream, identity, me, my_name)?;
    let theirs = recv_hello(stream)?;
    build_offer(identity, theirs, Some(lock))
}

fn respond(
    stream: &mut TcpStream,
    identity: &Identity,
    me: &str,
    my_name: &str,
    lock: crate::lock::ProcessLock,
) -> Result<PairingOffer, String> {
    let theirs = recv_hello(stream)?;
    send_hello(stream, identity, me, my_name)?;
    build_offer(identity, theirs, Some(lock))
}

fn send_hello(
    stream: &mut TcpStream,
    identity: &Identity,
    me: &str,
    my_name: &str,
) -> Result<(), String> {
    let hello = PairHello {
        device_id: me.to_string(),
        name: my_name.to_string(),
        static_key: identity.public_key_b64(),
    };
    let bytes = serde_json::to_vec(&hello).map_err(|e| format!("Couldn't encode: {e}"))?;
    write_frame(stream, &bytes)
}

fn recv_hello(stream: &mut TcpStream) -> Result<PairHello, String> {
    let bytes = read_frame(stream)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Couldn't understand that machine: {e}"))
}

fn build_offer(
    identity: &Identity,
    theirs: PairHello,
    lock: Option<crate::lock::ProcessLock>,
) -> Result<PairingOffer, String> {
    let their_key = decode_key(&theirs.static_key)?;
    let mine = identity.public_key();

    // A machine presenting our own key is either a loopback mix-up or someone
    // replaying our announcement back at us. Either way it is not a peer.
    if their_key.as_bytes() == mine.as_bytes() {
        return Err("That machine presented this machine's own key.".into());
    }

    Ok(PairingOffer {
        confirmation_code: short_auth_string(&mine, &their_key),
        device_id: theirs.device_id,
        name: theirs.name,
        static_key: encode_key(&their_key),
        _lock: lock,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use x25519_dalek::{PublicKey, StaticSecret};

    fn hello_from(seed: u8, id: &str) -> PairHello {
        let key = PublicKey::from(&StaticSecret::from([seed; 32]));
        PairHello {
            device_id: id.into(),
            name: format!("Machine {id}"),
            static_key: encode_key(&key),
        }
    }

    #[test]
    fn both_sides_derive_the_same_confirmation_code() {
        let identity = Identity::load_or_create().expect("identity");
        let theirs = hello_from(42, "other");
        let offer = build_offer(&identity, theirs.clone(), None).expect("offer");

        // What the other machine computes, from its side, must match.
        let their_key = decode_key(&theirs.static_key).unwrap();
        let expected = short_auth_string(&their_key, &identity.public_key());
        assert_eq!(offer.confirmation_code, expected);
        assert_eq!(offer.confirmation_code.len(), 6);
    }

    #[test]
    fn a_substituted_key_produces_a_different_code() {
        let identity = Identity::load_or_create().expect("identity");
        let honest = build_offer(&identity, hello_from(1, "other"), None).expect("offer");
        // Same device id, different key — exactly what a machine in the middle
        // would present. The user sees a mismatch and says no.
        let tampered = build_offer(&identity, hello_from(2, "other"), None).expect("offer");
        assert_ne!(honest.confirmation_code, tampered.confirmation_code);
    }

    #[test]
    fn a_machine_echoing_our_own_key_is_refused() {
        let identity = Identity::load_or_create().expect("identity");
        let echo = PairHello {
            device_id: "mirror".into(),
            name: "Mirror".into(),
            static_key: identity.public_key_b64(),
        };
        let err = build_offer(&identity, echo, None).expect_err("must refuse");
        assert!(err.contains("own key"), "{err}");
    }

    /// The pairing lock is machine-wide, so the tests that exercise it cannot
    /// run at the same time as each other — one would see the other's lock and
    /// conclude the guard is broken.
    static LOCK_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn only_one_pairing_session_may_hold_the_lock() {
        use crate::lock::ProcessLock;
        let _serial = LOCK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let first = ProcessLock::try_acquire(PAIRING_LOCK).expect("first acquires");
        // A second `clipd pair`, or the GUI while the CLI is pairing, must be
        // turned away rather than advertising the same service twice.
        assert!(
            ProcessLock::try_acquire(PAIRING_LOCK).is_none(),
            "a second pairing session must not start"
        );
        drop(first);
        // And once the first finishes, pairing is available again.
        assert!(ProcessLock::try_acquire(PAIRING_LOCK).is_some());
    }

    #[test]
    fn dropping_an_offer_releases_the_lock() {
        use crate::lock::ProcessLock;
        let _serial = LOCK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let lock = ProcessLock::try_acquire(PAIRING_LOCK).expect("acquire");
        let offer = build_offer(
            &Identity::load_or_create().expect("identity"),
            hello_from(5, "other"),
            Some(lock),
        )
        .expect("offer");
        assert!(ProcessLock::try_acquire(PAIRING_LOCK).is_none());

        // Cancelling — i.e. dropping the offer without accepting — must not
        // leave the machine unable to pair again.
        drop(offer);
        assert!(ProcessLock::try_acquire(PAIRING_LOCK).is_some());
    }

    #[test]
    fn a_malformed_key_is_refused() {
        let identity = Identity::load_or_create().expect("identity");
        let bad = PairHello {
            device_id: "x".into(),
            name: "X".into(),
            static_key: "not-a-key".into(),
        };
        assert!(build_offer(&identity, bad, None).is_err());
    }
}
