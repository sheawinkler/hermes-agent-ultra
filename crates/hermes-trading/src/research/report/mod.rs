//! Minimal HTML report (P0.5) + SVG stub (P2).

pub mod chat_brief;
pub mod content;
pub mod dim_charts;
pub mod dim_viz;
pub mod disk;
pub mod html;
pub mod html_acceptance;
pub mod identity;
pub mod institutional;
pub mod labels;
pub mod markdown;
pub mod quick_scan;
pub mod sections;
pub mod styles;
pub mod svg;

pub use chat_brief::render_chat_brief_markdown;
pub use content::{
    ExternalContextOverlay, ReportContent, build_report_content, merge_external_overlay,
    merge_macro_dim_from_overlay, merge_web_dims_from_overlay, needs_external_web_fill,
    refresh_web_dim_labels, web_dim_has_fill, web_dim_summary,
};
pub use disk::{WrittenReportPaths, write_equity_report};
pub use html::render_html_report;
pub use identity::{ReportIdentity, infer_target_name_from_peers};
pub use institutional::render_institutional_html;
pub use markdown::render_summary_markdown;
pub use quick_scan::render_quick_scan_markdown;
pub use svg::{render_svg_gauge, render_svg_percentile};
