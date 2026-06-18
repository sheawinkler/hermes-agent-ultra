//! Minimal HTML report (P0.5) + SVG stub (P2).

pub mod html;
pub mod svg;

pub use html::render_html_report;
pub use svg::render_svg_gauge;
