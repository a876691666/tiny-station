use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// XChaCha20-Poly1305 nonce length. 24 random bytes make nonce reuse
/// negligible without any per-node counter state.
pub const NONCE_LEN: usize = 24;
/// Poly1305 authentication tag length.
const TAG_LEN: usize = 16;

/// Extract 32 bytes from `ikm` with HKDF-SHA256.
pub fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out)
        .expect("32 bytes is a valid HKDF output length");
    out
}

/// Cryptographically-random bytes.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

/// Cryptographically-random bytes of a runtime length (for the kernel API).
pub fn random_vec(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut v);
    v
}

/// HKDF-SHA256 expansion to an arbitrary length (max 255*32 bytes).
pub fn hkdf_expand(ikm: &[u8], salt: &[u8], info: &[u8], len: usize) -> Result<Vec<u8>, String> {
    if len > 255 * 32 {
        return Err("HKDF 输出长度超限 (最大 8160)".to_string());
    }
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = vec![0u8; len];
    hk.expand(info, &mut out)
        .map_err(|_| "HKDF 展开失败".to_string())?;
    Ok(out)
}

/// HMAC-SHA256.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Lowercase hex encoding.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Decode lowercase/uppercase hex.
pub fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err("hex 长度必须为偶数".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "非法 hex".to_string()))
        .collect()
}

// ---------------- Ed25519 (kernel bundle signing) ----------------

/// Generate an Ed25519 keypair. Returns `(secret_hex, public_hex)`.
pub fn ed25519_keypair() -> (String, String) {
    use ed25519_dalek::SigningKey;
    let sk = SigningKey::from_bytes(&random_bytes::<32>());
    (
        to_hex(&sk.to_bytes()),
        to_hex(sk.verifying_key().as_bytes()),
    )
}

/// Sign `msg` with a hex-encoded Ed25519 secret key.
pub fn ed25519_sign(secret_hex: &str, msg: &[u8]) -> Result<String, String> {
    use ed25519_dalek::{Signer, SigningKey};
    let bytes = from_hex(secret_hex)?;
    let sk = SigningKey::from_bytes(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| "Ed25519 私钥必须为 32 字节".to_string())?,
    );
    Ok(to_hex(&sk.sign(msg).to_bytes()))
}

/// Verify an Ed25519 signature. Returns false on any malformed input.
pub fn ed25519_verify(public_hex: &str, msg: &[u8], sig_hex: &str) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let (Ok(pub_bytes), Ok(sig_bytes)) = (from_hex(public_hex), from_hex(sig_hex)) else {
        return false;
    };
    let (Ok(pub_arr), Ok(sig_arr)) = (
        <[u8; 32]>::try_from(pub_bytes.as_slice()),
        <[u8; 64]>::try_from(sig_bytes.as_slice()),
    ) else {
        return false;
    };
    match VerifyingKey::from_bytes(&pub_arr) {
        Ok(vk) => vk.verify(msg, &Signature::from_bytes(&sig_arr)).is_ok(),
        Err(_) => false,
    }
}

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    to_hex(&Sha256::digest(data))
}

/// Encrypt `plain` under a 32-byte `key`, binding `aad` into the tag.
/// Returns `nonce || ciphertext`.
pub fn seal(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let nonce = random_bytes::<NONCE_LEN>();
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .expect("aead encryption is infallible for in-memory buffers");
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Decrypt `nonce || ciphertext` under `key`, verifying the tag against `aad`.
pub fn open(key: &[u8; 32], aad: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < NONCE_LEN + TAG_LEN {
        return Err("密文过短".to_string());
    }
    let (nonce, ct) = data.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| "解密失败(密钥不匹配或报文被篡改)".to_string())
}

/// The stable handshake key, derived only from the root/enrollment secret.
/// Used to authenticate the enrollment handshake (PSK).
pub fn handshake_key(root: &[u8]) -> [u8; 32] {
    hkdf32(root, b"tiny-station-root-v1", b"handshake")
}

/// Initial (epoch 0) cluster key derived from the root secret, so every node
/// that shares the same `mesh_key` agrees on it before any rotation.
pub fn cluster_key_from_root(root: &[u8]) -> [u8; 32] {
    hkdf32(root, b"tiny-station-root-v1", b"cluster-key-v1")
}

/// New cluster key material for a rotation: derived from a provided passphrase,
/// or a fresh random key when the operator gives none.
pub fn cluster_key_from_passphrase(passphrase: Option<&str>) -> [u8; 32] {
    match passphrase {
        Some(p) if !p.trim().is_empty() => {
            hkdf32(p.as_bytes(), b"tiny-station-cluster-v1", b"cluster-key-v1")
        }
        _ => random_bytes::<32>(),
    }
}

/// X25519 diffie-hellman.
pub fn dh(secret: &StaticSecret, public: &PublicKey) -> [u8; 32] {
    secret.diffie_hellman(public).to_bytes()
}

/// Derive the pairwise session key from the static-static X25519 shared secret
/// and the two nodes' current ephemeral shared secret, mixed with the cluster
/// key.
///
/// Both nodes keep a *current* ephemeral keypair (rotated periodically) and
/// reuse it for every handshake until rotated. That makes the derivation
/// **idempotent and role-independent**: two concurrent handshakes between the
/// same pair produce the same key, so the mesh never desyncs. Forward secrecy
/// comes from discarding old ephemerals — a later static-key compromise cannot
/// recompute past session keys once their ephemerals are gone.
pub fn derive_session(
    our_eph: &StaticSecret,
    our_id: &StaticSecret,
    peer_eph_pub: [u8; 32],
    peer_id_pub: [u8; 32],
    cluster: &[u8; 32],
) -> [u8; 32] {
    let peer_eph = PublicKey::from(peer_eph_pub);
    let peer_id = PublicKey::from(peer_id_pub);
    let mut ikm = Vec::with_capacity(64);
    ikm.extend_from_slice(&dh(our_id, &peer_id));
    ikm.extend_from_slice(&dh(our_eph, &peer_eph));
    let mut info = b"ts-session-v1".to_vec();
    info.extend_from_slice(cluster);
    hkdf32(&ikm, b"tiny-station-session-v1", &info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_roundtrip() {
        let k = random_bytes::<32>();
        let sealed = seal(&k, b"aad", b"payload");
        assert_eq!(open(&k, b"aad", &sealed).unwrap(), b"payload".to_vec());
    }

    #[test]
    fn wrong_aad_or_key_fails() {
        let k = random_bytes::<32>();
        let sealed = seal(&k, b"aad", b"payload");
        assert!(open(&k, b"other", &sealed).is_err());
        assert!(open(&random_bytes::<32>(), b"aad", &sealed).is_err());
    }

    #[test]
    fn tamper_detected() {
        let k = random_bytes::<32>();
        let mut sealed = seal(&k, b"aad", b"payload");
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(open(&k, b"aad", &sealed).is_err());
    }

    #[test]
    fn handshake_key_is_stable() {
        assert_eq!(handshake_key(b"secret"), handshake_key(b"secret"));
        assert_ne!(handshake_key(b"secret"), handshake_key(b"other"));
    }

    #[test]
    fn dh_is_symmetric() {
        let a = StaticSecret::from(random_bytes::<32>());
        let b = StaticSecret::from(random_bytes::<32>());
        let (pa, pb) = (PublicKey::from(&a), PublicKey::from(&b));
        assert_eq!(dh(&a, &pb), dh(&b, &pa));
    }

    #[test]
    fn derive_session_symmetric_and_idempotent() {
        let a_eph = StaticSecret::from(random_bytes::<32>());
        let a_id = StaticSecret::from(random_bytes::<32>());
        let b_eph = StaticSecret::from(random_bytes::<32>());
        let b_id = StaticSecret::from(random_bytes::<32>());
        let pa_eph = PublicKey::from(&a_eph).to_bytes();
        let pa_id = PublicKey::from(&a_id).to_bytes();
        let pb_eph = PublicKey::from(&b_eph).to_bytes();
        let pb_id = PublicKey::from(&b_id).to_bytes();
        let cluster = random_bytes::<32>();
        // A is the initiator (peer = B).
        let ka = derive_session(&a_eph, &a_id, pb_eph, pb_id, &cluster);
        // B is the responder (peer = A).
        let kb = derive_session(&b_eph, &b_id, pa_eph, pa_id, &cluster);
        assert_eq!(ka, kb);
        // Idempotent: repeating the same handshake yields the same key, so two
        // concurrent handshakes between the pair can never desync.
        let ka2 = derive_session(&a_eph, &a_id, pb_eph, pb_id, &cluster);
        assert_eq!(ka, ka2);
    }
}
