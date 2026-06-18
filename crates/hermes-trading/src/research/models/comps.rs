//! Comparable company analysis (UZI fin_models.build_comps_table).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompsTarget {
    pub name: Option<String>,
    pub ticker: Option<String>,
    pub price: Option<f64>,
    pub pe: Option<f64>,
    pub pb: Option<f64>,
    pub ps: Option<f64>,
    pub eps: Option<f64>,
    pub bvps: Option<f64>,
    pub roe: Option<f64>,
    pub net_margin: Option<f64>,
    pub revenue_growth: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompsPeer {
    pub name: Option<String>,
    pub ticker: Option<String>,
    pub pe: Option<f64>,
    pub pb: Option<f64>,
    pub ps: Option<f64>,
    pub ev_ebitda: Option<f64>,
    pub ev_sales: Option<f64>,
    pub roe: Option<f64>,
    pub net_margin: Option<f64>,
    pub revenue_growth: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricStats {
    pub min: f64,
    pub p25: f64,
    pub median: f64,
    pub p75: f64,
    pub max: f64,
    pub mean: f64,
    pub n: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum CompsResult {
    Ok(CompsOk),
    Error { error: String, target: CompsTarget },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompsOk {
    pub method: String,
    pub target: CompsTarget,
    pub peers: Vec<CompsPeer>,
    pub peer_stats: std::collections::BTreeMap<String, MetricStats>,
    pub target_percentile: std::collections::BTreeMap<String, f64>,
    pub implied_price: std::collections::BTreeMap<String, f64>,
    pub current_price: f64,
    pub valuation_verdict: String,
    pub methodology_log: Vec<String>,
}

/// Peer multiples benchmarking.
#[must_use]
pub fn build_comps_table(target: CompsTarget, peers: &[CompsPeer]) -> CompsResult {
    if peers.is_empty() {
        return CompsResult::Error {
            error: "no peers provided".into(),
            target,
        };
    }

    let metrics = [
        "pe",
        "pb",
        "ps",
        "ev_ebitda",
        "ev_sales",
        "roe",
        "net_margin",
        "revenue_growth",
    ];
    let mut stats = std::collections::BTreeMap::new();

    for m in metrics {
        let values: Vec<f64> = peers
            .iter()
            .filter_map(|p| peer_metric(p, m))
            .filter(|v| *v > 0.0)
            .collect();
        if values.is_empty() {
            continue;
        }
        stats.insert(m.to_string(), compute_stats(&values));
    }

    let mut target_pct = std::collections::BTreeMap::new();
    for m in stats.keys() {
        let tv = target_metric(&target, m).unwrap_or(0.0);
        if tv <= 0.0 {
            continue;
        }
        let mut values: Vec<f64> = peers
            .iter()
            .filter_map(|p| peer_metric(p, m))
            .filter(|v| *v > 0.0)
            .collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let rank = values.iter().filter(|v| **v < tv).count();
        let pct = if values.is_empty() {
            50.0
        } else {
            (rank as f64 / values.len() as f64 * 100.0).round()
        };
        target_pct.insert(m.clone(), pct);
    }

    let cur_px = target.price.unwrap_or(0.0);
    let mut implied = std::collections::BTreeMap::new();
    if let Some(pe_stats) = stats.get("pe")
        && let Some(eps) = target.eps
    {
        implied.insert("via_median_pe".into(), round2(pe_stats.median * eps));
    }
    if let Some(pb_stats) = stats.get("pb")
        && let Some(bvps) = target.bvps
    {
        implied.insert("via_median_pb".into(), round2(pb_stats.median * bvps));
    }

    let pe_pct = target_pct.get("pe").copied().unwrap_or(50.0);
    let val_verdict = if pe_pct <= 25.0 {
        "🟢 便宜（PE 低于 75% 同行）"
    } else if pe_pct <= 50.0 {
        "🟡 合理偏低"
    } else if pe_pct <= 75.0 {
        "⚪ 合理偏高"
    } else {
        "🔴 昂贵（PE 高于 75% 同行）"
    };

    let pe_median = stats.get("pe").map(|s| s.median);
    let methodology_log = vec![
        format!("Step 1 · 同行池 n={}", peers.len()),
        format!(
            "Step 2 · PE 中位数 {:?}，目标 PE {:?}",
            pe_median, target.pe
        ),
        format!("Step 3 · 目标 PE 分位 {pe_pct}%"),
        format!(
            "Step 4 · 隐含价 (中位 PE × EPS) = ¥{}",
            implied
                .get("via_median_pe")
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "-".into())
        ),
        format!("Step 5 · 结论: {val_verdict}"),
    ];

    CompsResult::Ok(CompsOk {
        method: "Comparable Company Analysis (peer multiples)".into(),
        target,
        peers: peers.to_vec(),
        peer_stats: stats,
        target_percentile: target_pct,
        implied_price: implied,
        current_price: cur_px,
        valuation_verdict: val_verdict.into(),
        methodology_log,
    })
}

fn peer_metric(p: &CompsPeer, m: &str) -> Option<f64> {
    match m {
        "pe" => p.pe,
        "pb" => p.pb,
        "ps" => p.ps,
        "ev_ebitda" => p.ev_ebitda,
        "ev_sales" => p.ev_sales,
        "roe" => p.roe,
        "net_margin" => p.net_margin,
        "revenue_growth" => p.revenue_growth,
        _ => None,
    }
}

fn target_metric(t: &CompsTarget, m: &str) -> Option<f64> {
    match m {
        "pe" => t.pe,
        "pb" => t.pb,
        "ps" => t.ps,
        "roe" => t.roe,
        "net_margin" => t.net_margin,
        "revenue_growth" => t.revenue_growth,
        _ => None,
    }
}

fn compute_stats(values: &[f64]) -> MetricStats {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let median = if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    };
    let (p25, p75) = if n > 1 {
        (quantile(&sorted, 0.25), quantile(&sorted, 0.75))
    } else {
        (sorted[0], sorted[0])
    };
    MetricStats {
        min: round2(sorted[0]),
        p25: round2(p25),
        median: round2(median),
        p75: round2(p75),
        max: round2(*sorted.last().unwrap_or(&0.0)),
        mean: round2(sorted.iter().sum::<f64>() / n as f64),
        n,
    }
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (q * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comps_empty_peers() {
        let t = CompsTarget {
            price: Some(18.5),
            pe: Some(35.0),
            ..Default::default()
        };
        match build_comps_table(t, &[]) {
            CompsResult::Error { error, .. } => assert_eq!(error, "no peers provided"),
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn comps_with_peers() {
        let target = CompsTarget {
            price: Some(18.5),
            pe: Some(35.0),
            pb: Some(2.8),
            eps: Some(0.53),
            bvps: Some(6.6),
            ..Default::default()
        };
        let peers = vec![
            CompsPeer {
                pe: Some(28.0),
                pb: Some(2.1),
                ps: Some(3.0),
                roe: Some(18.0),
                net_margin: Some(14.0),
                revenue_growth: Some(12.0),
                ..Default::default()
            },
            CompsPeer {
                pe: Some(32.0),
                pb: Some(2.5),
                ps: Some(3.5),
                roe: Some(16.0),
                net_margin: Some(12.0),
                revenue_growth: Some(10.0),
                ..Default::default()
            },
        ];
        let CompsResult::Ok(ok) = build_comps_table(target, &peers) else {
            panic!("expected ok");
        };
        assert!(ok.peer_stats.contains_key("pe"));
        assert!(ok.implied_price.contains_key("via_median_pe"));
    }
}
