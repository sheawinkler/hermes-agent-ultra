//! Generic fetcher for dimensions delegated to Hermes `web_search`.

use async_trait::async_trait;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimResult, FetcherSpec, Market};
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;

pub struct WebSkippedFetcher {
    spec: FetcherSpec,
    note: &'static str,
}

impl WebSkippedFetcher {
    #[must_use]
    pub const fn new(spec: FetcherSpec, note: &'static str) -> Self {
        Self { spec, note }
    }
}

#[async_trait]
impl DimFetcher for WebSkippedFetcher {
    fn spec(&self) -> &FetcherSpec {
        &self.spec
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        DimResult::skipped(self.spec.dim_key, &ctx.symbol, self.note)
    }
}

pub fn macro_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::MACRO,
            depends_on: &[dim_keys::BASIC],
            markets: &[Market::A, Market::H, Market::U],
            sources: &["web_search"],
            web_only: true,
        },
        "宏观维度用 web_search + 权威域（gov.cn/pbc/stats）",
    )
}

pub fn chain_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::CHAIN,
            depends_on: &[],
            markets: &[Market::A, Market::H],
            sources: &["web_search", "ths_f10"],
            web_only: true,
        },
        "产业链需 Playwright/定性搜索，Rust 不抓取",
    )
}

pub fn research_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::RESEARCH,
            depends_on: &[],
            markets: &[Market::A, Market::H, Market::U],
            sources: &["web_search", "em_data"],
            web_only: true,
        },
        "券商研报用 web_search",
    )
}

pub fn materials_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::MATERIALS,
            depends_on: &[],
            markets: &[Market::A],
            sources: &["web_search", "100ppi"],
            web_only: true,
        },
        "原材料现货价用 web_search",
    )
}

pub fn futures_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::FUTURES,
            depends_on: &[dim_keys::BASIC],
            markets: &[Market::A],
            sources: &["web_search", "cfachina"],
            web_only: true,
        },
        "期货关联用 web_search",
    )
}

pub fn governance_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::GOVERNANCE,
            depends_on: &[],
            markets: &[Market::A, Market::H],
            sources: &["web_search", "cninfo"],
            web_only: true,
        },
        "治理结构用 web_search / 公告",
    )
}

pub fn policy_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::POLICY,
            depends_on: &[dim_keys::BASIC],
            markets: &[Market::A, Market::H],
            sources: &["web_search"],
            web_only: true,
        },
        "政策维度用 web_search site:gov.cn/csrc",
    )
}

pub fn moat_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::MOAT,
            depends_on: &[],
            markets: &[Market::A, Market::H, Market::U],
            sources: &["web_search"],
            web_only: true,
        },
        "护城河定性评估用 web_search + LLM",
    )
}

pub fn events_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::EVENTS,
            depends_on: &[],
            markets: &[Market::A, Market::H, Market::U],
            sources: &["web_search", "em_kuaixun"],
            web_only: true,
        },
        "事件/新闻用 web_search",
    )
}

pub fn sentiment_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::SENTIMENT,
            depends_on: &[],
            markets: &[Market::A, Market::H, Market::U],
            sources: &["web_search", "guba_em_list"],
            web_only: true,
        },
        "舆情用 web_search",
    )
}

pub fn trap_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::TRAP,
            depends_on: &[],
            markets: &[Market::A],
            sources: &["web_search"],
            web_only: true,
        },
        "杀猪盘信号规则可移植，数据源仍靠 web_search",
    )
}

pub fn contests_fetcher() -> WebSkippedFetcher {
    WebSkippedFetcher::new(
        FetcherSpec {
            dim_key: dim_keys::CONTESTS,
            depends_on: &[],
            markets: &[Market::A, Market::H],
            sources: &["web_search"],
            web_only: true,
        },
        "实盘大赛/社区热度用 web_search",
    )
}
