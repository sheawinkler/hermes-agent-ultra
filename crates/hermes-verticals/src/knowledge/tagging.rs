use super::item::KnowledgeItem;

pub fn auto_tag(item: &KnowledgeItem) -> Vec<String> {
    let mut tags = item.tags.clone();
    if tags.is_empty() {
        tags.push("untagged".to_string());
    }
    tags
}
