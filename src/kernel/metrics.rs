use std::collections::BTreeMap;
use std::sync::Mutex;

/// In-memory counters/gauges exposed by kernel scripts via `ctx.metrics`.
/// Shell-owned and shared across worker VMs (like `ctx.store`), but not
/// persisted — metrics are per-process and reset on restart.
#[derive(Default)]
pub struct Metrics {
    inner: Mutex<BTreeMap<String, f64>>,
}

impl Metrics {
    /// Add `delta` (default 1) to a counter.
    pub fn counter(&self, name: &str, delta: f64) {
        let mut m = self.inner.lock().unwrap();
        *m.entry(name.to_string()).or_insert(0.0) += delta;
    }

    /// Set a gauge to `value`.
    pub fn gauge(&self, name: &str, value: f64) {
        self.inner.lock().unwrap().insert(name.to_string(), value);
    }

    /// Record an observation: accumulates `name.count` and `name.sum`.
    pub fn histogram(&self, name: &str, value: f64) {
        let mut m = self.inner.lock().unwrap();
        *m.entry(format!("{name}.count")).or_insert(0.0) += 1.0;
        *m.entry(format!("{name}.sum")).or_insert(0.0) += value;
    }

    pub fn snapshot(&self) -> BTreeMap<String, f64> {
        self.inner.lock().unwrap().clone()
    }
}
