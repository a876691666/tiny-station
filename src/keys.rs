use crate::crypto;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Upper bound on a plausible epoch. Real rotations are tiny numbers; anything
/// beyond this is a corrupted/rogue value and is refused, so a single bad node
/// cannot pin the whole mesh's epoch out of reach.
pub const MAX_EPOCH: u64 = u32::MAX as u64;

/// A versioned cluster key. Epoch increases monotonically on every rotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterKey {
    pub epoch: u64,
    pub key: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    epoch: u64,
    key: String,
}

/// Persisted, rotatable cluster key. The previous key is intentionally not
/// retained: rotations change the *input* to session-key derivation, while live
/// pairwise sessions keep their already-negotiated key, so no grace decryption
/// is required at the cluster-key layer.
pub struct KeyStore {
    current: ClusterKey,
    path: PathBuf,
}

impl KeyStore {
    /// Load the persisted cluster key, or initialize epoch 0 from the root
    /// secret (so every node sharing the same `mesh_key` starts in agreement).
    /// A corrupt or out-of-range file is reported and ignored (epoch 0).
    pub fn load_or_init(path: &Path, root: &str) -> Self {
        if path.exists() {
            match read(path) {
                Some(ck) if ck.epoch <= MAX_EPOCH => {
                    return KeyStore {
                        current: ck,
                        path: path.to_path_buf(),
                    };
                }
                _ => tracing::warn!(
                    "cluster key 文件损坏或非法({}),将回退到 epoch 0;如需恢复请重新轮换",
                    path.display()
                ),
            }
        }
        KeyStore {
            current: ClusterKey {
                epoch: 0,
                key: crypto::cluster_key_from_root(root.as_bytes()),
            },
            path: path.to_path_buf(),
        }
    }

    pub fn current_key(&self) -> [u8; 32] {
        self.current.key
    }

    pub fn epoch(&self) -> u64 {
        self.current.epoch
    }

    /// Adopt a cluster key that is newer under the total order `(epoch, key)`.
    /// Using the key bytes as the tie-breaker makes two concurrent rotations
    /// (same epoch, different keys) converge to the same winner instead of
    /// splitting the mesh. Out-of-range epochs are refused. Returns true only
    /// when it advanced.
    pub fn accept(&mut self, epoch: u64, key: [u8; 32]) -> bool {
        if epoch > MAX_EPOCH {
            tracing::warn!("拒绝非法 cluster key epoch {} (超过上限)", epoch);
            return false;
        }
        if (epoch, key) <= (self.current.epoch, self.current.key) {
            return false;
        }
        self.current = ClusterKey { epoch, key };
        self.persist();
        true
    }

    fn persist(&self) {
        let p = Persisted {
            epoch: self.current.epoch,
            key: STANDARD.encode(self.current.key),
        };
        if let Ok(raw) = serde_json::to_string(&p) {
            if let Err(e) = crate::atomic::write(&self.path, raw.as_bytes()) {
                tracing::warn!("写入 cluster key 失败: {}", e);
            }
        }
    }
}

/// Pick the newer of two candidate cluster keys under the `(epoch, key)` total
/// order. Both sides of a handshake call this, so they agree even when two
/// concurrent rotations produced the same epoch (different keys).
pub fn newer(a: (u64, [u8; 32]), b: (u64, [u8; 32])) -> (u64, [u8; 32]) {
    if b > a {
        b
    } else {
        a
    }
}

fn read(path: &Path) -> Option<ClusterKey> {
    let raw = std::fs::read_to_string(path).ok()?;
    let p: Persisted = serde_json::from_str(&raw).ok()?;
    let bytes = STANDARD.decode(p.key.trim()).ok()?;
    let key = <[u8; 32]>::try_from(bytes.as_slice()).ok()?;
    Some(ClusterKey {
        epoch: p.epoch,
        key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ts-keys-{}-{}", name, std::process::id()))
    }

    #[test]
    fn init_from_root_is_deterministic() {
        let p = tmp("a");
        let _ = std::fs::remove_file(&p);
        let a = KeyStore::load_or_init(&p, "shared-root");
        let b = KeyStore::load_or_init(&p, "shared-root");
        assert_eq!(a.current_key(), b.current_key());
        assert_eq!(a.epoch(), 0);
    }

    #[test]
    fn accept_advances_and_persists() {
        let p = tmp("b");
        let _ = std::fs::remove_file(&p);
        let mut ks = KeyStore::load_or_init(&p, "root");
        assert!(ks.accept(2, [9u8; 32]));
        assert_eq!(ks.epoch(), 2);
        assert!(!ks.accept(2, [7u8; 32])); // not newer
        assert!(!ks.accept(1, [7u8; 32])); // older
        let reloaded = KeyStore::load_or_init(&p, "root");
        assert_eq!(reloaded.epoch(), 2);
        assert_eq!(reloaded.current_key(), [9u8; 32]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn concurrent_same_epoch_rotation_converges() {
        // A and B both rotate from epoch 0 to epoch 1 independently, with
        // different keys. The `(epoch, key)` total order must make both pick the
        // SAME winner, otherwise the mesh would split permanently.
        let (ea, ka) = (1u64, [3u8; 32]);
        let (eb, kb) = (1u64, [9u8; 32]);
        let winner = newer((ea, ka), (eb, kb));
        assert_eq!(winner, (1, [9u8; 32]));

        let pa = tmp("ca");
        let pb = tmp("cb");
        let _ = std::fs::remove_file(&pa);
        let _ = std::fs::remove_file(&pb);
        let mut a = KeyStore::load_or_init(&pa, "root");
        let mut b = KeyStore::load_or_init(&pb, "root");
        a.accept(ea, ka);
        b.accept(eb, kb);
        // Each side now sees the other's key and adopts the winner.
        a.accept(eb, kb);
        b.accept(ea, ka);
        assert_eq!(a.current_key(), b.current_key());
        assert_eq!(a.epoch(), b.epoch());
        let _ = std::fs::remove_file(&pa);
        let _ = std::fs::remove_file(&pb);
    }

    #[test]
    fn rejects_out_of_range_epoch() {
        let p = tmp("range");
        let _ = std::fs::remove_file(&p);
        let mut ks = KeyStore::load_or_init(&p, "root");
        assert!(!ks.accept(u64::MAX, [1u8; 32]));
        assert_eq!(ks.epoch(), 0);
        assert!(ks.accept(MAX_EPOCH, [1u8; 32]));
        assert_eq!(ks.epoch(), MAX_EPOCH);
        let _ = std::fs::remove_file(&p);
    }
}
