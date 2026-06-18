//! Investor persona rule engine (UZI investor_evaluator + investor_criteria).

pub mod evaluator;
pub mod investors;
pub mod rules;

pub use evaluator::{PersonaVote, evaluate, evaluate_all};
