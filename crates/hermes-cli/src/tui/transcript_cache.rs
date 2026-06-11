//! Incremental transcript render cache — divergence detection and line-span bookkeeping.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ratatui::text::Line;

use super::{TuiState, ViewDensity};

#[derive(Debug, Clone)]
pub(super) struct TranscriptCache {
    pub fingerprint: u64,
    pub width: u16,
    pub lines: Vec<Line<'static>>,
    pub visual_rows: usize,
    pub total_messages: usize,
    pub rendered_messages: usize,
    pub message_fingerprints: Vec<u64>,
    /// Exclusive line index after each transcript message (`messages[i]`).
    pub message_line_ends: Vec<usize>,
    /// `lines.len()` before the optional streaming tail block.
    pub messages_only_len: usize,
    pub show_timestamps: bool,
    pub view_density: ViewDensity,
    pub had_streaming: bool,
    pub expanded_tool_cards_sig: u64,
}

impl Default for TranscriptCache {
    fn default() -> Self {
        Self {
            fingerprint: 0,
            width: 0,
            lines: Vec::new(),
            visual_rows: 1,
            total_messages: 0,
            rendered_messages: 0,
            message_fingerprints: Vec::new(),
            message_line_ends: Vec::new(),
            messages_only_len: 0,
            show_timestamps: false,
            view_density: ViewDensity::Detailed,
            had_streaming: false,
            expanded_tool_cards_sig: 0,
        }
    }
}

impl TranscriptCache {
    pub fn line_start_for_message(&self, message_index: usize) -> usize {
        if message_index == 0 {
            return 0;
        }
        self.message_line_ends
            .get(message_index.saturating_sub(1))
            .copied()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TranscriptRefreshPlan {
    /// Fingerprint matches — reuse cached lines as-is.
    CacheHit,
    /// Append-only: new messages after an unchanged prefix.
    AppendFrom { message_index: usize },
    /// Re-render from the first diverging message index (truncate prefix lines).
    RebuildFrom { message_index: usize },
    /// Only the in-flight streaming tail changed; message prefix is stable.
    StreamTailOnly,
    /// Width, density, timestamps, undo, or other global change.
    FullRebuild,
}

pub(super) fn expanded_tool_cards_signature(expanded: &std::collections::HashSet<String>) -> u64 {
    let mut hasher = DefaultHasher::new();
    let mut keys: Vec<_> = expanded.iter().map(String::as_str).collect();
    keys.sort_unstable();
    for key in keys {
        key.hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) fn find_message_fingerprint_divergence(cached: &[u64], current: &[u64]) -> usize {
    let shared = cached.len().min(current.len());
    for idx in 0..shared {
        if cached[idx] != current[idx] {
            return idx;
        }
    }
    if current.len() < cached.len() {
        return current.len();
    }
    if current.len() > cached.len() {
        return cached.len();
    }
    shared
}

/// First tool message index affected by a tool-card expand/collapse change.
pub(super) fn first_tool_card_divergence_message_index(
    expanded: &std::collections::HashSet<String>,
) -> usize {
    if expanded.contains("__all__") {
        return 0;
    }
    expanded
        .iter()
        .filter_map(|key| key.strip_prefix("tool:"))
        .filter_map(|idx| idx.parse::<usize>().ok())
        .min()
        .unwrap_or(0)
}

pub(super) fn plan_transcript_refresh(
    cache: &TranscriptCache,
    fingerprint: u64,
    message_fingerprints: &[u64],
    wrap_width: u16,
    state: &TuiState,
    streaming_active: bool,
) -> TranscriptRefreshPlan {
    if cache.fingerprint == fingerprint && cache.width == wrap_width {
        return TranscriptRefreshPlan::CacheHit;
    }

    let expanded_sig = expanded_tool_cards_signature(&state.expanded_tool_cards);
    let layout_changed = cache.width != wrap_width
        || cache.show_timestamps != state.show_timestamps
        || cache.view_density != state.view_density;

    if layout_changed {
        return TranscriptRefreshPlan::FullRebuild;
    }

    if streaming_active
        && cache.message_fingerprints == message_fingerprints
        && cache.messages_only_len > 0
        && cache.message_line_ends.len() == message_fingerprints.len()
    {
        return TranscriptRefreshPlan::StreamTailOnly;
    }

    if !streaming_active
        && cache.had_streaming
        && cache.message_fingerprints == message_fingerprints
    {
        return TranscriptRefreshPlan::FullRebuild;
    }

    let diverge =
        find_message_fingerprint_divergence(&cache.message_fingerprints, message_fingerprints);

    let can_append = !cache.had_streaming
        && !streaming_active
        && cache.width == wrap_width
        && cache.total_messages > 0
        && message_fingerprints.len() > cache.total_messages
        && cache.show_timestamps == state.show_timestamps
        && cache.view_density == state.view_density
        && cache.message_fingerprints.len() == cache.total_messages
        && cache.message_line_ends.len() == cache.total_messages
        && message_fingerprints.starts_with(&cache.message_fingerprints);

    if can_append {
        return TranscriptRefreshPlan::AppendFrom {
            message_index: cache.total_messages,
        };
    }

    if cache.message_fingerprints == message_fingerprints
        && cache.expanded_tool_cards_sig != expanded_sig
        && !cache.message_line_ends.is_empty()
    {
        return TranscriptRefreshPlan::RebuildFrom {
            message_index: first_tool_card_divergence_message_index(&state.expanded_tool_cards),
        };
    }

    if cache.message_line_ends.len() == message_fingerprints.len()
        && diverge < message_fingerprints.len()
        && cache
            .message_fingerprints
            .starts_with(&message_fingerprints[..diverge])
    {
        return TranscriptRefreshPlan::RebuildFrom {
            message_index: diverge,
        };
    }

    TranscriptRefreshPlan::FullRebuild
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::ViewDensity;

    fn empty_cache() -> TranscriptCache {
        TranscriptCache::default()
    }

    #[test]
    fn find_divergence_on_mid_history_edit() {
        assert_eq!(
            find_message_fingerprint_divergence(&[1, 2, 3], &[1, 9, 3]),
            1
        );
    }

    #[test]
    fn find_divergence_on_append() {
        assert_eq!(find_message_fingerprint_divergence(&[1, 2], &[1, 2, 3]), 2);
    }

    #[test]
    fn find_divergence_on_undo_truncate() {
        assert_eq!(find_message_fingerprint_divergence(&[1, 2, 3], &[1, 2]), 2);
    }

    #[test]
    fn plan_cache_hit_when_fingerprint_matches() {
        let cache = TranscriptCache {
            fingerprint: 42,
            width: 80,
            ..empty_cache()
        };
        assert_eq!(
            plan_transcript_refresh(&cache, 42, &[], 80, &TuiState::default(), false),
            TranscriptRefreshPlan::CacheHit
        );
    }

    #[test]
    fn plan_append_when_messages_grow_with_stable_prefix() {
        let cache = TranscriptCache {
            fingerprint: 1,
            width: 80,
            total_messages: 2,
            message_fingerprints: vec![10, 20],
            message_line_ends: vec![3, 6],
            messages_only_len: 6,
            show_timestamps: false,
            view_density: ViewDensity::Detailed,
            ..empty_cache()
        };
        assert_eq!(
            plan_transcript_refresh(&cache, 2, &[10, 20, 30], 80, &TuiState::default(), false),
            TranscriptRefreshPlan::AppendFrom { message_index: 2 }
        );
    }

    #[test]
    fn plan_stream_tail_only_when_messages_stable() {
        let cache = TranscriptCache {
            fingerprint: 1,
            width: 80,
            total_messages: 1,
            message_fingerprints: vec![10],
            message_line_ends: vec![4],
            messages_only_len: 4,
            show_timestamps: false,
            view_density: ViewDensity::Detailed,
            ..empty_cache()
        };
        assert_eq!(
            plan_transcript_refresh(&cache, 2, &[10], 80, &TuiState::default(), true),
            TranscriptRefreshPlan::StreamTailOnly
        );
    }

    #[test]
    fn plan_rebuild_from_on_tool_card_toggle() {
        let mut state = TuiState::default();
        state.expanded_tool_cards.insert("tool:2".to_string());
        let cache = TranscriptCache {
            fingerprint: 1,
            width: 80,
            total_messages: 3,
            message_fingerprints: vec![10, 20, 30],
            message_line_ends: vec![2, 4, 8],
            messages_only_len: 8,
            show_timestamps: false,
            view_density: ViewDensity::Detailed,
            expanded_tool_cards_sig: 0,
            ..empty_cache()
        };
        assert_eq!(
            plan_transcript_refresh(&cache, 2, &[10, 20, 30], 80, &state, false),
            TranscriptRefreshPlan::RebuildFrom { message_index: 2 }
        );
    }

    #[test]
    fn first_tool_card_divergence_respects_all_toggle() {
        assert_eq!(
            first_tool_card_divergence_message_index(
                &["__all__".to_string()].into_iter().collect()
            ),
            0
        );
        assert_eq!(
            first_tool_card_divergence_message_index(
                &["tool:5".to_string(), "tool:2".to_string()]
                    .into_iter()
                    .collect()
            ),
            2
        );
    }
}
