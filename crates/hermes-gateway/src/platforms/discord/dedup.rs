//! Inbound message deduplication (RESUME replay protection).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const DEDUP_WINDOW: Duration = Duration::from_secs(300);
const DEDUP_MAX: usize = 1000;

/// Tracks recently seen Discord message IDs.
#[derive(Debug, Default)]
pub struct MessageDedup {
    queue: VecDeque<(String, Instant)>,
}

impl MessageDedup {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Returns `true` if this message id was already seen (duplicate).
    pub fn is_duplicate(&mut self, message_id: &str) -> bool {
        if message_id.is_empty() {
            return false;
        }
        self.evict_expired();
        if self.queue.iter().any(|(id, _)| id == message_id) {
            return true;
        }
        if self.queue.len() >= DEDUP_MAX {
            self.queue.pop_front();
        }
        self.queue.push_back((message_id.to_string(), Instant::now()));
        false
    }

    fn evict_expired(&mut self) {
        let cutoff = Instant::now() - DEDUP_WINDOW;
        while self
            .queue
            .front()
            .is_some_and(|(_, t)| *t < cutoff)
        {
            self.queue.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d01_first_id_not_duplicate() {
        let mut d = MessageDedup::new();
        assert!(!d.is_duplicate("msg-1"));
    }

    #[test]
    fn d02_repeat_id_is_duplicate() {
        let mut d = MessageDedup::new();
        assert!(!d.is_duplicate("msg-1"));
        assert!(d.is_duplicate("msg-1"));
    }

    #[test]
    fn d03_capacity_evicts_oldest() {
        let mut d = MessageDedup::new();
        for i in 0..DEDUP_MAX {
            let id = format!("msg-{i}");
            assert!(!d.is_duplicate(&id));
        }
        assert!(!d.is_duplicate("msg-new"));
        // oldest msg-0 should be evicted
        assert!(!d.is_duplicate("msg-0"));
    }

    #[test]
    fn d04_empty_id_never_deduped() {
        let mut d = MessageDedup::new();
        assert!(!d.is_duplicate(""));
        assert!(!d.is_duplicate(""));
    }
}
