use crate::crypto;
use crate::replay::ReplayCache;
use crate::state::now_ms;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Wire magic — identifies the tiny-station private protocol on the raw body.
pub const MAGIC: [u8; 4] = *b"TSN1";
/// Protocol version.
pub const VER: u8 = 1;
/// magic(4) + version(1) + kind(1).
const HEADER_LEN: usize = 6;
/// Accept a message only if its timestamp is within this window. It must exceed
/// the worst expected clock skew between nodes; **keep node clocks synchronised
/// (NTP)** or messages get rejected. Replays inside the window are rejected by
/// the `ReplayCache`, so this window only bounds cache size, not freshness.
pub const MAX_SKEW_MS: u64 = 300_000;

/// Message kind. Bound into the AEAD associated data so a packet sealed for one
/// endpoint can never be replayed against another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Peers = 1,
    Gossip = 2,
    Reach = 3,
    Tunnel = 4,
    ReversePoll = 5,
    /// Enrollment/handshake — sealed with the root handshake key (PSK).
    Handshake = 6,
    /// Cluster-key rotation broadcast — sealed with the pairwise session key.
    KeyRotate = 7,
    /// Signed kernel-bundle broadcast — sealed with the pairwise session key.
    KernelAnnounce = 8,
}

impl Kind {
    fn id(self) -> u8 {
        self as u8
    }
}

/// Every sealed body is timestamped and carries a random id: the timestamp
/// bounds the accepted clock skew, the id lets the receiver reject replays.
#[derive(Serialize, Deserialize)]
struct Timed<T> {
    ts: u64,
    mid: [u8; 16],
    body: T,
}

fn header(kind: Kind) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(&MAGIC);
    h[4] = VER;
    h[5] = kind.id();
    h
}

/// Encode `body` with the private binary codec and seal it under `key`:
/// `header || nonce || ct`.
pub fn pack<T: Serialize>(key: &[u8; 32], kind: Kind, body: &T) -> Vec<u8> {
    let timed = Timed {
        ts: now_ms(),
        mid: crypto::random_bytes::<16>(),
        body,
    };
    let plain = bincode::serialize(&timed).expect("bincode serialization of an in-memory struct");
    let sealed = crypto::seal(key, &header(kind), &plain);
    let mut out = Vec::with_capacity(HEADER_LEN + sealed.len());
    out.extend_from_slice(&header(kind));
    out.extend_from_slice(&sealed);
    out
}

/// Verify and decode a sealed message of the expected `kind`. When `guard` is
/// provided, a message id seen inside the skew window is rejected as a replay.
pub fn unpack<T: DeserializeOwned>(
    key: &[u8; 32],
    kind: Kind,
    data: &[u8],
    guard: Option<&ReplayCache>,
) -> Result<T, String> {
    if data.len() < HEADER_LEN {
        return Err("报文过短".to_string());
    }
    if data[..4] != MAGIC {
        return Err("协议标识不匹配".to_string());
    }
    if data[4] != VER {
        return Err(format!("协议版本不支持: {}", data[4]));
    }
    if data[5] != kind.id() {
        return Err("报文类型不匹配".to_string());
    }
    let plain = crypto::open(key, &header(kind), &data[HEADER_LEN..])?;
    let timed: Timed<T> =
        bincode::deserialize(&plain).map_err(|e| format!("私有编码解码失败: {}", e))?;
    let now = now_ms();
    let skew = now.abs_diff(timed.ts);
    if skew > MAX_SKEW_MS {
        return Err(format!("报文时间戳超出窗口({}ms),请检查各节点时钟是否同步", skew));
    }
    if let Some(guard) = guard {
        if !guard.fresh(timed.mid, now, MAX_SKEW_MS) {
            return Err("检测到重放报文".to_string());
        }
    }
    Ok(timed.body)
}
