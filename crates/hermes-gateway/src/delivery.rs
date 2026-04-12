//! Delivery queue for deferred platform sends.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct DeliveryItem {
    pub platform: String,
    pub chat_id: String,
    pub text: String,
}

#[derive(Clone, Default)]
pub struct DeliveryQueue {
    queue: Arc<Mutex<VecDeque<DeliveryItem>>>,
}

impl DeliveryQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue(&self, item: DeliveryItem) {
        if let Ok(mut q) = self.queue.lock() {
            q.push_back(item);
        }
    }

    pub fn dequeue(&self) -> Option<DeliveryItem> {
        self.queue.lock().ok().and_then(|mut q| q.pop_front())
    }

    pub fn len(&self) -> usize {
        self.queue.lock().map(|q| q.len()).unwrap_or(0)
    }
}
