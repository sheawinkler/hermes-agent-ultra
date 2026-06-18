//! Chinese display names and stable ordering for 19 scoring dimensions.

/// Canonical dimension key order (1 → 19).
pub const DIM_ORDER: &[&str] = &[
    "1_financials",
    "2_kline",
    "3_macro",
    "4_peers",
    "5_chain",
    "6_research",
    "7_industry",
    "8_materials",
    "9_futures",
    "10_valuation",
    "11_governance",
    "12_capital_flow",
    "13_policy",
    "14_moat",
    "15_events",
    "16_lhb",
    "17_sentiment",
    "18_trap",
    "19_contests",
];

#[must_use]
pub fn dimension_display_name(key: &str) -> String {
    match key {
        "1_financials" => "财务面".into(),
        "2_kline" => "技术面 (K线)".into(),
        "3_macro" => "宏观环境".into(),
        "4_peers" => "同行对比".into(),
        "5_chain" => "产业链".into(),
        "6_research" => "券商研报".into(),
        "7_industry" => "行业景气".into(),
        "8_materials" => "原材料成本".into(),
        "9_futures" => "期货关联".into(),
        "10_valuation" => "估值 (PE/PB)".into(),
        "11_governance" => "治理结构".into(),
        "12_capital_flow" => "资金流向".into(),
        "13_policy" => "政策环境".into(),
        "14_moat" => "护城河".into(),
        "15_events" => "事件驱动".into(),
        "16_lhb" => "龙虎榜".into(),
        "17_sentiment" => "舆情".into(),
        "18_trap" => "推广陷阱".into(),
        "19_contests" => "实盘比赛".into(),
        _ => key.to_string(),
    }
}
