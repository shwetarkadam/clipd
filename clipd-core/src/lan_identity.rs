//! Who a machine is on the local network, and who it trusts.
//!
//! The folder transport got its trust for free: both machines were signed into
//! one Apple account, so anything that could write the folder was already you.
//! A LAN has no such boundary — anything on the Wi-Fi can speak our protocol —
//! so trust has to be established explicitly, once, by a human.
//!
//! Each machine holds a long-lived X25519 keypair. Pairing is a one-time
//! exchange of public keys, confirmed out-of-band by comparing a six-digit
//! **short authentication string** derived from both keys. If a machine in the
//! middle swapped either key, the two numbers differ and the user says no. That
//! is the entire defence, and it is why the code must be *compared* rather than
//! merely displayed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use x25519_dalek::{PublicKey, StaticSecret};

/// Length of the confirmation code shown during pairing.
///
/// Six digits is a one-in-a-million chance that a tampered handshake happens to
/// display the right number — and the attacker has to win it live, during the
/// seconds a user is looking at both screens, with no way to retry silently.
const SAS_DIGITS: u32 = 6;

/// A machine we have paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    /// The peer's clipd device id — the same id the folder transport uses.
    pub device_id: String,
    /// Display name at pairing time.
    pub name: String,
    /// The peer's long-lived X25519 public key, base64.
    pub public_key: String,
    pub paired_at: chrono::DateTime<chrono::Utc>,
}

/// This machine's long-lived identity on the network.
pub struct Identity {
    secret: StaticSecret,
}

impl Identity {
    /// Load this machine's keypair, generating one on first use.
    pub fn load_or_create() -> Result<Self, String> {
        let path = key_path();
        if let Ok(raw) = std::fs::read(&path) {
            if raw.len() == 32 {
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&raw);
                return Ok(Identity {
                    secret: StaticSecret::from(bytes),
                });
            }
            // A wrong-sized key file means a truncated write or a different
            // format. Refuse rather than silently re-pairing every machine.
            return Err(format!(
                "{} is not a valid key. Delete it to generate a new identity \
                 (you'll have to pair again).",
                path.display()
            ));
        }

        let secret = StaticSecret::random_from_rng(rand_core::OsRng);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Couldn't create the key directory: {e}"))?;
        }
        write_private(&path, secret.as_bytes())?;
        Ok(Identity { secret })
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey::from(&self.secret)
    }

    /// Base64 of the public key — what travels during pairing.
    pub fn public_key_b64(&self) -> String {
        encode_key(&self.public_key())
    }

    pub(crate) fn secret(&self) -> &StaticSecret {
        &self.secret
    }
}

/// Write a secret key so only this user can read it.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("Couldn't save the key: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 0600. A private key readable by other accounts on the machine is not
        // a private key.
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn key_path() -> PathBuf {
    clipd_dir().join("lan-identity.key")
}

fn trust_path() -> PathBuf {
    clipd_dir().join("trusted-peers.json")
}

fn clipd_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
}

pub fn encode_key(key: &PublicKey) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(key.as_bytes())
}

pub fn decode_key(encoded: &str) -> Result<PublicKey, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|e| format!("That isn't a valid key: {e}"))?;
    if bytes.len() != 32 {
        return Err("That key is the wrong length.".into());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(PublicKey::from(arr))
}

/// The six-digit code both machines display during pairing.
///
/// Derived from *both* public keys in a fixed order, so each side computes the
/// same number without exchanging anything extra. Sorting the keys means the
/// code does not depend on who initiated.
pub fn short_auth_string(a: &PublicKey, b: &PublicKey) -> String {
    let (first, second) = if a.as_bytes() <= b.as_bytes() {
        (a, b)
    } else {
        (b, a)
    };
    let mut hasher = Sha256::new();
    hasher.update(b"clipd-pairing-sas-v1");
    hasher.update(first.as_bytes());
    hasher.update(second.as_bytes());
    let digest = hasher.finalize();

    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    let modulus = 10u32.pow(SAS_DIGITS);
    format!("{:0width$}", n % modulus, width = SAS_DIGITS as usize)
}

/// Every machine this one has paired with, keyed by device id.
pub fn trusted_peers() -> BTreeMap<String, TrustedPeer> {
    let Ok(raw) = std::fs::read_to_string(trust_path()) else {
        return BTreeMap::new();
    };
    match serde_json::from_str(&raw) {
        Ok(map) => map,
        Err(e) => {
            log::warn!("Couldn't read the paired-machines list: {e}");
            BTreeMap::new()
        }
    }
}

/// Record a machine as paired. Re-pairing an existing device replaces its key,
/// which is what lets someone recover after wiping the other machine.
pub fn trust_peer(peer: TrustedPeer) -> Result<(), String> {
    let mut peers = trusted_peers();
    peers.insert(peer.device_id.clone(), peer);
    save_peers(&peers)
}

/// Forget a machine. Sends from it are refused afterwards.
pub fn forget_peer(device_id: &str) -> Result<bool, String> {
    let mut peers = trusted_peers();
    let removed = peers.remove(device_id).is_some();
    if removed {
        save_peers(&peers)?;
    }
    Ok(removed)
}

/// Resolve a name, name fragment, or device id to exactly one paired machine.
///
/// Same rules as everywhere else in clipd: an exact name wins over a substring,
/// and ambiguity is reported rather than guessed — unpairing the wrong machine
/// is quiet and only noticed later, when a send mysteriously fails.
pub fn resolve_trusted(query: &str) -> Result<TrustedPeer, String> {
    let peers = trusted_peers();
    if peers.is_empty() {
        return Err("No machines are paired with this one.".into());
    }
    let q = query.trim().to_lowercase();

    let exact: Vec<&TrustedPeer> = peers
        .values()
        .filter(|p| p.name.to_lowercase() == q || p.device_id == query.trim())
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }

    let partial: Vec<&TrustedPeer> = peers
        .values()
        .filter(|p| p.name.to_lowercase().contains(&q) || p.device_id.starts_with(query.trim()))
        .collect();
    match partial.len() {
        1 => Ok(partial[0].clone()),
        0 => Err(format!("No paired machine matching \"{query}\".")),
        _ => Err(format!(
            "\"{query}\" matches more than one paired machine: {}",
            partial
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn save_peers(peers: &BTreeMap<String, TrustedPeer>) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(peers)
        .map_err(|e| format!("Couldn't encode the paired-machines list: {e}"))?;
    crate::devices::write_atomically(&trust_path(), &json)
}

/// Whether `device_id` presenting `key` is a machine we have paired with.
///
/// Both halves matter: an unknown device id is a stranger, and a known device
/// id presenting the wrong key is either a machine that was reinstalled or
/// somebody impersonating it. Neither may be accepted silently.
pub fn is_trusted(device_id: &str, key: &PublicKey) -> bool {
    trusted_peers()
        .get(device_id)
        .and_then(|p| decode_key(&p.public_key).ok())
        .is_some_and(|known| known.as_bytes() == key.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_from(seed: u8) -> PublicKey {
        PublicKey::from(&StaticSecret::from([seed; 32]))
    }

    #[test]
    fn the_confirmation_code_is_the_same_on_both_machines() {
        let a = key_from(1);
        let b = key_from(2);
        // Whoever initiated, both sides must show the same number — otherwise
        // every honest pairing looks like an attack.
        assert_eq!(short_auth_string(&a, &b), short_auth_string(&b, &a));
    }

    #[test]
    fn a_swapped_key_changes_the_code() {
        let a = key_from(1);
        let b = key_from(2);
        let attacker = key_from(3);
        // This is the whole point: a machine in the middle substituting its own
        // key cannot make the two screens agree.
        assert_ne!(short_auth_string(&a, &b), short_auth_string(&a, &attacker));
    }

    #[test]
    fn the_code_is_six_digits_including_leading_zeros() {
        for seed in 0..40u8 {
            let sas = short_auth_string(&key_from(seed), &key_from(seed.wrapping_add(7)));
            assert_eq!(sas.len(), 6, "got {sas}");
            assert!(sas.chars().all(|c| c.is_ascii_digit()), "got {sas}");
        }
    }

    #[test]
    fn keys_survive_a_base64_round_trip() {
        let key = key_from(9);
        let encoded = encode_key(&key);
        assert_eq!(decode_key(&encoded).unwrap().as_bytes(), key.as_bytes());
    }

    #[test]
    fn malformed_keys_are_rejected() {
        assert!(decode_key("not base64!!").is_err());
        // Valid base64, wrong length — the case that would otherwise panic on
        // copy_from_slice.
        assert!(decode_key("aGVsbG8=").is_err());
    }

    #[test]
    fn an_unknown_device_is_not_trusted() {
        assert!(!is_trusted("never-seen-this-one", &key_from(4)));
    }

    #[test]
    fn a_known_device_presenting_the_wrong_key_is_not_trusted() {
        let real = key_from(11);
        let impostor = key_from(12);
        let peers = BTreeMap::from([(
            "device-a".to_string(),
            TrustedPeer {
                device_id: "device-a".into(),
                name: "Mac mini".into(),
                public_key: encode_key(&real),
                paired_at: chrono::Utc::now(),
            },
        )]);

        // Exercised directly so the test doesn't depend on the real trust file.
        let check = |key: &PublicKey| {
            peers
                .get("device-a")
                .and_then(|p| decode_key(&p.public_key).ok())
                .is_some_and(|known| known.as_bytes() == key.as_bytes())
        };
        assert!(check(&real));
        assert!(!check(&impostor), "a stolen device id must not be enough");
    }
}
