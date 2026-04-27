//! Theme / skin engine (Requirement 9.8).
//!
//! Defines color themes and text styles for the TUI. Supports loading
//! custom themes from TOML/JSON files and provides built-in dark and
//! light themes.

use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ThemeError
// ---------------------------------------------------------------------------

/// Errors that can occur when loading a theme.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ThemeError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Validation error: {0}")]
    Validation(String),
}

// ---------------------------------------------------------------------------
// ThemeColors
// ---------------------------------------------------------------------------

/// Named color palette for a theme.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeColors {
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub background: String,
    pub foreground: String,
    pub error: String,
    pub warning: String,
    pub success: String,
    #[serde(default)]
    pub status_bar_bg: Option<String>,
    #[serde(default)]
    pub status_bar_text: Option<String>,
    #[serde(default)]
    pub status_bar_strong: Option<String>,
    #[serde(default)]
    pub status_bar_dim: Option<String>,
    #[serde(default)]
    pub status_bar_good: Option<String>,
    #[serde(default)]
    pub status_bar_warn: Option<String>,
    #[serde(default)]
    pub status_bar_bad: Option<String>,
    #[serde(default)]
    pub status_bar_critical: Option<String>,
}

impl ThemeColors {
    /// Parse a color string into a ratatui Color.
    ///
    /// Supports:
    /// - Named colors: "red", "blue", "green", etc.
    /// - Hex: "#ff0000", "#00ff00"
    /// - Indexed: "7" (ANSI index)
    pub fn parse_color(s: &str) -> Color {
        let lower = s.trim().to_lowercase();
        match lower.as_str() {
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "white" => Color::White,
            "gray" | "grey" => Color::Gray,
            "darkgray" | "dark_grey" | "dark-grey" => Color::DarkGray,
            "lightred" | "light_red" | "light-red" => Color::LightRed,
            "lightgreen" | "light_green" | "light-green" => Color::LightGreen,
            "lightyellow" | "light_yellow" | "light-yellow" => Color::LightYellow,
            "lightblue" | "light_blue" | "light-blue" => Color::LightBlue,
            "lightmagenta" | "light_magenta" | "light-magenta" => Color::LightMagenta,
            "lightcyan" | "light_cyan" | "light-cyan" => Color::LightCyan,
            _ => {
                // Try hex
                if let Some(hex) = s.strip_prefix('#') {
                    if let Ok(rgb) = parse_hex(hex) {
                        Color::Rgb(rgb.0, rgb.1, rgb.2)
                    } else {
                        Color::White
                    }
                } else {
                    // Try ANSI index
                    if let Ok(idx) = s.parse::<u8>() {
                        Color::Indexed(idx)
                    } else {
                        Color::White
                    }
                }
            }
        }
    }

    /// Convert the entire color palette to ratatui Colors.
    pub fn to_ratatui_colors(&self) -> RatatuiColors {
        RatatuiColors {
            primary: Self::parse_color(&self.primary),
            secondary: Self::parse_color(&self.secondary),
            accent: Self::parse_color(&self.accent),
            background: Self::parse_color(&self.background),
            foreground: Self::parse_color(&self.foreground),
            error: Self::parse_color(&self.error),
            warning: Self::parse_color(&self.warning),
            success: Self::parse_color(&self.success),
            status_bar_bg: self
                .status_bar_bg
                .as_deref()
                .map(Self::parse_color)
                .unwrap_or_else(|| Self::parse_color(&self.primary)),
            status_bar_text: self
                .status_bar_text
                .as_deref()
                .map(Self::parse_color)
                .unwrap_or_else(|| Self::parse_color(&self.foreground)),
            status_bar_strong: self
                .status_bar_strong
                .as_deref()
                .map(Self::parse_color)
                .unwrap_or_else(|| Self::parse_color(&self.accent)),
            status_bar_dim: self
                .status_bar_dim
                .as_deref()
                .map(Self::parse_color)
                .unwrap_or_else(|| Self::parse_color(&self.secondary)),
            status_bar_good: self
                .status_bar_good
                .as_deref()
                .map(Self::parse_color)
                .unwrap_or_else(|| Self::parse_color(&self.success)),
            status_bar_warn: self
                .status_bar_warn
                .as_deref()
                .map(Self::parse_color)
                .unwrap_or_else(|| Self::parse_color(&self.warning)),
            status_bar_bad: self
                .status_bar_bad
                .as_deref()
                .map(Self::parse_color)
                .unwrap_or_else(|| Self::parse_color(&self.error)),
            status_bar_critical: self
                .status_bar_critical
                .as_deref()
                .map(Self::parse_color)
                .unwrap_or_else(|| Self::parse_color(&self.error)),
        }
    }
}

/// Resolved ratatui Color values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RatatuiColors {
    pub primary: Color,
    pub secondary: Color,
    pub accent: Color,
    pub background: Color,
    pub foreground: Color,
    pub error: Color,
    pub warning: Color,
    pub success: Color,
    pub status_bar_bg: Color,
    pub status_bar_text: Color,
    pub status_bar_strong: Color,
    pub status_bar_dim: Color,
    pub status_bar_good: Color,
    pub status_bar_warn: Color,
    pub status_bar_bad: Color,
    pub status_bar_critical: Color,
}

// ---------------------------------------------------------------------------
// ThemeStyles
// ---------------------------------------------------------------------------

/// Text styles for different content types in the TUI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeStyles {
    pub user_input: StyleDef,
    pub assistant_response: StyleDef,
    pub system_message: StyleDef,
    pub tool_call: StyleDef,
    pub tool_result: StyleDef,
    pub error: StyleDef,
}

/// Serializable style definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StyleDef {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl StyleDef {
    /// Convert to a ratatui Style.
    pub fn to_style(&self) -> Style {
        let mut style = Style::default();
        if let Some(ref fg) = self.fg {
            style = style.fg(ThemeColors::parse_color(fg));
        }
        if let Some(ref bg) = self.bg {
            style = style.bg(ThemeColors::parse_color(bg));
        }
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.underline {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        style
    }
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// A complete theme definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Theme {
    /// Human-readable theme name.
    pub name: String,
    /// Named color palette.
    pub colors: ThemeColors,
    /// Text styles for content types.
    pub styles: ThemeStyles,
}

impl Default for Theme {
    fn default() -> Self {
        default_theme()
    }
}

impl Theme {
    /// Create a default theme.
    pub fn default_theme() -> Self {
        Self::default()
    }

    /// Load a theme from a file (JSON or TOML).
    pub fn load_theme(path: &Path) -> Result<Self, ThemeError> {
        let content = std::fs::read_to_string(path).map_err(|e| ThemeError::Io(e.to_string()))?;

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "json" => serde_json::from_str(&content).map_err(|e| ThemeError::Parse(e.to_string())),
            "toml" => {
                // TOML parsing requires the `toml` crate. If not available,
                // we return a parse error suggesting JSON.
                // In a full implementation, add `toml = "0.8"` to Cargo.toml.
                Err(ThemeError::Parse(
                    "TOML theme loading requires the `toml` crate. Use JSON format.".to_string(),
                ))
            }
            _ => {
                // Try JSON first, then TOML
                serde_json::from_str(&content).or_else(|_| {
                    Err(ThemeError::Parse(format!(
                        "Unsupported theme file format: {}. Use .json or .toml",
                        ext
                    )))
                })
            }
        }
    }

    /// Resolve all style definitions to ratatui Styles.
    pub fn resolved_styles(&self) -> ResolvedStyles {
        ResolvedStyles {
            user_input: self.styles.user_input.to_style(),
            assistant_response: self.styles.assistant_response.to_style(),
            system_message: self.styles.system_message.to_style(),
            tool_call: self.styles.tool_call.to_style(),
            tool_result: self.styles.tool_result.to_style(),
            error: self.styles.error.to_style(),
        }
    }
}

/// Resolved ratatui Styles for each content type.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedStyles {
    pub user_input: Style,
    pub assistant_response: Style,
    pub system_message: Style,
    pub tool_call: Style,
    pub tool_result: Style,
    pub error: Style,
}

// ---------------------------------------------------------------------------
// Built-in themes
// ---------------------------------------------------------------------------

/// Default dark theme.
pub fn default_theme() -> Theme {
    Theme {
        name: "dark".to_string(),
        colors: ThemeColors {
            primary: "#160f2f".to_string(),
            secondary: "#6f7a94".to_string(),
            accent: "#ff4fd8".to_string(),
            background: "#070b14".to_string(),
            foreground: "#e6ecff".to_string(),
            error: "#ff4d7d".to_string(),
            warning: "#ffbf47".to_string(),
            success: "#3df2a4".to_string(),
            status_bar_bg: Some("#140d2d".to_string()),
            status_bar_text: Some("#e8ecff".to_string()),
            status_bar_strong: Some("#ff5adf".to_string()),
            status_bar_dim: Some("#7d88a8".to_string()),
            status_bar_good: Some("#37e8a1".to_string()),
            status_bar_warn: Some("#ffbf47".to_string()),
            status_bar_bad: Some("#ff4d7d".to_string()),
            status_bar_critical: Some("#ff2366".to_string()),
        },
        styles: ThemeStyles {
            user_input: StyleDef {
                fg: Some("#79b8ff".to_string()),
                bg: None,
                bold: true,
                italic: false,
                underline: false,
            },
            assistant_response: StyleDef {
                fg: Some("#e6ecff".to_string()),
                bg: None,
                bold: false,
                italic: false,
                underline: false,
            },
            system_message: StyleDef {
                fg: Some("#7f8cb2".to_string()),
                bg: None,
                bold: false,
                italic: true,
                underline: false,
            },
            tool_call: StyleDef {
                fg: Some("#ff9d4d".to_string()),
                bg: None,
                bold: false,
                italic: false,
                underline: false,
            },
            tool_result: StyleDef {
                fg: Some("#33e8a0".to_string()),
                bg: None,
                bold: false,
                italic: false,
                underline: false,
            },
            error: StyleDef {
                fg: Some("#ff4d7d".to_string()),
                bg: None,
                bold: true,
                italic: false,
                underline: false,
            },
        },
    }
}

/// Built-in hyper-saturated theme used by Hermes Agent Ultra.
pub fn ultra_neon_theme() -> Theme {
    let mut theme = default_theme();
    theme.name = "ultra-neon".to_string();
    theme.colors.accent = "#ff2be3".to_string();
    theme.colors.status_bar_strong = Some("#ff2be3".to_string());
    theme.colors.primary = "#1a0b39".to_string();
    theme.styles.user_input.fg = Some("#8dc5ff".to_string());
    theme.styles.tool_call.fg = Some("#ffab5a".to_string());
    theme.styles.tool_result.fg = Some("#2effb3".to_string());
    theme
}

/// Built-in amber neon variant (high warmth, high contrast).
pub fn ultra_amber_theme() -> Theme {
    let mut theme = default_theme();
    theme.name = "ultra-amber".to_string();
    theme.colors.accent = "#ffb347".to_string();
    theme.colors.primary = "#2b1200".to_string();
    theme.colors.secondary = "#a58662".to_string();
    theme.colors.background = "#090704".to_string();
    theme.colors.status_bar_bg = Some("#1f1003".to_string());
    theme.colors.status_bar_strong = Some("#ffb347".to_string());
    theme.styles.user_input.fg = Some("#ffd08a".to_string());
    theme.styles.tool_call.fg = Some("#ffcf73".to_string());
    theme.styles.tool_result.fg = Some("#56f4b5".to_string());
    theme
}

/// Built-in ice neon variant (cool tones, high clarity).
pub fn ultra_ice_theme() -> Theme {
    let mut theme = default_theme();
    theme.name = "ultra-ice".to_string();
    theme.colors.accent = "#00d5ff".to_string();
    theme.colors.primary = "#081b2b".to_string();
    theme.colors.secondary = "#7ea7c1".to_string();
    theme.colors.background = "#040b10".to_string();
    theme.colors.status_bar_bg = Some("#071623".to_string());
    theme.colors.status_bar_strong = Some("#00d5ff".to_string());
    theme.styles.user_input.fg = Some("#8ee7ff".to_string());
    theme.styles.tool_call.fg = Some("#54c7ff".to_string());
    theme.styles.tool_result.fg = Some("#39f4ba".to_string());
    theme
}

/// Built-in high-contrast accessibility-first variant.
pub fn ultra_hc_theme() -> Theme {
    let mut theme = default_theme();
    theme.name = "ultra-hc".to_string();
    theme.colors.accent = "#ffd400".to_string();
    theme.colors.primary = "#000000".to_string();
    theme.colors.secondary = "#a9a9a9".to_string();
    theme.colors.background = "#000000".to_string();
    theme.colors.foreground = "#ffffff".to_string();
    theme.colors.status_bar_bg = Some("#111111".to_string());
    theme.colors.status_bar_text = Some("#ffffff".to_string());
    theme.colors.status_bar_strong = Some("#ffd400".to_string());
    theme.colors.status_bar_dim = Some("#b3b3b3".to_string());
    theme.styles.user_input.fg = Some("#8ad7ff".to_string());
    theme.styles.assistant_response.fg = Some("#ffffff".to_string());
    theme.styles.system_message.fg = Some("#c3c3c3".to_string());
    theme.styles.tool_call.fg = Some("#ffd400".to_string());
    theme.styles.tool_result.fg = Some("#52ffa8".to_string());
    theme
}

/// Built-in light theme.
pub fn light_theme() -> Theme {
    Theme {
        name: "light".to_string(),
        colors: ThemeColors {
            primary: "#2e5cb8".to_string(),    // Deep blue
            secondary: "#5a5a8a".to_string(),  // Muted purple
            accent: "#c0509a".to_string(),     // Dark pink
            background: "#f5f5f5".to_string(), // Off-white
            foreground: "#333333".to_string(), // Dark gray
            error: "#cc3333".to_string(),      // Red
            warning: "#b8860b".to_string(),    // Dark goldenrod
            success: "#2e8b57".to_string(),    // Sea green
            status_bar_bg: None,
            status_bar_text: None,
            status_bar_strong: None,
            status_bar_dim: None,
            status_bar_good: None,
            status_bar_warn: None,
            status_bar_bad: None,
            status_bar_critical: None,
        },
        styles: ThemeStyles {
            user_input: StyleDef {
                fg: Some("#2e5cb8".to_string()), // Blue
                bg: None,
                bold: true,
                italic: false,
                underline: false,
            },
            assistant_response: StyleDef {
                fg: Some("#333333".to_string()), // Foreground
                bg: None,
                bold: false,
                italic: false,
                underline: false,
            },
            system_message: StyleDef {
                fg: Some("#888888".to_string()), // Gray
                bg: None,
                bold: false,
                italic: true,
                underline: false,
            },
            tool_call: StyleDef {
                fg: Some("#b8600a".to_string()), // Dark orange
                bg: None,
                bold: false,
                italic: false,
                underline: false,
            },
            tool_result: StyleDef {
                fg: Some("#2e8b57".to_string()), // Green
                bg: None,
                bold: false,
                italic: false,
                underline: false,
            },
            error: StyleDef {
                fg: Some("#cc3333".to_string()), // Red
                bg: None,
                bold: true,
                italic: false,
                underline: false,
            },
        },
    }
}

/// Apply this theme's colors and styles to a Tui instance.
///
/// This sets the terminal background color and updates all rendering
/// styles used by the TUI components.
impl Theme {
    pub fn apply_theme_to_tui(&self, tui: &mut crate::tui::Tui) {
        tui.set_theme(self.clone());
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a hex color string (without `#` prefix) into an RGB tuple.
fn parse_hex(hex: &str) -> Result<(u8, u8, u8), ()> {
    let hex = hex.trim();
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
            Ok((r, g, b))
        }
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).map_err(|_| ())?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).map_err(|_| ())?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).map_err(|_| ())?;
            Ok((r, g, b))
        }
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_theme() {
        let theme = default_theme();
        assert_eq!(theme.name, "dark");
        assert!(!theme.colors.primary.is_empty());
        assert!(theme.styles.user_input.fg.is_some());
    }

    #[test]
    fn test_light_theme() {
        let theme = light_theme();
        assert_eq!(theme.name, "light");
    }

    #[test]
    fn test_ultra_neon_theme() {
        let theme = ultra_neon_theme();
        assert_eq!(theme.name, "ultra-neon");
        assert_eq!(theme.colors.accent, "#ff2be3");
    }

    #[test]
    fn test_ultra_variants_have_distinct_names() {
        assert_eq!(ultra_amber_theme().name, "ultra-amber");
        assert_eq!(ultra_ice_theme().name, "ultra-ice");
        assert_eq!(ultra_hc_theme().name, "ultra-hc");
    }

    #[test]
    fn test_theme_serialization() {
        let theme = default_theme();
        let json = serde_json::to_string(&theme).unwrap();
        let back: Theme = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "dark");
        assert_eq!(back.colors.primary, theme.colors.primary);
    }

    #[test]
    fn test_parse_color_named() {
        assert_eq!(ThemeColors::parse_color("red"), Color::Red);
        assert_eq!(ThemeColors::parse_color("blue"), Color::Blue);
        assert_eq!(ThemeColors::parse_color("Green"), Color::Green);
    }

    #[test]
    fn test_parse_color_hex() {
        match ThemeColors::parse_color("#ff0000") {
            Color::Rgb(255, 0, 0) => {}
            other => panic!("Expected Rgb(255, 0, 0), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_color_indexed() {
        assert_eq!(ThemeColors::parse_color("7"), Color::Indexed(7));
    }

    #[test]
    fn test_parse_hex() {
        assert_eq!(parse_hex("ff0000").unwrap(), (255, 0, 0));
        assert_eq!(parse_hex("f00").unwrap(), (255, 0, 0));
        assert!(parse_hex("xyz").is_err());
    }

    #[test]
    fn test_style_def_to_style() {
        let def = StyleDef {
            fg: Some("red".to_string()),
            bg: None,
            bold: true,
            italic: false,
            underline: false,
        };
        let style = def.to_style();
        assert!(style.fg.is_some());
    }

    #[test]
    fn test_load_theme_missing_file() {
        let result = Theme::load_theme(Path::new("/nonexistent/theme.json"));
        assert!(result.is_err());
    }

    #[test]
    fn test_resolved_styles() {
        let theme = default_theme();
        let styles = theme.resolved_styles();
        // Just verify that we can resolve all styles without panic
        assert!(styles.user_input.fg.is_some());
        assert!(styles.assistant_response.fg.is_some());
        assert!(styles.error.fg.is_some());
    }

    #[test]
    fn test_status_bar_color_fields_set_for_default_theme() {
        let colors = default_theme().colors.to_ratatui_colors();
        assert_eq!(colors.status_bar_bg, Color::Rgb(0x14, 0x0d, 0x2d));
        assert_eq!(colors.status_bar_text, Color::Rgb(0xe8, 0xec, 0xff));
        assert_eq!(colors.status_bar_warn, Color::Rgb(0xff, 0xbf, 0x47));
        assert_eq!(colors.status_bar_critical, Color::Rgb(0xff, 0x23, 0x66));
    }

    #[test]
    fn test_status_bar_color_fields_respect_overrides() {
        let mut theme = default_theme();
        theme.colors.status_bar_bg = Some("#112233".to_string());
        theme.colors.status_bar_text = Some("#445566".to_string());
        theme.colors.status_bar_warn = Some("#778899".to_string());
        let colors = theme.colors.to_ratatui_colors();

        assert_eq!(colors.status_bar_bg, Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(colors.status_bar_text, Color::Rgb(0x44, 0x55, 0x66));
        assert_eq!(colors.status_bar_warn, Color::Rgb(0x77, 0x88, 0x99));
    }
}
