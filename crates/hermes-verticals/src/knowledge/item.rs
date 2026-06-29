use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KnowledgeSourceType {
    Url,
    Image,
    Video,
    Audio,
    Note,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeItem {
    pub id: String,
    pub source_type: KnowledgeSourceType,
    pub content: String,
    pub embedding_vec: Option<Vec<f32>>,
    pub tags: Vec<String>,
    pub topics: Vec<String>,
}
