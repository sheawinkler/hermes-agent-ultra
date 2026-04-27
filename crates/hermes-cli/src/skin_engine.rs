use crate::theme::Theme;

pub fn resolve_theme(name: &str) -> Theme {
    match name.trim().to_ascii_lowercase().as_str() {
        "ultra" | "ultra-neon" | "neon" => crate::theme::ultra_neon_theme(),
        "ultra-amber" | "amber" => crate::theme::ultra_amber_theme(),
        "ultra-ice" | "ice" => crate::theme::ultra_ice_theme(),
        "ultra-hc" | "hc" | "high-contrast" => crate::theme::ultra_hc_theme(),
        "light" => crate::theme::light_theme(),
        "dark" => crate::theme::default_theme(),
        _ => crate::theme::ultra_neon_theme(),
    }
}
