use std::collections::HashMap;
use std::sync::Mutex;

/// A bounded, time-windowed set of recently seen message ids, used to reject
/// replayed sealed messages inside the clock-skew window.
pub struct ReplayCache {
    seen: Mutex<HashMap<[u8; 16], u64>>,
}

impl ReplayCache {
    pub fn new() -> Self {
        ReplayCache {
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Record `id`. Returns `false` if it was already seen and is still inside
    /// the window (i.e. it is a replay). Expired entries are pruned on the way.
    pub fn fresh(&self, id: [u8; 16], now: u64, window_ms: u64) -> bool {
        let mut seen = self.seen.lock().unwrap();
        seen.retain(|_, expires| *expires > now);
        if seen.contains_key(&id) {
            return false;
        }
        seen.insert(id, now + window_ms);
        true
    }
}

impl Default for ReplayCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_replay_within_window() {
        let c = ReplayCache::new();
        let id = [1u8; 16];
        assert!(c.fresh(id, 1_000, 5_000));
        assert!(!c.fresh(id, 2_000, 5_000)); // replay
    }

    #[test]
    fn allows_after_window_expires() {
        let c = ReplayCache::new();
        let id = [2u8; 16];
        assert!(c.fresh(id, 1_000, 5_000));
        assert!(c.fresh(id, 10_000, 5_000)); // entry expired and pruned
    }

    #[test]
    fn distinct_ids_pass() {
        let c = ReplayCache::new();
        assert!(c.fresh([3u8; 16], 0, 1_000));
        assert!(c.fresh([4u8; 16], 0, 1_000));
    }
}
