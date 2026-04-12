use crate::theme::Theme;

pub fn resolve_theme(name: &str) -> Theme {
    match name {
        "light" => crate::theme::light_theme(),
        _ => Theme::default(),
    }
}
