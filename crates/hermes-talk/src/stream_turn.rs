//! LLM → TTS turn handling ported from [tts-stream](https://github.com/...) `session.rs`.
//!
//! Rockchip bundles (`feature = "rockchip"`, no `sherpa-asr-tts`) use this module so dialog
//! behavior matches tts-stream for tool routing, while still stripping reasoning / think blocks
//! before TTS (reasoning_content is log-only).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::asr::AsrEngine;
use crate::orchestrator::{
    IncrementalThinkStripper, assistant_content_tts_allowed, extract_inline_thinking,
    flush_remainder, normalize_tts_text, strip_think_blocks, take_early_chunk, take_sentence,
};
use crate::llm::{AccumulatedToolCall, ChatMessage, LlmClient, ToolCall};
use crate::tools;
use crate::tools::hermes_queue::HermesQueueSender;
use crate::tts::TtsEngine;
use crate::{audio::AudioPlayback, config::LlmConfig};

fn flush_llm_reasoning_log(round: u32, reasoning_buf: &mut String, emitted: &mut bool) {
    if *emitted || reasoning_buf.trim().is_empty() {
        reasoning_buf.clear();
        return;
    }
    info!(
        round,
        chars = reasoning_buf.chars().count(),
        reasoning = %reasoning_buf.trim(),
        "llm reasoning"
    );
    *emitted = true;
    reasoning_buf.clear();
}

fn flush_llm_content_log(round: u32, content: &str, emitted: &mut bool) {
    if *emitted || content.trim().is_empty() {
        return;
    }
    info!(
        round,
        chars = content.chars().count(),
        content = %content.trim(),
        "llm assistant content"
    );
    *emitted = true;
}

fn prepare_llm_speakable_text(
    raw: &str,
    reasoning_buf: &mut String,
    reasoning_log_emitted: &mut bool,
    round: u32,
) -> String {
    let inline = extract_inline_thinking(raw);
    if !inline.trim().is_empty() {
        if !reasoning_buf.trim().is_empty() {
            reasoning_buf.push('\n');
        }
        reasoning_buf.push_str(inline.trim());
        flush_llm_reasoning_log(round, reasoning_buf, reasoning_log_emitted);
    }
    strip_think_blocks(raw)
}

async fn append_tts_text(tts: &Arc<dyn TtsEngine>, text: &str, tts_sent: &mut bool) {
    let normalized = normalize_tts_text(text);
    if normalized.trim().is_empty() {
        return;
    }
    match tts.append_text(&normalized).await {
        Ok(()) => *tts_sent = true,
        Err(e) => warn!(error = %e, %normalized, "tts append failed"),
    }
}

async fn drain_tts_buf(
    tts: &Arc<dyn TtsEngine>,
    tts_buf: &mut String,
    tts_first_chunk: usize,
    sentence_min: usize,
    sent_early: &mut bool,
    tts_sent: &mut bool,
) {
    if !*sent_early {
        if let Some(chunk) = take_early_chunk(tts_buf, tts_first_chunk) {
            info!(%chunk, "tts early chunk");
            append_tts_text(tts, &chunk, tts_sent).await;
            *sent_early = true;
        }
    }
    while let Some(sentence) = take_sentence(tts_buf, sentence_min) {
        info!(%sentence, "tts sentence");
        append_tts_text(tts, &sentence, tts_sent).await;
    }
    if let Some(rest) = flush_remainder(tts_buf) {
        append_tts_text(tts, &rest, tts_sent).await;
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

fn has_actionable_tool_deltas(map: &HashMap<u32, AccumulatedToolCall>) -> bool {
    map.values().any(|acc| !acc.name.trim().is_empty())
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

async fn speak_plain_assistant_reply(
    tts: &Arc<dyn TtsEngine>,
    plain: &str,
    tts_first_chunk: usize,
    sentence_min: usize,
    tts_sent: &mut bool,
) {
    let plain = strip_think_blocks(plain);
    if plain.trim().is_empty() {
        return;
    }
    let mut stripper = IncrementalThinkStripper::new();
    let cleaned = stripper.push(&plain);
    let tail = stripper.flush();
    let mut buf = format!("{cleaned}{tail}");
    if buf.trim().is_empty() {
        return;
    }
    info!(
        chars = buf.chars().count(),
        "tts speaking plain assistant reply"
    );
    let mut sent_early = false;
    drain_tts_buf(
        tts,
        &mut buf,
        tts_first_chunk,
        sentence_min,
        &mut sent_early,
        tts_sent,
    )
    .await;
}

fn log_llm_tool_calls(round: u32, tool_calls: &[ToolCall]) {
    for tc in tool_calls {
        info!(
            round,
            tool = %tc.function.name,
            args = %tc.function.arguments,
            "llm tool_call"
        );
    }
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

/// Reopen mic path after assistant turn (tts-stream: `set_gate(true)` only).
pub async fn reopen_asr_after_turn(asr: Arc<dyn AsrEngine>, wake_enabled: bool) {
    if wake_enabled {
        let _ = asr.set_gate(true).await;
    }
}

/// Spawn body for [`super::session::start_reply_turn`] (tts-stream parity).
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
    done_tx: mpsc::Sender<(String, u64, bool)>,
    sentence_min: usize,
    tts_first_chunk: usize,
    tools_enabled: bool,
    llm_cfg: LlmConfig,
    hermes_sender: Option<HermesQueueSender>,
) {
    tokio::spawn(async move {
        let mut msgs_local = msgs;
        let mut assistant_buf = String::new();
        let mut with_tools = tools_enabled;
        let mut should_go_dormant = false;
        let max_rounds: u32 = if tools_enabled { 2 } else { 1 };

        let tool_defs = if tools_enabled {
            Some(tools::get_tool_definitions())
        } else {
            None
        };

        for round in 0..max_rounds {
            let tools = if with_tools && round == 0 {
                tool_defs.as_deref()
            } else {
                None
            };
            with_tools = false;

            let stream_started = Instant::now();
            let mut stream = match llm.stream_chat(&msgs_local, tools, cancel.clone()).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "llm failed");
                    let _ = done_tx.send((String::new(), epoch_at_start, false)).await;
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
            let mut tts_sent = false;
            let mut buf = String::new();
            let mut tts_buf = String::new();
            let mut reasoning_buf = String::new();
            let mut reasoning_log_emitted = false;
            let mut content_log_emitted = false;
            let mut had_reasoning = false;
            let mut think_strip = IncrementalThinkStripper::new();
            let mut tool_call_map: HashMap<u32, AccumulatedToolCall> = HashMap::new();

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
                    reasoning_buf.push_str(reasoning);
                    had_reasoning = true;
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
                if has_actionable_tool_deltas(&tool_call_map) {
                    flush_llm_reasoning_log(round, &mut reasoning_buf, &mut reasoning_log_emitted);
                }

                if let Some(ref token) = stream_item.content {
                    flush_llm_reasoning_log(round, &mut reasoning_buf, &mut reasoning_log_emitted);
                    buf.push_str(token);
                    assistant_buf.push_str(token);

                    if assistant_content_tts_allowed(
                        &buf,
                        has_actionable_tool_deltas(&tool_call_map),
                    ) {
                        let speakable = think_strip.push(token);
                        if !speakable.is_empty() {
                            tts_buf.push_str(&speakable);
                        }
                        drain_tts_buf(
                            &tts,
                            &mut tts_buf,
                            tts_first_chunk,
                            sentence_min,
                            &mut sent_early,
                            &mut tts_sent,
                        )
                        .await;
                    }
                }
            }

            flush_llm_reasoning_log(round, &mut reasoning_buf, &mut reasoning_log_emitted);

            let mut tool_calls = tool_calls_from_stream_map(&tool_call_map);
            let (plain, inline) = hermes_core::separate_text_and_calls(&buf);
            let speakable_buf = prepare_llm_speakable_text(
                &plain,
                &mut reasoning_buf,
                &mut reasoning_log_emitted,
                round,
            );
            if tool_calls.is_empty() && !inline.is_empty() {
                info!(
                    count = inline.len(),
                    "parsed inline tool_calls from assistant content"
                );
                tool_calls.extend(inline.into_iter().map(core_tool_call_to_talk));
            }
            tool_calls.retain(|tc| !tc.function.name.trim().is_empty());
            flush_llm_content_log(round, &speakable_buf, &mut content_log_emitted);
            log_llm_tool_calls(round, &tool_calls);

            if tool_calls.is_empty() {
                if assistant_content_tts_allowed(&buf, false) {
                    let tail = think_strip.flush();
                    if !tail.is_empty() {
                        tts_buf.push_str(&tail);
                    }
                    drain_tts_buf(
                        &tts,
                        &mut tts_buf,
                        tts_first_chunk,
                        sentence_min,
                        &mut sent_early,
                        &mut tts_sent,
                    )
                    .await;
                }
                if !tts_sent {
                    speak_plain_assistant_reply(
                        &tts,
                        &speakable_buf,
                        tts_first_chunk,
                        sentence_min,
                        &mut tts_sent,
                    )
                    .await;
                }
                if !tts_sent {
                    if had_reasoning && speakable_buf.trim().is_empty() {
                        warn!(
                            round,
                            "llm turn ended with reasoning only; no assistant content for TTS"
                        );
                    } else if !speakable_buf.trim().is_empty() {
                        warn!(
                            chars = speakable_buf.chars().count(),
                            "assistant reply had text but nothing was sent to TTS"
                        );
                    }
                }
                if let Err(e) = tts.finish_turn().await {
                    warn!(error = %e, "tts finish");
                }
                playback_wait.wait_drain(Duration::from_secs(30)).await;
                let _ = done_tx
                    .send((assistant_buf, epoch_at_start, should_go_dormant))
                    .await;
                return;
            }

            buf.clear();

            let mut spoken_list: Vec<String> = Vec::new();

            for tc in &tool_calls {
                let mut has_spoken = false;
                if let Some(spoken) = tools::extract_spoken(&tc.function.arguments) {
                    spoken_list.push(spoken);
                    has_spoken = true;
                }
                if !has_spoken && tc.function.name == "call_hermes" {
                    if let Some(spoken) = tools::generate_hermes_spoken(&tc.function.arguments) {
                        spoken_list.push(spoken);
                    }
                }
            }

            if !spoken_list.is_empty() {
                for spoken in &spoken_list {
                    info!(%spoken, "tool: spoken notification");
                    if let Err(e) = tts.append_text(&normalize_tts_text(spoken)).await {
                        warn!(error = %e, "tts spoken append");
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

            info!(
                count = tool_calls.len(),
                suppressed_chars = buf.len(),
                "llm returned tool_calls"
            );
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
                if tc.function.name == "shutup" {
                    should_go_dormant = true;
                }
                msgs_local.push(ChatMessage {
                    role: "tool".to_string(),
                    content: result,
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
            }
            if should_go_dormant {
                break;
            }
        }

        if let Err(e) = tts.finish_turn().await {
            warn!(error = %e, "tts finish");
        }
        playback_wait.wait_drain(Duration::from_secs(30)).await;
        let _ = done_tx
            .send((assistant_buf, epoch_at_start, should_go_dormant))
            .await;
    });
}

/// Hermes push replay (tts-stream parity).
#[allow(clippy::too_many_arguments)]
pub fn spawn_hermes_replay(
    cancel: CancellationToken,
    epoch_at_start: u64,
    go_dormant: bool,
    msgs: Vec<ChatMessage>,
    llm: Arc<dyn LlmClient>,
    tts: Arc<dyn TtsEngine>,
    playback: Arc<AudioPlayback>,
    done_tx: mpsc::Sender<(String, u64, bool)>,
    sentence_min: usize,
    tts_first_chunk: usize,
) {
    tokio::spawn(async move {
        let mut assistant_buf = String::new();

        let mut stream = match llm.stream_chat(&msgs, None, cancel.clone()).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "hermes replay llm failed");
                let _ = done_tx
                    .send((String::new(), epoch_at_start, go_dormant))
                    .await;
                return;
            }
        };

        let mut buf = String::new();
        let mut tts_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut reasoning_log_emitted = false;
        let mut content_log_emitted = false;
        let mut think_strip = IncrementalThinkStripper::new();
        let mut sent_early = false;
        let mut tts_sent = false;
        while let Some(item) = stream.next().await {
            if cancel.is_cancelled() {
                break;
            }
            let Ok(stream_item) = item else { continue };
            if let Some(ref reasoning) = stream_item.reasoning_content {
                reasoning_buf.push_str(reasoning);
            }
            if let Some(ref token) = stream_item.content {
                flush_llm_reasoning_log(0, &mut reasoning_buf, &mut reasoning_log_emitted);
                buf.push_str(token);
                assistant_buf.push_str(token);
                if assistant_content_tts_allowed(&buf, false) {
                    let speakable = think_strip.push(token);
                    if !speakable.is_empty() {
                        tts_buf.push_str(&speakable);
                    }
                    drain_tts_buf(
                        &tts,
                        &mut tts_buf,
                        tts_first_chunk,
                        sentence_min,
                        &mut sent_early,
                        &mut tts_sent,
                    )
                    .await;
                }
            }
        }

        flush_llm_reasoning_log(0, &mut reasoning_buf, &mut reasoning_log_emitted);
        let speakable_buf =
            prepare_llm_speakable_text(&buf, &mut reasoning_buf, &mut reasoning_log_emitted, 0);
        flush_llm_content_log(0, &speakable_buf, &mut content_log_emitted);
        if assistant_content_tts_allowed(&buf, false) {
            let tail = think_strip.flush();
            if !tail.is_empty() {
                tts_buf.push_str(&tail);
            }
            drain_tts_buf(
                &tts,
                &mut tts_buf,
                tts_first_chunk,
                sentence_min,
                &mut sent_early,
                &mut tts_sent,
            )
            .await;
        }
        if !tts_sent {
            speak_plain_assistant_reply(
                &tts,
                &speakable_buf,
                tts_first_chunk,
                sentence_min,
                &mut tts_sent,
            )
            .await;
        }
        if let Err(e) = tts.finish_turn().await {
            warn!(error = %e, "tts finish");
        }
        playback.wait_drain(Duration::from_secs(30)).await;
        let _ = done_tx
            .send((assistant_buf, epoch_at_start, go_dormant))
            .await;
    });
}

/// Hermes result gate (tts-stream: final | error only).
pub fn hermes_status_accepted(status: &str) -> bool {
    status == "final" || status == "error"
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
        assert!(should_flush_asr(false, 800, 800, &Some("天气".into()), 2,));
        assert!(!should_flush_asr(false, 400, 800, &Some("天气".into()), 2));
        assert!(!should_flush_asr(true, 800, 800, &Some("天气".into()), 2));
    }

    #[test]
    fn hermes_final_only() {
        assert!(hermes_status_accepted("final"));
        assert!(hermes_status_accepted("error"));
        assert!(!hermes_status_accepted("ok"));
        assert!(!hermes_status_accepted("pending"));
    }
}
