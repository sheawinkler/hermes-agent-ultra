use super::item::KnowledgeItem;

pub fn cluster_items(items: &[KnowledgeItem]) -> Vec<Vec<String>> {
    if items.is_empty() {
        return vec![];
    }
    vec![items.iter().map(|i| i.id.clone()).collect()]
}
