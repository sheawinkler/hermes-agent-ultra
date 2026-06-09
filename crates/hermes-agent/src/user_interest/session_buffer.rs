//! In-memory per-session POI accumulation (no SQLite until session end).

use std::collections::HashMap;

use hermes_config::InterestConfig;

use super::extract::{extract_signals_from_text, filter_poi_signals};
use super::quality::{filter_persistable_signals, should_extract_user_turn};
use super::store::InterestSignal;
use super::types::ExtractOptions;

/// Session-scoped buffer: merge signals by topic id until flush at session end.
#[derive(Debug, Default)]
pub struct SessionPoiBuffer {
    by_id: HashMap<String, InterestSignal>,
}

impl SessionPoiBuffer {
    pub fn absorb_turn(&mut self, user_text: &str, config: &InterestConfig) {
        if !config.per_turn_buffer {
            return;
        }
        let trimmed = user_text.trim();
        if !should_extract_user_turn(trimmed, config.min_turn_chars) {
            return;
        }
        // LLM mode: only buffer high-trust explicit declarations; semantic POI at session end.
        let raw = if config.uses_llm() && !config.uses_rules() {
            super::declared::extract_declared_interests(trimmed, 0.35)
        } else {
            extract_signals_from_text(
                trimmed,
                0.35,
                ExtractOptions {
                    include_keywords: false,
                },
            )
        };
        let signals = filter_persistable_signals(filter_poi_signals(raw));
        for signal in signals {
            self.merge_signal(signal);
        }
    }

    fn merge_signal(&mut self, signal: InterestSignal) {
        match self.by_id.get_mut(&signal.id) {
            Some(existing) => {
                existing.weight_delta = (existing.weight_delta + signal.weight_delta).min(0.5);
                existing.confidence = existing.confidence.max(signal.confidence);
                if signal.summary.len() > existing.summary.len() {
                    existing.summary = signal.summary;
                }
                if signal.label.len() > existing.label.len() {
                    existing.label = signal.label;
                }
                for tag in signal.tags {
                    if !existing.tags.contains(&tag) {
                        existing.tags.push(tag);
                    }
                }
            }
            None => {
                self.by_id.insert(signal.id.clone(), signal);
            }
        }
    }

    pub fn drain(&mut self) -> Vec<InterestSignal> {
        self.by_id.drain().map(|(_, v)| v).collect()
    }

    pub fn clear(&mut self) {
        self.by_id.clear();
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::InterestConfig;

    #[test]
    fn buffer_dedupes_by_id() {
        let mut config = InterestConfig::default();
        config.extract_mode = "hybrid".to_string();
        let mut buf = SessionPoiBuffer::default();
        buf.absorb_turn(
            "Continue the Rust parity port in crates/hermes-parity-tests",
            &config,
        );
        buf.absorb_turn(
            "More Rust work in crates/hermes-parity-tests please",
            &config,
        );
        let drained = buf.drain();
        assert!(drained.iter().any(|s| s.id.contains("rust") || s.id.contains("parity")));
        assert!(drained.len() < 6);
    }
}
