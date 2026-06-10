//! Environment-driven configuration for meta-search.

/// Which CN HTML engines are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CnEngineKind {
    Sogou,
    BingCn,
}

impl CnEngineKind {
    pub fn parse_name(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sogou" => Some(Self::Sogou),
            "bing_cn" | "bing-cn" | "bingcn" | "bing" => Some(Self::BingCn),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Sogou => "sogou",
            Self::BingCn => "bing_cn",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetaSearchConfig {
    pub cn_engines: Vec<CnEngineKind>,
    pub cn_timeout_secs: u64,
    pub global_timeout_secs: u64,
    pub cn_weight: f64,
    /// When set (tests only), CN engines fetch from this origin instead of production hosts.
    pub cn_base_url_override: Option<String>,
}

impl Default for MetaSearchConfig {
    fn default() -> Self {
        Self {
            cn_engines: vec![CnEngineKind::Sogou, CnEngineKind::BingCn],
            cn_timeout_secs: 8,
            global_timeout_secs: 12,
            cn_weight: 1.25,
            cn_base_url_override: None,
        }
    }
}

impl MetaSearchConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(raw) = std::env::var("HERMES_CN_SEARCH_ENGINES") {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                cfg.cn_engines.clear();
            } else {
                cfg.cn_engines = trimmed
                    .split(',')
                    .filter_map(CnEngineKind::parse_name)
                    .collect();
            }
        }
        if let Some(v) = parse_positive_secs("HERMES_CN_SEARCH_TIMEOUT_SECS", 30) {
            cfg.cn_timeout_secs = v;
        }
        if let Some(v) = parse_positive_secs("HERMES_META_SEARCH_TIMEOUT_SECS", 60) {
            cfg.global_timeout_secs = v;
        }
        if let Ok(raw) = std::env::var("HERMES_META_SEARCH_CN_WEIGHT")
            && let Ok(w) = raw.trim().parse::<f64>()
            && w > 0.0
        {
            cfg.cn_weight = w;
        }
        if let Ok(raw) = std::env::var("HERMES_CN_SEARCH_BASE_URL_OVERRIDE") {
            let t = raw.trim();
            if !t.is_empty() {
                cfg.cn_base_url_override = Some(t.trim_end_matches('/').to_string());
            }
        }
        cfg
    }
}

fn parse_positive_secs(name: &str, max: u64) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(|v| v.min(max))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        keys: Vec<&'static str>,
    }

    impl EnvGuard {
        fn new() -> Self {
            for k in [
                "HERMES_CN_SEARCH_ENGINES",
                "HERMES_CN_SEARCH_TIMEOUT_SECS",
                "HERMES_META_SEARCH_TIMEOUT_SECS",
                "HERMES_META_SEARCH_CN_WEIGHT",
                "HERMES_CN_SEARCH_BASE_URL_OVERRIDE",
            ] {
                hermes_core::test_env::remove_var(k);
            }
            Self {
                keys: vec![
                    "HERMES_CN_SEARCH_ENGINES",
                    "HERMES_CN_SEARCH_TIMEOUT_SECS",
                    "HERMES_META_SEARCH_TIMEOUT_SECS",
                    "HERMES_META_SEARCH_CN_WEIGHT",
                    "HERMES_CN_SEARCH_BASE_URL_OVERRIDE",
                ],
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for k in &self.keys {
                hermes_core::test_env::remove_var(k);
            }
        }
    }

    #[test]
    fn defaults_include_sogou_and_bing_cn() {
        let _g = EnvGuard::new();
        let cfg = MetaSearchConfig::from_env();
        assert_eq!(
            cfg.cn_engines,
            vec![CnEngineKind::Sogou, CnEngineKind::BingCn]
        );
        assert_eq!(cfg.cn_timeout_secs, 8);
        assert_eq!(cfg.global_timeout_secs, 12);
    }

    #[test]
    fn empty_cn_engines_env_disables_cn() {
        let _g = EnvGuard::new();
        hermes_core::test_env::set_var("HERMES_CN_SEARCH_ENGINES", "");
        let cfg = MetaSearchConfig::from_env();
        assert!(cfg.cn_engines.is_empty());
    }

    #[test]
    fn parses_engine_list_and_filters_unknown() {
        let _g = EnvGuard::new();
        hermes_core::test_env::set_var("HERMES_CN_SEARCH_ENGINES", "sogou,unknown,bing_cn");
        let cfg = MetaSearchConfig::from_env();
        assert_eq!(
            cfg.cn_engines,
            vec![CnEngineKind::Sogou, CnEngineKind::BingCn]
        );
    }

    #[test]
    fn timeout_clamp() {
        let _g = EnvGuard::new();
        hermes_core::test_env::set_var("HERMES_CN_SEARCH_TIMEOUT_SECS", "999");
        hermes_core::test_env::set_var("HERMES_META_SEARCH_TIMEOUT_SECS", "5");
        let cfg = MetaSearchConfig::from_env();
        assert_eq!(cfg.cn_timeout_secs, 30);
        assert_eq!(cfg.global_timeout_secs, 5);
    }

    #[test]
    fn cn_weight_override() {
        let _g = EnvGuard::new();
        hermes_core::test_env::set_var("HERMES_META_SEARCH_CN_WEIGHT", "2.0");
        let cfg = MetaSearchConfig::from_env();
        assert_eq!(cfg.cn_weight, 2.0);
    }
}
