//! VAD-bound utterance queue: slices tagged by `seq`, fed to RK ASR in order.
//!
//! Flow: enqueue slices → parallel reorder feed → collect ASR text per seq →
//! concat in seq order → `finish_utterance` → LLM → clear queue.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::asr::AsrEngine;
use crate::error::Result;
use crate::orchestrator::asr_text::merge_hypothesis;

/// Control plane for the ordered ASR feed task.
#[derive(Debug)]
pub enum FeedCommand {
    Begin {
        utterance_id: u64,
    },
    Slice {
        utterance_id: u64,
        seq: u64,
        pcm: Vec<u8>,
    },
    /// `total_slices` = count pushed before seal; feeder signals when all are sent to ASR.
    Seal {
        utterance_id: u64,
        total_slices: u64,
    },
    Reset,
}

#[derive(Debug, Default)]
struct FeederState {
    utterance_id: u64,
    next_feed_seq: u64,
    expected_slices: Option<u64>,
    pending: BTreeMap<u64, Vec<u8>>,
    sealed: bool,
    feed_complete_sent: bool,
}

impl FeederState {
    fn reset(&mut self) {
        self.utterance_id = 0;
        self.next_feed_seq = 0;
        self.expected_slices = None;
        self.pending.clear();
        self.sealed = false;
        self.feed_complete_sent = false;
    }

    fn begin(&mut self, utterance_id: u64) {
        self.utterance_id = utterance_id;
        self.next_feed_seq = 0;
        self.expected_slices = None;
        self.pending.clear();
        self.sealed = false;
        self.feed_complete_sent = false;
    }

    async fn push_and_drain(
        &mut self,
        asr: &Arc<dyn AsrEngine>,
        utterance_id: u64,
        seq: u64,
        pcm: Vec<u8>,
        ack_tx: &mpsc::Sender<(u64, u64)>,
    ) -> Result<()> {
        if utterance_id != self.utterance_id {
            debug!(
                expected = self.utterance_id,
                got = utterance_id,
                seq,
                "asr feed: drop slice for stale utterance"
            );
            return Ok(());
        }
        self.pending.insert(seq, pcm);
        self.drain_ready(asr, ack_tx).await
    }

    async fn drain_ready(
        &mut self,
        asr: &Arc<dyn AsrEngine>,
        ack_tx: &mpsc::Sender<(u64, u64)>,
    ) -> Result<()> {
        while let Some(pcm) = self.pending.remove(&self.next_feed_seq) {
            let seq = self.next_feed_seq;
            let _ = ack_tx.try_send((self.utterance_id, seq));
            asr.send_audio(pcm).await?;
            self.next_feed_seq += 1;
        }
        Ok(())
    }

    fn seal(&mut self, utterance_id: u64, total_slices: u64) {
        if utterance_id == self.utterance_id {
            self.sealed = true;
            self.expected_slices = Some(total_slices);
        }
    }

    fn feed_complete(&self) -> bool {
        self.sealed
            && self.pending.is_empty()
            && self
                .expected_slices
                .is_some_and(|n| self.next_feed_seq >= n)
    }

    fn signal_feed_complete_if_ready(&mut self, done_tx: &mpsc::Sender<(u64, u64)>) {
        if self.feed_complete_sent || !self.feed_complete() {
            return;
        }
        self.feed_complete_sent = true;
        info!(
            utterance_id = self.utterance_id,
            slices = self.next_feed_seq,
            "utterance feed: queue fully sent to ASR"
        );
        let _ = done_tx.try_send((self.utterance_id, self.next_feed_seq));
    }
}

/// Handle returned by [`spawn_ordered_asr_feeder`].
pub struct UtteranceFeeder {
    pub cmd_tx: mpsc::Sender<FeedCommand>,
    pub feed_done_rx: mpsc::Receiver<(u64, u64)>,
    pub feed_ack_rx: mpsc::Receiver<(u64, u64)>,
}

/// Per-slice ASR text collected in queue `seq` order; flush via [`Self::best_transcript`].
#[derive(Debug, Default)]
pub struct UtteranceTranscript {
    utterance_id: u64,
    last_fed_seq: u64,
    by_seq: BTreeMap<u64, String>,
    /// Finalized SDK segments (`ASR_STATE_FINISH` append per ROCKASR2 demo).
    committed: String,
    /// In-progress sentence hypothesis (`result` while `ASR_STATE_RUNNING`).
    current: String,
    best_full: String,
}

impl UtteranceTranscript {
    pub fn reset(&mut self, utterance_id: u64) {
        self.utterance_id = utterance_id;
        self.last_fed_seq = 0;
        self.by_seq.clear();
        self.committed.clear();
        self.current.clear();
        self.best_full.clear();
    }

    pub fn on_slice_fed(&mut self, utterance_id: u64, seq: u64) {
        if utterance_id == self.utterance_id {
            self.last_fed_seq = seq;
        }
    }

    pub fn append_hypothesis(&mut self, piece: &str, full: Option<&str>) {
        self.record_peak(piece);
        if let Some(full) = full {
            self.record_peak(full);
        }
        let before = self.concat();
        self.record_peak(&before);
        merge_hypothesis(&mut self.current, piece, full);
        let after = self.concat();
        self.record_peak(&after);
        if after == before {
            return;
        }
        let delta = if after.starts_with(&before) {
            &after[before.len()..]
        } else {
            after.as_str()
        };
        self.by_seq
            .entry(self.last_fed_seq)
            .or_default()
            .push_str(delta);
    }

    /// Commit a finalized SDK sentence (`ASR_STATE_FINISH`) into the multi-segment transcript.
    pub fn commit_segment(&mut self, sentence: &str) {
        let s = sentence.trim();
        if s.is_empty() {
            return;
        }
        self.record_peak(s);

        let seg = if self.current.chars().count() >= s.chars().count() {
            std::mem::take(&mut self.current)
        } else {
            self.current.clear();
            s.to_string()
        };
        if seg.is_empty() {
            return;
        }
        if self.committed.ends_with(&seg) {
            return;
        }
        self.committed.push_str(&seg);
        let committed = self.committed.clone();
        self.record_peak(&committed);
        self.by_seq
            .entry(self.last_fed_seq)
            .or_default()
            .push_str(&seg);
    }

    fn concat(&self) -> String {
        format!("{}{}", self.committed, self.current)
    }

    fn record_peak(&mut self, text: &str) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        if t.chars().count() > self.best_full.chars().count() {
            self.best_full = t.to_string();
        }
    }

    /// Longest full hypothesis seen during the utterance (preferred at flush).
    pub fn best_full(&self) -> &str {
        &self.best_full
    }

    /// Best transcript for flush: longest of tracked full vs assembled merge.
    pub fn best_transcript(&self) -> String {
        let assembled = self.concat();
        if self.best_full.chars().count() >= assembled.chars().count() {
            self.best_full.clone()
        } else {
            assembled
        }
    }

    /// Concatenate committed segments plus the in-progress sentence.
    pub fn concat_transcript(&self) -> String {
        self.concat()
    }

    pub fn clear(&mut self) {
        self.by_seq.clear();
        self.committed.clear();
        self.current.clear();
        self.best_full.clear();
    }
}

/// Spawn ordered ASR feeder (enqueue can outrun SDK; drain restores seq order).
pub fn spawn_ordered_asr_feeder(asr: Arc<dyn AsrEngine>) -> UtteranceFeeder {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<FeedCommand>(256);
    let (feed_done_tx, feed_done_rx) = mpsc::channel::<(u64, u64)>(8);
    let (feed_ack_tx, feed_ack_rx) = mpsc::channel::<(u64, u64)>(512);
    tokio::spawn(async move {
        let mut state = FeederState::default();
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                FeedCommand::Begin { utterance_id } => {
                    debug!(utterance_id, "utterance feed: begin");
                    state.begin(utterance_id);
                }
                FeedCommand::Slice {
                    utterance_id,
                    seq,
                    pcm,
                } => {
                    if let Err(e) = state
                        .push_and_drain(&asr, utterance_id, seq, pcm, &feed_ack_tx)
                        .await
                    {
                        warn!(error = %e, utterance_id, seq, "asr feed: send_audio failed");
                    }
                    state.signal_feed_complete_if_ready(&feed_done_tx);
                }
                FeedCommand::Seal {
                    utterance_id,
                    total_slices,
                } => {
                    debug!(utterance_id, total_slices, "utterance feed: seal");
                    state.seal(utterance_id, total_slices);
                    if let Err(e) = state.drain_ready(&asr, &feed_ack_tx).await {
                        warn!(error = %e, utterance_id, "asr feed: drain on seal failed");
                    }
                    state.signal_feed_complete_if_ready(&feed_done_tx);
                }
                FeedCommand::Reset => {
                    debug!("utterance feed: reset");
                    state.reset();
                }
            }
        }
    });
    UtteranceFeeder {
        cmd_tx,
        feed_done_rx,
        feed_ack_rx,
    }
}

/// Session-side utterance queue (VAD → slice seq → feed commands).
#[derive(Debug)]
pub struct UtterancePipeline {
    utterance_id: u64,
    open: bool,
    sealed: bool,
    next_push_seq: u64,
    feed_tx: mpsc::Sender<FeedCommand>,
}

impl UtterancePipeline {
    pub fn new(feed_tx: mpsc::Sender<FeedCommand>) -> Self {
        Self {
            utterance_id: 0,
            open: false,
            sealed: false,
            next_push_seq: 0,
            feed_tx,
        }
    }

    pub fn utterance_id(&self) -> u64 {
        self.utterance_id
    }

    pub fn slice_count(&self) -> u64 {
        self.next_push_seq
    }

    pub fn is_open(&self) -> bool {
        self.open && !self.sealed
    }

    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    pub fn begin(&mut self) {
        self.utterance_id = self.utterance_id.saturating_add(1);
        self.open = true;
        self.sealed = false;
        self.next_push_seq = 0;
        let _ = self.feed_tx.try_send(FeedCommand::Begin {
            utterance_id: self.utterance_id,
        });
        debug!(
            utterance_id = self.utterance_id,
            "utterance pipeline: begin"
        );
    }

    pub fn push_pcm(&mut self, pcm: Vec<u8>) -> Option<u64> {
        if !self.is_open() {
            return None;
        }
        let seq = self.next_push_seq;
        self.next_push_seq += 1;
        let _ = self.feed_tx.try_send(FeedCommand::Slice {
            utterance_id: self.utterance_id,
            seq,
            pcm,
        });
        Some(seq)
    }

    pub fn seal(&mut self) {
        if !self.open {
            return;
        }
        self.sealed = true;
        self.open = false;
        let total_slices = self.next_push_seq;
        let _ = self.feed_tx.try_send(FeedCommand::Seal {
            utterance_id: self.utterance_id,
            total_slices,
        });
        debug!(
            utterance_id = self.utterance_id,
            total_slices, "utterance pipeline: seal"
        );
    }

    pub fn clear_after_llm(&mut self) {
        self.open = false;
        self.sealed = false;
        self.next_push_seq = 0;
        let _ = self.feed_tx.try_send(FeedCommand::Reset);
        debug!(
            utterance_id = self.utterance_id,
            "utterance pipeline: clear after llm"
        );
    }
}

/// Wait until every slice in the sealed utterance has been fed to ASR in order.
pub async fn wait_utterance_fed(
    feed_done_rx: &mut mpsc::Receiver<(u64, u64)>,
    utterance_id: u64,
    timeout_ms: u64,
) -> bool {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, feed_done_rx.recv()).await {
            Ok(Some((id, slices))) if id == utterance_id => {
                debug!(utterance_id = id, slices, "utterance feed complete ack");
                return true;
            }
            Ok(Some(_)) => continue,
            Ok(None) => return false,
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_concat_in_seq_order() {
        let mut t = UtteranceTranscript::default();
        t.reset(1);
        t.on_slice_fed(1, 0);
        t.append_hypothesis("帮", None);
        t.on_slice_fed(1, 1);
        t.append_hypothesis("我查一下", Some("帮我查一下"));
        t.append_hypothesis("查一下明天的", Some("帮我查一下明天的"));
        t.append_hypothesis("查一下明天的天气。", Some("帮我查一下明天的天气。"));
        assert_eq!(t.concat_transcript(), "帮我查一下明天的天气。");
    }

    #[test]
    fn transcript_ignores_asr_rollback() {
        let mut t = UtteranceTranscript::default();
        t.reset(1);
        t.on_slice_fed(1, 0);
        t.append_hypothesis("现", None);
        t.append_hypothesis("现在几点", Some("现在几点"));
        t.append_hypothesis("现在", Some("现在"));
        t.append_hypothesis("了。", None);
        assert_eq!(t.concat_transcript(), "现在几点了。");
    }

    #[test]
    fn transcript_tracks_best_full() {
        let mut t = UtteranceTranscript::default();
        t.reset(1);
        t.on_slice_fed(1, 0);
        t.append_hypothesis("你刚才", None);
        t.append_hypothesis("你刚才说", Some("你刚才说"));
        t.append_hypothesis("的关于蚂蚁的那个笑话", Some("的关于蚂蚁的那个笑话"));
        t.append_hypothesis("是啥意思？", Some("你刚才说的关于蚂蚁的那个笑话是啥意思？"));
        assert_eq!(t.best_full(), "你刚才说的关于蚂蚁的那个笑话是啥意思？");
    }

    #[test]
    fn transcript_commits_sdk_segment_finish() {
        let mut t = UtteranceTranscript::default();
        t.reset(1);
        t.on_slice_fed(1, 0);
        t.append_hypothesis("你好", Some("你好。"));
        t.commit_segment("你好。");
        t.on_slice_fed(1, 1);
        t.append_hypothesis("世界", Some("世界"));
        t.append_hypothesis("。", Some("世界。"));
        t.commit_segment("世界。");
        assert_eq!(t.concat_transcript(), "你好。世界。");
    }

    #[test]
    fn transcript_segment_finish_without_duplicate() {
        let mut t = UtteranceTranscript::default();
        t.reset(1);
        t.on_slice_fed(1, 0);
        t.append_hypothesis("现在几点", Some("现在几点"));
        t.append_hypothesis("了。", Some("现在几点了。"));
        t.commit_segment("现在几点了。");
        t.commit_segment("现在几点了。");
        assert_eq!(t.concat_transcript(), "现在几点了。");
    }

    #[test]
    fn feeder_state_drains_out_of_order_by_seq() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut state = FeederState::default();
            state.begin(1);
            state.pending.insert(1, vec![2]);
            state.pending.insert(0, vec![1]);
            state.next_feed_seq = 0;
            let mut fed = Vec::new();
            while let Some(pcm) = state.pending.remove(&state.next_feed_seq) {
                fed.push((state.next_feed_seq, pcm));
                state.next_feed_seq += 1;
            }
            assert_eq!(fed, vec![(0, vec![1]), (1, vec![2])]);
            state.seal(1, 2);
            assert!(state.feed_complete());
        });
    }
}
