//! Scoring engine + panel generation.

pub mod dims;

use serde::{Deserialize, Serialize};

use crate::research::personas::{PersonaVote, evaluate_all};
use crate::research::types::FeatureVector;

pub use dims::{DimScore, ScoreDimensionsResult, score_dimensions};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PanelResult {
    pub investors: Vec<PersonaVote>,
    pub vote_distribution: VoteDistribution,
    pub signal_distribution: SignalDistribution,
    pub panel_consensus: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VoteDistribution {
    pub strongly_buy: u32,
    pub buy: u32,
    pub watch: u32,
    pub wait: u32,
    pub avoid: u32,
    pub n_a: u32,
    pub skip: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SignalDistribution {
    pub bullish: u32,
    pub neutral: u32,
    pub bearish: u32,
    pub skip: u32,
}

/// Generate investor panel from scored dimensions + features.
#[must_use]
pub fn generate_panel(scored: &ScoreDimensionsResult, features: &FeatureVector) -> PanelResult {
    let votes = evaluate_all(features);
    let mut vote_dist = VoteDistribution::default();
    let mut sig_dist = SignalDistribution::default();
    let mut score_sum = 0.0;
    let mut score_count = 0.0;

    for v in &votes {
        match v.signal.as_str() {
            "bullish" => sig_dist.bullish += 1,
            "bearish" => sig_dist.bearish += 1,
            "skip" => sig_dist.skip += 1,
            _ => sig_dist.neutral += 1,
        }
        if v.signal == "skip" {
            vote_dist.skip += 1;
        } else {
            score_sum += v.score;
            score_count += 1.0;
            if v.score >= 80.0 && v.signal == "bullish" {
                vote_dist.strongly_buy += 1;
            } else if v.signal == "bullish" {
                vote_dist.buy += 1;
            } else if v.signal == "bearish" {
                vote_dist.avoid += 1;
            } else {
                vote_dist.watch += 1;
            }
        }
    }

    let panel_consensus = if score_count > 0.0 {
        (score_sum / score_count * 10.0).round() / 10.0
    } else {
        scored.fundamental_score
    };

    PanelResult {
        investors: votes,
        vote_distribution: vote_dist,
        signal_distribution: sig_dist,
        panel_consensus,
    }
}
