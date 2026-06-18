//! Investor metadata (id, name, group, school).

#[derive(Debug, Clone, Copy)]
pub struct InvestorMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub group: char,
    pub market_scope: MarketScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketScope {
    All,
    AShareOnly,
}

/// All investors (66 in UZI v3.9).
pub static INVESTORS: &[InvestorMeta] = &[
    // A · classic value
    InvestorMeta {
        id: "buffett",
        name: "沃伦·巴菲特",
        group: 'A',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "graham",
        name: "本杰明·格雷厄姆",
        group: 'A',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "fisher",
        name: "菲利普·费雪",
        group: 'A',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "munger",
        name: "查理·芒格",
        group: 'A',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "templeton",
        name: "约翰·邓普顿",
        group: 'A',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "klarman",
        name: "塞思·卡拉曼",
        group: 'A',
        market_scope: MarketScope::All,
    },
    // B · growth
    InvestorMeta {
        id: "lynch",
        name: "彼得·林奇",
        group: 'B',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "oneill",
        name: "威廉·欧奈尔",
        group: 'B',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "wood",
        name: "凯茜·伍德",
        group: 'B',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "thiel",
        name: "彼得·蒂尔",
        group: 'B',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "andreessen",
        name: "马克·安德森",
        group: 'B',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "gurley",
        name: "比尔·格利",
        group: 'B',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "naval",
        name: "Naval",
        group: 'B',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "gerstner",
        name: "郭士纳",
        group: 'B',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "chamath",
        name: "Chamath",
        group: 'B',
        market_scope: MarketScope::All,
    },
    // C · macro
    InvestorMeta {
        id: "soros",
        name: "乔治·索罗斯",
        group: 'C',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "dalio",
        name: "瑞·达利欧",
        group: 'C',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "marks",
        name: "霍华德·马克斯",
        group: 'C',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "druck",
        name: "斯坦·德鲁肯米勒",
        group: 'C',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "robertson",
        name: "朱利安·罗伯逊",
        group: 'C',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "burry",
        name: "迈克尔·伯里",
        group: 'C',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "chanos",
        name: "吉姆·查诺斯",
        group: 'C',
        market_scope: MarketScope::All,
    },
    // D · technical
    InvestorMeta {
        id: "livermore",
        name: "杰西·利弗莫尔",
        group: 'D',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "minervini",
        name: "Mark Minervini",
        group: 'D',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "darvas",
        name: "Nicolas Darvas",
        group: 'D',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "gann",
        name: "江恩",
        group: 'D',
        market_scope: MarketScope::All,
    },
    // E · China value
    InvestorMeta {
        id: "duan",
        name: "段永平",
        group: 'E',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "zhangkun",
        name: "张坤",
        group: 'E',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "zhushaoxing",
        name: "朱少醒",
        group: 'E',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "xiezhiyu",
        name: "谢治宇",
        group: 'E',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "fengliu",
        name: "冯柳",
        group: 'E',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "dengxiaofeng",
        name: "邓晓峰",
        group: 'E',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "zhang_lei",
        name: "张磊",
        group: 'E',
        market_scope: MarketScope::All,
    },
    // F · 游资 (A-share only)
    InvestorMeta {
        id: "zhang_mz",
        name: "章盟主",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "zhao_lg",
        name: "赵老哥",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "fs_wyj",
        name: "佛山无影脚",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "yangjia",
        name: "养家",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "ghzw",
        name: "股海贼王",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    // G · quant
    InvestorMeta {
        id: "simons",
        name: "詹姆斯·西蒙斯",
        group: 'G',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "thorp",
        name: "Ed Thorp",
        group: 'G',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "shaw",
        name: "David Shaw",
        group: 'G',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "asness",
        name: "Cliff Asness",
        group: 'G',
        market_scope: MarketScope::All,
    },
    // H · tech leaders
    InvestorMeta {
        id: "serenity",
        name: "Serenity",
        group: 'H',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "jensen_huang",
        name: "黄仁勋",
        group: 'H',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "musk",
        name: "马斯克",
        group: 'H',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "altman",
        name: "Sam Altman",
        group: 'H',
        market_scope: MarketScope::All,
    },
    InvestorMeta {
        id: "saylor",
        name: "Michael Saylor",
        group: 'H',
        market_scope: MarketScope::All,
    },
    // Additional F-group youzi (wave 3)
    InvestorMeta {
        id: "bj_cj",
        name: "北京炒家",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "chen_xq",
        name: "陈小群",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "fang_xx",
        name: "方新侠",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "gu_bl",
        name: "古北路",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "hu_jl",
        name: "胡金镠",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "jiao_yy",
        name: "焦扬",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "lasa",
        name: "拉萨",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "liu_sh",
        name: "刘上海",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "liuyi_zl",
        name: "六一中路",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "mao_lb",
        name: "毛老板",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "ningbo_st",
        name: "宁波桑田",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "sun_ge",
        name: "孙哥",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "sunan",
        name: "苏南",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "wang_zr",
        name: "王座荣",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "xiao_ey",
        name: "小鳄鱼",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "xiao_xian",
        name: "小仙",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "xin_dd",
        name: "新丁",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "zuoshou",
        name: "作手新一",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
    InvestorMeta {
        id: "chengdu",
        name: "成都系",
        group: 'F',
        market_scope: MarketScope::AShareOnly,
    },
];

pub fn find_investor(id: &str) -> Option<&'static InvestorMeta> {
    INVESTORS.iter().find(|i| i.id == id)
}

pub fn locked_school() -> Option<char> {
    std::env::var("HERMES_SCHOOL")
        .or_else(|_| std::env::var("UZI_SCHOOL"))
        .ok()
        .and_then(|s| s.trim().chars().next())
        .filter(|c| c.is_ascii_uppercase())
}
