use crate::state::now_ms;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// A small persistent key/value store for kernel scripts.
///
/// The whole map is a single JSON file rewritten atomically on every mutation.
/// Kernel data is small and write volume is low, so a full rewrite keeps the
/// implementation trivial and crash-safe (temp file + rename, see `atomic`).
/// Values are UTF-8 strings; use `ctx.crypto.b64_enc` for binary payloads.
pub struct Store {
    path: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    /// key -> (value, expire_at_ms). `None` means "never expires".
    map: BTreeMap<String, (String, Option<u64>)>,
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, (String, Option<u64>)>>(&raw).ok())
            .unwrap_or_default();
        Store {
            path,
            inner: Mutex::new(Inner { map }),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let mut g = self.inner.lock().unwrap();
        let expired = match g.map.get(key) {
            Some((_, Some(exp))) if *exp <= now_ms() => true,
            Some(_) => false,
            None => return None,
        };
        if expired {
            g.map.remove(key);
            self.persist(&g);
            return None;
        }
        g.map.get(key).map(|(v, _)| v.clone())
    }

    pub fn set(&self, key: &str, value: &str, ttl_ms: Option<u64>) {
        let mut g = self.inner.lock().unwrap();
        let exp = ttl_ms.filter(|t| *t > 0).map(|t| now_ms() + t);
        g.map.insert(key.to_string(), (value.to_string(), exp));
        self.persist(&g);
    }

    pub fn del(&self, key: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        let existed = g.map.remove(key).is_some();
        if existed {
            self.persist(&g);
        }
        existed
    }

    /// Atomically add `delta` to the numeric value at `key` and return the new
    /// value. A missing or non-numeric value is treated as 0.
    pub fn incr(&self, key: &str, delta: i64) -> i64 {
        let mut g = self.inner.lock().unwrap();
        let cur = g
            .map
            .get(key)
            .and_then(|(v, _)| v.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let next = cur.saturating_add(delta);
        let exp = g.map.get(key).and_then(|(_, e)| *e);
        g.map.insert(key.to_string(), (next.to_string(), exp));
        self.persist(&g);
        next
    }

    /// Keys (not values) beginning with `prefix`, sorted.
    pub fn keys(&self, prefix: &str) -> Vec<String> {
        let g = self.inner.lock().unwrap();
        let now = now_ms();
        g.map
            .iter()
            .filter(|(k, (_, exp))| {
                exp.map(|e| e > now).unwrap_or(true) && k.starts_with(prefix)
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn persist(&self, g: &Inner) {
        if let Ok(raw) = serde_json::to_string(&g.map) {
            if let Err(e) = crate::atomic::write(&self.path, raw.as_bytes()) {
                tracing::warn!("内核 store 落盘失败 {}: {}", self.path.display(), e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (Store, PathBuf) {
        let mut p = std::env::temp_dir();
        p.push(format!("ts-kernel-store-{}.json", uuid::Uuid::new_v4()));
        (Store::load(p.clone()), p)
    }

    #[test]
    fn set_get_del_roundtrip() {
        let (s, path) = tmp_store();
        assert_eq!(s.get("a"), None);
        s.set("a", "1", None);
        assert_eq!(s.get("a"), Some("1".to_string()));
        assert!(s.del("a"));
        assert_eq!(s.get("a"), None);
        assert!(!s.del("a"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn incr_is_atomic_and_persists() {
        let (s, path) = tmp_store();
        assert_eq!(s.incr("n", 1), 1);
        assert_eq!(s.incr("n", 5), 6);
        assert_eq!(s.incr("n", -2), 4);
        // A reload from disk sees the persisted counter.
        let s2 = Store::load(path.clone());
        assert_eq!(s2.get("n"), Some("4".to_string()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ttl_expires() {
        let (s, path) = tmp_store();
        s.set("long", "v", Some(1000));
        s.set("short", "v", Some(1));
        assert_eq!(s.get("long"), Some("v".to_string()));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(s.get("long"), Some("v".to_string()));
        assert_eq!(s.get("short"), None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn keys_by_prefix() {
        let (s, path) = tmp_store();
        s.set("user:1", "a", None);
        s.set("user:2", "b", None);
        s.set("other", "c", None);
        assert_eq!(s.keys("user:"), vec!["user:1".to_string(), "user:2".to_string()]);
        let _ = std::fs::remove_file(path);
    }
}
