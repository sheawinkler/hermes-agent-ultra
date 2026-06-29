use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphEdgeKind {
    Citation,
    Entity,
    Causal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KGNode {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KGEdge {
    pub from: String,
    pub to: String,
    pub kind: GraphEdgeKind,
    pub weight: f32,
}
