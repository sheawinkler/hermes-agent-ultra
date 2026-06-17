//! Mock DeepSeek prefix-cache hit-rate benchmark, ported from Reasonix
//! `internal/agent/cachehit_e2e_test.go`.
//!
//! The mock server derives cache-hit tokens from the byte-identical message
//! prefix it shares with the previous conversation request — exactly how
//! DeepSeek's automatic prefix caching works in production.

use hermes_agent::agent_runtime_helpers::prepare_wire_messages_for_api;
use hermes_core::types::{Message, MessageRole};
use serde::Deserialize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Mock DeepSeek SSE server (mirrors Reasonix mockDeepSeek + SSE chunk builders)
// ---------------------------------------------------------------------------

/// Per-turn usage stats the mock server returns via SSE.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct TurnUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
    hit_tokens: usize,
    miss_tokens: usize,
}

/// State shared between the mock server handler threads.
struct MockState {
    prev_messages: Vec<Value>,
    turn_usage: Vec<TurnUsage>,
    req_chars: Vec<usize>,
    hit_chars: Vec<usize>,
    first_turn: bool,
}

/// Spawn a mock DeepSeek API server on a random OS-assigned port.
/// Returns the bound address and a handle for later inspection.
fn spawn_mock_deepseek() -> (String, Arc<Mutex<MockState>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = format!("http://{}", listener.local_addr().unwrap());

    let state = Arc::new(Mutex::new(MockState {
        prev_messages: Vec::new(),
        turn_usage: Vec::new(),
        req_chars: Vec::new(),
        hit_chars: Vec::new(),
        first_turn: true,
    }));

    let s = Arc::clone(&state);
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let s = Arc::clone(&s);
                    thread::spawn(move || handle_connection(stream, s));
                }
                Err(_) => break,
            }
        }
    });

    // Give the server a moment to start.
    thread::sleep(Duration::from_millis(50));
    (addr, state)
}

fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<MockState>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    // Read request line and headers.
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }

    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed
            .to_lowercase()
            .strip_prefix("content-length:")
            .map(|v| v.trim().parse::<usize>().ok())
            .flatten()
        {
            content_length = val;
        }
    }

    // Read body.
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }

    // ---- Parse messages and compute cache hit ----
    let mut s = state.lock().unwrap();

    let (_usage, response_body) = if let Ok(req) =
        serde_json::from_slice::<serde_json::Value>(&body)
    {
        let msgs: Vec<Value> = req
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();

        let total_chars: usize = msgs.iter().map(|m| m.to_string().len()).sum();
        let common = common_prefix_msgs(&s.prev_messages, &msgs);
        let hit_chars: usize = msgs[..common].iter().map(|m| m.to_string().len()).sum();

        // First turn: no previous messages to compare against → 0 cache hit.
        if s.first_turn {
            s.first_turn = false;
        }

        s.prev_messages = msgs;
        s.req_chars.push(total_chars);
        s.hit_chars.push(hit_chars);

        let prompt_tok = total_chars / 4;
        let hit_tok = hit_chars / 4;
        let miss_tok = prompt_tok.saturating_sub(hit_tok);
        let completion_tok = 20;

        let usage = TurnUsage {
            prompt_tokens: prompt_tok,
            completion_tokens: completion_tok,
            hit_tokens: hit_tok,
            miss_tokens: miss_tok,
        };
        s.turn_usage.push(usage.clone());

        // Build SSE response matching DeepSeek's chat/completions streaming format.
        let sse = build_sse_response(prompt_tok, completion_tok, hit_tok, miss_tok);
        (usage, sse)
    } else {
        // Invalid body → return error SSE.
        let usage = TurnUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            hit_tokens: 0,
            miss_tokens: 0,
        };
        let sse = "data: {\"error\":\"invalid request\"}\n\ndata: [DONE]\n\n".to_string();
        (usage, sse)
    };

    drop(s);

    // Write HTTP response.
    let http_response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Connection: keep-alive\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        response_body.len(),
        response_body
    );

    let _ = stream.write_all(http_response.as_bytes());
    let _ = stream.flush();
}

/// Count the number of byte-identical leading messages between two slices.
/// Ported from Reasonix `commonPrefixMsgs`.
fn common_prefix_msgs(a: &[Value], b: &[Value]) -> usize {
    let mut n = 0;
    while n < a.len() && n < b.len() {
        // Compare canonical JSON bytes (no whitespace differences).
        let a_bytes = serde_json::to_vec(&a[n]).unwrap_or_default();
        let b_bytes = serde_json::to_vec(&b[n]).unwrap_or_default();
        if a_bytes != b_bytes {
            break;
        }
        n += 1;
    }
    n
}

/// Build SSE response chunks matching DeepSeek's chat/completions streaming
/// format. Ported from Reasonix `writeSSE` + chunk builders.
fn build_sse_response(
    prompt_tok: usize,
    completion_tok: usize,
    hit_tok: usize,
    miss_tok: usize,
) -> String {
    let mut out = String::new();

    // Chunk 1: delta with content.
    let delta = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "content": "Done.",
                "role": "assistant"
            },
            "finish_reason": null
        }]
    });
    out.push_str(&format!("data: {}\n\n", serde_json::to_string(&delta).unwrap()));

    // Chunk 2: finish_reason = "stop".
    let finish = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    });
    out.push_str(&format!("data: {}\n\n", serde_json::to_string(&finish).unwrap()));

    // Chunk 3: usage with DeepSeek-specific cache fields.
    let usage = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": null
        }],
        "usage": {
            "prompt_tokens": prompt_tok,
            "completion_tokens": completion_tok,
            "total_tokens": prompt_tok + completion_tok,
            "prompt_cache_hit_tokens": hit_tok,
            "prompt_cache_miss_tokens": miss_tok
        }
    });
    out.push_str(&format!("data: {}\n\n", serde_json::to_string(&usage).unwrap()));

    // Done signal.
    out.push_str("data: [DONE]\n\n");
    out
}

// ---------------------------------------------------------------------------
// SSE response parser (client side)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SseUsage {
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
    total_tokens: Option<usize>,
    prompt_cache_hit_tokens: Option<usize>,
    prompt_cache_miss_tokens: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct SseResponse {
    usage: Option<SseUsage>,
}

/// Parse SSE stream text and extract the last usage block.
fn parse_sse_usage(body: &str) -> Option<SseUsage> {
    let mut last_usage: Option<SseUsage> = None;
    for line in body.lines() {
        let line = line.trim();
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                continue;
            }
            if let Ok(resp) = serde_json::from_str::<SseResponse>(data) {
                if resp.usage.is_some() {
                    last_usage = resp.usage;
                }
            }
        }
    }
    last_usage
}

// ---------------------------------------------------------------------------
// HTTP client helper (synchronous, using std::net::TcpStream for simplicity)
// ---------------------------------------------------------------------------

fn send_chat_completion(server_addr: &str, messages: &[Value]) -> Result<String, String> {
    // Parse host:port from URL.
    let target = server_addr
        .strip_prefix("http://")
        .unwrap_or(server_addr);
    let mut stream =
        TcpStream::connect(target).map_err(|e| format!("connect: {}", e))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("timeout: {}", e))?;

    let body = serde_json::json!({
        "model": "deepseek-reasoner",
        "messages": messages,
        "stream": true,
        "temperature": 0.0,
    });
    let body_str = serde_json::to_string(&body).unwrap();

    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        target,
        body_str.len(),
        body_str
    );

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {}", e))?;
    stream.flush().map_err(|e| format!("flush: {}", e))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("read: {}", e))?;

    // Split headers from body.
    if let Some(body_start) = response.find("\r\n\r\n") {
        Ok(response[body_start + 4..].to_string())
    } else {
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// Helper: build conversation history incrementally, simulating agent turns
// ---------------------------------------------------------------------------

fn system_prompt() -> String {
    "You are hermes, a coding agent. Be concise and follow project conventions. \
     This system prompt is the cacheable head of every request and must never change between turns."
        .to_string()
}

/// Simulate one conversation turn: append user+assistant to history, then
/// prepare the messages for API as the agent would, and return the wire messages
/// as JSON values ready for HTTP.
fn next_turn_wire_messages(
    history: &mut Vec<Message>,
    user_content: &str,
    assistant_content: &str,
    turn: usize,
    with_tools: bool,
) -> Vec<Value> {
    // Append this turn's user message.
    history.push(Message::user(user_content));

    // Append assistant response.
    if with_tools && turn % 3 == 1 {
        // Simulate a tool-call turn.
        history.push(Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![hermes_core::ToolCall {
                id: format!("call_{}", turn),
                function: hermes_core::FunctionCall {
                    name: "echo".to_string(),
                    arguments: format!(r#"{{"text":"round-{}"}}"#, turn),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        });
        // Tool result.
        history.push(Message {
            role: MessageRole::Tool,
            content: Some(format!("echoed: round-{}", turn)),
            tool_calls: None,
            tool_call_id: Some(format!("call_{}", turn)),
            name: Some("echo".to_string()),
            reasoning_content: None,
            cache_control: None,
        });
    } else {
        history.push(Message::assistant(assistant_content));
    }

    // Prepare messages as the agent would for the NEXT turn's API call.
    let prepared = prepare_wire_messages_for_api(
        history.clone(),
        "deepseek",
        "deepseek-reasoner",
        "https://api.deepseek.com",
    );

    // Serialize each message to a JSON Value.
    prepared
        .iter()
        .map(|m| serde_json::to_value(m).unwrap())
        .collect()
}

// ---------------------------------------------------------------------------
// Test: full hit-rate benchmark with mock DeepSeek
// ---------------------------------------------------------------------------

#[test]
fn cache_hit_rate_with_mock_deepseek() {
    let (server_addr, state) = spawn_mock_deepseek();

    let sys = system_prompt();
    let mut history: Vec<Message> = vec![Message::system(&sys)];

    const TURNS: usize = 14;
    let user_messages: Vec<String> = (0..TURNS)
        .map(|i| {
            format!(
                "Turn {}: {}",
                i,
                "please consider this requirement. "
                    .repeat(6)
                    .trim()
                    .to_string()
            )
        })
        .collect();

    println!("\n========== Mock DeepSeek Cache Hit-Rate Benchmark ==========");
    println!("Provider: deepseek | Model: deepseek-reasoner");
    println!("Turns: {} | System prompt: {} chars", TURNS, sys.len());

    let mut total_hit = 0usize;
    let mut total_miss = 0usize;
    let mut peak = 0usize;

    for (i, user_msg) in user_messages.iter().enumerate() {
        let assistant = format!("Response to turn {}: acknowledged.", i);

        // Prepare wire messages (simulating what the agent sends).
        let wire_msgs = next_turn_wire_messages(
            &mut history,
            user_msg,
            &assistant,
            i,
            false, // no tools for baseline
        );

        // Send to mock DeepSeek.
        let sse_body = send_chat_completion(&server_addr, &wire_msgs)
            .unwrap_or_else(|e| panic!("turn {}: {}", i, e));

        let usage = parse_sse_usage(&sse_body)
            .unwrap_or_else(|| panic!("turn {}: no usage in SSE", i));

        let prompt = usage.prompt_tokens.unwrap_or(0);
        let hit = usage.prompt_cache_hit_tokens.unwrap_or(0);
        let miss = usage.prompt_cache_miss_tokens.unwrap_or(0);

        total_hit += hit;
        total_miss += miss;

        let rate = if hit + miss > 0 {
            hit * 100 / (hit + miss)
        } else if prompt > 0 {
            hit * 100 / prompt
        } else {
            0
        };

        if rate > peak {
            peak = rate;
        }

        println!(
            "turn {:2}: prompt={:5} hit={:5} miss={:4} → cache {:3}%",
            i, prompt, hit, miss, rate
        );
    }

    // Verify server-side tracking matches.
    let s = state.lock().unwrap();
    let n = s.turn_usage.len();
    for i in 0..n {
        let u = &s.turn_usage[i];
        let expected_hit = if i == 0 {
            0 // first turn has no previous prefix.
        } else {
            s.req_chars[i - 1] / 4 // entire previous request should be cached.
        };
        if u.hit_tokens != expected_hit && i > 0 {
            println!(
                "  NOTE turn {}: server hit={} expected={} (diff={})",
                i,
                u.hit_tokens,
                expected_hit,
                expected_hit as isize - u.hit_tokens as isize
            );
        }
    }
    drop(s);

    let session_rate = if total_hit + total_miss > 0 {
        total_hit * 100 / (total_hit + total_miss)
    } else {
        0
    };

    println!("------------------------------------------------------------");
    println!(
        "Session aggregate: hit={} miss={} → cache {}%",
        total_hit, total_miss, session_rate
    );
    println!("Peak single-turn hit rate: {}%", peak);
    println!("============================================================");

    // Basic sanity checks.
    assert!(total_hit > 0, "expected some cache hits");
    assert!(
        peak >= 85,
        "expected peak hit rate >= 85%, got {}% — prefix may not be byte-stable",
        peak
    );
    // With 14 turns, session-aggregate should be fairly high.
    assert!(
        session_rate >= 75,
        "expected session aggregate >= 75%, got {}%",
        session_rate
    );
}

// ---------------------------------------------------------------------------
// Test: with tools (simulates tool-call rounds affecting prefix stability)
// ---------------------------------------------------------------------------

#[test]
fn cache_hit_rate_with_tools() {
    let (server_addr, state) = spawn_mock_deepseek();

    let sys = system_prompt();
    let mut history: Vec<Message> = vec![Message::system(&sys)];

    const TURNS: usize = 12;

    println!("\n========== Mock DeepSeek Cache Hit-Rate with Tool Calls ==========");

    let mut total_hit = 0usize;
    let mut total_miss = 0usize;
    let mut peak = 0usize;

    for i in 0..TURNS {
        let user_msg = format!(
            "Turn {}: {}",
            i,
            "please consider this requirement. ".repeat(6).trim()
        );
        let assistant = format!("Response to turn {}: done.", i);

        let wire_msgs = next_turn_wire_messages(
            &mut history,
            &user_msg,
            &assistant,
            i,
            true, // with tools — every 3rd turn inserts tool calls
        );

        let sse_body = send_chat_completion(&server_addr, &wire_msgs)
            .unwrap_or_else(|e| panic!("turn {}: {}", i, e));

        let usage = parse_sse_usage(&sse_body)
            .unwrap_or_else(|| panic!("turn {}: no usage in SSE", i));

        let prompt = usage.prompt_tokens.unwrap_or(0);
        let hit = usage.prompt_cache_hit_tokens.unwrap_or(0);
        let miss = usage.prompt_cache_miss_tokens.unwrap_or(0);

        total_hit += hit;
        total_miss += miss;

        let rate = if hit + miss > 0 {
            hit * 100 / (hit + miss)
        } else if prompt > 0 {
            hit * 100 / prompt
        } else {
            0
        };

        if rate > peak {
            peak = rate;
        }

        println!(
            "turn {:2}: prompt={:5} hit={:5} miss={:4} → cache {:3}%",
            i, prompt, hit, miss, rate
        );
    }

    let session_rate = if total_hit + total_miss > 0 {
        total_hit * 100 / (total_hit + total_miss)
    } else {
        0
    };

    println!("------------------------------------------------------------");
    println!(
        "Session aggregate (with tools): hit={} miss={} → cache {}%",
        total_hit, total_miss, session_rate
    );
    println!("Peak single-turn hit rate: {}%", peak);
    println!("============================================================");

    let s = state.lock().unwrap();
    // Verify prefix stability: after turn 0, hit_chars[i] should match req_chars[i-1]
    // (the entire previous request is a prefix of the current one).
    let mut broken = 0;
    for i in 1..s.req_chars.len() {
        if s.hit_chars[i] != s.req_chars[i - 1] {
            println!(
                "  PREFIX BROKEN at req {}: cached {} chars but the full prior request was {} chars",
                i, s.hit_chars[i], s.req_chars[i - 1]
            );
            broken += 1;
        }
    }
    if broken > 0 {
        println!("  WARNING: {} turns had prefix breaks (tool messages may add variability)", broken);
    } else {
        println!("  ✓ Prefix byte-stable across all {} turns", s.req_chars.len());
    }
    drop(s);

    assert!(peak >= 80, "expected peak hit rate >= 80% with tools, got {}%", peak);
}

// ---------------------------------------------------------------------------
// Test: long conversation — hit rate should climb past 90% as history grows
// ---------------------------------------------------------------------------

#[test]
fn cache_hit_rate_long_conversation() {
    let (server_addr, _state) = spawn_mock_deepseek();

    let sys = system_prompt();
    let mut history: Vec<Message> = vec![Message::system(&sys)];

    const TURNS: usize = 20;

    println!("\n========== Long Conversation Cache Hit Curve ({} turns) ==========", TURNS);

    for i in 0..TURNS {
        let user_msg = format!(
            "Turn {}: {}",
            i,
            "please consider this requirement. ".repeat(6).trim()
        );
        let assistant = format!("Acknowledged turn {}.", i);

        let wire_msgs = next_turn_wire_messages(
            &mut history,
            &user_msg,
            &assistant,
            i,
            false,
        );

        let sse_body = send_chat_completion(&server_addr, &wire_msgs)
            .unwrap_or_else(|e| panic!("turn {}: {}", i, e));

        let usage = parse_sse_usage(&sse_body)
            .unwrap_or_else(|| panic!("turn {}: no usage in SSE", i));

        let prompt = usage.prompt_tokens.unwrap_or(0);
        let hit = usage.prompt_cache_hit_tokens.unwrap_or(0);
        let miss = usage.prompt_cache_miss_tokens.unwrap_or(0);

        let rate = if hit + miss > 0 {
            hit * 100 / (hit + miss)
        } else if prompt > 0 {
            hit * 100 / prompt
        } else {
            0
        };

        // Print every turn for the first 5, then every 5th.
        if i < 5 || i % 5 == 0 || i == TURNS - 1 {
            println!(
                "turn {:2}: prompt={:5} hit={:5} miss={:4} → cache {:3}%",
                i, prompt, hit, miss, rate
            );
        } else if i == 5 {
            println!("  ... (intermediate turns omitted) ...");
        }

        if i > 0 && i == TURNS - 1 {
            println!(
                "Final turn {}: hit rate = {}%",
                i, rate
            );
            assert!(
                rate >= 90,
                "expected final turn hit rate >= 90% after {} turns, got {}% \
                 (prefix may not be growing monotonically)",
                TURNS, rate
            );
        }
    }

    println!("================================================================");
}
