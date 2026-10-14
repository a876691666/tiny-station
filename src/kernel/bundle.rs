//! Signed kernel bundles: the unit a node distributes across the mesh.
//!
//! A bundle wraps a [`ModuleSet`] with a monotonic revision and an Ed25519
//! signature. Nodes verify it against a pinned `release_pubkey` before
//! installing, so a compromised relay can never push code to the mesh.

use crate::crypto;
use crate::kernel::types::{ModuleSet, default_entry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A signed, versioned, multi-module Lua kernel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelBundle {
    /// Host ABI the script targets; must equal the node's ABI.
    pub abi: u32,
    /// Monotonic revision. Nodes reject anything `<=` the highest they accepted.
    pub rev: u64,
    /// Human-readable version (display only).
    pub version: String,
    /// Lowercase-hex SHA-256 of the module set (integrity + dedup key).
    pub sha256: String,
    /// Entry module path.
    #[serde(default = "default_entry")]
    pub entry: String,
    /// module path → source.
    pub files: BTreeMap<String, String>,
    /// Lowercase-hex Ed25519 signature over `canonical()`.
    #[serde(default)]
    pub sig: String,
    /// Lowercase-hex Ed25519 public key of the signer.
    #[serde(default)]
    pub signer: String,
}

impl KernelBundle {
    /// Build an unsigned bundle from a module set.
    pub fn new(set: &ModuleSet, abi: u32, rev: u64, version: &str) -> Self {
        KernelBundle {
            abi,
            rev,
            version: version.to_string(),
            sha256: set.hash(),
            entry: set.entry.clone(),
            files: set.files.clone(),
            sig: String::new(),
            signer: String::new(),
        }
    }

    pub fn modules(&self) -> ModuleSet {
        ModuleSet {
            entry: self.entry.clone(),
            files: self.files.clone(),
        }
    }

    /// Deterministic bytes that are signed/verified.
    pub fn canonical(&self) -> Vec<u8> {
        let mut s = format!(
            "ts-kernel-v2|{}|{}|{}|{}|{}",
            self.abi, self.rev, self.version, self.entry, self.sha256
        );
        for (k, v) in &self.files {
            s.push('|');
            s.push_str(k);
            s.push('\0');
            s.push_str(v);
        }
        s.into_bytes()
    }

    /// Sign in place with a hex-encoded Ed25519 secret key; sets `signer` too.
    pub fn sign_with(&mut self, secret_hex: &str) -> Result<(), String> {
        use ed25519_dalek::SigningKey;
        let sk = SigningKey::from_bytes(
            crypto::from_hex(secret_hex)?
                .as_slice()
                .try_into()
                .map_err(|_| "Ed25519 私钥必须为 32 字节".to_string())?,
        );
        self.signer = crypto::to_hex(sk.verifying_key().as_bytes());
        self.sig = crypto::ed25519_sign(secret_hex, &self.canonical())?;
        Ok(())
    }

    /// Verify the signature against the pinned key and the content hash. Fails
    /// on any mismatch — callers must not install on error.
    pub fn verify(&self, release_pubkey_hex: &str) -> Result<(), String> {
        let expected = release_pubkey_hex.trim().to_ascii_lowercase();
        if self.signer.to_ascii_lowercase() != expected {
            return Err("签名者与配置的 release_pubkey 不符".to_string());
        }
        if self.sha256.to_ascii_lowercase() != self.modules().hash() {
            return Err("模块内容哈希与清单不符".to_string());
        }
        if !crypto::ed25519_verify(&expected, &self.canonical(), &self.sig) {
            return Err("签名校验失败".to_string());
        }
        Ok(())
    }
}

/// The highest revision this node has accepted, plus what is currently running.
/// Persisted so a restart cannot be tricked into accepting an older bundle.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KernelMeta {
    /// Highest `rev` ever accepted (monotonic; never decreased, even on rollback).
    pub floor_rev: u64,
    /// Version string of what is currently running (display).
    pub version: String,
    /// SHA-256 of what is currently running.
    pub sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> ModuleSet {
        let mut files = BTreeMap::new();
        files.insert("main.lua".to_string(), "return { abi = 1 }".to_string());
        files.insert("lib/util.lua".to_string(), "return {}".to_string());
        ModuleSet {
            entry: "main.lua".to_string(),
            files,
        }
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let (sk, pk) = crypto::ed25519_keypair();
        let mut b = KernelBundle::new(&set(), 1, 7, "v7");
        b.sign_with(&sk).unwrap();
        assert_eq!(b.signer, pk);
        assert!(b.verify(&pk).is_ok());
        let (_, other) = crypto::ed25519_keypair();
        assert!(b.verify(&other).is_err());
    }

    #[test]
    fn tampered_module_rejected() {
        let (sk, pk) = crypto::ed25519_keypair();
        let mut b = KernelBundle::new(&set(), 1, 1, "v1");
        b.sign_with(&sk).unwrap();
        b.files.insert("main.lua".to_string(), "return 2".to_string());
        assert!(b.verify(&pk).is_err());
    }

    #[test]
    fn module_paths_validated() {
        let mut files = BTreeMap::new();
        files.insert("main.lua".to_string(), "return {}".to_string());
        files.insert("../evil.lua".to_string(), "return {}".to_string());
        let ms = ModuleSet {
            entry: "main.lua".to_string(),
            files,
        };
        assert!(ms.validate().is_err());
    }
}
