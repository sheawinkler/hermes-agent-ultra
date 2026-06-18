//! Standalone HTML report from analyze_stock JSON.

use serde_json::Value;

use super::labels::{DIM_ORDER, dimension_display_name};

use super::svg::{render_svg_gauge, render_svg_percentile};

/// Render minimal HTML table report.
#[must_use]
pub fn render_html_report(analysis: &Value, narrative: Option<&str>) -> String {
    let symbol = analysis
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or("—");
    let confidence = analysis
        .get("data_confidence")
        .and_then(|c| c.get("score"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let fund_score = analysis
        .get("scores")
        .and_then(|s| s.get("fundamental_score"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let dcf = analysis.get("dcf");
    let intrinsic = dcf
        .and_then(|d| d.get("intrinsic_per_share"))
        .and_then(|v| v.as_f64())
        .map(|v| format!("¥{v:.2}"))
        .unwrap_or_else(|| "—".into());
    let verdict = dcf
        .and_then(|d| d.get("verdict"))
        .and_then(|v| v.as_str())
        .unwrap_or("—");
    let safety = dcf
        .and_then(|d| d.get("safety_margin_pct"))
        .and_then(|v| v.as_f64())
        .map(|v| format!("{v:+.1}%"))
        .unwrap_or_else(|| "—".into());
    let center = dcf
        .and_then(|d| d.get("sensitivity_table"))
        .and_then(|t| t.get("center_cell"))
        .and_then(|v| v.as_f64())
        .map(|v| format!("¥{v:.2}"))
        .unwrap_or_else(|| "—".into());
    let fallbacks = dcf
        .and_then(|d| d.get("used_fallback"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "无".into());
    let panel = analysis
        .get("personas")
        .and_then(|p| p.get("panel_consensus"))
        .and_then(|v| v.as_f64())
        .map(|v| format!("{v:.1}"))
        .unwrap_or_else(|| "—".into());
    let pe_pct = analysis
        .get("raw_dims")
        .and_then(|r| r.get("10_valuation"))
        .and_then(|v| v.get("data"))
        .and_then(|d| d.get("pe_percentile"))
        .and_then(|v| v.as_f64());

    let confidence_pct = confidence * 100.0;
    let conf_gauge = render_svg_gauge(confidence_pct, 100.0);
    let pe_gauge = pe_pct.map(render_svg_percentile).unwrap_or_default();

    let mut html = format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>{symbol} 研报</title>
<style>
body{{font-family:system-ui,sans-serif;margin:2rem;max-width:900px}}
table{{border-collapse:collapse;width:100%;margin:1rem 0}}
th,td{{border:1px solid #ccc;padding:8px;text-align:left}}
.gauges{{display:flex;gap:1.5rem;align-items:center;margin:1rem 0}}
.warn{{color:#b45309;font-size:0.9rem}}
</style></head>
<body>
<h1>{symbol} · 深度分析</h1>
<div class="gauges">{conf_gauge}{pe_gauge}</div>
<p>数据置信度: {confidence_pct:.0}% · 基本面评分: {fund_score:.1}/100</p>
<table>
<tr><th>指标</th><th>值</th></tr>
<tr><td>DCF 内在价值</td><td>{intrinsic}</td></tr>
<tr><td>DCF 安全边际</td><td>{safety}</td></tr>
<tr><td>敏感性中心格</td><td>{center}</td></tr>
<tr><td>DCF 结论</td><td>{verdict}</td></tr>
<tr><td>used_fallback</td><td class="warn">{fallbacks}</td></tr>
<tr><td>评委共识</td><td>{panel}</td></tr>
</table>
"#
    );

    if let Some(dims) = analysis
        .get("scores")
        .and_then(|s| s.get("dimensions"))
        .and_then(|v| v.as_object())
    {
        html.push_str("<h2>19 维评分</h2><table><tr><th>维度</th><th>分数</th><th>说明</th></tr>");
        for key in DIM_ORDER {
            let Some(v) = dims.get(*key) else {
                continue;
            };
            let score = v.get("score").and_then(|s| s.as_u64()).unwrap_or(0);
            let label = v
                .get("label")
                .and_then(|s| s.as_str())
                .or_else(|| v.get("display_name").and_then(|s| s.as_str()))
                .unwrap_or("—");
            let name = v
                .get("display_name")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| dimension_display_name(key));
            html.push_str(&format!(
                "<tr><td>{name}</td><td>{score}/10</td><td>{label}</td></tr>"
            ));
        }
        html.push_str("</table>");
    }

    if let Some(personas) = analysis.get("personas") {
        if let Some(vd) = personas.get("vote_distribution") {
            html.push_str("<h2>66 位评委投票</h2><table><tr><th>类别</th><th>人数</th></tr>");
            for (k, label) in [
                ("strongly_buy", "强烈买入"),
                ("buy", "买入"),
                ("watch", "关注"),
                ("wait", "观望"),
                ("avoid", "回避"),
                ("skip", "跳过"),
            ] {
                let n = vd.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                html.push_str(&format!("<tr><td>{label}</td><td>{n}</td></tr>"));
            }
            html.push_str("</table>");
        }
        if let Some(investors) = personas.get("investors").and_then(|v| v.as_array()) {
            html.push_str(
                "<h2>评委明细</h2><table><tr><th>评委</th><th>结论</th><th>分数</th></tr>",
            );
            for inv in investors {
                let id = inv.get("id").and_then(|v| v.as_str()).unwrap_or("—");
                let vote = inv.get("vote").and_then(|v| v.as_str()).unwrap_or("—");
                let score = inv
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .map(|s| format!("{s:.0}"))
                    .unwrap_or_else(|| "—".into());
                html.push_str(&format!(
                    "<tr><td>{id}</td><td>{vote}</td><td>{score}</td></tr>"
                ));
            }
            html.push_str("</table>");
        }
    }

    if let Some(text) = narrative {
        html.push_str(&format!("<h2>分析结论</h2><p>{text}</p>"));
    }

    html.push_str("</body></html>");
    html
}
