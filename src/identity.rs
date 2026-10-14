use crate::crypto;
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::path::Path;
use x25519_dalek::{PublicKey, StaticSecret};

/// This node's long-lived X25519 identity. Generated automatically on first run
/// and persisted next to the config — the operator never configures it.
pub struct Identity {
    secret: StaticSecret,
    public: PublicKey,
}

impl Identity {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let existing = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| STANDARD.decode(s.trim()).ok())
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
        let secret = match existing {
            Some(bytes) => StaticSecret::from(bytes),
            None => {
                if path.exists() {
                    tracing::warn!(
                        "身份密钥文件损坏({}),已重新生成;其它节点将重新握手",
                        path.display()
                    );
                }
                let bytes = crypto::random_bytes::<32>();
                crate::atomic::write(path, STANDARD.encode(bytes).as_bytes())
                    .with_context(|| format!("writing identity {}", path.display()))?;
                StaticSecret::from(bytes)
            }
        };
        let public = PublicKey::from(&secret);
        Ok(Identity { secret, public })
    }

    pub fn secret(&self) -> &StaticSecret {
        &self.secret
    }

    pub fn pubkey(&self) -> [u8; 32] {
        self.public.to_bytes()
    }
}
