//! Dimension scoring ported from UZI score_fns.py (all 19 dims).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::research::report::labels::dimension_display_name;
use crate::research::types::FeatureVector;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DimScore {
    pub score: u8,
    pub weight: u8,
    #[serde(default)]
    pub display_name: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons_pass: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons_fail: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreDimensionsResult {
    pub ticker: String,
    pub fundamental_score: f64,
    pub dimensions: std::collections::BTreeMap<String, DimScore>,
}

/// Score all 19 fundamental dimensions from raw dimension data + features.
#[must_use]
pub fn score_dimensions(
    ticker: &str,
    raw_dims: &Value,
    features: &FeatureVector,
) -> ScoreDimensionsResult {
    let get = |key: &str| -> Value {
        raw_dims
            .get(key)
            .and_then(|v| v.get("data"))
            .cloned()
            .unwrap_or(Value::Null)
    };

    let mut out = std::collections::BTreeMap::new();

    // 1 · financials
    let fin = get("1_financials");
    let roe = f64_val(&fin, "roe").or(features.roe_latest).unwrap_or(0.0);
    let last_roe = fin
        .get("roe_history")
        .and_then(|h| h.as_array())
        .and_then(|a| a.last())
        .and_then(|v| v.as_f64())
        .unwrap_or(roe);
    let net_margin = f64_val(&fin, "net_margin")
        .or(features.net_margin)
        .unwrap_or(0.0);
    let debt = f64_val(
        fin.get("financial_health").unwrap_or(&Value::Null),
        "debt_ratio",
    )
    .or(features.debt_ratio)
    .unwrap_or(0.0);
    let rev_hist = fin.get("revenue_history").and_then(|v| v.as_array());
    let growth = rev_hist
        .and_then(|h| {
            if h.len() >= 2 {
                let prev = h[h.len() - 2].as_f64()?;
                let last = h[h.len() - 1].as_f64()?;
                if prev != 0.0 {
                    Some((last - prev) / prev * 100.0)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .or(features.revenue_growth_latest)
        .unwrap_or(0.0);

    let mut score_1: i32 = 5;
    let mut missing_1 = Vec::new();
    if features.revenue_latest_yi.is_none() && rev_hist.is_none() {
        missing_1.push("revenue".into());
    }
    if last_roe >= 15.0 {
        score_1 += 2;
    } else if last_roe >= 10.0 {
        score_1 += 1;
    } else if last_roe < 5.0 {
        score_1 -= 2;
    }
    if net_margin >= 15.0 {
        score_1 += 1;
    }
    if growth >= 20.0 {
        score_1 += 1;
    }
    if debt >= 60.0 {
        score_1 -= 1;
    }
    score_1 = score_1.clamp(1, 10);
    out.insert(
        "1_financials".into(),
        DimScore {
            score: score_1 as u8,
            weight: 5,
            display_name: String::new(),
            label: format!("ROE {last_roe:.1}% · 营收增速 {growth:+.1}% · 负债率 {debt:.0}%"),
            missing: missing_1,
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );

    // 2 · kline (momentum)
    let kline = get("2_kline");
    let stage = kline
        .get("stage")
        .and_then(|v| v.as_str())
        .or(features.stage.as_deref())
        .unwrap_or("")
        .to_string();
    let ma_align = kline
        .get("ma_align")
        .and_then(|v| v.as_str())
        .or(features.ma_align.as_deref())
        .unwrap_or("")
        .to_string();
    let dd = f64_val(
        kline.get("kline_stats").unwrap_or(&Value::Null),
        "max_drawdown",
    )
    .or(features.max_drawdown_1y)
    .unwrap_or(0.0);
    let mut score_2: i32 = 5;
    if stage.contains("Stage 2") {
        score_2 += 2;
    } else if stage.contains("Stage 1") {
        score_2 += 1;
    } else if stage.contains("Stage 3") || stage.contains("Stage 4") {
        score_2 -= 2;
    }
    if ma_align.contains("多头") {
        score_2 += 1;
    }
    if dd <= -30.0 {
        score_2 -= 1;
    }
    score_2 = score_2.clamp(1, 10);
    out.insert(
        "2_kline".into(),
        DimScore {
            score: score_2 as u8,
            weight: 4,
            display_name: String::new(),
            label: format!("{stage} · 均线{ma_align}"),
            missing: if stage.is_empty() {
                vec!["stage".into()]
            } else {
                vec![]
            },
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );

    // 3-9 stubs / light logic
    out.insert("3_macro".into(), neutral_dim(6, 3, "宏观环境中性"));
    out.insert(
        "4_peers".into(),
        DimScore {
            score: if get("4_peers")
                .get("peer_table")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty())
            {
                7
            } else {
                5
            },
            weight: 4,
            display_name: String::new(),
            label: "同行对比".into(),
            missing: vec![],
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );
    out.insert("5_chain".into(), neutral_dim(6, 4, "产业链"));
    let research = get("6_research");
    let research_count = research
        .get("research_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut score_6: i32 = 5;
    if research_count >= 8 {
        score_6 += 2;
    } else if research_count >= 3 {
        score_6 += 1;
    } else if research_count == 0 {
        score_6 -= 1;
    }
    out.insert(
        "6_research".into(),
        DimScore {
            score: score_6.clamp(1, 10) as u8,
            weight: 3,
            display_name: String::new(),
            label: format!("券商研报 {research_count} 篇"),
            missing: if research_count == 0 {
                vec!["research_reports".into()]
            } else {
                vec![]
            },
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );
    let industry_dim = get("7_industry");
    let ind_growth = f64_val(&industry_dim, "growth").unwrap_or(0.0);
    let ind_pe = f64_val(&industry_dim, "industry_pe")
        .or(features.industry_pe)
        .unwrap_or(0.0);
    let ind_name = industry_dim
        .get("industry")
        .and_then(|v| v.as_str())
        .or(features.industry.as_deref())
        .unwrap_or("—");
    let mut score_7: i32 = 5;
    if ind_growth >= 15.0 {
        score_7 += 2;
    } else if ind_growth >= 5.0 {
        score_7 += 1;
    } else if ind_growth < 0.0 {
        score_7 -= 2;
    }
    if ind_pe > 0.0 && features.pe.is_some_and(|pe| pe < ind_pe) {
        score_7 += 1;
    }
    out.insert(
        "7_industry".into(),
        DimScore {
            score: score_7.clamp(1, 10) as u8,
            weight: 4,
            display_name: String::new(),
            label: format!("{ind_name} · 增速 {ind_growth:+.1}% · 行业PE {ind_pe:.1}"),
            missing: if ind_name == "—" {
                vec!["industry".into()]
            } else {
                vec![]
            },
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );
    out.insert("8_materials".into(), neutral_dim(6, 3, "原材料成本关注中"));
    out.insert("9_futures".into(), neutral_dim(5, 2, "无强关联期货品种"));

    // 10 · valuation
    let val = get("10_valuation");
    let pe_q = val
        .get("pe_percentile")
        .and_then(|v| v.as_f64())
        .map(|v| v.round() as i32)
        .or_else(|| {
            val.get("pe_quantile")
                .and_then(|v| v.as_str())
                .and_then(parse_pe_quantile)
        })
        .or(features.pe_quantile_5y.map(|v| v as i32))
        .unwrap_or(50);
    let score_10 = if pe_q < 30 {
        9
    } else if pe_q < 50 {
        7
    } else if pe_q < 70 {
        5
    } else if pe_q < 85 {
        3
    } else {
        2
    };
    out.insert(
        "10_valuation".into(),
        DimScore {
            score: score_10,
            weight: 5,
            display_name: String::new(),
            label: format!(
                "PE {} · 5 年 {pe_q} 分位",
                val.get("pe_ttm")
                    .or_else(|| val.get("pe"))
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into())
            ),
            missing: if features.pe.is_none() {
                vec!["pe".into()]
            } else {
                vec![]
            },
            reasons_pass: if pe_q < 50 {
                vec!["PE 在 5 年中位数以下".into()]
            } else {
                vec![]
            },
            reasons_fail: if pe_q >= 75 {
                vec!["PE 已在 5 年高位区".into()]
            } else {
                vec![]
            },
        },
    );

    // 11-19
    out.insert("11_governance".into(), neutral_dim(6, 3, "治理结构"));
    let cf = get("12_capital_flow");
    let main_5d = f64_val(&cf, "main_fund_5d_net_yi").unwrap_or(0.0);
    let fh = get("6_fund_holders");
    let holder_chg = f64_val(&fh, "holder_change_ratio").unwrap_or(0.0);
    let mut score_12: i32 = 5;
    if main_5d > 0.0 {
        score_12 += 2;
    } else if main_5d < 0.0 {
        score_12 -= 1;
    }
    if holder_chg < -5.0 {
        score_12 += 1;
    } else if holder_chg > 10.0 {
        score_12 -= 1;
    }
    out.insert(
        "12_capital_flow".into(),
        DimScore {
            score: score_12.clamp(1, 10) as u8,
            weight: 4,
            display_name: String::new(),
            label: format!("主力 5日 {main_5d:.2} 亿 · 户数变化 {holder_chg:+.1}%"),
            missing: if main_5d == 0.0 && holder_chg == 0.0 {
                vec!["capital_flow".into()]
            } else {
                vec![]
            },
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );
    out.insert("13_policy".into(), neutral_dim(6, 3, "政策环境中性"));
    out.insert("14_moat".into(), neutral_dim(6, 3, "护城河需定性评估"));
    out.insert(
        "15_events".into(),
        DimScore {
            score: 6,
            weight: 4,
            display_name: String::new(),
            label: "事件驱动".into(),
            missing: vec![],
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );
    let lhb = get("16_lhb");
    let lhb_count = lhb
        .get("lhb_count_30d")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as i32;
    out.insert(
        "16_lhb".into(),
        DimScore {
            score: (5 + (lhb_count / 2).min(3)).min(10) as u8,
            weight: 4,
            display_name: String::new(),
            label: format!("近 30 天上榜 {lhb_count} 次"),
            missing: vec![],
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );
    out.insert("17_sentiment".into(), neutral_dim(6, 3, "舆情"));
    let events = get("15_events");
    let trap_keywords = ["涨停", "牛股", "翻倍", "龙头", "妖股"];
    let mut trap_score: i32 = 9;
    let mut trap_label = "🟢 未发现推广痕迹".to_string();
    if let Some(items) = events.get("news").and_then(|v| v.as_array()) {
        for item in items {
            if let Some(title) = item.get("title").and_then(|v| v.as_str())
                && trap_keywords.iter().any(|k| title.contains(k))
            {
                trap_score = 3;
                trap_label = "🔴 舆情含推广措辞".into();
                break;
            }
        }
    }
    out.insert(
        "18_trap".into(),
        DimScore {
            score: trap_score.clamp(1, 10) as u8,
            weight: 5,
            display_name: String::new(),
            label: trap_label,
            missing: if events.is_null() {
                vec!["events".into()]
            } else {
                vec![]
            },
            reasons_pass: vec![],
            reasons_fail: vec![],
        },
    );
    out.insert("19_contests".into(), neutral_dim(5, 4, "实盘比赛"));

    let dim_keys: Vec<String> = out.keys().cloned().collect();
    for key in dim_keys {
        if let Some(d) = out.get_mut(&key) {
            d.display_name = dimension_display_name(&key);
        }
    }

    let total_weighted: f64 = out.values().map(|d| f64::from(d.score * d.weight)).sum();
    let total_weight: f64 = out.values().map(|d| f64::from(d.weight)).sum();
    let fundamental = if total_weight > 0.0 {
        (total_weighted / total_weight * 10.0 * 10.0).round() / 100.0
    } else {
        0.0
    };

    ScoreDimensionsResult {
        ticker: ticker.to_string(),
        fundamental_score: fundamental,
        dimensions: out,
    }
}

fn neutral_dim(score: u8, weight: u8, label: &str) -> DimScore {
    DimScore {
        score,
        weight,
        display_name: String::new(),
        label: label.into(),
        missing: vec![],
        reasons_pass: vec![],
        reasons_fail: vec![],
    }
}

fn f64_val(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_f64())
}

fn parse_pe_quantile(s: &str) -> Option<i32> {
    s.chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}
