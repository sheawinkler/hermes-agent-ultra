//! Deterministic Markdown summary (19 dims + 66-investor panel).

use crate::research::personas::investors::find_investor;
use crate::research::scoring::{PanelResult, ScoreDimensionsResult};
use crate::research::types::DataConfidence;

use super::labels::{DIM_ORDER, dimension_display_name};

fn score_badge(score: u8) -> &'static str {
    if score >= 7 {
        " 🟢"
    } else if score <= 4 {
        " 🔴"
    } else {
        ""
    }
}

/// Full 19-dimension table + 66-investor panel (paste into chat as-is).
#[must_use]
pub fn render_summary_markdown(
    symbol: &str,
    scored: &ScoreDimensionsResult,
    panel: &PanelResult,
    confidence: &DataConfidence,
    dcf_verdict: Option<&str>,
) -> String {
    let mut out = format!(
        "## {symbol} · 深度分析\n\n\
         数据置信度: {:.0}% · 基本面综合: {:.1}/100\n",
        confidence.score * 100.0,
        scored.fundamental_score
    );
    if let Some(v) = dcf_verdict {
        out.push_str(&format!("DCF 结论: {v}\n"));
    }
    out.push_str(&format!(
        "评委共识: {:.1}/10（{} 位投资人格评委）\n\n",
        panel.panel_consensus,
        panel.investors.len()
    ));

    out.push_str("### 19 维评分概览\n\n");
    out.push_str("| 维度 | 评分 | 说明 |\n| --- | --- | --- |\n");
    for key in DIM_ORDER {
        let Some(d) = scored.dimensions.get(*key) else {
            continue;
        };
        let name = if d.display_name.is_empty() {
            dimension_display_name(key)
        } else {
            d.display_name.clone()
        };
        let badge = score_badge(d.score);
        out.push_str(&format!(
            "| {name} | {}/{}{} | {} |\n",
            d.score, 10, badge, d.label
        ));
    }

    let vd = &panel.vote_distribution;
    let sd = &panel.signal_distribution;
    out.push_str("\n### 66 位评委投票分布\n\n");
    out.push_str("| 类别 | 人数 |\n| --- | --- |\n");
    out.push_str(&format!(
        "| 强烈买入 | {} |\n| 买入 | {} |\n| 关注/观望 | {} |\n| 回避 | {} |\n| 不适合/跳过 | {} |\n",
        vd.strongly_buy, vd.buy, vd.watch + vd.wait, vd.avoid, vd.skip + vd.n_a
    ));
    out.push_str(&format!(
        "| 看多 | {} | 中性 | {} | 看空 | {} | 跳过 | {} |\n",
        sd.bullish, sd.neutral, sd.bearish, sd.skip
    ));

    out.push_str("\n### 评委明细\n\n");
    out.push_str("| 评委 | 派系 | 结论 | 分数 | 引用规则 |\n| --- | --- | --- | --- | --- |\n");
    for vote in &panel.investors {
        let name = find_investor(&vote.id).map(|m| m.name).unwrap_or(&vote.id);
        let group = find_investor(&vote.id)
            .map(|m| m.group.to_string())
            .unwrap_or_else(|| "—".into());
        let score_cell = if vote.signal == "skip" {
            "—".into()
        } else {
            format!("{:.0}", vote.score)
        };
        let rule = vote
            .cited_rule
            .as_deref()
            .or(vote.skip_reason.as_deref())
            .unwrap_or("—");
        out.push_str(&format!(
            "| {name} | {group} | {} | {score_cell} | {rule} |\n",
            vote.vote
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::scoring::{generate_panel, score_dimensions};
    use crate::research::types::{DataConfidence, FundamentalsSnapshot};
    use serde_json::json;

    #[test]
    fn summary_has_19_dim_rows_and_panel_header() {
        let raw = json!({
            "10_valuation": { "data": { "pe_percentile": 50, "pe_ttm": 22.0 } },
            "2_kline": { "data": { "stage": "Stage 4", "ma_align": "空头" } },
            "6_research": { "data": { "research_count": 12 } },
            "12_capital_flow": { "data": { "main_fund_5d_net_yi": 9.14 } },
        });
        let snap = FundamentalsSnapshot {
            symbol: "300750.SZ".into(),
            pe: Some(22.94),
            ..Default::default()
        };
        let features = snap.clone();
        let scored = score_dimensions("300750.SZ", &raw, &features);
        assert_eq!(scored.dimensions.len(), 19);
        let panel = generate_panel(&scored, &features);
        assert_eq!(panel.investors.len(), 66);
        let md = render_summary_markdown(
            "300750.SZ",
            &scored,
            &panel,
            &DataConfidence::from_snapshot(&snap),
            Some("持有"),
        );
        assert!(md.contains("### 19 维评分概览"));
        assert!(md.contains("| 财务面 |"));
        assert!(md.contains("| 推广陷阱 |"));
        assert!(md.contains("66 位投资人格评委"));
        assert!(md.contains("| 沃伦·巴菲特 |"));
        let dim_rows = md
            .lines()
            .filter(|l| l.starts_with("| ") && !l.contains("维度") && !l.contains("---"))
            .filter(|l| l.contains("/10"))
            .count();
        assert_eq!(dim_rows, 19, "expected 19 dimension rows in markdown");
    }
}
