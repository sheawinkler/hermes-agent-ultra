//! Domain taxonomy for LLM-guided POI extraction (hints only — not keyword matchers).

/// One durable interest domain the LLM may infer (open-ended; not an exhaustive enum).
#[derive(Debug, Clone, Copy)]
pub struct DomainTaxon {
    pub key: &'static str,
    pub label_zh: &'static str,
    pub label_en: &'static str,
    pub examples: &'static str,
}

/// Expanded domain catalog — semantic guidance for the interest LLM, not rule triggers.
pub const DOMAIN_TAXONOMY: &[DomainTaxon] = &[
    DomainTaxon {
        key: "personal-finance",
        label_zh: "个人理财与资产配置",
        label_en: "Personal finance & investing",
        examples: "stocks, funds, retirement planning, risk tolerance, portfolio allocation",
    },
    DomainTaxon {
        key: "tax-accounting",
        label_zh: "税务与会计",
        label_en: "Tax & accounting",
        examples: "income tax, deductions, invoicing, bookkeeping, VAT",
    },
    DomainTaxon {
        key: "insurance",
        label_zh: "保险规划",
        label_en: "Insurance planning",
        examples: "health/life/auto insurance, coverage comparison, claims",
    },
    DomainTaxon {
        key: "real-estate",
        label_zh: "房产与居住",
        label_en: "Real estate & housing",
        examples: "buy/rent, mortgage, renovation, school districts",
    },
    DomainTaxon {
        key: "legal-compliance",
        label_zh: "法律与合规",
        label_en: "Legal & compliance",
        examples: "contracts, labor law, IP, privacy/GDPR, disputes",
    },
    DomainTaxon {
        key: "health-wellness",
        label_zh: "健康与医疗",
        label_en: "Health & medical",
        examples: "symptoms, checkups, chronic care, diet, sleep",
    },
    DomainTaxon {
        key: "mental-health",
        label_zh: "心理健康",
        label_en: "Mental health",
        examples: "stress, anxiety, therapy, burnout, relationships",
    },
    DomainTaxon {
        key: "career-growth",
        label_zh: "职业发展与求职",
        label_en: "Career & job search",
        examples: "resume, interviews, salary negotiation, promotion",
    },
    DomainTaxon {
        key: "startup-business",
        label_zh: "创业与商业经营",
        label_en: "Startup & business ops",
        examples: "business model, fundraising, ops, partnerships",
    },
    DomainTaxon {
        key: "marketing-growth",
        label_zh: "营销与增长",
        label_en: "Marketing & growth",
        examples: "SEO, ads, branding, conversion, content strategy",
    },
    DomainTaxon {
        key: "education-learning",
        label_zh: "学习与考试",
        label_en: "Education & exams",
        examples: "courses, certifications, study plans, grad school",
    },
    DomainTaxon {
        key: "software-engineering",
        label_zh: "软件工程与开发",
        label_en: "Software engineering",
        examples: "coding, architecture, debugging, DevOps, APIs",
    },
    DomainTaxon {
        key: "design-creative",
        label_zh: "设计与创意",
        label_en: "Design & creative work",
        examples: "UI/UX, visual design, writing, video editing",
    },
    DomainTaxon {
        key: "science-research",
        label_zh: "科研与学术",
        label_en: "Science & research",
        examples: "experiments, papers, lab work, data analysis",
    },
    DomainTaxon {
        key: "travel-lifestyle",
        label_zh: "旅行与生活安排",
        label_en: "Travel & lifestyle planning",
        examples: "itineraries, visas, flights, local life",
    },
    DomainTaxon {
        key: "automotive",
        label_zh: "汽车与出行",
        label_en: "Automotive & mobility",
        examples: "car buying, EVs, maintenance, driving license",
    },
    DomainTaxon {
        key: "parenting-family",
        label_zh: "育儿与家庭",
        label_en: "Parenting & family",
        examples: "childcare, schooling, family logistics",
    },
    DomainTaxon {
        key: "elderly-care",
        label_zh: "养老与照护",
        label_en: "Elder care",
        examples: "aging parents, nursing, retirement communities",
    },
    DomainTaxon {
        key: "consumer-shopping",
        label_zh: "消费与选购",
        label_en: "Consumer purchases",
        examples: "product comparison, value for money, reviews",
    },
    DomainTaxon {
        key: "food-cooking",
        label_zh: "美食与烹饪",
        label_en: "Food & cooking",
        examples: "recipes, meal prep, restaurants, nutrition goals",
    },
    DomainTaxon {
        key: "sports-fitness",
        label_zh: "运动与健身",
        label_en: "Sports & fitness",
        examples: "training plans, gear, events, recovery",
    },
    DomainTaxon {
        key: "entertainment-media",
        label_zh: "娱乐与媒体",
        label_en: "Entertainment & media",
        examples: "films, games, music, streaming, hobbies",
    },
    DomainTaxon {
        key: "pets",
        label_zh: "宠物养护",
        label_en: "Pets & animal care",
        examples: "pet health, training, breeds, vet visits",
    },
    DomainTaxon {
        key: "wedding-events",
        label_zh: "婚礼与活动策划",
        label_en: "Weddings & events",
        examples: "wedding planning, budgets, vendors, ceremonies",
    },
    DomainTaxon {
        key: "immigration",
        label_zh: "移民与签证",
        label_en: "Immigration & visas",
        examples: "visa types, PR paths, relocation paperwork",
    },
    DomainTaxon {
        key: "government-policy",
        label_zh: "政策与政务",
        label_en: "Government & public policy",
        examples: "subsidies, permits, local regulations, benefits",
    },
    DomainTaxon {
        key: "agriculture",
        label_zh: "农业与种植",
        label_en: "Agriculture",
        examples: "farming, crops, livestock, rural business",
    },
    DomainTaxon {
        key: "energy-climate",
        label_zh: "能源与环保",
        label_en: "Energy & climate",
        examples: "solar, carbon, sustainability, utilities",
    },
    DomainTaxon {
        key: "crypto-web3",
        label_zh: "加密与 Web3",
        label_en: "Crypto & Web3",
        examples: "blockchain, DeFi, wallets (non-advice)",
    },
];

/// Markdown-ish block injected into the interest LLM system prompt.
pub fn domain_taxonomy_prompt_block() -> String {
    let mut out = String::from(
        "Reference domain taxonomy (infer semantically — user need NOT match keywords; \
         invent a finer label when none fit exactly):\n",
    );
    for t in DOMAIN_TAXONOMY {
        out.push_str(&format!(
            "- {} / {} (`{}`): e.g. {}\n",
            t.label_zh, t.label_en, t.key, t.examples
        ));
    }
    out.push_str(
        "\nYou may output domains outside this list when the user's intent is clearly different.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_has_breadth() {
        assert!(DOMAIN_TAXONOMY.len() >= 25);
        assert!(domain_taxonomy_prompt_block().contains("personal-finance"));
        assert!(domain_taxonomy_prompt_block().contains("mental-health"));
    }
}
