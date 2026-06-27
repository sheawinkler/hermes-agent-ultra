//! Deterministic delivery for equity-research slash commands (`/analyze-stock`, etc.).

use hermes_core::{ToolCall, ToolResult};
use hermes_trading::research::analyze::AnalyzeStockResult;
use hermes_trading::research::report::{
    render_chat_brief_markdown, render_institutional_html, write_equity_report,
};
use hermes_trading::research::synthesis::build_synthesis_format_output;

use crate::analyze_stock_cache;

/// Slash mode parsed from the agent's first user message (skill frontmatter injection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EquitySlashMode {
    #[default]
    None,
    QuickScan,
    AnalyzeStock,
}

/// Outcome of attempting slash delivery after tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EquitySlashDeliveryOutcome {
    /// Full delivery — end turn with this assistant text.
    Deliver(String),
    /// Brief markdown only — attachment or write step failed.
    Partial(String),
    /// Hard failure — end turn; do not fall back to LLM improvisation.
    Failed(String),
    /// Slash workflow active — continue agent loop (analyze pending or web gap-fill).
    Pending,
}

/// Per-turn slash progress: analyze → optional web_search → deliver.
#[derive(Debug, Clone, Default)]
pub struct EquitySlashSession {
    pub analyze_done: bool,
    pub symbol: Option<String>,
    pub depth: Option<String>,
    pub web_searches_done: u32,
    pending_web_batches: u32,
    /// LLM turns that returned text while web gap-fill was still required.
    text_stall_turns: u32,
}

const WEB_FILL_MIN_SEARCHES: u32 = 1;
const WEB_FILL_FORCE_DELIVER_BATCHES: u32 = 4;
/// LLM replied with text before calling `analyze_stock` (common when session has history).
const SLASH_SKIP_TOOLS_MAX_STALLS: u32 = 2;

/// Agent returned text without tools while slash delivery is still pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EquitySlashStallAction {
    /// Inject user nudge and call LLM again (must run web_search).
    ContinueAgent(String),
    /// Stop stalling — deliver brief/HTML from cache (web optional).
    ForceDeliver(EquitySlashDeliveryOutcome),
}

/// Detect slash workflow from skill `[MODE: …]` block or bare `/analyze-stock` lines.
#[must_use]
pub fn detect_equity_slash_mode(user_message: &str) -> EquitySlashMode {
    if user_message.contains("[MODE: quick-scan")
        || user_message.contains("invoked /quick-scan")
        || line_starts_with_slash(user_message, "/quick-scan")
    {
        EquitySlashMode::QuickScan
    } else if user_message.contains("[MODE: analyze-stock")
        || user_message.contains("[MODE: equity-research")
        || user_message.contains("invoked /analyze-stock")
        || user_message.contains("invoked /equity-research")
        || line_starts_with_slash(user_message, "/analyze-stock")
        || line_starts_with_slash(user_message, "/equity-research")
    {
        EquitySlashMode::AnalyzeStock
    } else {
        EquitySlashMode::None
    }
}

/// Injected at turn start so reused sessions still run tools before free-text replies.
#[must_use]
pub fn slash_turn_start_system_hint(mode: EquitySlashMode) -> Option<String> {
    match mode {
        EquitySlashMode::AnalyzeStock => Some(
            "Slash /analyze-stock active: call analyze_stock(symbol, depth=medium, use_providers=true) \
             in this turn before writing assistant text. Do not reuse prior-turn analysis from history. \
             Brief + HTML attachment are delivered automatically after the tool succeeds — do NOT call web_search \
             or write a long analysis; one analyze_stock call is enough."
                .into(),
        ),
        EquitySlashMode::QuickScan => Some(
            "Slash /quick-scan active: call analyze_stock(symbol, depth=lite, use_providers=true) \
             before assistant text. Do not summarize from prior turns."
                .into(),
        ),
        EquitySlashMode::None => None,
    }
}

fn line_starts_with_slash(message: &str, cmd: &str) -> bool {
    message.lines().any(|line| {
        let t = line.trim();
        t.starts_with(cmd)
            && t.len() > cmd.len()
            && t.as_bytes()
                .get(cmd.len())
                .is_some_and(|b| b.is_ascii_whitespace())
    })
}

#[must_use]
pub fn wants_md_only_attachment(user_message: &str) -> bool {
    let lower = user_message.to_lowercase();
    [
        "md-only",
        "不要附件",
        "仅markdown",
        "no attachment",
        "no html",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

/// Medium analysis: wait for web fill when external context not yet merged.
#[must_use]
pub fn needs_web_fill(result: &AnalyzeStockResult) -> bool {
    hermes_trading::research::report::needs_external_web_fill(
        &result.content,
        result.data_confidence.score,
    )
}

/// System hint when slash waits for web gap-fill after `analyze_stock`.
#[must_use]
pub fn web_fill_system_hint(result: &AnalyzeStockResult) -> String {
    let dims = if result.missing_dims.is_empty() {
        "(low confidence)".into()
    } else {
        result.missing_dims.join(", ")
    };
    format!(
        "[equity slash] analyze_stock 完成（置信度 {:.0}%）。missing_dims=[{dims}]。\
         请按缺口调用 web_search 或 web_extract（2–4 条定向查询：宏观/行业/政策/舆情/FCF/同业等），\
         然后调用 analyze_stock(symbol, depth=medium, merge_external_only=true, external_context={{macro_bullets,policy_bullets,sentiment_bullets,sources}})。\
         不要重复完整 analyze_stock。补数完成后系统会自动投递简报与 HTML 附件。",
        result.data_confidence.score * 100.0
    )
}

/// Hint for the model when analyze is done but web fill is still pending.
#[must_use]
pub fn slash_web_fill_system_hint(_session: &EquitySlashSession) -> Option<String> {
    None
}

/// Block premature text-only turn end while slash delivery is still pending.
#[must_use]
pub fn handle_slash_text_stall(
    mode: EquitySlashMode,
    user_message: &str,
    session: &mut EquitySlashSession,
) -> Option<EquitySlashStallAction> {
    if mode == EquitySlashMode::None {
        return None;
    }

    if !session.analyze_done {
        return handle_slash_skip_tools_stall(mode, user_message, session);
    }

    if mode != EquitySlashMode::AnalyzeStock {
        return None;
    }

    let symbol = session.symbol.as_deref()?;
    let depth = session.depth.as_deref()?;
    let parsed = analyze_stock_cache::get(symbol, depth)?;
    if !needs_web_fill(&parsed) || web_fill_satisfied(session, &parsed) {
        return None;
    }

    // Slash now auto-delivers after analyze_stock; optional web fill is user-initiated.
    None
}

fn handle_slash_skip_tools_stall(
    mode: EquitySlashMode,
    user_message: &str,
    session: &mut EquitySlashSession,
) -> Option<EquitySlashStallAction> {
    session.text_stall_turns += 1;
    let depth = match mode {
        EquitySlashMode::QuickScan => "lite",
        EquitySlashMode::AnalyzeStock => "medium",
        EquitySlashMode::None => return None,
    };

    if session.text_stall_turns >= SLASH_SKIP_TOOLS_MAX_STALLS {
        if let Some(symbol) = slash_symbol_from_user_message(user_message) {
            session.symbol = Some(symbol.clone());
            session.depth = Some(depth.into());
            session.analyze_done = true;
            if let Some(parsed) = analyze_stock_cache::get(&symbol, depth) {
                tracing::warn!(
                    symbol = %symbol,
                    stall_turns = session.text_stall_turns,
                    "equity slash: LLM skipped analyze_stock — delivering from cache"
                );
                let outcome = match mode {
                    EquitySlashMode::QuickScan => {
                        let Some(parsed) = analyze_stock_cache::take(&symbol, depth) else {
                            return Some(EquitySlashStallAction::ForceDeliver(
                                EquitySlashDeliveryOutcome::Failed(format!(
                                    "内部分析结果不可用（{symbol}），请重试 /quick-scan。"
                                )),
                            ));
                        };
                        deliver_quick_scan(&parsed)
                    }
                    EquitySlashMode::AnalyzeStock => {
                        force_deliver_analyze_stock(user_message, session, &parsed)
                    }
                    EquitySlashMode::None => return None,
                };
                return Some(EquitySlashStallAction::ForceDeliver(outcome));
            }
        }
        return Some(EquitySlashStallAction::ForceDeliver(
            EquitySlashDeliveryOutcome::Failed(
                "本回合未调用 analyze_stock，无法生成研报附件。请重试 /analyze-stock。".into(),
            ),
        ));
    }

    tracing::info!(
        ?mode,
        stall_turns = session.text_stall_turns,
        "equity slash: text-only without analyze_stock — continuing agent"
    );
    Some(EquitySlashStallAction::ContinueAgent(format!(
        "【强制 slash】禁止直接用文字回复或复述历史对话中的旧分析。\
         必须先调用 analyze_stock(symbol=…, depth={depth}, use_providers=true)。\
         工具成功后系统会自动投递 brief 与 HTML 附件。"
    )))
}

fn slash_symbol_from_user_message(user_message: &str) -> Option<String> {
    if let Some(rest) = user_message.split("Analyze:").nth(1) {
        let token = rest
            .split_whitespace()
            .next()?
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
        if !token.is_empty() {
            return normalize_slash_symbol_token(token);
        }
    }
    for line in user_message.lines() {
        if let Some(args) = line.strip_prefix("User args:") {
            let token = args
                .split_whitespace()
                .next()?
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
            if !token.is_empty() {
                return normalize_slash_symbol_token(token);
            }
        }
    }
    extract_bare_a_share_code(user_message)
}

fn normalize_slash_symbol_token(token: &str) -> Option<String> {
    let upper = token.to_uppercase();
    if upper.contains('.') {
        return Some(upper);
    }
    if upper.len() == 6 && upper.chars().all(|c| c.is_ascii_digit()) {
        let suffix = if upper.starts_with('6') { "SH" } else { "SZ" };
        return Some(format!("{upper}.{suffix}"));
    }
    None
}

fn extract_bare_a_share_code(message: &str) -> Option<String> {
    for word in message.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
        if let Some(sym) = normalize_slash_symbol_token(clean) {
            return Some(sym);
        }
    }
    None
}

/// Replace bloated analyze_stock tool JSON before feeding the LLM (slash continuation only).
#[must_use]
pub fn slim_analyze_stock_tool_content(
    tool_calls: &[ToolCall],
    results: &[ToolResult],
) -> Option<String> {
    let (tc, res) = tool_calls
        .iter()
        .zip(results.iter())
        .find(|(tc, r)| tc.function.name == "analyze_stock" && !r.is_error)?;
    if res.content.contains("\"external_merged\"") {
        return Some(
            "analyze_stock: external_context merged into cache; slash will re-deliver brief+HTML."
                .into(),
        );
    }
    let (symbol, depth) = parse_symbol_depth(&tc.function.arguments)?;
    let parsed = analyze_stock_cache::get(&symbol, &depth)?;
    let slim = build_synthesis_format_output(&parsed);
    serde_json::to_string_pretty(&serde_json::json!({
        "_orchestration": "Slash auto-delivers brief+HTML. Do not paste tables or run web_search unless the user explicitly asks.",
        "symbol": slim.symbol,
        "depth": slim.depth,
        "data_confidence": slim.data_confidence,
        "missing_dims": slim.missing_dims,
        "fundamental_score": slim.fundamental_score,
        "panel_consensus": slim.panel_consensus,
        "synthesis": slim.synthesis,
    }))
    .ok()
}

/// Update slash session from the latest tool batch.
pub fn update_slash_session_from_batch(
    session: &mut EquitySlashSession,
    mode: EquitySlashMode,
    user_message: &str,
    tool_calls: &[ToolCall],
    results: &[ToolResult],
) {
    if mode == EquitySlashMode::None {
        return;
    }
    let default_depth = match mode {
        EquitySlashMode::QuickScan => "lite",
        EquitySlashMode::AnalyzeStock => "medium",
        EquitySlashMode::None => "medium",
    };
    for (tc, res) in tool_calls.iter().zip(results.iter()) {
        if res.is_error {
            continue;
        }
        match tc.function.name.as_str() {
            "analyze_stock" => {
                if let Some((sym, depth)) = parse_symbol_depth(&tc.function.arguments) {
                    session.analyze_done = true;
                    session.symbol = Some(sym);
                    session.depth = Some(depth);
                } else if let Some(sym) = slash_symbol_from_user_message(user_message) {
                    session.analyze_done = true;
                    session.symbol = Some(sym);
                    session.depth = Some(default_depth.into());
                }
            }
            "web_search" | "web_extract" if session.analyze_done => {
                session.web_searches_done += 1;
            }
            _ => {}
        }
    }
}

/// After tool execution in slash mode, build chat text from structured analysis cache.
#[must_use]
pub fn try_equity_slash_delivery(
    mode: EquitySlashMode,
    user_message: &str,
    tool_calls: &[ToolCall],
    results: &[ToolResult],
    session: &mut EquitySlashSession,
) -> EquitySlashDeliveryOutcome {
    if mode == EquitySlashMode::None {
        return EquitySlashDeliveryOutcome::Pending;
    }

    update_slash_session_from_batch(session, mode, user_message, tool_calls, results);

    if let Some((tc, result)) = find_analyze_stock_error(tool_calls, results) {
        let preview = result.content.chars().take(400).collect::<String>();
        tracing::warn!(
            tool_call_id = %tc.id,
            "equity slash: analyze_stock failed"
        );
        return EquitySlashDeliveryOutcome::Failed(format!(
            "数据采集或分析失败，无法生成研报。\n\n{preview}"
        ));
    }

    if !session.analyze_done {
        return EquitySlashDeliveryOutcome::Pending;
    }

    let (symbol, depth) = match (session.symbol.as_deref(), session.depth.as_deref()) {
        (Some(s), Some(d)) => (s, d),
        _ => {
            return EquitySlashDeliveryOutcome::Failed(
                "analyze_stock 已完成但缺少 symbol/depth，无法投递。".into(),
            );
        }
    };

    let Some(parsed) = analyze_stock_cache::get(symbol, depth) else {
        tracing::error!(
            symbol = %symbol,
            depth = %depth,
            "equity slash: structured analyze_stock cache miss after successful tool call"
        );
        return EquitySlashDeliveryOutcome::Failed(format!(
            "内部分析结果不可用（{symbol}），请重试 /analyze-stock。"
        ));
    };

    match mode {
        EquitySlashMode::QuickScan => {
            let Some(parsed) = analyze_stock_cache::take(symbol, depth) else {
                return EquitySlashDeliveryOutcome::Failed(format!(
                    "内部分析结果不可用（{symbol}），请重试 /quick-scan。"
                ));
            };
            deliver_quick_scan(&parsed)
        }
        EquitySlashMode::AnalyzeStock => {
            let low_confidence = needs_web_fill(&parsed);
            let Some(parsed) = analyze_stock_cache::take(symbol, depth) else {
                return EquitySlashDeliveryOutcome::Failed(format!(
                    "内部分析结果不可用（{symbol}），请重试 /analyze-stock。"
                ));
            };
            let mut outcome = deliver_analyze_stock(user_message, &parsed);
            if low_confidence {
                let note = "\n\n⚠️ 数据置信度偏低，宏观/政策/舆情未 web 补数；如需补全请另说「补宏观舆情」。";
                outcome = match outcome {
                    EquitySlashDeliveryOutcome::Deliver(t) => {
                        EquitySlashDeliveryOutcome::Deliver(format!("{t}{note}"))
                    }
                    EquitySlashDeliveryOutcome::Partial(t) => {
                        EquitySlashDeliveryOutcome::Partial(format!("{t}{note}"))
                    }
                    other => other,
                };
            }
            outcome
        }
        EquitySlashMode::None => EquitySlashDeliveryOutcome::Pending,
    }
}

fn web_fill_satisfied(session: &EquitySlashSession, result: &AnalyzeStockResult) -> bool {
    if !needs_web_fill(result) {
        return true;
    }
    if result.content.external.coverage
        == hermes_trading::research::report::content::ExternalCoverage::WebFilled
    {
        return true;
    }
    if session.web_searches_done >= WEB_FILL_MIN_SEARCHES {
        return true;
    }
    session.pending_web_batches >= WEB_FILL_FORCE_DELIVER_BATCHES
}

fn force_deliver_analyze_stock(
    user_message: &str,
    session: &EquitySlashSession,
    parsed: &AnalyzeStockResult,
) -> EquitySlashDeliveryOutcome {
    let symbol = session.symbol.as_deref().unwrap_or(&parsed.symbol);
    let depth = session.depth.as_deref().unwrap_or(parsed.depth.as_str());
    let Some(parsed) = analyze_stock_cache::take(symbol, depth) else {
        return EquitySlashDeliveryOutcome::Failed(format!(
            "内部分析结果不可用（{symbol}），无法投递。"
        ));
    };
    let mut outcome = deliver_analyze_stock(user_message, &parsed);
    if session.web_searches_done == 0 {
        let note = "\n\n⚠️ web 补数未完成，以下为 HTTP 硬数据分析结果。";
        outcome = match outcome {
            EquitySlashDeliveryOutcome::Deliver(t) => {
                EquitySlashDeliveryOutcome::Deliver(format!("{t}{note}"))
            }
            EquitySlashDeliveryOutcome::Partial(t) => {
                EquitySlashDeliveryOutcome::Partial(format!("{t}{note}"))
            }
            other => other,
        };
    }
    outcome
}

fn deliver_quick_scan(result: &AnalyzeStockResult) -> EquitySlashDeliveryOutcome {
    let body = result.summary_markdown.trim();
    if body.is_empty() {
        return EquitySlashDeliveryOutcome::Failed("速判分析未生成 markdown 内容。".into());
    }
    EquitySlashDeliveryOutcome::Deliver(body.to_string())
}

fn deliver_analyze_stock(
    user_message: &str,
    parsed: &AnalyzeStockResult,
) -> EquitySlashDeliveryOutcome {
    let started = std::time::Instant::now();
    let brief = render_chat_brief_markdown(parsed);
    if wants_md_only_attachment(user_message) {
        return EquitySlashDeliveryOutcome::Deliver(brief);
    }

    let html = std::thread::scope(|scope| {
        scope
            .spawn(|| render_institutional_html(parsed, None))
            .join()
            .unwrap_or_else(|_| String::new())
    });
    tracing::info!(
        symbol = %parsed.symbol,
        html_bytes = html.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "equity slash: institutional HTML rendered"
    );
    match write_equity_report(parsed, &html, None) {
        Ok(paths) => {
            let html_path = paths.html.to_string_lossy();
            EquitySlashDeliveryOutcome::Deliver(format!(
                "{brief}\n\n📎 完整研报见附件 HTML（含 66 评委明细与图表）。\nMEDIA:{html_path}"
            ))
        }
        Err(e) => {
            tracing::warn!(error = %e, "equity slash: write_equity_report failed");
            EquitySlashDeliveryOutcome::Partial(format!(
                "{brief}\n\n⚠️ 简报已生成，但 HTML 附件写入失败：{e}"
            ))
        }
    }
}

fn parse_symbol_depth(args_json: &str) -> Option<(String, String)> {
    let v = serde_json::from_str::<serde_json::Value>(args_json).ok()?;
    let symbol = v.get("symbol")?.as_str()?.trim();
    if symbol.is_empty() {
        return None;
    }
    let depth = v.get("depth").and_then(|d| d.as_str()).unwrap_or("medium");
    Some((symbol.to_string(), depth.to_string()))
}

fn find_analyze_stock_error<'a>(
    tool_calls: &'a [ToolCall],
    results: &'a [ToolResult],
) -> Option<(&'a ToolCall, &'a ToolResult)> {
    tool_calls
        .iter()
        .zip(results.iter())
        .rev()
        .find_map(|(tc, res)| {
            if tc.function.name == "analyze_stock" && res.is_error {
                Some((tc, res))
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::FunctionCall;
    use hermes_trading::research::synthesis::{PanelSummary, SynthesisReport};
    use hermes_trading::research::types::DataConfidence;
    use std::sync::{Mutex, MutexGuard};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_guard() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fresh_session() -> EquitySlashSession {
        analyze_stock_cache::clear_for_tests();
        EquitySlashSession::default()
    }

    fn tc(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "1".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
            extra_content: None,
        }
    }

    fn ok(content: &str) -> ToolResult {
        ToolResult {
            tool_call_id: "1".into(),
            content: content.into(),
            is_error: false,
        }
    }

    fn err(content: &str) -> ToolResult {
        ToolResult {
            tool_call_id: "1".into(),
            content: content.into(),
            is_error: true,
        }
    }

    use hermes_trading::research::report::content::{
        ExternalBlock, ExternalCoverage, ReportContent,
    };

    fn sample_result(symbol: &str) -> AnalyzeStockResult {
        AnalyzeStockResult {
            symbol: symbol.into(),
            depth: "medium".into(),
            dcf: serde_json::json!({}),
            comps: serde_json::json!({}),
            three_statement: serde_json::json!({}),
            lbo: serde_json::json!({}),
            scores: serde_json::json!({"fundamental_score": 5.6, "dimensions": {}}),
            personas: serde_json::json!({"panel_consensus": 77.5, "investors": [], "vote_distribution": {}, "signal_distribution": {}}),
            data_confidence: DataConfidence {
                score: 0.75,
                present: vec!["price".into()],
                missing: vec![],
            },
            missing_dims: vec![],
            dim_summary: vec![],
            used_fallback: vec![],
            summary_markdown: format!("## {symbol} · 深度分析"),
            synthesis: SynthesisReport {
                headline: format!("{symbol} · 偏多"),
                verdict: "buy".into(),
                confidence_tier: "high".into(),
                key_metrics: vec![],
                risks: vec![],
                missing_highlights: vec![],
                panel_summary: PanelSummary {
                    consensus: 77.5,
                    vote_buy: 30,
                    vote_avoid: 10,
                    investor_count: 66,
                },
                dcf_one_liner: "🔴 明显高估 · 安全边际 -46.1%".into(),
            },
            content: ReportContent {
                external: ExternalBlock {
                    coverage: ExternalCoverage::WebFilled,
                    ..Default::default()
                },
                ..Default::default()
            },
            raw_dims: serde_json::json!({}),
        }
    }

    fn sample_needs_web_fill(symbol: &str) -> AnalyzeStockResult {
        let mut r = sample_result(symbol);
        r.content.external.coverage = ExternalCoverage::NotRetrieved;
        r
    }

    fn sample_with_gaps(symbol: &str) -> AnalyzeStockResult {
        let mut r = sample_needs_web_fill(symbol);
        r.missing_dims = vec!["3_macro".into(), "10_valuation".into()];
        r
    }

    #[test]
    fn text_stall_inactive_after_analyze_even_if_low_confidence() {
        let _guard = test_guard();
        let mut session = fresh_session();
        session.analyze_done = true;
        session.symbol = Some("600522.SH".into());
        session.depth = Some("medium".into());
        let mut sample = sample_with_gaps("600522.SH");
        sample.data_confidence.score = 0.40;
        analyze_stock_cache::store("600522.SH", "medium", sample);
        assert!(
            handle_slash_text_stall(
                EquitySlashMode::AnalyzeStock,
                "[MODE: analyze-stock] Analyze: 600522",
                &mut session,
            )
            .is_none()
        );
    }

    #[test]
    fn text_stall_inactive_when_confidence_ok() {
        let _guard = test_guard();
        let mut session = fresh_session();
        session.analyze_done = true;
        session.symbol = Some("600522.SH".into());
        session.depth = Some("medium".into());
        analyze_stock_cache::store("600522.SH", "medium", sample_with_gaps("600522.SH"));
        assert!(
            handle_slash_text_stall(
                EquitySlashMode::AnalyzeStock,
                "[MODE: analyze-stock] Analyze: 600522",
                &mut session,
            )
            .is_none()
        );
    }

    #[test]
    fn detects_analyze_stock_from_skill_system_line() {
        let msg = "[SYSTEM: The user invoked /analyze-stock from skill \"equity-research\".";
        assert_eq!(detect_equity_slash_mode(msg), EquitySlashMode::AnalyzeStock);
    }

    #[test]
    fn text_stall_nudges_when_analyze_stock_skipped() {
        let _guard = test_guard();
        let mut session = fresh_session();
        let msg = "[MODE: analyze-stock / depth=medium] Analyze: 600521";
        let action = handle_slash_text_stall(EquitySlashMode::AnalyzeStock, msg, &mut session)
            .expect("first text-only reply should nudge");
        assert!(matches!(action, EquitySlashStallAction::ContinueAgent(_)));
        assert_eq!(session.text_stall_turns, 1);
    }

    #[test]
    fn text_stall_delivers_from_cache_when_tools_skipped_twice() {
        let _guard = test_guard();
        let mut session = fresh_session();
        session.text_stall_turns = 1;
        analyze_stock_cache::store("600521.SH", "medium", sample_result("600521.SH"));
        let msg = "[MODE: analyze-stock / depth=medium] Analyze: 600521";
        let action = handle_slash_text_stall(EquitySlashMode::AnalyzeStock, msg, &mut session)
            .expect("second stall should force deliver");
        let text = match action {
            EquitySlashStallAction::ForceDeliver(EquitySlashDeliveryOutcome::Deliver(t)) => t,
            other => panic!("expected ForceDeliver Deliver, got {other:?}"),
        };
        assert!(text.contains("MEDIA:"));
    }

    #[test]
    fn detects_analyze_stock_mode() {
        let _guard = test_guard();
        let msg = "[MODE: analyze-stock / depth=medium] Analyze: 600519";
        assert_eq!(detect_equity_slash_mode(msg), EquitySlashMode::AnalyzeStock);
    }

    #[test]
    fn quick_scan_returns_markdown_from_cache() {
        let _guard = test_guard();
        let mut session = fresh_session();
        analyze_stock_cache::store("688126.SH", "lite", {
            let mut r = sample_result("688126.SH");
            r.depth = "lite".into();
            r.summary_markdown = "## 688126 · 速判\n\nfoo".into();
            r
        });
        let msg = "[MODE: quick-scan / depth=lite] Analyze: 688126";
        let out = try_equity_slash_delivery(
            EquitySlashMode::QuickScan,
            msg,
            &[tc(
                "analyze_stock",
                r#"{"symbol":"688126.SH","depth":"lite"}"#,
            )],
            &[ok("truncated tool output ignored")],
            &mut session,
        );
        assert_eq!(
            out,
            EquitySlashDeliveryOutcome::Deliver("## 688126 · 速判\n\nfoo".into())
        );
    }

    #[test]
    fn analyze_stock_builds_brief_and_media_tag() {
        let _guard = test_guard();
        let mut session = fresh_session();
        analyze_stock_cache::store("600519.SH", "medium", sample_result("600519.SH"));
        let msg = "[MODE: analyze-stock / depth=medium] Analyze: 600519";
        let out = try_equity_slash_delivery(
            EquitySlashMode::AnalyzeStock,
            msg,
            &[tc(
                "analyze_stock",
                r#"{"symbol":"600519.SH","depth":"medium","use_providers":true}"#,
            )],
            &[ok("rtk-truncated string must not matter")],
            &mut session,
        );
        let text = match out {
            EquitySlashDeliveryOutcome::Deliver(text) => text,
            other => panic!("expected Deliver, got {other:?}"),
        };
        assert!(text.contains("摘要"));
        assert!(text.contains("MEDIA:"));
        assert!(text.contains("full-report-standalone.html"));
    }

    #[test]
    fn analyze_stock_delivers_immediately_with_gaps_in_cache() {
        let _guard = test_guard();
        let mut session = fresh_session();
        analyze_stock_cache::store("600522.SH", "medium", sample_with_gaps("600522.SH"));
        let msg = "[MODE: analyze-stock / depth=medium] Analyze: 600522";
        let out = try_equity_slash_delivery(
            EquitySlashMode::AnalyzeStock,
            msg,
            &[tc(
                "analyze_stock",
                r#"{"symbol":"600522.SH","depth":"medium","use_providers":true}"#,
            )],
            &[ok("truncated")],
            &mut session,
        );
        let text = match out {
            EquitySlashDeliveryOutcome::Deliver(text) => text,
            other => panic!("expected immediate Deliver, got {other:?}"),
        };
        assert!(text.contains("MEDIA:"));
    }

    #[test]
    fn analyze_stock_delivers_even_when_low_confidence() {
        let _guard = test_guard();
        let mut session = fresh_session();
        let mut sample = sample_with_gaps("600522.SH");
        sample.data_confidence.score = 0.40;
        analyze_stock_cache::store("600522.SH", "medium", sample);
        let msg = "[MODE: analyze-stock / depth=medium] Analyze: 600522";
        let out = try_equity_slash_delivery(
            EquitySlashMode::AnalyzeStock,
            msg,
            &[tc(
                "analyze_stock",
                r#"{"symbol":"600522.SH","depth":"medium","use_providers":true}"#,
            )],
            &[ok("truncated")],
            &mut session,
        );
        let text = match out {
            EquitySlashDeliveryOutcome::Deliver(text) => text,
            other => panic!("expected immediate Deliver, got {other:?}"),
        };
        assert!(text.contains("MEDIA:"));
        assert!(text.contains("置信度偏低"));
    }

    #[test]
    fn cache_miss_after_success_is_failed_not_pending() {
        let _guard = test_guard();
        let mut session = fresh_session();
        let out = try_equity_slash_delivery(
            EquitySlashMode::AnalyzeStock,
            "[MODE: analyze-stock] Analyze: 600522",
            &[tc(
                "analyze_stock",
                r#"{"symbol":"600522.SH","depth":"medium"}"#,
            )],
            &[ok("ok")],
            &mut session,
        );
        assert!(matches!(out, EquitySlashDeliveryOutcome::Failed(_)));
    }

    #[test]
    fn analyze_stock_error_is_failed() {
        let _guard = test_guard();
        let out = try_equity_slash_delivery(
            EquitySlashMode::AnalyzeStock,
            "[MODE: analyze-stock] Analyze: 600522",
            &[tc("analyze_stock", r#"{"symbol":"600522.SH"}"#)],
            &[err(r#"{"error":"fetch failed"}"#)],
            &mut fresh_session(),
        );
        assert!(matches!(out, EquitySlashDeliveryOutcome::Failed(_)));
    }

    #[test]
    fn pending_when_analyze_stock_not_called() {
        let _guard = test_guard();
        let out = try_equity_slash_delivery(
            EquitySlashMode::AnalyzeStock,
            "[MODE: analyze-stock] Analyze: 600522",
            &[tc("resolve_symbol", r#"{"query":"600522"}"#)],
            &[ok("resolved")],
            &mut fresh_session(),
        );
        assert_eq!(out, EquitySlashDeliveryOutcome::Pending);
    }

    #[test]
    fn md_only_skips_media_tag() {
        let _guard = test_guard();
        let mut session = fresh_session();
        analyze_stock_cache::store("600519.SH", "medium", sample_result("600519.SH"));
        let msg = "[MODE: analyze-stock / depth=medium] Analyze: 600519 md-only";
        let out = try_equity_slash_delivery(
            EquitySlashMode::AnalyzeStock,
            msg,
            &[tc("analyze_stock", r#"{"symbol":"600519.SH"}"#)],
            &[ok("")],
            &mut session,
        );
        let text = match out {
            EquitySlashDeliveryOutcome::Deliver(text) => text,
            other => panic!("expected Deliver, got {other:?}"),
        };
        assert!(!text.contains("MEDIA:"));
    }

    #[tokio::test]
    #[ignore = "live network — end-to-end slash delivery for 600528"]
    async fn live_slash_delivery_600528() {
        let _guard = test_guard();
        use std::path::Path;
        use std::time::{Duration, Instant};

        use crate::tools::trading_analyze_stock::AnalyzeStockHandler;
        use hermes_core::ToolHandler;
        use serde_json::json;

        const MAX_ELAPSED: Duration = Duration::from_secs(120);

        analyze_stock_cache::clear_for_tests();
        let started = Instant::now();
        let handler = AnalyzeStockHandler::new();
        handler
            .execute(json!({
                "symbol": "600528.SH",
                "depth": "medium",
                "use_providers": true
            }))
            .await
            .expect("analyze_stock live 600528");
        let analyze_ms = started.elapsed().as_millis();
        eprintln!("live 600528 analyze_stock: {analyze_ms}ms");

        let msg = "[MODE: analyze-stock / depth=medium] Analyze: 600528";
        let analyze = tc(
            "analyze_stock",
            r#"{"symbol":"600528.SH","depth":"medium","use_providers":true}"#,
        );
        let mut session = EquitySlashSession::default();
        let deliver_started = Instant::now();
        let out = try_equity_slash_delivery(
            EquitySlashMode::AnalyzeStock,
            msg,
            std::slice::from_ref(&analyze),
            &[ok("analyze_stock ok")],
            &mut session,
        );
        let deliver_ms = deliver_started.elapsed().as_millis();
        let total_ms = started.elapsed().as_millis();
        eprintln!("live 600528 slash deliver: {deliver_ms}ms total: {total_ms}ms");

        let text = match out {
            EquitySlashDeliveryOutcome::Deliver(text) => text,
            other => panic!("live 600528 delivery failed: {other:?}"),
        };
        assert!(text.contains("MEDIA:"), "must attach HTML report");
        assert!(text.chars().count() > 200, "brief must be substantive");

        let html_path = text
            .lines()
            .find_map(|line| line.strip_prefix("MEDIA:"))
            .expect("MEDIA path line");
        let html = std::fs::read_to_string(Path::new(html_path.trim()))
            .unwrap_or_else(|e| panic!("read HTML at {html_path}: {e}"));
        assert!(html.contains("01 / CORE"), "HTML missing 01 / CORE");
        assert!(
            html.contains("05 / DEEP SCAN"),
            "HTML missing 05 / DEEP SCAN"
        );
        assert!(
            html.contains("06 / VALUATION"),
            "HTML missing 06 / VALUATION"
        );
        assert!(html.contains("600528"), "HTML missing symbol");

        assert!(
            started.elapsed() < MAX_ELAPSED,
            "600528 slash E2E exceeded {:?}: {:?}",
            MAX_ELAPSED,
            started.elapsed()
        );
    }
}
