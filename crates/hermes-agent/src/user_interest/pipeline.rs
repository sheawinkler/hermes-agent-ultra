//! Extract → Compare → Update pipeline for durable POI rows.

use hermes_config::InterestConfig;

use super::quality::filter_persistable_signals;
use super::store::{InterestSignal, InterestStore, InterestTopic};
use super::topic_id::normalize_canonical_key;
use super::types::{PoiApplyReport, TopicStatus};

/// Compare outcome before writing SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CompareAction {
    Skip,
    Reinforce { topic_id: String },
    MergeInto { topic_id: String },
    Insert { status: TopicStatus },
}

/// Mem0-style batch: extract signals are compared against existing rows before update.
pub struct PoiPipeline<'a> {
    store: &'a InterestStore,
    config: &'a InterestConfig,
}

/// Apply a batch of extracted signals through compare → update (entry point for ingest).
pub fn apply_signal_batch(
    store: &InterestStore,
    config: &InterestConfig,
    signals: Vec<InterestSignal>,
) -> Result<PoiApplyReport, String> {
    PoiPipeline::new(store, config).apply_batch(signals)
}

impl<'a> PoiPipeline<'a> {
    pub fn new(store: &'a InterestStore, config: &'a InterestConfig) -> Self {
        Self { store, config }
    }

    /// Apply a batch of extracted signals through compare → update.
    pub fn apply_batch(&self, signals: Vec<InterestSignal>) -> Result<PoiApplyReport, String> {
        let signals = filter_persistable_signals(signals);
        if signals.is_empty() {
            return Ok(PoiApplyReport::default());
        }

        let existing = self.store.list_topics_for_pipeline()?;
        let mut report = PoiApplyReport::default();

        for signal in signals {
            let action = self.compare(&signal, &existing);
            match action {
                CompareAction::Skip => report.skipped += 1,
                CompareAction::Reinforce { topic_id } => {
                    let promoted = self.store.reinforce_topic(
                        &topic_id,
                        &signal,
                        self.config.promote_min_evidence,
                        self.config.promote_min_confidence,
                    )?;
                    report.reinforced += 1;
                    if promoted {
                        report.promoted += 1;
                    }
                }
                CompareAction::MergeInto { topic_id } => {
                    let promoted = self.store.reinforce_topic(
                        &topic_id,
                        &signal,
                        self.config.promote_min_evidence,
                        self.config.promote_min_confidence,
                    )?;
                    report.merged += 1;
                    if promoted {
                        report.promoted += 1;
                    }
                }
                CompareAction::Insert { status } => {
                    self.store.insert_topic(&signal, status)?;
                    report.inserted += 1;
                }
            }
        }

        self.store.enforce_max_topics()?;
        Ok(report)
    }

    fn compare(&self, signal: &InterestSignal, existing: &[InterestTopic]) -> CompareAction {
        if existing.iter().any(|t| t.id == signal.id && t.status != TopicStatus::Rejected) {
            return CompareAction::Reinforce {
                topic_id: signal.id.clone(),
            };
        }

        if let Some(target) = find_merge_target(signal, existing) {
            return CompareAction::MergeInto {
                topic_id: target.id.clone(),
            };
        }

        let status = initial_status(signal, self.config);
        CompareAction::Insert { status }
    }
}

fn initial_status(signal: &InterestSignal, config: &InterestConfig) -> TopicStatus {
    if signal.source().inserts_as_active(signal.confidence, config.promote_min_confidence) {
        TopicStatus::Active
    } else {
        TopicStatus::Candidate
    }
}

/// Lexical merge: same declared phrase, or high token overlap on labels/summaries.
fn find_merge_target<'a>(
    signal: &InterestSignal,
    existing: &'a [InterestTopic],
) -> Option<&'a InterestTopic> {
    let sig_key = normalize_canonical_key(&signal.label);
    if sig_key.is_empty() {
        return None;
    }
    let sig_tokens = token_set(&sig_key);

    let mut best: Option<(&InterestTopic, f64)> = None;
    for topic in existing {
        if topic.status == TopicStatus::Rejected {
            continue;
        }
        if topic.id == signal.id {
            continue;
        }
        let topic_key = normalize_canonical_key(&topic.label);
        if topic_key == sig_key {
            return Some(topic);
        }
        let overlap = jaccard(&sig_tokens, &token_set(&topic_key));
        if overlap >= 0.72 {
            match best {
                Some((_, score)) if overlap <= score => {}
                _ => best = Some((topic, overlap)),
            }
        }
    }
    best.map(|(t, _)| t)
}

fn token_set(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_interest::types::SignalSource;
    use hermes_config::InterestConfig;
    use tempfile::TempDir;

    fn signal(id: &str, label: &str, source: SignalSource, confidence: f64) -> InterestSignal {
        InterestSignal {
            id: id.to_string(),
            label: label.to_string(),
            summary: label.to_string(),
            weight_delta: 0.2,
            tags: vec![source.as_str().to_string()],
            source,
            confidence,
        }
    }

    #[test]
    fn declared_inserts_active() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("interest.db");
        let config = InterestConfig::default();
        let store = InterestStore::open(&db, config.clone()).unwrap();
        let pipeline = PoiPipeline::new(&store, &config);
        let report = pipeline
            .apply_batch(vec![signal(
                "interest:打篮球",
                "兴趣: 打篮球",
                SignalSource::Declared,
                0.92,
            )])
            .unwrap();
        assert_eq!(report.inserted, 1);
        let topics = store.list_for_cli(true).unwrap();
        assert_eq!(topics[0].status, TopicStatus::Active);
    }

    #[test]
    fn second_hit_promotes_candidate() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("interest.db");
        let mut config = InterestConfig::default();
        config.promote_min_evidence = 2;
        config.promote_min_confidence = 0.5;
        let store = InterestStore::open(&db, config.clone()).unwrap();
        let pipeline = PoiPipeline::new(&store, &config);
        let tech = signal("tech:rust", "topic: rust", SignalSource::Rules, 0.48);
        pipeline.apply_batch(vec![tech.clone()]).unwrap();
        let topics = store.list_for_cli(true).unwrap();
        assert_eq!(topics[0].status, TopicStatus::Candidate);
        let mut second = tech;
        second.confidence = 0.52;
        let report = pipeline.apply_batch(vec![second]).unwrap();
        assert_eq!(report.reinforced, 1);
        assert_eq!(report.promoted, 1);
        let topics = store.list_for_cli(true).unwrap();
        assert_eq!(topics[0].status, TopicStatus::Active);
    }
}
