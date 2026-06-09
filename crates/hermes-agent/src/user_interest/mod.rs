//! Local user interest (POI) topic store and memory provider integration.
//!
//! Production flow: **Extract → Compare → Update** (`pipeline` module), with per-session
//! buffering and session-end commit.

mod catalog;
mod contextual;
mod declared;
mod domain_taxonomy;
mod task_oriented;
mod extract;
mod ingest;
mod llm;
mod pipeline;
mod plugin;
mod quality;
mod session_buffer;
mod store;
mod topic_id;
mod types;

pub use extract::{
    extract_signals_from_messages, extract_signals_from_text, filter_poi_signals,
    is_rejected_poi_topic, message_text_from_value,
};
pub use contextual::extract_contextual_interests;
pub use declared::extract_declared_interests;
pub use domain_taxonomy::{domain_taxonomy_prompt_block, DomainTaxon, DOMAIN_TAXONOMY};
pub use task_oriented::has_task_or_domain_signal;
pub use ingest::{
    format_user_transcript_for_llm, ingest_user_message, is_poi_synthetic_user_text,
    spawn_session_end_ingest,
};
pub use llm::extract_signals_from_transcript_llm;
pub use pipeline::{apply_signal_batch, PoiPipeline};
pub use plugin::InterestMemoryPlugin;
pub use session_buffer::SessionPoiBuffer;
pub use store::{InterestSignal, InterestStore, InterestTopic, load_interest_snapshot};
pub use quality::filter_persistable_signals;
pub use types::{ExtractOptions, PoiApplyReport, SignalSource, TopicStatus};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Open the shared interest DB (independent of `skip_memory`).
pub fn open_interest_store(
    hermes_home: &str,
    config: &hermes_config::InterestConfig,
) -> Option<Arc<Mutex<InterestStore>>> {
    if !config.enabled {
        return None;
    }
    let db_path = PathBuf::from(hermes_home).join("interest.db");
    InterestStore::open(&db_path, config.clone())
        .ok()
        .map(|store| Arc::new(Mutex::new(store)))
}
