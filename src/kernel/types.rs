//! Data contracts exchanged across the shell ↔ kernel boundary.
//!
//! These are the *only* shapes that cross the boundary. They are plain serde
//! structs so they can be (de)serialized from Lua tables without any bespoke
//! conversion code, and so the seam stays language-agnostic.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A kernel is a **set of modules**, not a single file. `entry` is the module
/// that returns the kernel table (`main.lua` by default); other modules are
/// loaded on demand through the sandboxed `require` (see the runtime).
///
/// Module paths are relative, forward-slashed, `.lua`-suffixed keys such as
/// `main.lua`, `lib/util.lua`, `routes/admin.lua`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleSet {
    #[serde(default = "default_entry")]
    pub entry: String,
    pub files: BTreeMap<String, String>,
}

pub fn default_entry() -> String {
    "main.lua".to_string()
}

impl ModuleSet {
    /// A single-file kernel (the common quick-edit case).
    pub fn single(code: &str) -> Self {
        let mut files = BTreeMap::new();
        files.insert(default_entry(), code.to_string());
        ModuleSet {
            entry: default_entry(),
            files,
        }
    }

    /// Stable content hash (`sha256` hex) over the entry + sorted module files.
    pub fn hash(&self) -> String {
        let mut s = String::new();
        s.push_str(&self.entry);
        s.push('\0');
        for (k, v) in &self.files {
            s.push_str(k);
            s.push('\0');
            s.push_str(v);
            s.push('\0');
        }
        crate::crypto::sha256_hex(s.as_bytes())
    }

    /// Reject anything that could escape the kernel dir or is not a Lua file.
    pub fn validate(&self) -> Result<(), String> {
        if self.files.is_empty() {
            return Err("内核为空".to_string());
        }
        if !self.files.contains_key(&self.entry) {
            return Err(format!("入口模块 {} 不存在", self.entry));
        }
        for k in self.files.keys() {
            if k.starts_with('/')
                || k.contains("..")
                || k.contains('\\')
                || !k.ends_with(".lua")
            {
                return Err(format!("非法模块路径: {}", k));
            }
        }
        Ok(())
    }
}

/// What the `route` hook receives.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RouteQuery {
    pub url: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub method: String,
}

/// What the `route` hook may return. An empty decision (all `None`) or a `nil`
/// return means "use the built-in router".
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RouteDecision {
    #[serde(default)]
    pub via: Option<String>,
    #[serde(default)]
    pub egress: Option<String>,
}

/// A request delivered to a kernel `endpoints` handler.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct KernelHttpReq {
    pub method: String,
    /// Full path including the `/x/` prefix, e.g. `/x/ping`.
    pub path: String,
    /// Raw query string (without `?`), may be empty.
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// A response returned by a kernel `endpoints` handler.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct KernelHttpResp {
    #[serde(default = "default_status")]
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: String,
}

fn default_status() -> u16 {
    200
}

/// Identity/status of a loaded kernel, surfaced to the admin API.
#[derive(Clone, Debug, Serialize)]
pub struct KernelInfo {
    pub abi: u32,
    pub version: String,
    pub workers: usize,
    pub source: String,
}
