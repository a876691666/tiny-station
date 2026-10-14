//! Per-VM timer registry.
//!
//! Timers are owned by the worker that created them (their callbacks are Lua
//! values local to that VM). The worker's event loop polls [`TimerRegistry`]
//! for the next deadline and fires due callbacks — no shell involvement, and no
//! cross-VM state. On hot reload the old VM is dropped and the new script's
//! `init` re-registers its timers.

use crate::kernel::monotonic_ms;
use mlua::Function;

/// A scheduled callback.
struct TimerItem {
    id: u64,
    /// `Some(ms)` repeats; `None` fires once.
    interval: Option<u64>,
    next_ms: u64,
    func: Function,
}

#[derive(Default)]
pub(crate) struct TimerRegistry {
    items: Vec<TimerItem>,
    next_id: u64,
}

impl TimerRegistry {
    /// Schedule `func` after `delay_ms`; when `interval` is set it repeats.
    pub(crate) fn add(&mut self, delay_ms: u64, interval: Option<u64>, func: Function) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(TimerItem {
            id,
            interval,
            next_ms: monotonic_ms() + delay_ms,
            func,
        });
        id
    }

    pub(crate) fn cancel(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|t| t.id != id);
        self.items.len() != before
    }

    /// Absolute time (ms) of the earliest pending callback, if any.
    pub(crate) fn next_deadline(&self) -> Option<u64> {
        self.items.iter().map(|t| t.next_ms).min()
    }

    /// Remove and return the callbacks that are due (re-arming repeaters).
    pub(crate) fn take_due(&mut self, now: u64) -> Vec<Function> {
        let mut due = Vec::new();
        for item in self.items.iter_mut() {
            if item.next_ms <= now {
                due.push(item.func.clone());
                match item.interval {
                    Some(iv) => item.next_ms = now + iv.max(1),
                    None => item.next_ms = u64::MAX, // marked for removal
                }
            }
        }
        self.items.retain(|t| t.next_ms != u64::MAX);
        due
    }
}
