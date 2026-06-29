use hermes_billing::Language;

pub fn render_output_directive(template: &str, user_locale: Language) -> String {
    template.replace("{{user_locale}}", user_locale.tag())
}

pub fn default_output_directive(user_locale: Language) -> String {
    render_output_directive(
        "Always respond in {{user_locale}}. Use markdown.",
        user_locale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_user_locale() {
        let rendered =
            render_output_directive("Always respond in {{user_locale}}.", Language::ZhCN);
        assert_eq!(rendered, "Always respond in zh-CN.");
    }
}
