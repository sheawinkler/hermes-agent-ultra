//! Minimal SVG gauge (P2 stub — full dim_viz deferred).

/// Render a simple score gauge SVG.
#[must_use]
pub fn render_svg_gauge(score: f64, max: f64) -> String {
    let pct = (score / max * 100.0).clamp(0.0, 100.0);
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"120\" height=\"24\" viewBox=\"0 0 120 24\">\
         <rect x=\"0\" y=\"8\" width=\"120\" height=\"8\" fill=\"#eee\"/>\
         <rect x=\"0\" y=\"8\" width=\"{pct:.1}\" height=\"8\" fill=\"#4a90d9\"/>\
         <text x=\"60\" y=\"6\" text-anchor=\"middle\" font-size=\"10\">{score:.0}/{max:.0}</text>\
         </svg>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_contains_score() {
        let s = render_svg_gauge(72.0, 100.0);
        assert!(s.contains("72/100"));
    }
}
