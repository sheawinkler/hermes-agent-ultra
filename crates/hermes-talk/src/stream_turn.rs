//! LLM → TTS turn handling for Rockchip (`feature = "rockchip"`, no `sherpa-asr-tts`).
//!
//! Stream policy: only `content` streams to TTS (thinking gate strips inline think blocks).
//! `reasoning_content` is logged to stderr only, never spoken.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::asr::AsrEngine;
use crate::llm::{AccumulatedToolCall, ChatMessage, LlmClient, ToolCall};
use crate::orchestrator::{
    StreamingThinkTtsGate, append_speakable_stream_delta, flush_remainder,
    has_actionable_tool_deltas, normalize_tts_text, speakable_after_think_close,
    stream_has_think_close_tag, strip_think_blocks, take_early_chunk, take_sentence,
};
use crate::tools;
use crate::tools::hermes_queue::HermesQueueSender;
use crate::tts::TtsEngine;
use crate::{audio::AudioPlayback, config::LlmConfig};

/// Turn completion signal from LLM/TTS worker → session main loop.
#[derive(Debug, Clone)]
pub struct TurnDone {
    pub assistant_text: String,
    pub epoch: u64,
    pub shutup: bool,
    /// False when no audio was synthesized/played for this turn.
    pub tts_spoken: bool,
}

pub async fn send_turn_done(
    tx: &mpsc::Sender<TurnDone>,
    assistant_text: impl Into<String>,
    epoch: u64,
    shutup: bool,
    tts_spoken: bool,
) {
    let _ = tx
        .send(TurnDone {
            assistant_text: assistant_text.into(),
            epoch,
            shutup,
            tts_spoken,
        })
        .await;
}

fn log_llm_output(round: u32, content: &str) {
    if content.trim().is_empty() {
        return;
    }
    info!(
        round,
        chars = content.chars().count(),
        content = %content,
        "llm output"
    );
}

#[derive(Debug, Default)]
struct TtsPipelineStats {
    gate_out_chars: usize,
    blocked_actionable_events: u32,
    blocked_actionable_chars: usize,
    deferred_chars: usize,
    append_accepted_events: u32,
    flush_tail_chars: usize,
    fallback_strip_chars: usize,
}

fn partial_tool_names(map: &HashMap<u32, AccumulatedToolCall>) -> Vec<String> {
    let mut indices: Vec<u32> = map.keys().copied().collect();
    indices.sort();
    indices
        .into_iter()
        .filter_map(|idx| {
            let name = map.get(&idx)?.name.trim();
            if name.is_empty() {
                None
            } else {
                Some(format!("{idx}:{name}"))
            }
        })
        .collect()
}

fn primary_tts_skip_reason(
    gate: &crate::orchestrator::TtsGateDiagnostics,
    merged_has_close: bool,
    stats: &TtsPipelineStats,
    merged_strip_chars: usize,
    tts_buf_chars: usize,
    actionable_at_end: bool,
    partial_tools: &[String],
) -> &'static str {
    if actionable_at_end && stats.blocked_actionable_chars > 0 {
        return "tool_call_deltas_blocked_tts_while_actionable";
    }
    if gate.thinking_enabled && gate.waiting_close && !merged_has_close {
        return "thinking_gate_waiting_close_tag_never_seen";
    }
    if gate.thinking_enabled && gate.waiting_close && merged_has_close {
        return "thinking_gate_still_waiting_despite_close_tag_in_merged";
    }
    if stats.gate_out_chars == 0 && merged_strip_chars == 0 {
        return "no_speakable_text_in_merged_stream";
    }
    if stats.gate_out_chars > 0 && tts_buf_chars == 0 && stats.append_accepted_events == 0 {
        return "gate_emitted_text_but_nothing_reached_tts_buf";
    }
    if tts_buf_chars > 0 && stats.append_accepted_events > 0 {
        return "tts_buf_had_text_but_append_or_pump_failed";
    }
    if !partial_tools.is_empty() && stats.blocked_actionable_events > 0 {
        return "partial_tool_stream_blocked_tts";
    }
    "unknown"
}

fn log_tts_skip_diagnosis(
    round: u32,
    thinking_enabled: bool,
    gate: &crate::orchestrator::TtsGateDiagnostics,
    reasoning_buf: &str,
    content_buf: &str,
    tts_buf: &str,
    stats: &TtsPipelineStats,
    tool_call_map: &HashMap<u32, AccumulatedToolCall>,
    actionable_at_end: bool,
    tools_attached: bool,
) {
    let merged = format!("{reasoning_buf}{content_buf}");
    let merged_chars = merged.chars().count();
    let merged_has_close = stream_has_think_close_tag(&merged);
    let merged_strip = speakable_after_think_close(&merged);
    let merged_strip_chars = merged_strip.chars().count();
    let tts_buf_chars = tts_buf.chars().count();
    let partial_tools = partial_tool_names(tool_call_map);
    let primary = primary_tts_skip_reason(
        gate,
        merged_has_close,
        stats,
        merged_strip_chars,
        tts_buf_chars,
        actionable_at_end,
        &partial_tools,
    );

    warn!(
        round,
        thinking_enabled,
        gate_waiting_close = gate.waiting_close,
        gate_speaking = gate.thinking_enabled && !gate.waiting_close,
        gate_pending_chars = gate.pending_chars,
        reasoning_chars = reasoning_buf.chars().count(),
        content_chars = content_buf.chars().count(),
        merged_chars,
        merged_has_close_tag = merged_has_close,
        merged_strip_chars,
        gate_emitted_chars = stats.gate_out_chars,
        blocked_actionable_events = stats.blocked_actionable_events,
        blocked_actionable_chars = stats.blocked_actionable_chars,
        deferred_chars = stats.deferred_chars,
        append_accepted_events = stats.append_accepted_events,
        flush_tail_chars = stats.flush_tail_chars,
        fallback_strip_chars = stats.fallback_strip_chars,
        tts_buf_chars,
        actionable_at_end,
        tools_attached,
        partial_tool_names = ?partial_tools,
        primary_reason = primary,
        "tts skip diagnosis: LLM had text but no audio was synthesized"
    );

    if !merged_strip.is_empty() {
        let preview: String = merged_strip.chars().take(80).collect();
        warn!(
            round,
            strip_preview = %preview,
            "tts skip diagnosis: speakable text existed after strip_think_blocks"
        );
    }
}

async fn speak_fallback_stripped(
    tts: &Arc<dyn TtsEngine>,
    content_buf: &str,
    tts_any: &mut bool,
    stats: &mut TtsPipelineStats,
) {
    if *tts_any {
        return;
    }
    if content_buf.trim().is_empty() {
        return;
    }
    let speakable = speakable_after_think_close(content_buf);
    stats.fallback_strip_chars = speakable.chars().count();
    if speakable.trim().is_empty() {
        return;
    }
    info!(
        chars = speakable.chars().count(),
        "tts: fallback speak stripped content"
    );
    if tts
        .append_text(&normalize_tts_text(&speakable))
        .await
        .is_ok()
    {
        *tts_any = true;
    }
}

async fn pump_gate_delta(
    tts: &Arc<dyn TtsEngine>,
    tts_gate: &mut StreamingThinkTtsGate,
    delta: &str,
    tts_buf: &mut String,
    deferred_speakable: &mut String,
    sentence_min: usize,
    tts_first_chunk: usize,
    sent_early: &mut bool,
    tts_any: &mut bool,
    actionable: bool,
    log_early_latency: Option<Instant>,
    stats: &mut TtsPipelineStats,
) {
    let speakable = tts_gate.push(delta);
    stats.gate_out_chars += speakable.chars().count();
    if actionable {
        if !speakable.is_empty() {
            stats.blocked_actionable_events += 1;
            stats.blocked_actionable_chars += speakable.chars().count();
            deferred_speakable.push_str(&speakable);
            stats.deferred_chars += speakable.chars().count();
        }
        return;
    }
    if append_speakable_stream_delta(tts_buf, &speakable, false) {
        stats.append_accepted_events += 1;
        pump_streaming_tts(
            tts,
            tts_buf,
            sentence_min,
            tts_first_chunk,
            sent_early,
            tts_any,
            log_early_latency,
        )
        .await;
    }
}

async fn drain_deferred_speakable(
    tts: &Arc<dyn TtsEngine>,
    deferred_speakable: &mut String,
    tts_buf: &mut String,
    sentence_min: usize,
    tts_first_chunk: usize,
    sent_early: &mut bool,
    tts_any: &mut bool,
    stats: &mut TtsPipelineStats,
) {
    if deferred_speakable.is_empty() {
        return;
    }
    info!(
        chars = deferred_speakable.chars().count(),
        "tts: draining speakable deferred while tool deltas were actionable"
    );
    if append_speakable_stream_delta(tts_buf, deferred_speakable, false) {
        stats.append_accepted_events += 1;
        pump_streaming_tts(
            tts,
            tts_buf,
            sentence_min,
            tts_first_chunk,
            sent_early,
            tts_any,
            None,
        )
        .await;
    }
    deferred_speakable.clear();
    stats.deferred_chars = 0;
}

/// Push accumulated speakable text to TTS as early as each stream delta allows.
async fn pump_streaming_tts(
    tts: &Arc<dyn TtsEngine>,
    tts_buf: &mut String,
    sentence_min: usize,
    tts_first_chunk: usize,
    sent_early: &mut bool,
    tts_any: &mut bool,
    _log_early_latency: Option<Instant>,
) {
    if !*sent_early {
        if let Some(chunk) = take_early_chunk(tts_buf, tts_first_chunk) {
            if tts.append_text(&normalize_tts_text(&chunk)).await.is_ok() {
                *tts_any = true;
            }
            *sent_early = true;
        }
    }
    while let Some(sentence) = take_sentence(tts_buf, sentence_min) {
        if tts
            .append_text(&normalize_tts_text(&sentence))
            .await
            .is_ok()
        {
            *tts_any = true;
        }
    }
    if let Some(rest) = flush_remainder(tts_buf) {
        if !rest.trim().is_empty() {
            if tts.append_text(&normalize_tts_text(&rest)).await.is_ok() {
                *tts_any = true;
            }
            *sent_early = true;
        }
    }
}

/// Drain any remaining buffered speakable text at end of LLM stream.
async fn pump_streaming_tts_tail(
    tts: &Arc<dyn TtsEngine>,
    tts_buf: &mut String,
    tts_any: &mut bool,
) {
    if let Some(rest) = flush_remainder(tts_buf) {
        if tts.append_text(&normalize_tts_text(&rest)).await.is_ok() {
            *tts_any = true;
        }
    }
}

fn tool_calls_from_stream_map(map: &HashMap<u32, AccumulatedToolCall>) -> Vec<ToolCall> {
    let mut indices: Vec<u32> = map.keys().copied().collect();
    indices.sort();
    indices
        .into_iter()
        .filter_map(|idx| {
            let acc = map.get(&idx)?;
            if acc.name.trim().is_empty() {
                return None;
            }
            Some(ToolCall {
                id: if acc.id.is_empty() {
                    format!("call_{idx}")
                } else {
                    acc.id.clone()
                },
                r#type: "function".to_string(),
                function: crate::llm::ToolCallFunction {
                    name: acc.name.clone(),
                    arguments: acc.arguments.clone(),
                },
            })
        })
        .collect()
}

fn core_tool_call_to_talk(tc: hermes_core::ToolCall) -> ToolCall {
    let name = match tc.function.name.as_str() {
        "execute_command" => "execute",
        other => other,
    };
    ToolCall {
        id: tc.id,
        r#type: "function".to_string(),
        function: crate::llm::ToolCallFunction {
            name: name.to_string(),
            arguments: tc.function.arguments,
        },
    }
}

fn resolve_tool_calls(
    buf: &str,
    tool_call_map: &HashMap<u32, AccumulatedToolCall>,
) -> Vec<ToolCall> {
    let mut tool_calls = tool_calls_from_stream_map(tool_call_map);
    let (_plain, inline) = hermes_core::separate_text_and_calls(buf);
    if tool_calls.is_empty() && !inline.is_empty() {
        info!(
            count = inline.len(),
            "parsed inline tool_calls from assistant content"
        );
        tool_calls.extend(inline.into_iter().map(core_tool_call_to_talk));
    }
    tool_calls.retain(|tc| !tc.function.name.trim().is_empty());
    tool_calls
}

/// Resume Rockchip ASR after wake (tts-stream `session.rs` retry loop).
pub async fn resume_asr_with_retry(asr: Arc<dyn AsrEngine>) -> bool {
    let mut retries = 3u32;
    loop {
        match asr.resume().await {
            Ok(()) => return true,
            Err(e) => {
                retries = retries.saturating_sub(1);
                if retries == 0 {
                    tracing::error!(error = %e, "asr resume failed, giving up");
                    return false;
                }
                tracing::warn!(error = %e, remaining = retries, "asr resume failed, retrying");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Wait for RK ASR FINISH after `finish_utterance` (SDK often >300ms).
pub const ROCKCHIP_ASR_FLUSH_WAIT_MS: u64 = 2500;

/// Short wait when assembled transcript already looks like a complete sentence.
pub const ROCKCHIP_ASR_FLUSH_WAIT_MS_COMPLETE: u64 = 600;

/// RK driver internal deadline while waiting for SDK FINISH after LAST packet.
pub const ROCKCHIP_FINISH_UTTERANCE_TIMEOUT_MS: u64 = 800;

/// Pick session-layer wait after flush based on transcript completeness.
pub fn rockchip_asr_flush_wait_ms(assembled: &str, min_final_chars: usize) -> u64 {
    let p = assembled.trim();
    if !p.is_empty()
        && p.chars().count() >= min_final_chars
        && !crate::orchestrator::utterance_likely_incomplete(p)
    {
        ROCKCHIP_ASR_FLUSH_WAIT_MS_COMPLETE
    } else {
        ROCKCHIP_ASR_FLUSH_WAIT_MS
    }
}

/// Ignore mic/ASR triggers briefly after TTS ends (speaker echo often contains 「现在」).
pub const ROCKCHIP_POST_TURN_ASR_COOLDOWN_MS: u64 = 1800;

/// Drop ASR while wake ack TTS is playing (mic picks up「哎，我在！」).
pub const ROCKCHIP_WAKE_ACK_ASR_COOLDOWN_MS: u64 = 2800;

/// TTS pump drains stale PCM for up to 200ms after `turn_epoch` bumps; wait slightly longer.
pub const TTS_PUMP_STALE_DRAIN_MS: u64 = 220;

/// Reopen mic path after assistant turn (tts-stream: `set_gate(true)` only).
pub async fn reopen_asr_after_turn(asr: Arc<dyn AsrEngine>, wake_enabled: bool) {
    if wake_enabled {
        let _ = asr.set_gate(true).await;
    }
}

/// Shutup tool: interrupt synthesis, clear playback, enter dormant — no spoken reply.
pub async fn complete_shutup_turn(
    tts: &Arc<dyn TtsEngine>,
    playback: &Arc<AudioPlayback>,
    done_tx: &mpsc::Sender<TurnDone>,
    epoch_at_start: u64,
) {
    info!("shutup: skipping assistant TTS");
    if let Err(e) = tts.interrupt_turn().await {
        warn!(error = %e, "tts interrupt on shutup failed");
    }
    playback.stop_clear();
    send_turn_done(done_tx, String::new(), epoch_at_start, true, false).await;
}

/// Spawn body for [`super::session::start_reply_turn`] — mirrors tts-stream `session.rs`.
#[allow(clippy::too_many_arguments)]
pub fn spawn_reply_turn(
    cancel: CancellationToken,
    trigger_at: Instant,
    speculative: bool,
    epoch_at_start: u64,
    msgs: Vec<ChatMessage>,
    llm: Arc<dyn LlmClient>,
    tts: Arc<dyn TtsEngine>,
    playback_wait: Arc<AudioPlayback>,
    done_tx: mpsc::Sender<TurnDone>,
    sentence_min: usize,
    tts_first_chunk: usize,
    tools_enabled: bool,
    llm_cfg: LlmConfig,
    hermes_sender: Option<HermesQueueSender>,
) {
    tokio::spawn(async move {
        let mut msgs_local = msgs;
        let mut assistant_buf = String::new();
        let mut should_go_dormant = false;
        let mut turn_tts_spoken = false;
        let max_rounds: u32 = if tools_enabled { 2 } else { 1 };

        for round in 0..max_rounds {
            let tools_ref = if tools_enabled && round == 0 {
                Some(tools::get_tool_definitions())
            } else {
                None
            };
            let tools = tools_ref.as_deref();

            if round == 0 && tools.is_some() {
                info!(
                    round,
                    tool_defs = tools.map(|t| t.len()).unwrap_or(0),
                    "llm stream with tools"
                );
            }

            let stream_started = Instant::now();
            let mut stream = match llm.stream_chat(&msgs_local, tools, cancel.clone()).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "llm failed");
                    send_turn_done(&done_tx, String::new(), epoch_at_start, false, false).await;
                    return;
                }
            };
            if round == 0 && !speculative {
                info!(
                    trigger_to_llm_stream_ms =
                        stream_started.duration_since(trigger_at).as_millis(),
                    "latency: trigger -> llm stream ready"
                );
            }

            let mut first_token = true;
            let mut sent_early = false;
            let mut tts_any = false;
            let mut buf = String::new();
            let mut reasoning_buf = String::new();
            let mut tts_buf = String::new();
            let mut tts_gate = StreamingThinkTtsGate::new(llm_cfg.thinking_enabled);
            let mut tool_call_map: HashMap<u32, AccumulatedToolCall> = HashMap::new();
            let mut tts_stats = TtsPipelineStats::default();
            let mut deferred_speakable = String::new();

            while let Some(item) = stream.next().await {
                if cancel.is_cancelled() {
                    break;
                }
                let Ok(stream_item) = item else { continue };
                if first_token {
                    if round == 0 && !speculative {
                        info!(
                            trigger_to_llm_first_token_ms = trigger_at.elapsed().as_millis(),
                            "latency: trigger -> llm first token"
                        );
                    }
                    first_token = false;
                }

                if let Some(ref reasoning) = stream_item.reasoning_content {
                    eprint!("{reasoning}");
                    reasoning_buf.push_str(reasoning);
                }

                for tc_delta in &stream_item.tool_calls {
                    let entry = tool_call_map.entry(tc_delta.index).or_insert_with(|| {
                        AccumulatedToolCall {
                            index: tc_delta.index,
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        }
                    });
                    if let Some(ref id) = tc_delta.id {
                        if !id.is_empty() {
                            entry.id = id.clone();
                        }
                    }
                    if let Some(ref name) = tc_delta.function_name {
                        entry.name.push_str(name);
                    }
                    if let Some(ref args) = tc_delta.function_arguments {
                        entry.arguments.push_str(args);
                    }
                }

                if let Some(ref token) = stream_item.content {
                    buf.push_str(token);
                    assistant_buf.push_str(token);

                    let actionable = has_actionable_tool_deltas(&tool_call_map);
                    pump_gate_delta(
                        &tts,
                        &mut tts_gate,
                        token,
                        &mut tts_buf,
                        &mut deferred_speakable,
                        sentence_min,
                        tts_first_chunk,
                        &mut sent_early,
                        &mut tts_any,
                        actionable,
                        if round == 0 && !speculative {
                            Some(trigger_at)
                        } else {
                            None
                        },
                        &mut tts_stats,
                    )
                    .await;
                }
            }

            let actionable = has_actionable_tool_deltas(&tool_call_map);

            log_llm_output(round, &buf);

            let tool_calls = resolve_tool_calls(&buf, &tool_call_map);

            if tool_calls.is_empty() {
                if !actionable {
                    let tail = tts_gate.flush();
                    tts_stats.flush_tail_chars = tail.chars().count();
                    tts_stats.gate_out_chars += tail.chars().count();
                    if append_speakable_stream_delta(&mut tts_buf, &tail, false) {
                        tts_stats.append_accepted_events += 1;
                        pump_streaming_tts(
                            &tts,
                            &mut tts_buf,
                            sentence_min,
                            tts_first_chunk,
                            &mut sent_early,
                            &mut tts_any,
                            None,
                        )
                        .await;
                    }
                } else {
                    let tail = tts_gate.flush();
                    if !tail.is_empty() {
                        deferred_speakable.push_str(&tail);
                        tts_stats.deferred_chars += tail.chars().count();
                        tts_stats.flush_tail_chars = tail.chars().count();
                    }
                    drain_deferred_speakable(
                        &tts,
                        &mut deferred_speakable,
                        &mut tts_buf,
                        sentence_min,
                        tts_first_chunk,
                        &mut sent_early,
                        &mut tts_any,
                        &mut tts_stats,
                    )
                    .await;
                }
                if let Some(rest) = flush_remainder(&mut tts_buf) {
                    if tts.append_text(&normalize_tts_text(&rest)).await.is_ok() {
                        tts_any = true;
                    }
                }
                pump_streaming_tts_tail(&tts, &mut tts_buf, &mut tts_any).await;
                speak_fallback_stripped(&tts, &buf, &mut tts_any, &mut tts_stats).await;
                if tools.is_some() && !tts_any {
                    warn!(
                        round,
                        assistant_chars = buf.chars().count(),
                        "llm stream ended with tools attached but no tool_calls received"
                    );
                }
                if !tts_any {
                    log_tts_skip_diagnosis(
                        round,
                        llm_cfg.thinking_enabled,
                        &tts_gate.diagnostics(),
                        &reasoning_buf,
                        &buf,
                        &tts_buf,
                        &tts_stats,
                        &tool_call_map,
                        actionable,
                        tools.is_some(),
                    );
                }
                if let Err(e) = tts.finish_turn().await {
                    warn!(error = %e, "tts finish");
                }
                turn_tts_spoken |= tts_any;
                playback_wait.wait_drain(Duration::from_secs(30)).await;
                send_turn_done(
                    &done_tx,
                    assistant_buf,
                    epoch_at_start,
                    should_go_dormant,
                    turn_tts_spoken,
                )
                .await;
                return;
            }

            let shutup_turn = tools::tool_calls_include_shutup(
                tool_calls.iter().map(|tc| tc.function.name.as_str()),
            );

            // Discard streaming buffer — tool turn speaks via `spoken` only.
            buf.clear();

            let mut spoken_list: Vec<String> = Vec::new();
            for tc in &tool_calls {
                if let Some(spoken) =
                    tools::extract_tool_spoken(&tc.function.name, &tc.function.arguments)
                {
                    spoken_list.push(spoken);
                }
            }

            if !shutup_turn && !spoken_list.is_empty() {
                for spoken in &spoken_list {
                    info!(%spoken, "tool: spoken notification");
                    if let Err(e) = tts.append_text(&normalize_tts_text(spoken)).await {
                        warn!(error = %e, "tts spoken append");
                    } else {
                        turn_tts_spoken = true;
                    }
                }
                if let Err(e) = tts.finish_turn().await {
                    warn!(error = %e, "tts finish after spoken");
                }
            }

            msgs_local.push(ChatMessage {
                role: "assistant".to_string(),
                content: String::new(),
                tool_calls: Some(tool_calls.clone()),
                tool_call_id: None,
            });

            info!(count = tool_calls.len(), "llm returned tool_calls");
            let mut tool_results: Vec<String> = Vec::with_capacity(tool_calls.len());
            for tc in &tool_calls {
                info!(tool = %tc.function.name, args = %tc.function.arguments, "tool: calling");
                eprintln!(
                    "\n═══ LLM tool: {} ═══\n{}",
                    tc.function.name, tc.function.arguments
                );
                let result = match tools::execute_tool(
                    &tc.function.name,
                    &tc.function.arguments,
                    &llm_cfg,
                    hermes_sender.as_ref(),
                )
                .await
                {
                    Ok(r) => r,
                    Err(e) => format!("error: {e}"),
                };
                info!(tool = %tc.function.name, result_len = result.len(), "tool: result");
                eprintln!("═══ tool result: {} ═══\n{}", tc.function.name, result);
                if tools::is_shutup_tool(&tc.function.name) {
                    should_go_dormant = true;
                }
                tool_results.push(result.clone());
                msgs_local.push(ChatMessage {
                    role: "tool".to_string(),
                    content: result,
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
            }
            if should_go_dormant {
                complete_shutup_turn(&tts, &playback_wait, &done_tx, epoch_at_start).await;
                return;
            }
            turn_tts_spoken |= tts_any;
            if tools::should_skip_call_hermes_confirmation(
                tool_calls.iter().map(|tc| tc.function.name.as_str()),
                &tool_results,
            ) {
                info!("call_hermes enqueued: skipping follow-up LLM round");
                playback_wait.wait_drain(Duration::from_secs(30)).await;
                send_turn_done(
                    &done_tx,
                    assistant_buf,
                    epoch_at_start,
                    should_go_dormant,
                    turn_tts_spoken,
                )
                .await;
                return;
            }
        }

        if let Err(e) = tts.finish_turn().await {
            warn!(error = %e, "tts finish");
        }
        playback_wait.wait_drain(Duration::from_secs(30)).await;
        send_turn_done(
            &done_tx,
            assistant_buf,
            epoch_at_start,
            should_go_dormant,
            turn_tts_spoken,
        )
        .await;
    });
}

/// Hermes push replay — mirrors tts-stream `handle_hermes_result` spawn body.
#[allow(clippy::too_many_arguments)]
pub fn spawn_hermes_replay(
    cancel: CancellationToken,
    epoch_at_start: u64,
    go_dormant: bool,
    msgs: Vec<ChatMessage>,
    llm: Arc<dyn LlmClient>,
    tts: Arc<dyn TtsEngine>,
    playback: Arc<AudioPlayback>,
    done_tx: mpsc::Sender<TurnDone>,
    sentence_min: usize,
    tts_first_chunk: usize,
    llm_cfg: LlmConfig,
) {
    tokio::spawn(async move {
        let mut assistant_buf = String::new();
        let mut tts_spoken = false;

        let mut stream = match llm.stream_chat(&msgs, None, cancel.clone()).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "hermes replay llm failed");
                send_turn_done(&done_tx, String::new(), epoch_at_start, go_dormant, false).await;
                return;
            }
        };

        let mut buf = String::new();
        let mut reasoning_buf = String::new();
        let mut tts_buf = String::new();
        let mut tts_gate = StreamingThinkTtsGate::new(llm_cfg.thinking_enabled);
        let mut sent_early = false;
        let mut tts_stats = TtsPipelineStats::default();
        let mut deferred_speakable = String::new();
        while let Some(item) = stream.next().await {
            if cancel.is_cancelled() {
                break;
            }
            let Ok(stream_item) = item else { continue };
            if let Some(ref reasoning) = stream_item.reasoning_content {
                eprint!("{reasoning}");
                reasoning_buf.push_str(reasoning);
            }
            if let Some(ref token) = stream_item.content {
                buf.push_str(token);
                assistant_buf.push_str(token);
                pump_gate_delta(
                    &tts,
                    &mut tts_gate,
                    token,
                    &mut tts_buf,
                    &mut deferred_speakable,
                    sentence_min,
                    tts_first_chunk,
                    &mut sent_early,
                    &mut tts_spoken,
                    false,
                    None,
                    &mut tts_stats,
                )
                .await;
            }
        }

        let tail = tts_gate.flush();
        tts_stats.flush_tail_chars = tail.chars().count();
        tts_stats.gate_out_chars += tail.chars().count();
        if append_speakable_stream_delta(&mut tts_buf, &tail, false) {
            tts_stats.append_accepted_events += 1;
            pump_streaming_tts(
                &tts,
                &mut tts_buf,
                sentence_min,
                tts_first_chunk,
                &mut sent_early,
                &mut tts_spoken,
                None,
            )
            .await;
        }

        log_llm_output(0, &buf);

        if let Some(rest) = flush_remainder(&mut tts_buf) {
            if tts.append_text(&normalize_tts_text(&rest)).await.is_ok() {
                tts_spoken = true;
            }
        }
        speak_fallback_stripped(&tts, &buf, &mut tts_spoken, &mut tts_stats).await;
        if !tts_spoken {
            log_tts_skip_diagnosis(
                0,
                llm_cfg.thinking_enabled,
                &tts_gate.diagnostics(),
                &reasoning_buf,
                &buf,
                &tts_buf,
                &tts_stats,
                &HashMap::new(),
                false,
                false,
            );
        }
        if let Err(e) = tts.finish_turn().await {
            warn!(error = %e, "tts finish");
        }
        playback.wait_drain(Duration::from_secs(30)).await;
        send_turn_done(
            &done_tx,
            assistant_buf,
            epoch_at_start,
            go_dormant,
            tts_spoken,
        )
        .await;
    });
}

/// Hermes result gate (tts-stream: final | error only).
pub fn hermes_status_accepted(status: &str) -> bool {
    status == "final" || status == "error"
}

/// VAD end-of-speech flush gate using streaming partial (utterance still open).
pub fn should_flush_asr_partial(
    input_gated: bool,
    trailing_silence_ms: u32,
    endpoint_silence_ms: u32,
    partial: &str,
    min_final_chars: usize,
    utterance_open: bool,
) -> bool {
    !input_gated
        && utterance_open
        && trailing_silence_ms >= endpoint_silence_ms
        && partial.trim().chars().count() >= min_final_chars
}

/// Early flush when partial is a stable complete sentence (shorter trailing silence).
pub fn should_flush_asr_partial_complete(
    input_gated: bool,
    trailing_silence_ms: u32,
    early_endpoint_silence_ms: u32,
    partial: &str,
    min_final_chars: usize,
    utterance_open: bool,
    partial_stable_since: Option<Instant>,
    speculative_stable_ms: u32,
) -> bool {
    if early_endpoint_silence_ms == 0 || input_gated || !utterance_open {
        return false;
    }
    let p = partial.trim();
    if p.chars().count() < min_final_chars {
        return false;
    }
    if crate::orchestrator::utterance_likely_incomplete(p) {
        return false;
    }
    let Some(since) = partial_stable_since else {
        return false;
    };
    since.elapsed() >= Duration::from_millis(speculative_stable_ms as u64)
        && trailing_silence_ms >= early_endpoint_silence_ms
}

/// VAD end-of-speech flush gate (tts-stream parity, streaming ASR).
pub fn should_flush_asr(
    input_gated: bool,
    trailing_silence_ms: u32,
    endpoint_silence_ms: u32,
    last_final: &Option<String>,
    min_final_chars: usize,
) -> bool {
    !input_gated
        && trailing_silence_ms >= endpoint_silence_ms
        && last_final
            .as_ref()
            .is_some_and(|t| t.trim().chars().count() >= min_final_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_when_silent_with_final() {
        assert!(should_flush_asr(false, 150, 150, &Some("天气".into()), 2));
        assert!(!should_flush_asr(false, 100, 150, &Some("天气".into()), 2));
        assert!(!should_flush_asr(true, 150, 150, &Some("天气".into()), 2));
    }

    #[test]
    fn hermes_final_only() {
        assert!(hermes_status_accepted("final"));
        assert!(hermes_status_accepted("error"));
        assert!(!hermes_status_accepted("ok"));
        assert!(!hermes_status_accepted("pending"));
    }

    #[test]
    fn rockchip_flush_wait_shorter_for_complete_sentence() {
        assert_eq!(
            rockchip_asr_flush_wait_ms("帮我查一下明天的天气。", 2),
            ROCKCHIP_ASR_FLUSH_WAIT_MS_COMPLETE
        );
        assert_eq!(
            rockchip_asr_flush_wait_ms("帮我查一下明天的", 2),
            ROCKCHIP_ASR_FLUSH_WAIT_MS
        );
    }

    #[test]
    fn early_flush_when_complete_and_stable() {
        let since = Instant::now() - Duration::from_millis(500);
        assert!(should_flush_asr_partial_complete(
            false,
            400,
            400,
            "帮我查一下明天的天气。",
            2,
            true,
            Some(since),
            300,
        ));
        assert!(!should_flush_asr_partial_complete(
            false,
            200,
            400,
            "帮我查一下明天的天气。",
            2,
            true,
            Some(since),
            300,
        ));
        assert!(!should_flush_asr_partial_complete(
            false,
            400,
            0,
            "帮我查一下明天的天气。",
            2,
            true,
            Some(since),
            300,
        ));
    }
}
