use std::sync::atomic::{AtomicU64, Ordering};

/// Shared by concurrent workers; reservation never exceeds the remaining total.
pub struct EventBudget {
    remaining: AtomicU64,
}

impl EventBudget {
    pub fn new(total: u64) -> Self {
        Self {
            remaining: AtomicU64::new(total),
        }
    }

    pub fn claim(&self, requested: usize) -> Option<usize> {
        self.remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                (remaining != 0).then(|| remaining.saturating_sub(requested as u64))
            })
            .ok()
            .map(|remaining| remaining.min(requested as u64) as usize)
    }

    pub fn is_exhausted(&self) -> bool {
        self.remaining.load(Ordering::Relaxed) == 0
    }
}
