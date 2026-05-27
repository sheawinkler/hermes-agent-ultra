//! `meeting_notes` tool — offline audio file → structured Chinese meeting notes.
//!
//! # Pipeline
//!
//! ```text
//! audio_path (WAV/MP3/…)
//!   → SttEngine::transcribe_file          (existing voice_providers)
//!   → optional pyannote diarization       (HTTP sidecar)
//!   → chunked LLM summary (10-min slices)
//!   → merge chunk summaries → MeetingNotes JSON
//!   → MeetingMemorySink::write            (holographic facts + MEMORY.md stub)
//!   → transcript file saved to $HERMES_HOME/meetings/
//! ```
//!
//! # Memory strategy
//!
//! - Each `action_item`, `key_decision`, and `risk` becomes **one row** in the
//!   `holographic` `facts` table (≤400 chars, `category="meeting"`).
//! - A single overview entry is appended to `MEMORY.md` (≤200 chars) so the
//!   agent can sense the meeting's existence at conversation start.
//! - Raw transcript is written to `$HERMES_HOME/meetings/<date>-<slug>.txt`.
//!   It is **not** stored in any DB to avoid schema pollution.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use hermes_config::voice::MeetingTranscriptionMode;
use hermes_config::{DiarizationProvider, MeetingConfig, SttConfig};
use hermes_core::{tool_schema, ToolError, ToolHandler, ToolSchema};
use hermes_core::JsonSchema;
use indexmap::IndexMap;
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::voice_providers::SttEngine;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A single turn in the diarized transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptTurn {
    /// "Speaker A" (mic) or "Speaker B" (loopback) or "SPEAKER_XX" from pyannote.
    pub speaker: String,
    /// Start time in seconds (0.0 when timestamps unavailable).
    pub start_s: f32,
    pub text: String,
}

/// Structured meeting notes produced by the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingNotes {
    pub title: String,
    pub date: String,
    /// Overall meeting summary (≤400 chars per chunk, merged).
    pub summary: String,
    pub key_decisions: Vec<String>,
    pub action_items: Vec<String>,
    pub risks: Vec<String>,
    pub follow_ups: Vec<String>,
    /// Full raw transcript (may be empty for very short meetings).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub transcript: Vec<TranscriptTurn>,
    /// Path to the saved transcript file (set after save).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_file: Option<String>,
}

impl MeetingNotes {
    fn memory_entry(&self) -> String {
        let items = self.action_items.len();
        let decisions = self.key_decisions.len();
        let summary_short: String = self.summary.chars().take(120).collect();
        let entry = format!(
            "{} {}（行动项{}条，决策{}条）：{}",
            self.date, self.title, items, decisions, summary_short
        );
        // hard cap at 200 chars for MEMORY.md entry
        entry.chars().take(200).collect()
    }
}

// ---------------------------------------------------------------------------
// LLM chunk summary
// ---------------------------------------------------------------------------

/// Prompt template for a single transcript chunk (10-min slice).
fn chunk_summary_prompt(chunk_text: &str) -> String {
    format!(
        r#"你是一名专业会议纪要助手。请对以下会议片段进行结构化分析。

**要求**：
- 仅返回合法 JSON，不要有任何 markdown 代码块或额外文字
- 所有字段均用中文填写
- summary 限 200 字以内
- 每条 action_item / key_decision / risk / follow_up 限 100 字以内

**JSON 格式**：
{{
  "summary": "...",
  "key_decisions": ["..."],
  "action_items": ["..."],
  "risks": ["..."],
  "follow_ups": ["..."]
}}

**会议片段**：
{chunk_text}
"#
    )
}

/// Prompt for merging multiple chunk summaries into a final summary.
fn merge_summary_prompt(chunk_summaries: &[Value]) -> String {
    let chunks_json = serde_json::to_string_pretty(chunk_summaries).unwrap_or_default();
    format!(
        r#"你是一名专业会议纪要助手。以下是同一次会议多个片段的结构化摘要，请将它们合并为一份完整纪要。

**要求**：
- 仅返回合法 JSON，不要有任何 markdown 代码块或额外文字
- 所有字段均用中文
- 去除重复项，保留最重要的条目
- summary 限 400 字以内
- action_items / key_decisions / risks / follow_ups 每条限 100 字以内

**JSON 格式**：
{{
  "summary": "...",
  "key_decisions": ["..."],
  "action_items": ["..."],
  "risks": ["..."],
  "follow_ups": ["..."]
}}

**各片段摘要**：
{chunks_json}
"#
    )
}

/// Call an OpenAI-compatible chat endpoint to summarize a transcript chunk.
async fn llm_summarize_chunk(
    client: &Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
) -> Result<Value, ToolError> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "temperature": 0.2,
        "max_tokens": 900,
        "messages": [
            {"role": "system", "content": "你是一名精确、简洁的会议纪要助手。"},
            {"role": "user", "content": prompt}
        ]
    });

    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("LLM request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed(format!(
            "LLM returned {status}: {text}"
        )));
    }

    let json: Value = resp
        .json()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("LLM JSON parse: {e}")))?;

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    // Strip markdown code fences if the model added them
    let cleaned = content
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string();

    serde_json::from_str::<Value>(&cleaned).map_err(|e| {
        ToolError::ExecutionFailed(format!("LLM returned invalid JSON: {e}\nContent: {cleaned}"))
    })
}

// ---------------------------------------------------------------------------
// Diarization (pyannote HTTP sidecar or no-op)
// ---------------------------------------------------------------------------

/// Add speaker labels to a flat transcript text.
///
/// When `provider` is `None` (dual-track not available), all text is assigned
/// to "Speaker A". The caller is expected to have already split the text by
/// channel before calling this function; this is the fallback for single-file
/// input.
fn label_transcript_plain(text: &str) -> Vec<TranscriptTurn> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| TranscriptTurn {
            speaker: "Speaker A".into(),
            start_s: 0.0,
            text: line.trim().to_string(),
        })
        .collect()
}

/// Call pyannote HTTP sidecar and merge result with transcript text.
///
/// The sidecar accepts `multipart/form-data` with `audio` field and returns
/// RTTM-formatted text.  We parse that and do a best-effort alignment with the
/// STT output (time-aligned alignment is approximate for batch STT).
async fn diarize_with_pyannote(
    client: &Client,
    endpoint: &str,
    audio_path: &str,
    transcript_text: &str,
) -> Result<Vec<TranscriptTurn>, ToolError> {
    let bytes = tokio::fs::read(audio_path)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read audio for diarization: {e}")))?;

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
    let form = reqwest::multipart::Form::new().part("audio", part);

    let url = format!("{}/diarize", endpoint.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Pyannote request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        warn!("Pyannote returned {status}: {text} — falling back to single-speaker");
        return Ok(label_transcript_plain(transcript_text));
    }

    let rttm = resp
        .text()
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    // Parse RTTM: "SPEAKER file 1 start dur <NA> <NA> SPEAKER_XX <NA> <NA>"
    let mut segments: Vec<(f32, f32, String)> = Vec::new();
    for line in rttm.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 9 && parts[0] == "SPEAKER" {
            let start: f32 = parts[3].parse().unwrap_or(0.0);
            let dur: f32 = parts[4].parse().unwrap_or(0.0);
            let speaker = parts[7].to_string();
            segments.push((start, start + dur, speaker));
        }
    }

    // Distribute transcript lines across segments (simple round-robin approximation)
    let lines: Vec<&str> = transcript_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();

    if segments.is_empty() {
        return Ok(label_transcript_plain(transcript_text));
    }

    let turns: Vec<TranscriptTurn> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let seg = &segments[i % segments.len()];
            TranscriptTurn {
                speaker: seg.2.clone(),
                start_s: seg.0,
                text: line.trim().to_string(),
            }
        })
        .collect();

    Ok(turns)
}

// ---------------------------------------------------------------------------
// Memory sink
// ---------------------------------------------------------------------------

/// Writes meeting notes into the holographic `memory_store.db` and appends a
/// stub to `MEMORY.md`.
struct MeetingMemorySink {
    db_path: PathBuf,
    memory_md_path: PathBuf,
}

impl MeetingMemorySink {
    fn new(hermes_home: &Path) -> Self {
        Self {
            db_path: hermes_home.join("memory_store.db"),
            memory_md_path: hermes_home.join("memories").join("MEMORY.md"),
        }
    }

    fn write(&self, notes: &MeetingNotes) -> Result<(), String> {
        self.write_facts(notes)?;
        self.append_memory_md(notes)?;
        Ok(())
    }

    fn write_facts(&self, notes: &MeetingNotes) -> Result<(), String> {
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;

        // Ensure the facts table exists (mirrors holographic.rs schema).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS facts (
                fact_id         INTEGER PRIMARY KEY AUTOINCREMENT,
                content         TEXT NOT NULL UNIQUE,
                category        TEXT DEFAULT 'general',
                tags            TEXT DEFAULT '',
                trust_score     REAL DEFAULT 0.5,
                retrieval_count INTEGER DEFAULT 0,
                helpful_count   INTEGER DEFAULT 0,
                created_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .map_err(|e| e.to_string())?;

        let tags = format!(
            "{},meeting,{}",
            notes.date,
            slugify(&notes.title)
        );

        let insert_fact = |content: &str, prefix: &str| {
            let full = format!("[{}] {}: {}", notes.date, prefix, content);
            let truncated: String = full.chars().take(400).collect();
            conn.execute(
                "INSERT OR IGNORE INTO facts (content, category, tags, trust_score) VALUES (?1, 'meeting', ?2, 0.7)",
                params![truncated, tags],
            )
            .ok();
        };

        // summary as one fact
        if !notes.summary.is_empty() {
            insert_fact(&notes.summary, "摘要");
        }
        for item in &notes.key_decisions {
            insert_fact(item, "决策");
        }
        for item in &notes.action_items {
            insert_fact(item, "行动项");
        }
        for item in &notes.risks {
            insert_fact(item, "风险");
        }
        for item in &notes.follow_ups {
            insert_fact(item, "跟进");
        }

        info!("MeetingMemorySink: wrote facts to {:?}", self.db_path);
        Ok(())
    }

    fn append_memory_md(&self, notes: &MeetingNotes) -> Result<(), String> {
        // Ensure parent directory exists.
        if let Some(parent) = self.memory_md_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let entry = notes.memory_entry();
        let separator = "\n§\n";
        let append_text = format!("{separator}{entry}");

        // Check if file exists; create if not.
        if !self.memory_md_path.exists() {
            std::fs::write(&self.memory_md_path, entry.as_bytes())
                .map_err(|e| e.to_string())?;
        } else {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.memory_md_path)
                .map_err(|e| e.to_string())?;
            f.write_all(append_text.as_bytes())
                .map_err(|e| e.to_string())?;
        }

        debug!("MeetingMemorySink: appended to {:?}", self.memory_md_path);
        Ok(())
    }
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .to_lowercase()
        .trim_matches('-')
        .to_string()
}

// ---------------------------------------------------------------------------
// Core pipeline function (pub for reuse by MeetingRecorder in Phase 2)
// ---------------------------------------------------------------------------

/// Full offline pipeline: audio file → `MeetingNotes`.
///
/// This function is intentionally `pub` so Phase 2's `MeetingRecorder` can
/// call it after the live recording ends.
pub async fn run_offline_pipeline(
    audio_path: &str,
    title: &str,
    stt_config: SttConfig,
    meeting_config: MeetingConfig,
    llm_base_url: &str,
    llm_api_key: &str,
    llm_model: &str,
    hermes_home: &Path,
) -> Result<MeetingNotes, ToolError> {
    let client = Client::new();
    let date = Utc::now().format("%Y-%m-%d").to_string();

    // 1. Transcribe audio
    info!("meeting_notes: transcribing {audio_path}");
    let stt = SttEngine::new(stt_config);
    let transcript_text = stt.transcribe_file(audio_path).await?;

    if transcript_text.trim().is_empty() {
        return Err(ToolError::ExecutionFailed(
            "STT returned empty transcript".into(),
        ));
    }

    // 2. Diarization (optional)
    let turns = match meeting_config.diarization_provider() {
        DiarizationProvider::Pyannote => {
            let endpoint = meeting_config
                .pyannote_endpoint
                .as_deref()
                .unwrap_or("http://localhost:8765");
            diarize_with_pyannote(&client, endpoint, audio_path, &transcript_text).await?
        }
        _ => label_transcript_plain(&transcript_text),
    };

    // 3. Chunk into N-minute slices and summarize each
    let chunk_minutes = meeting_config.summary_chunk_minutes() as usize;
    // Approximate: split by line count (100 lines ≈ 10 min for typical meetings)
    let lines_per_chunk = chunk_minutes * 10;
    let all_lines: Vec<String> = transcript_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();

    let chunks: Vec<String> = all_lines
        .chunks(lines_per_chunk.max(1))
        .map(|c| c.join("\n"))
        .collect();

    info!(
        "meeting_notes: {} lines → {} chunks ({}min each)",
        all_lines.len(),
        chunks.len(),
        chunk_minutes
    );

    let mut chunk_summaries: Vec<Value> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        debug!("meeting_notes: summarizing chunk {}/{}", i + 1, chunks.len());
        let prompt = chunk_summary_prompt(chunk);
        match llm_summarize_chunk(&client, llm_base_url, llm_api_key, llm_model, &prompt).await {
            Ok(v) => chunk_summaries.push(v),
            Err(e) => warn!("Chunk {} summary failed: {e} — skipping", i + 1),
        }
    }

    if chunk_summaries.is_empty() {
        return Err(ToolError::ExecutionFailed(
            "All chunk summaries failed".into(),
        ));
    }

    // 4. Merge chunk summaries
    let final_notes: Value = if chunk_summaries.len() == 1 {
        chunk_summaries.remove(0)
    } else {
        let merge_prompt = merge_summary_prompt(&chunk_summaries);
        llm_summarize_chunk(&client, llm_base_url, llm_api_key, llm_model, &merge_prompt).await?
    };

    let extract_strings = |key: &str| -> Vec<String> {
        final_notes[key]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut notes = MeetingNotes {
        title: title.to_string(),
        date: date.clone(),
        summary: final_notes["summary"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string(),
        key_decisions: extract_strings("key_decisions"),
        action_items: extract_strings("action_items"),
        risks: extract_strings("risks"),
        follow_ups: extract_strings("follow_ups"),
        transcript: turns,
        transcript_file: None,
    };

    // 5. Save transcript file
    let meetings_dir = hermes_home.join("meetings");
    if let Err(e) = tokio::fs::create_dir_all(&meetings_dir).await {
        warn!("Cannot create meetings dir: {e}");
    } else {
        let fname = format!("{}-{}.txt", date, slugify(title));
        let fpath = meetings_dir.join(&fname);
        let content = notes
            .transcript
            .iter()
            .map(|t| format!("[{}] {}", t.speaker, t.text))
            .collect::<Vec<_>>()
            .join("\n");
        if let Err(e) = tokio::fs::write(&fpath, content).await {
            warn!("Cannot save transcript file: {e}");
        } else {
            notes.transcript_file = Some(fpath.to_string_lossy().into_owned());
            info!("Transcript saved to {:?}", fpath);
        }
    }

    // 6. Write to memory system (if enabled)
    if meeting_config.memory_sink_enabled() {
        let sink = MeetingMemorySink::new(hermes_home);
        if let Err(e) = sink.write(&notes) {
            warn!("Memory sink failed: {e}");
        }
    }

    Ok(notes)
}

// ---------------------------------------------------------------------------
// ToolHandler
// ---------------------------------------------------------------------------

pub struct MeetingNotesHandler {
    meeting_config: MeetingConfig,
    stt_config: SttConfig,
    llm_base_url: String,
    llm_api_key: String,
    llm_model: String,
    hermes_home: PathBuf,
}

impl MeetingNotesHandler {
    pub fn new(
        meeting_config: MeetingConfig,
        stt_config: SttConfig,
        llm_base_url: String,
        llm_api_key: String,
        llm_model: String,
        hermes_home: PathBuf,
    ) -> Self {
        Self {
            meeting_config,
            stt_config,
            llm_base_url,
            llm_api_key,
            llm_model,
            hermes_home,
        }
    }

    pub fn with_env_defaults(hermes_home: PathBuf) -> Self {
        let llm_base_url = std::env::var("MEETING_LLM_BASE_URL")
            .or_else(|_| std::env::var("OPENAI_BASE_URL"))
            .unwrap_or_else(|_| "https://api.openai.com/v1".into());
        let llm_api_key = std::env::var("MEETING_LLM_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .unwrap_or_default();
        let llm_model = std::env::var("MEETING_LLM_MODEL")
            .unwrap_or_else(|_| "gpt-4o-mini".into());
        Self::new(
            MeetingConfig::default(),
            SttConfig::default(),
            llm_base_url,
            llm_api_key,
            llm_model,
            hermes_home,
        )
    }
}

#[async_trait]
impl ToolHandler for MeetingNotesHandler {
    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "audio_path".into(),
            json!({"type": "string", "description": "Absolute path to the audio file (WAV, MP3, M4A, …). Required unless transcript_text is provided."}),
        );
        props.insert(
            "transcript_text".into(),
            json!({"type": "string", "description": "Pre-existing transcript text. If provided, STT is skipped."}),
        );
        props.insert(
            "title".into(),
            json!({"type": "string", "description": "Meeting title or topic (used for file naming and memory tags)."}),
        );
        props.insert(
            "transcription_mode".into(),
            json!({"type": "string", "enum": ["offline", "realtime"], "description": "Override the configured transcription mode."}),
        );
        props.insert(
            "diarization".into(),
            json!({"type": "boolean", "description": "Enable pyannote diarization (requires pyannote sidecar). Default: false."}),
        );
        tool_schema(
            "meeting_notes",
            "Generate structured Chinese meeting notes from an audio file or an existing \
             transcript. Produces summary, key decisions, action items, risks and follow-ups. \
             Results are automatically stored in memory (holographic facts + MEMORY.md).",
            JsonSchema::object(props, vec![]),
        )
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let title = params["title"]
            .as_str()
            .unwrap_or("会议")
            .to_string();

        // Resolve transcription mode override
        let mut meeting_cfg = self.meeting_config.clone();
        if let Some(mode_str) = params["transcription_mode"].as_str() {
            meeting_cfg.transcription_mode = Some(match mode_str {
                "realtime" => MeetingTranscriptionMode::Realtime,
                _ => MeetingTranscriptionMode::Offline,
            });
        }
        if params["diarization"].as_bool() == Some(true) {
            meeting_cfg.diarization_provider =
                Some(hermes_config::DiarizationProvider::Pyannote);
        }

        // Determine source: audio file or pre-supplied transcript
        if let Some(transcript_text) = params["transcript_text"].as_str() {
            // Skip STT — build notes directly from transcript text
            let turns = label_transcript_plain(transcript_text);
            let notes = self
                .build_notes_from_text(transcript_text, &title, turns, meeting_cfg)
                .await?;
            return Ok(serde_json::to_string_pretty(&notes).unwrap_or_default());
        }

        let audio_path = params["audio_path"]
            .as_str()
            .ok_or_else(|| ToolError::ExecutionFailed("audio_path is required".into()))?;

        if !Path::new(audio_path).exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "audio_path not found: {audio_path}"
            )));
        }

        let notes = run_offline_pipeline(
            audio_path,
            &title,
            self.stt_config.clone(),
            meeting_cfg,
            &self.llm_base_url,
            &self.llm_api_key,
            &self.llm_model,
            &self.hermes_home,
        )
        .await?;

        Ok(serde_json::to_string_pretty(&notes).unwrap_or_default())
    }
}

impl MeetingNotesHandler {
    /// Build notes from an already-decoded transcript (no STT needed).
    async fn build_notes_from_text(
        &self,
        text: &str,
        title: &str,
        turns: Vec<TranscriptTurn>,
        meeting_config: MeetingConfig,
    ) -> Result<MeetingNotes, ToolError> {
        let client = Client::new();
        let date = Utc::now().format("%Y-%m-%d").to_string();

        let chunk_minutes = meeting_config.summary_chunk_minutes() as usize;
        let lines_per_chunk = chunk_minutes * 10;
        let all_lines: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();

        let chunks: Vec<String> = all_lines
            .chunks(lines_per_chunk.max(1))
            .map(|c| c.join("\n"))
            .collect();

        let mut chunk_summaries: Vec<Value> = Vec::new();
        for chunk in &chunks {
            let prompt = chunk_summary_prompt(chunk);
            match llm_summarize_chunk(
                &client,
                &self.llm_base_url,
                &self.llm_api_key,
                &self.llm_model,
                &prompt,
            )
            .await
            {
                Ok(v) => chunk_summaries.push(v),
                Err(e) => warn!("Chunk summary failed: {e}"),
            }
        }

        if chunk_summaries.is_empty() {
            return Err(ToolError::ExecutionFailed("All chunks failed".into()));
        }

        let final_notes: Value = if chunk_summaries.len() == 1 {
            chunk_summaries.remove(0)
        } else {
            let merge_prompt = merge_summary_prompt(&chunk_summaries);
            llm_summarize_chunk(
                &client,
                &self.llm_base_url,
                &self.llm_api_key,
                &self.llm_model,
                &merge_prompt,
            )
            .await?
        };

        let extract = |key: &str| -> Vec<String> {
            final_notes[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default()
        };

        let notes = MeetingNotes {
            title: title.to_string(),
            date,
            summary: final_notes["summary"].as_str().unwrap_or("").to_string(),
            key_decisions: extract("key_decisions"),
            action_items: extract("action_items"),
            risks: extract("risks"),
            follow_ups: extract("follow_ups"),
            transcript: turns,
            transcript_file: None,
        };

        if meeting_config.memory_sink_enabled() {
            let sink = MeetingMemorySink::new(&self.hermes_home);
            if let Err(e) = sink.write(&notes) {
                warn!("Memory sink failed: {e}");
            }
        }

        Ok(notes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_entry_truncates_to_200() {
        let notes = MeetingNotes {
            title: "产品周会".into(),
            date: "2026-05-22".into(),
            summary: "a".repeat(300),
            key_decisions: vec!["决定A".into()],
            action_items: vec!["行动B".into()],
            risks: vec![],
            follow_ups: vec![],
            transcript: vec![],
            transcript_file: None,
        };
        let entry = notes.memory_entry();
        assert!(entry.chars().count() <= 200, "entry too long: {} chars", entry.chars().count());
    }

    #[test]
    fn slugify_chinese_and_spaces() {
        assert_eq!(slugify("产品 Q3"), "--q3");
        assert_eq!(slugify("hello world"), "hello-world");
    }

    #[test]
    fn label_transcript_plain_splits_lines() {
        let text = "Line one\nLine two\n\nLine three";
        let turns = label_transcript_plain(text);
        assert_eq!(turns.len(), 3);
        assert!(turns.iter().all(|t| t.speaker == "Speaker A"));
    }

    #[test]
    fn chunk_summary_prompt_contains_text() {
        let p = chunk_summary_prompt("test content");
        assert!(p.contains("test content"));
        assert!(p.contains("JSON"));
    }
}
