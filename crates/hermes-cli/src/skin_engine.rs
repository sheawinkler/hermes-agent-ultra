use crate::theme::Theme;

pub fn resolve_theme(name: &str) -> Theme {
    match name.trim().to_ascii_lowercase().as_str() {
        "ultra" | "ultra-neon" | "neon" => crate::theme::ultra_neon_theme(),
        "light" => crate::theme::light_theme(),
        "dark" => crate::theme::default_theme(),
        _ => crate::theme::ultra_neon_theme(),
    }
}
