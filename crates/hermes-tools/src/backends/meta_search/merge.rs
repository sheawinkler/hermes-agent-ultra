//! Merge and rank hits from multiple engines.

use std::collections::HashSet;

use super::SearchHit;

#[derive(Debug, Clone)]
struct ScoredHit {
    hit: SearchHit,
    score: f64,
    order: usize,
}

/// Default per-source weights when not in CJK-boost mode.
fn base_weight(source: &str) -> f64 {
    match source {
        "sogou" | "bing_cn" => 1.0,
        id if id.starts_with("ddgs_") => 0.9,
        _ => 0.8,
    }
}

/// CN sources receive `cn_weight` multiplier when query has CJK.
pub fn merge_and_rank(
    batches: Vec<Vec<SearchHit>>,
    limit: usize,
    prefer_cn: bool,
    cn_weight: f64,
) -> Vec<SearchHit> {
    let limit = limit.max(1);
    let mut seen = HashSet::new();
    let mut scored: Vec<ScoredHit> = Vec::new();
    let mut order = 0usize;

    for batch in batches {
        for hit in batch {
            let key = dedup_key(&hit.url, &hit.title);
            if !seen.insert(key) {
                continue;
            }
            let mut weight = base_weight(&hit.source);
            if prefer_cn && matches!(hit.source.as_str(), "sogou" | "bing_cn") {
                weight *= cn_weight;
            }
            scored.push(ScoredHit {
                hit,
                score: weight,
                order,
            });
            order += 1;
        }
    }

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.order.cmp(&b.order))
    });

    scored
        .into_iter()
        .take(limit)
        .map(|s| s.hit)
        .collect()
}

fn dedup_key(url: &str, title: &str) -> String {
    format!(
        "{}|{}",
        url.trim().to_ascii_lowercase(),
        title.trim().to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(source: &str, url: &str, title: &str) -> SearchHit {
        SearchHit::new(title, url, "desc", source)
    }

    #[test]
    fn dedups_by_url_and_title() {
        let batches = vec![
            vec![hit("sogou", "https://a.com", "Title")],
            vec![hit("bing_cn", "https://a.com", "Title")],
            vec![hit("ddgs_lite", "https://b.com", "Other")],
        ];
        let merged = merge_and_rank(batches, 10, true, 1.25);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn cn_weight_prefers_sogou_over_ddgs() {
        let batches = vec![
            vec![hit("ddgs_lite", "https://ddg.com", "DDG")],
            vec![hit("sogou", "https://sg.com", "SG")],
        ];
        let merged = merge_and_rank(batches, 2, true, 2.0);
        assert_eq!(merged[0].source, "sogou");
    }

    #[test]
    fn limit_truncates() {
        let batches = vec![
            hit("sogou", "https://1.com", "A"),
            hit("sogou", "https://2.com", "B"),
            hit("sogou", "https://3.com", "C"),
        ]
        .into_iter()
        .map(|h| vec![h])
        .collect();
        assert_eq!(merge_and_rank(batches, 2, false, 1.0).len(), 2);
    }

    #[test]
    fn empty_input() {
        assert!(merge_and_rank(vec![], 5, false, 1.0).is_empty());
    }
}
