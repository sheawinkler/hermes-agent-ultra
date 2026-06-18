//! Declarative investor rules (ported from UZI investor_criteria.py).

use crate::research::types::FeatureVector;

#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub rule_id: &'static str,
    pub name: &'static str,
    pub weight: u8,
    pub check: fn(&FeatureVector) -> bool,
    pub pass_msg: &'static str,
    pub fail_msg: &'static str,
}

pub fn rules_for(investor_id: &str) -> &'static [Rule] {
    match investor_id {
        "buffett" => &BUFFETT_RULES,
        "graham" => &GRAHAM_RULES,
        "fisher" => &FISHER_RULES,
        "munger" => &MUNGER_RULES,
        "templeton" => &TEMPLETON_RULES,
        "klarman" => &KLARMAN_RULES,
        "lynch" => &LYNCH_RULES,
        "soros" => &SOROS_RULES,
        "livermore" => &LIVERMORE_RULES,
        "duan" => &DUAN_RULES,
        "zhang_mz" => &YOUZI_RULES,
        "simons" => &SIMONS_RULES,
        "serenity" => &SERENITY_RULES,
        id if id.starts_with("zhang_")
            || id.starts_with("zhao_")
            || id.starts_with("fs_")
            || id == "yangjia"
            || id == "ghzw" =>
        {
            &YOUZI_RULES
        }
        _ => &GENERIC_VALUE_RULES,
    }
}

fn roe_5y_15(f: &FeatureVector) -> bool {
    f.roe_5y_above_15.unwrap_or(0.0) >= 4.0 && f.roe_5y_min.unwrap_or(0.0) > 12.0
}
fn net_margin_15(f: &FeatureVector) -> bool {
    f.net_margin.unwrap_or(0.0) > 15.0
}
fn debt_ratio_50(f: &FeatureVector) -> bool {
    let d = f.debt_ratio.unwrap_or(100.0);
    d > 0.0 && d < 50.0
}
fn fcf_positive(f: &FeatureVector) -> bool {
    f.fcf_positive.unwrap_or(false)
}
fn moat_clear(f: &FeatureVector) -> bool {
    f.moat_total.unwrap_or(0.0) >= 24.0
}
fn pe_quantile_low(f: &FeatureVector) -> bool {
    f.pe_quantile_5y.unwrap_or(100.0) < 50.0
}
fn pe_under_15(f: &FeatureVector) -> bool {
    let pe = f.pe.unwrap_or(100.0);
    pe > 0.0 && pe < 15.0
}
fn pb_under_1_5(f: &FeatureVector) -> bool {
    let pb = f.pb.unwrap_or(100.0);
    pb > 0.0 && pb < 1.5
}
fn peg_under_1(f: &FeatureVector) -> bool {
    let pe = f.pe.unwrap_or(100.0);
    let g = f.revenue_growth_latest.unwrap_or(0.0);
    g > 0.0 && pe / g < 1.0
}
fn stage_2(f: &FeatureVector) -> bool {
    f.stage.as_deref().is_some_and(|s| s.contains("Stage 2"))
}
fn ma_bull(f: &FeatureVector) -> bool {
    f.ma_align.as_deref().is_some_and(|s| s.contains("多头"))
}
fn roe_above_20(f: &FeatureVector) -> bool {
    f.roe_latest.unwrap_or(0.0) >= 20.0
}
fn pe_reasonable(f: &FeatureVector) -> bool {
    let pe = f.pe.unwrap_or(100.0);
    pe > 0.0 && pe < 40.0
}
fn youzi_momentum(f: &FeatureVector) -> bool {
    f.stage
        .as_deref()
        .is_some_and(|s| s.contains("Stage 2") || s.contains("Stage 1"))
        || f.change_pct.unwrap_or(0.0) > 5.0
}
fn quant_factor_ok(f: &FeatureVector) -> bool {
    f.roe_latest.unwrap_or(0.0) > 10.0 && f.pe.unwrap_or(100.0) > 0.0
}

const BUFFETT_RULES: [Rule; 7] = [
    Rule {
        rule_id: "roe_5y_15",
        name: "ROE 连续 5 年 > 15%",
        weight: 5,
        check: roe_5y_15,
        pass_msg: "ROE 连续达标",
        fail_msg: "ROE 未持续 15%",
    },
    Rule {
        rule_id: "net_margin_15",
        name: "净利率 > 15%",
        weight: 3,
        check: net_margin_15,
        pass_msg: "净利率高质量",
        fail_msg: "净利率偏低",
    },
    Rule {
        rule_id: "debt_ratio_50",
        name: "资产负债率 < 50%",
        weight: 3,
        check: debt_ratio_50,
        pass_msg: "负债保守",
        fail_msg: "负债偏高",
    },
    Rule {
        rule_id: "fcf_positive",
        name: "自由现金流为正",
        weight: 4,
        check: fcf_positive,
        pass_msg: "FCF 健康",
        fail_msg: "FCF 不达标",
    },
    Rule {
        rule_id: "moat_clear",
        name: "护城河清晰",
        weight: 4,
        check: moat_clear,
        pass_msg: "护城河可见",
        fail_msg: "护城河不够",
    },
    Rule {
        rule_id: "safety_margin_pe",
        name: "PE 5 年分位低",
        weight: 3,
        check: pe_quantile_low,
        pass_msg: "PE 有安全边际",
        fail_msg: "PE 偏高",
    },
    Rule {
        rule_id: "dividend_history",
        name: "连续分红",
        weight: 2,
        check: |f| f.consecutive_dividend_years.unwrap_or(0.0) >= 5.0,
        pass_msg: "连续分红",
        fail_msg: "分红记录短",
    },
];

const GRAHAM_RULES: [Rule; 4] = [
    Rule {
        rule_id: "pe_under_15",
        name: "PE < 15",
        weight: 3,
        check: pe_under_15,
        pass_msg: "PE 达标",
        fail_msg: "PE 高于 15",
    },
    Rule {
        rule_id: "pb_under_1_5",
        name: "PB < 1.5",
        weight: 3,
        check: pb_under_1_5,
        pass_msg: "PB 达标",
        fail_msg: "PB 偏高",
    },
    Rule {
        rule_id: "pe_pb_22_5",
        name: "PE×PB < 22.5",
        weight: 3,
        check: |f| f.pe_x_pb.unwrap_or(100.0) > 0.0 && f.pe_x_pb.unwrap_or(100.0) < 22.5,
        pass_msg: "格雷厄姆 22.5 达标",
        fail_msg: "PE×PB 超红线",
    },
    Rule {
        rule_id: "current_ratio_2",
        name: "流动比率 > 2",
        weight: 2,
        check: |f| f.current_ratio.unwrap_or(0.0) > 2.0,
        pass_msg: "流动性好",
        fail_msg: "流动性不足",
    },
];

const FISHER_RULES: [Rule; 3] = [
    Rule {
        rule_id: "rev_growth",
        name: "营收增长",
        weight: 4,
        check: |f| f.revenue_growth_latest.unwrap_or(0.0) >= 15.0,
        pass_msg: "营收高增长",
        fail_msg: "增长不足",
    },
    Rule {
        rule_id: "roe_quality",
        name: "ROE 质量",
        weight: 4,
        check: roe_above_20,
        pass_msg: "ROE 优秀",
        fail_msg: "ROE 一般",
    },
    Rule {
        rule_id: "margin_stable",
        name: "净利率稳定",
        weight: 3,
        check: net_margin_15,
        pass_msg: "利润率好",
        fail_msg: "利润率弱",
    },
];

const MUNGER_RULES: [Rule; 3] = [
    Rule {
        rule_id: "moat",
        name: "护城河",
        weight: 5,
        check: moat_clear,
        pass_msg: "护城河强",
        fail_msg: "护城河弱",
    },
    Rule {
        rule_id: "roe",
        name: "ROE",
        weight: 4,
        check: roe_5y_15,
        pass_msg: "ROE 持续",
        fail_msg: "ROE 不稳",
    },
    Rule {
        rule_id: "debt",
        name: "低负债",
        weight: 3,
        check: debt_ratio_50,
        pass_msg: "负债低",
        fail_msg: "负债高",
    },
];

const TEMPLETON_RULES: [Rule; 2] = [
    Rule {
        rule_id: "pe_low",
        name: "低 PE",
        weight: 4,
        check: pe_under_15,
        pass_msg: "逆向机会",
        fail_msg: "不够便宜",
    },
    Rule {
        rule_id: "pb_low",
        name: "低 PB",
        weight: 3,
        check: pb_under_1_5,
        pass_msg: "PB 低",
        fail_msg: "PB 高",
    },
];

const KLARMAN_RULES: [Rule; 3] = [
    Rule {
        rule_id: "margin_safety",
        name: "安全边际",
        weight: 5,
        check: pe_quantile_low,
        pass_msg: "有安全边际",
        fail_msg: "缺乏安全边际",
    },
    Rule {
        rule_id: "fcf",
        name: "FCF",
        weight: 4,
        check: fcf_positive,
        pass_msg: "FCF 正",
        fail_msg: "FCF 负",
    },
    Rule {
        rule_id: "debt",
        name: "低杠杆",
        weight: 3,
        check: debt_ratio_50,
        pass_msg: "杠杆低",
        fail_msg: "杠杆高",
    },
];

const LYNCH_RULES: [Rule; 3] = [
    Rule {
        rule_id: "peg",
        name: "PEG < 1",
        weight: 5,
        check: peg_under_1,
        pass_msg: "PEG 合理",
        fail_msg: "PEG 过高",
    },
    Rule {
        rule_id: "growth",
        name: "盈利增长",
        weight: 4,
        check: |f| f.revenue_growth_latest.unwrap_or(0.0) >= 20.0,
        pass_msg: "高增长",
        fail_msg: "增长慢",
    },
    Rule {
        rule_id: "pe",
        name: "PE 合理",
        weight: 3,
        check: pe_reasonable,
        pass_msg: "PE OK",
        fail_msg: "PE 过高",
    },
];

const SOROS_RULES: [Rule; 2] = [
    Rule {
        rule_id: "trend",
        name: "趋势",
        weight: 4,
        check: stage_2,
        pass_msg: "趋势向上",
        fail_msg: "趋势不明",
    },
    Rule {
        rule_id: "momentum",
        name: "动量",
        weight: 3,
        check: ma_bull,
        pass_msg: "动量配合",
        fail_msg: "动量弱",
    },
];

const LIVERMORE_RULES: [Rule; 2] = [
    Rule {
        rule_id: "stage2",
        name: "Stage 2",
        weight: 5,
        check: stage_2,
        pass_msg: "上升趋势",
        fail_msg: "非上升",
    },
    Rule {
        rule_id: "volume",
        name: "量能",
        weight: 3,
        check: ma_bull,
        pass_msg: "均线多头",
        fail_msg: "均线弱",
    },
];

const DUAN_RULES: [Rule; 3] = [
    Rule {
        rule_id: "business",
        name: "好生意",
        weight: 5,
        check: moat_clear,
        pass_msg: "生意好",
        fail_msg: "生意一般",
    },
    Rule {
        rule_id: "roe",
        name: "ROE",
        weight: 4,
        check: roe_above_20,
        pass_msg: "ROE 高",
        fail_msg: "ROE 低",
    },
    Rule {
        rule_id: "price",
        name: "价格合理",
        weight: 3,
        check: pe_reasonable,
        pass_msg: "价格 OK",
        fail_msg: "价格贵",
    },
];

const YOUZI_RULES: [Rule; 2] = [
    Rule {
        rule_id: "momentum",
        name: "短线动量",
        weight: 5,
        check: youzi_momentum,
        pass_msg: "动量在线",
        fail_msg: "动量不足",
    },
    Rule {
        rule_id: "lhb",
        name: "龙虎榜活跃",
        weight: 3,
        check: |f| !f.matched_youzi.is_empty(),
        pass_msg: "席位活跃",
        fail_msg: "无席位",
    },
];

const SIMONS_RULES: [Rule; 2] = [
    Rule {
        rule_id: "factor",
        name: "多因子",
        weight: 5,
        check: quant_factor_ok,
        pass_msg: "因子正",
        fail_msg: "因子弱",
    },
    Rule {
        rule_id: "liquidity",
        name: "流动性",
        weight: 3,
        check: |f| f.market_cap_yi.unwrap_or(0.0) > 50.0,
        pass_msg: "流动性好",
        fail_msg: "流动性差",
    },
];

const SERENITY_RULES: [Rule; 2] = [
    Rule {
        rule_id: "growth",
        name: "科技成长",
        weight: 5,
        check: |f| f.revenue_growth_latest.unwrap_or(0.0) >= 25.0,
        pass_msg: "高成长",
        fail_msg: "成长不足",
    },
    Rule {
        rule_id: "moat",
        name: "卡位",
        weight: 4,
        check: moat_clear,
        pass_msg: "卡位清晰",
        fail_msg: "卡位不明",
    },
];

const GENERIC_VALUE_RULES: [Rule; 2] = [
    Rule {
        rule_id: "quality",
        name: "质量",
        weight: 4,
        check: |f| f.roe_latest.unwrap_or(0.0) >= 10.0,
        pass_msg: "质量尚可",
        fail_msg: "质量一般",
    },
    Rule {
        rule_id: "valuation",
        name: "估值",
        weight: 3,
        check: pe_reasonable,
        pass_msg: "估值合理",
        fail_msg: "估值偏高",
    },
];
