use crate::theme::Theme;

pub const BUILTIN_SKINS: &[(&str, &str)] = &[
    ("ultra-neon", "Default Ultra neon profile (magenta/cyan)"),
    (
        "neon-glow",
        "Extra glow and contrast for low-light terminals",
    ),
    (
        "hyper-ultra-hyper-saturated",
        "Maximum saturation profile for high-energy sessions",
    ),
    (
        "bleeding-edges",
        "Acid-lime + hot-magenta bleeding-edge profile",
    ),
    (
        "ultra-laserwave",
        "Synthwave palette with laser blue accents",
    ),
    ("ultra-voltage", "Electric cyan with amber highlights"),
    ("ultra-amber", "Warm amber neon profile"),
    ("ultra-ice", "Cool ice neon profile"),
    ("ultra-hc", "High-contrast accessibility profile"),
    ("dark", "Default dark profile"),
    ("light", "Light profile"),
];

pub fn canonical_skin_name(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "ultra" | "ultra-neon" | "neon" => Some("ultra-neon"),
        "neon-glow" | "glow" => Some("neon-glow"),
        "hyper-ultra-hyper-saturated" | "hyper-saturated" | "hypersat" => {
            Some("hyper-ultra-hyper-saturated")
        }
        "bleeding-edges" | "bleeding-edge" | "edge" => Some("bleeding-edges"),
        "ultra-laserwave" | "laserwave" => Some("ultra-laserwave"),
        "ultra-voltage" | "voltage" => Some("ultra-voltage"),
        "ultra-amber" | "amber" => Some("ultra-amber"),
        "ultra-ice" | "ice" => Some("ultra-ice"),
        "ultra-hc" | "hc" | "high-contrast" => Some("ultra-hc"),
        "light" => Some("light"),
        "dark" => Some("dark"),
        _ => None,
    }
}

pub fn resolve_theme(name: &str) -> Theme {
    match canonical_skin_name(name).unwrap_or("ultra-neon") {
        "ultra-neon" => crate::theme::ultra_neon_theme(),
        "neon-glow" => crate::theme::neon_glow_theme(),
        "hyper-ultra-hyper-saturated" => crate::theme::hyper_ultra_hyper_saturated_theme(),
        "bleeding-edges" => crate::theme::bleeding_edges_theme(),
        "ultra-laserwave" => crate::theme::ultra_laserwave_theme(),
        "ultra-voltage" => crate::theme::ultra_voltage_theme(),
        "ultra-amber" => crate::theme::ultra_amber_theme(),
        "ultra-ice" => crate::theme::ultra_ice_theme(),
        "ultra-hc" => crate::theme::ultra_hc_theme(),
        "light" => crate::theme::light_theme(),
        "dark" => crate::theme::default_theme(),
        _ => crate::theme::ultra_neon_theme(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_skin_aliases_resolve() {
        assert_eq!(canonical_skin_name("neon"), Some("ultra-neon"));
        assert_eq!(canonical_skin_name("glow"), Some("neon-glow"));
        assert_eq!(
            canonical_skin_name("hyper-saturated"),
            Some("hyper-ultra-hyper-saturated")
        );
        assert_eq!(canonical_skin_name("bleeding-edge"), Some("bleeding-edges"));
        assert_eq!(canonical_skin_name("voltage"), Some("ultra-voltage"));
    }

    #[test]
    fn resolve_theme_uses_new_builtins() {
        assert_eq!(resolve_theme("neon-glow").name, "neon-glow");
        assert_eq!(
            resolve_theme("hyper-ultra-hyper-saturated").name,
            "hyper-ultra-hyper-saturated"
        );
        assert_eq!(resolve_theme("bleeding-edges").name, "bleeding-edges");
    }
}
