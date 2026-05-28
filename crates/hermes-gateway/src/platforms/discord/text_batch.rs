//! Inbound text debouncing/aggregation (P2-4).

use std::sync::Arc;
use std::time::Duration;

use tracing::debug;

use crate::gateway::IncomingMessage;

use super::config::DiscordConfig;
use super::gateway_loop::DiscordInner;

/// When a single inbound chunk is at least this large, use split delay (client-side splits).
const INBOUND_SPLIT_THRESHOLD: usize = 1800;

pub struct PendingInboundText {
    message: IncomingMessage,
    last_chunk_len: usize,
}

fn batch_key(msg: &IncomingMessage) -> String {
    format!("{}:{}", msg.platform, msg.chat_id)
}

fn should_batch_inbound(msg: &IncomingMessage, config: &DiscordConfig) -> bool {
    config.text_batch_delay_seconds > 0.0
        && msg.interaction_id.is_none()
        && msg.media_urls.is_empty()
        && !msg.text.is_empty()
}

pub async fn deliver_inbounds(inner: &Arc<DiscordInner>, inbounds: Vec<IncomingMessage>) {
        let Some(tx) = inner.inbound_tx.read().await.clone() else {
            debug!("discord inbound dropped: no inbound_tx configured");
            return;
        };
        for msg in inbounds {
            if should_batch_inbound(&msg, &inner.config) {
                inner.enqueue_inbound_text(msg).await;
            } else {
                let _ = tx.send(msg).await;
            }
        }
}

impl DiscordInner {
    async fn enqueue_inbound_text(self: &Arc<Self>, event: IncomingMessage) {
        let key = batch_key(&event);
        let chunk_len = event.text.chars().count();
        let flush_delay = if chunk_len >= INBOUND_SPLIT_THRESHOLD {
            Duration::from_secs_f64(self.config.text_batch_split_delay_seconds)
        } else {
            Duration::from_secs_f64(self.config.text_batch_delay_seconds)
        };

        {
            let mut pending = self.inbound_text_pending.write().await;
            if let Some(existing) = pending.get_mut(&key) {
                if !event.text.is_empty() {
                    if existing.message.text.is_empty() {
                        existing.message.text = event.text.clone();
                    } else {
                        existing.message.text.push('\n');
                        existing.message.text.push_str(&event.text);
                    }
                    if event.message_id.is_some() {
                        existing.message.message_id = event.message_id.clone();
                    }
                }
                existing.last_chunk_len = chunk_len;
            } else {
                pending.insert(
                    key.clone(),
                    PendingInboundText {
                        message: event,
                        last_chunk_len: chunk_len,
                    },
                );
            }
        }

        if let Some(task) = self.inbound_text_tasks.write().await.remove(&key) {
            task.abort();
        }
        let inner = Arc::clone(self);
        let key_for_task = key.clone();
        let handle = tokio::spawn(async move {
            inner.flush_inbound_text(key_for_task, flush_delay).await;
        });
        self.inbound_text_tasks
            .write()
            .await
            .insert(key, handle);
    }

    async fn flush_inbound_text(self: Arc<Self>, key: String, delay: Duration) {
        tokio::time::sleep(delay).await;
        let batch = {
            let mut pending = self.inbound_text_pending.write().await;
            pending.remove(&key)
        };
        self.inbound_text_tasks.write().await.remove(&key);
        let Some(batch) = batch else {
            return;
        };
        debug!(
            key = %key,
            text_chars = batch.message.text.chars().count(),
            "Discord inbound text batch flushing"
        );
        if let Some(tx) = self.inbound_tx.read().await.clone() {
            let _ = tx.send(batch.message).await;
        }
    }
}
