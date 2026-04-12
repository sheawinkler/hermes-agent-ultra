use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedTrajectory {
    pub id: String,
    pub summary: String,
    pub original_chars: usize,
    pub compressed_chars: usize,
    pub compressed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct TrajectoryCompressor;

impl TrajectoryCompressor {
    pub fn compress(id: impl Into<String>, raw: &str, max_chars: usize) -> CompressedTrajectory {
        let raw_trimmed = raw.trim();
        let summary = if raw_trimmed.chars().count() <= max_chars {
            raw_trimmed.to_string()
        } else {
            let mut out = String::with_capacity(max_chars + 16);
            for c in raw_trimmed.chars().take(max_chars.saturating_sub(3)) {
                out.push(c);
            }
            out.push_str("...");
            out
        };

        CompressedTrajectory {
            id: id.into(),
            original_chars: raw_trimmed.chars().count(),
            compressed_chars: summary.chars().count(),
            summary,
            compressed_at: Utc::now(),
        }
    }
}
