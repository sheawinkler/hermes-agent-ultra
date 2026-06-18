//! Standalone HTML report from analyze_stock JSON.

use serde_json::Value;

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
    let intrinsic = analysis
        .get("dcf")
        .and_then(|d| d.get("intrinsic_per_share"))
        .and_then(|v| v.as_f64())
        .map(|v| format!("¥{v:.2}"))
        .unwrap_or_else(|| "—".into());
    let verdict = analysis
        .get("dcf")
        .and_then(|d| d.get("verdict"))
        .and_then(|v| v.as_str())
        .unwrap_or("—");
    let panel = analysis
        .get("personas")
        .and_then(|p| p.get("panel_consensus"))
        .and_then(|v| v.as_f64())
        .map(|v| format!("{v:.1}"))
        .unwrap_or_else(|| "—".into());

    let confidence_pct = confidence * 100.0;
    let mut html = format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>{symbol} 研报</title>
<style>
body{{font-family:system-ui,sans-serif;margin:2rem;max-width:900px}}
table{{border-collapse:collapse;width:100%}}
th,td{{border:1px solid #ccc;padding:8px;text-align:left}}
.bar{{background:#4a90d9;height:12px;border-radius:2px}}
</style></head>
<body>
<h1>{symbol} · 深度分析</h1>
<p>数据置信度: {confidence_pct:.0}% · 基本面评分: {fund_score:.1}/100</p>
<table>
<tr><th>指标</th><th>值</th></tr>
<tr><td>DCF 内在价值</td><td>{intrinsic}</td></tr>
<tr><td>DCF 结论</td><td>{verdict}</td></tr>
<tr><td>评委共识</td><td>{panel}</td></tr>
</table>
"#
    );

    if let Some(dims) = analysis
        .get("scores")
        .and_then(|s| s.get("dimensions"))
        .and_then(|v| v.as_object())
    {
        html.push_str("<h2>维度评分</h2><table><tr><th>维度</th><th>分数</th></tr>");
        for (k, v) in dims {
            let score = v.get("score").and_then(|s| s.as_u64()).unwrap_or(0);
            html.push_str(&format!("<tr><td>{k}</td><td>{score}/10</td></tr>"));
        }
        html.push_str("</table>");
    }

    if let Some(text) = narrative {
        html.push_str(&format!("<h2>分析结论</h2><p>{text}</p>"));
    }

    html.push_str("</body></html>");
    html
}
