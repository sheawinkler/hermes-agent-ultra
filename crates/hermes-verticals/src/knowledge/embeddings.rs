pub fn embed_text(text: &str) -> Vec<f32> {
    text.chars().take(64).map(|c| c as u32 as f32).collect()
}
