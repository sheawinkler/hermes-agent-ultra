//! CLI presentation — tool preview formatting, diff display, spinner helpers.
//!
//! Pure display functions with no agent dependency.

use std::collections::HashMap;
use std::fmt::Write;

// ---------------------------------------------------------------------------
// ANSI color constants
// ---------------------------------------------------------------------------

const ANSI_RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";

const DIFF_DIM: &str = "\x1b[38;2;150;150;150m";
const DIFF_FILE: &str = "\x1b[38;2;180;160;255m";
const DIFF_HUNK: &str = "\x1b[38;2;120;120;140m";
const DIFF_MINUS: &str = "\x1b[38;2;255;255;255;48;2;120;20;20m";
const DIFF_PLUS: &str = "\x1b[38;2;255;255;255;48;2;20;90;20m";

// Context pressure bar
const BAR_FILLED: char = '▰';
const BAR_EMPTY: char = '▱';
const BAR_WIDTH: usize = 20;

// ---------------------------------------------------------------------------
// Tool preview
// ---------------------------------------------------------------------------

/// Build a short preview of a tool call's primary argument for display.
pub fn build_tool_preview(
    tool_name: &str,
    args: &serde_json::Value,
    max_len: usize,
) -> Option<String> {
    let obj = args.as_object()?;
    if obj.is_empty() {
        return None;
    }

    let primary_args: HashMap<&str, &str> = [
        ("terminal", "command"),
        ("web_search", "query"),
        ("web_extract", "urls"),
        ("read_file", "path"),
        ("write_file", "path"),
        ("patch", "path"),
        ("search_files", "pattern"),
        ("browser_navigate", "url"),
        ("browser_click", "ref"),
        ("browser_type", "text"),
        ("image_generate", "prompt"),
        ("text_to_speech", "text"),
        ("vision_analyze", "question"),
        ("mixture_of_agents", "user_prompt"),
        ("skill_view", "name"),
        ("skills_list", "category"),
        ("execute_code", "code"),
        ("delegate_task", "goal"),
        ("clarify", "question"),
        ("skill_manage", "name"),
    ]
    .into();

    // Special handling for specific tools
    if tool_name == "process" {
        let action = obj.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let sid = obj.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let parts: Vec<&str> = [action, &sid[..sid.len().min(16)]]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect();
        return if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        };
    }

    if tool_name == "todo" {
        let todos = obj.get("todos");
        let merge = obj.get("merge").and_then(|v| v.as_bool()).unwrap_or(false);
        return match todos {
            None => Some("reading task list".to_string()),
            Some(t) => {
                let count = t.as_array().map(|a| a.len()).unwrap_or(0);
                if merge {
                    Some(format!("updating {} task(s)", count))
                } else {
                    Some(format!("planning {} task(s)", count))
                }
            }
        };
    }

    if tool_name == "memory" {
        let action = obj.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let target = obj.get("target").and_then(|v| v.as_str()).unwrap_or("");
        return match action {
            "add" => {
                let content = obj.get("content").and_then(|v| v.as_str()).unwrap_or("");
                Some(format!("+{}: \"{}\"", target, truncate(content, 25)))
            }
            "replace" => {
                let old = obj.get("old_text").and_then(|v| v.as_str()).unwrap_or("");
                Some(format!("~{}: \"{}\"", target, truncate(old, 20)))
            }
            "remove" => {
                let old = obj.get("old_text").and_then(|v| v.as_str()).unwrap_or("");
                Some(format!("-{}: \"{}\"", target, truncate(old, 20)))
            }
            _ => Some(action.to_string()),
        };
    }

    // Look up the primary argument for this tool
    let key = primary_args.get(tool_name).copied().or_else(|| {
        for fallback in &[
            "query", "text", "command", "path", "name", "prompt", "code", "goal",
        ] {
            if obj.contains_key(*fallback) {
                return Some(*fallback);
            }
        }
        None
    })?;

    let value = obj.get(key)?;
    let text = if let Some(s) = value.as_str() {
        s.to_string()
    } else if let Some(arr) = value.as_array() {
        arr.first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        return None;
    };

    let preview = oneline(&text);
    if preview.is_empty() {
        return None;
    }
    if max_len > 0 && preview.len() > max_len {
        Some(format!("{}...", &preview[..max_len - 3]))
    } else {
        Some(preview)
    }
}

/// Format a tool call for display.
pub fn format_tool_call(name: &str, args: &serde_json::Value) -> String {
    let preview = build_tool_preview(name, args, 60);
    match preview {
        Some(p) => format!("{}({})", name, p),
        None => name.to_string(),
    }
}

/// Format a tool result for display, truncated to max_len.
pub fn format_tool_result(result: &str, max_len: usize) -> String {
    if max_len == 0 || result.len() <= max_len {
        result.to_string()
    } else {
        format!("{}... ({} chars)", &result[..max_len], result.len())
    }
}

/// Generate a formatted tool completion line for CLI quiet mode.
pub fn get_cute_tool_message(
    tool_name: &str,
    args: &serde_json::Value,
    duration_secs: f64,
    result: Option<&str>,
) -> String {
    let dur = format!("{:.1}s", duration_secs);
    let (is_failure, failure_suffix) = detect_tool_failure(tool_name, result);
    let prefix = "┊";

    let trunc = |s: &str, n: usize| -> String {
        if s.len() > n {
            format!("{}...", &s[..n.saturating_sub(3)])
        } else {
            s.to_string()
        }
    };

    let path_trunc = |p: &str, n: usize| -> String {
        if p.len() > n {
            format!("...{}", &p[p.len().saturating_sub(n - 3)..])
        } else {
            p.to_string()
        }
    };

    let get_str = |key: &str| -> String {
        args.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let line = match tool_name {
        "web_search" => format!(
            "{} 🔍 search    {}  {}",
            prefix,
            trunc(&get_str("query"), 42),
            dur
        ),
        "terminal" => format!(
            "{} 💻 $         {}  {}",
            prefix,
            trunc(&get_str("command"), 42),
            dur
        ),
        "read_file" => format!(
            "{} 📖 read      {}  {}",
            prefix,
            path_trunc(&get_str("path"), 35),
            dur
        ),
        "write_file" => format!(
            "{} ✍️  write     {}  {}",
            prefix,
            path_trunc(&get_str("path"), 35),
            dur
        ),
        "patch" => format!(
            "{} 🔧 patch     {}  {}",
            prefix,
            path_trunc(&get_str("path"), 35),
            dur
        ),
        "search_files" => {
            let pattern = trunc(&get_str("pattern"), 35);
            let target = get_str("target");
            let verb = if target == "files" { "find" } else { "grep" };
            format!("{} 🔎 {:9} {}  {}", prefix, verb, pattern, dur)
        }
        "browser_navigate" => {
            let url = get_str("url");
            let domain = url
                .replace("https://", "")
                .replace("http://", "")
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
            format!("{} 🌐 navigate  {}  {}", prefix, trunc(&domain, 35), dur)
        }
        "execute_code" => {
            let code = get_str("code");
            let first_line = code.lines().next().unwrap_or("").trim();
            format!("{} 🐍 exec      {}  {}", prefix, trunc(first_line, 35), dur)
        }
        _ => {
            let preview = build_tool_preview(tool_name, args, 35).unwrap_or_default();
            format!(
                "{} ⚡ {:9} {}  {}",
                prefix,
                &tool_name[..tool_name.len().min(9)],
                trunc(&preview, 35),
                dur
            )
        }
    };

    if is_failure {
        format!("{}{}", line, failure_suffix)
    } else {
        line
    }
}

// ---------------------------------------------------------------------------
// Tool failure detection
// ---------------------------------------------------------------------------

fn detect_tool_failure(tool_name: &str, result: Option<&str>) -> (bool, String) {
    let result = match result {
        Some(r) => r,
        None => return (false, String::new()),
    };

    if tool_name == "terminal" {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(result) {
            if let Some(code) = data.get("exit_code").and_then(|c| c.as_i64()) {
                if code != 0 {
                    return (true, format!(" [exit {}]", code));
                }
            }
        }
        return (false, String::new());
    }

    let lower = &result[..result.len().min(500)].to_lowercase();
    if lower.contains("\"error\"") || lower.contains("\"failed\"") || result.starts_with("Error") {
        return (true, " [error]".to_string());
    }

    (false, String::new())
}

// ---------------------------------------------------------------------------
// Usage / stats formatting
// ---------------------------------------------------------------------------

/// Format usage statistics for display.
pub fn format_usage_stats(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: Option<f64>,
    duration_secs: Option<f64>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Model: {}", model);
    let _ = writeln!(out, "  Input tokens:  {}", format_token_count(input_tokens));
    let _ = writeln!(
        out,
        "  Output tokens: {}",
        format_token_count(output_tokens)
    );
    let _ = writeln!(
        out,
        "  Total tokens:  {}",
        format_token_count(input_tokens + output_tokens)
    );
    if let Some(cost) = cost_usd {
        let _ = writeln!(out, "  Cost: {}", format_cost(cost));
    }
    if let Some(dur) = duration_secs {
        let _ = writeln!(out, "  Duration: {}", format_duration_compact(dur));
    }
    out
}

/// Format a token count compactly (e.g. 1.5K, 2.3M).
pub fn format_token_count(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    if value >= 1_000_000_000 {
        let scaled = value as f64 / 1_000_000_000.0;
        return format!("{:.1}B", scaled);
    }
    if value >= 1_000_000 {
        let scaled = value as f64 / 1_000_000.0;
        return format!("{:.1}M", scaled);
    }
    let scaled = value as f64 / 1_000.0;
    if scaled < 10.0 {
        format!("{:.2}K", scaled)
    } else if scaled < 100.0 {
        format!("{:.1}K", scaled)
    } else {
        format!("{:.0}K", scaled)
    }
}

/// Format a cost in USD for human display.
pub fn format_cost(usd: f64) -> String {
    if usd == 0.0 {
        "included".to_string()
    } else if usd < 0.01 {
        format!("~${:.4}", usd)
    } else if usd < 1.0 {
        format!("~${:.2}", usd)
    } else {
        format!("~${:.2}", usd)
    }
}

/// Format a duration compactly.
pub fn format_duration_compact(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{:.0}s", seconds)
    } else if seconds < 3600.0 {
        format!("{:.0}m", seconds / 60.0)
    } else if seconds < 86400.0 {
        let hours = seconds / 3600.0;
        let remaining_min = ((seconds % 3600.0) / 60.0) as u32;
        if remaining_min > 0 {
            format!("{}h {}m", hours as u32, remaining_min)
        } else {
            format!("{}h", hours as u32)
        }
    } else {
        format!("{:.1}d", seconds / 86400.0)
    }
}

// ---------------------------------------------------------------------------
// Context pressure display
// ---------------------------------------------------------------------------

/// Build a formatted context pressure line for CLI display.
pub fn format_context_pressure(
    compaction_progress: f64,
    threshold_tokens: u64,
    threshold_percent: f64,
    compression_enabled: bool,
) -> String {
    let pct_int = (compaction_progress * 100.0).min(100.0) as u32;
    let filled = ((compaction_progress * BAR_WIDTH as f64) as usize).min(BAR_WIDTH);
    let bar: String = std::iter::repeat(BAR_FILLED)
        .take(filled)
        .chain(std::iter::repeat(BAR_EMPTY).take(BAR_WIDTH - filled))
        .collect();

    let threshold_k = if threshold_tokens >= 1000 {
        format!("{}k", threshold_tokens / 1000)
    } else {
        threshold_tokens.to_string()
    };
    let threshold_pct_int = (threshold_percent * 100.0) as u32;

    let hint = if compression_enabled {
        "compaction approaching"
    } else {
        "no auto-compaction"
    };

    format!(
        "  {BOLD}{YELLOW}⚠ context {} {}% to compaction{ANSI_RESET}  {DIM}{} threshold ({}%) · {}{ANSI_RESET}",
        bar, pct_int, threshold_k, threshold_pct_int, hint,
    )
}

/// Build a plain-text context pressure notification for messaging platforms.
pub fn format_context_pressure_gateway(
    compaction_progress: f64,
    threshold_percent: f64,
    compression_enabled: bool,
) -> String {
    let pct_int = (compaction_progress * 100.0).min(100.0) as u32;
    let filled = ((compaction_progress * BAR_WIDTH as f64) as usize).min(BAR_WIDTH);
    let bar: String = std::iter::repeat(BAR_FILLED)
        .take(filled)
        .chain(std::iter::repeat(BAR_EMPTY).take(BAR_WIDTH - filled))
        .collect();

    let threshold_pct_int = (threshold_percent * 100.0) as u32;
    let hint = if compression_enabled {
        format!(
            "Context compaction approaching (threshold: {}% of window).",
            threshold_pct_int
        )
    } else {
        "Auto-compaction is disabled — context may be truncated.".to_string()
    };

    format!("⚠️ Context: {} {}% to compaction\n{}", bar, pct_int, hint)
}

// ---------------------------------------------------------------------------
// Progress bar
// ---------------------------------------------------------------------------

/// Format a simple progress bar.
pub fn format_progress_bar(current: u64, total: u64, width: usize) -> String {
    if total == 0 {
        return format!("[{}]", " ".repeat(width));
    }
    let ratio = (current as f64 / total as f64).min(1.0);
    let filled = (ratio * width as f64) as usize;
    let empty = width - filled;
    format!(
        "[{}{}] {:.0}%",
        "█".repeat(filled),
        "░".repeat(empty),
        ratio * 100.0
    )
}

// ---------------------------------------------------------------------------
// Inline diff rendering
// ---------------------------------------------------------------------------

/// Render unified diff lines in Hermes' inline transcript style.
pub fn render_inline_unified_diff(diff: &str) -> Vec<String> {
    let mut rendered: Vec<String> = Vec::new();
    let mut from_file: Option<String> = None;

    for raw_line in diff.lines() {
        if let Some(rest) = raw_line.strip_prefix("--- ") {
            from_file = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = raw_line.strip_prefix("+++ ") {
            let to_file = rest.trim();
            if from_file.is_some() || !to_file.is_empty() {
                rendered.push(format!(
                    "{}{} → {}{}",
                    DIFF_FILE,
                    from_file.as_deref().unwrap_or("a/?"),
                    to_file,
                    ANSI_RESET,
                ));
            }
            from_file = None;
            continue;
        }
        if raw_line.starts_with("@@") {
            rendered.push(format!("{}{}{}", DIFF_HUNK, raw_line, ANSI_RESET));
        } else if raw_line.starts_with('-') {
            rendered.push(format!("{}{}{}", DIFF_MINUS, raw_line, ANSI_RESET));
        } else if raw_line.starts_with('+') {
            rendered.push(format!("{}{}{}", DIFF_PLUS, raw_line, ANSI_RESET));
        } else if raw_line.starts_with(' ') {
            rendered.push(format!("{}{}{}", DIFF_DIM, raw_line, ANSI_RESET));
        } else if !raw_line.is_empty() {
            rendered.push(raw_line.to_string());
        }
    }

    rendered
}

// ---------------------------------------------------------------------------
// Model response formatting
// ---------------------------------------------------------------------------

/// Format a model response for terminal display.
pub fn format_model_response(content: &str, reasoning: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(reasoning) = reasoning {
        if !reasoning.is_empty() {
            let _ = writeln!(out, "{}💭 Reasoning:{}", DIM, ANSI_RESET);
            for line in reasoning.lines() {
                let _ = writeln!(out, "{}  {}{}", DIM, line, ANSI_RESET);
            }
            let _ = writeln!(out);
        }
    }
    out.push_str(content);
    out
}

// ---------------------------------------------------------------------------
// Kawaii spinner frames
// ---------------------------------------------------------------------------

/// Spinner frame sets.
pub mod spinners {
    pub const DOTS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    pub const BOUNCE: &[&str] = &["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"];
    pub const GROW: &[&str] = &[
        "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█", "▇", "▆", "▅", "▄", "▃", "▂",
    ];
    pub const STAR: &[&str] = &["✶", "✷", "✸", "✹", "✺", "✹", "✸", "✷"];
    pub const PULSE: &[&str] = &["◜", "◠", "◝", "◞", "◡", "◟"];

    pub const KAWAII_WAITING: &[&str] = &["(｡◕‿◕｡)", "(◕‿◕✿)", "٩(◕‿◕｡)۶", "(✿◠‿◠)", "( ˘▽˘)っ"];

    pub const KAWAII_THINKING: &[&str] = &["(｡•́︿•̀｡)", "(◔_◔)", "(¬‿¬)", "( •_•)>⌐■-■", "(⌐■_■)"];

    pub const THINKING_VERBS: &[&str] = &[
        "pondering",
        "contemplating",
        "musing",
        "cogitating",
        "ruminating",
        "deliberating",
        "mulling",
        "reflecting",
        "processing",
        "reasoning",
    ];
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn oneline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() > max {
        format!("{}...", &text[..max.saturating_sub(3)])
    } else {
        text.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_token_count() {
        assert_eq!(format_token_count(500), "500");
        assert_eq!(format_token_count(1500), "1.50K");
        assert_eq!(format_token_count(15000), "15.0K");
        assert_eq!(format_token_count(150000), "150K");
        assert_eq!(format_token_count(1500000), "1.5M");
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.0), "included");
        assert_eq!(format_cost(0.001), "~$0.0010");
        assert_eq!(format_cost(0.50), "~$0.50");
        assert_eq!(format_cost(3.15), "~$3.15");
    }

    #[test]
    fn test_format_duration_compact() {
        assert_eq!(format_duration_compact(30.0), "30s");
        assert_eq!(format_duration_compact(120.0), "2m");
        assert_eq!(format_duration_compact(7200.0), "2h");
    }

    #[test]
    fn test_format_progress_bar() {
        let bar = format_progress_bar(50, 100, 10);
        assert!(bar.contains("50%"));
    }

    #[test]
    fn test_build_tool_preview() {
        let args = serde_json::json!({"command": "ls -la"});
        let preview = build_tool_preview("terminal", &args, 0);
        assert_eq!(preview, Some("ls -la".to_string()));
    }

    #[test]
    fn test_build_tool_preview_todo() {
        let args = serde_json::json!({"todos": [{"id": "1"}, {"id": "2"}], "merge": true});
        let preview = build_tool_preview("todo", &args, 0);
        assert_eq!(preview, Some("updating 2 task(s)".to_string()));
    }
}
