//! Equity research parity tests (UZI fin_models golden fixtures).

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use hermes_trading::research::models::{
    CompsPeer, CompsTarget, ThreeStmtResult, build_comps_table, compute_dcf, compute_wacc,
    project_three_stmt, quick_lbo,
};
use hermes_trading::research::types::FeatureVector;

#[derive(Debug, serde::Deserialize)]
struct FixtureFile {
    #[allow(dead_code)]
    schema_version: u32,
    #[allow(dead_code)]
    fixture_group: String,
    cases: Vec<FixtureCase>,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureCase {
    id: String,
    op: String,
    input: Value,
    expected: Value,
    #[serde(default)]
    skip: bool,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/trading_research/models_golden.json")
}

fn load_fixtures() -> FixtureFile {
    let content = fs::read_to_string(fixture_path()).expect("read models_golden.json");
    serde_json::from_str(&content).expect("parse fixture")
}

fn features_from(v: &Value) -> FeatureVector {
    let mut f: FeatureVector =
        serde_json::from_value(v.clone()).unwrap_or_else(|_| FeatureVector {
            symbol: v
                .get("symbol")
                .and_then(|s| s.as_str())
                .unwrap_or("TEST")
                .to_string(),
            ..Default::default()
        });
    if f.symbol.is_empty() {
        f.symbol = "TEST".into();
    }
    macro_rules! set_f64 {
        ($field:ident) => {
            if f.$field.is_none() {
                if let Some(n) = v.get(stringify!($field)).and_then(|x| x.as_f64()) {
                    f.$field = Some(n);
                }
            }
        };
    }
    set_f64!(price);
    set_f64!(market_cap_yi);
    set_f64!(shares_outstanding_yi);
    set_f64!(revenue_latest_yi);
    set_f64!(net_margin);
    set_f64!(total_debt_yi);
    set_f64!(cash_yi);
    set_f64!(fcf_latest_yi);
    set_f64!(ebitda_yi);
    set_f64!(equity_yi);
    f
}

fn approx_eq(a: f64, b: f64, tol_pct: f64) -> bool {
    if b == 0.0 {
        return a.abs() < 0.01;
    }
    ((a - b) / b).abs() <= tol_pct
}

fn run_case(case: &FixtureCase) {
    if case.skip {
        return;
    }
    match case.op.as_str() {
        "compute_wacc" => {
            let r = compute_wacc(None);
            let exp = case.expected["wacc"].as_f64().unwrap();
            assert!(
                approx_eq(r.wacc, exp, 0.01),
                "{} wacc: {} vs {}",
                case.id,
                r.wacc,
                exp
            );
        }
        "compute_dcf" => {
            let f = features_from(&case.input);
            let r = compute_dcf(&f, None);
            let exp = &case.expected;
            assert!(approx_eq(
                r.intrinsic_per_share,
                exp["intrinsic_per_share"].as_f64().unwrap(),
                0.01
            ));
            assert!(approx_eq(
                r.safety_margin_pct,
                exp["safety_margin_pct"].as_f64().unwrap(),
                0.05
            ));
            assert!(approx_eq(
                r.sensitivity_table.center_cell,
                exp["center_cell"].as_f64().unwrap(),
                0.01
            ));
        }
        "build_comps" => {
            let target: CompsTarget = serde_json::from_value(case.input["target"].clone()).unwrap();
            let peers: Vec<CompsPeer> =
                serde_json::from_value(case.input["peers"].clone()).unwrap();
            let r = build_comps_table(target, &peers);
            let hermes_trading::research::models::CompsResult::Ok(ok) = r else {
                panic!("{} expected comps ok", case.id);
            };
            let median = ok.peer_stats.get("pe").map(|s| s.median).unwrap();
            assert!(approx_eq(
                median,
                case.expected["median_pe"].as_f64().unwrap(),
                0.01
            ));
        }
        "quick_lbo" => {
            let f = features_from(&case.input);
            let r = quick_lbo(&f, None);
            assert!(approx_eq(
                r.irr_pct,
                case.expected["irr_pct"].as_f64().unwrap(),
                0.02
            ));
        }
        "project_three_stmt" => {
            let f = features_from(&case.input);
            let ThreeStmtResult::Ok(ok) = project_three_stmt(&f, None) else {
                panic!("{} three_stmt failed", case.id);
            };
            let y5 = ok.income_statement.net_income.last().copied().unwrap();
            assert!(approx_eq(
                y5,
                case.expected["y5_ni"].as_f64().unwrap(),
                0.02
            ));
        }
        other => panic!("unknown op {other}"),
    }
}

#[test]
fn equity_research_models_parity() {
    let fixture = load_fixtures();
    for case in &fixture.cases {
        run_case(case);
    }
}
