//! Lightweight turn gating for natural user utterances (requests, constraints).
//!
//! POI **extraction and summarization** is handled by the session-end LLM (`llm.rs`).
//! This module only decides whether a user turn is substantive enough to include in the
//! session transcript / buffer — no keyword-domain inference.

use regex::Regex;

lazy_static::lazy_static! {
    static ref TASK_CAPTURE_RES: Vec<Regex> = vec![
        Regex::new(
            r"(?:请|麻烦)?(?:帮我|帮忙|请帮(?:我)?)(?:看看|查(?:一下|下)?|分析(?:一下)?|了解|推荐|规划|选|配置|写|做|处理|解答|对比|评估|算|估|找|挑)",
        )
        .unwrap(),
        Regex::new(
            r"(?:给我|帮我)(?:推荐|写|找|算|估|选|配|出)(?:一下|个|些)?",
        )
        .unwrap(),
        Regex::new(
            r"(?:我|我们)?(?:想|需要|希望|打算|想要)(?:了解|知道|咨询|请教|搞懂|弄清楚|看看|查(?:一下)?|买|投|学)",
        )
        .unwrap(),
        Regex::new(
            r"(?:请问|想问(?:一下)?|能不能|可不可以|麻烦)(?:帮我|帮忙|帮)?",
        )
        .unwrap(),
        Regex::new(
            r"(?:怎么|如何|怎样|为啥|为什么)(?:做|选|买|投|配|搞|弄|办|查|看|规划|申请)",
        )
        .unwrap(),
        Regex::new(
            r"(?:我的)?(?:情况|背景|需求|约束|条件是|现状是|诉求是)[:：]",
        )
        .unwrap(),
        Regex::new(
            r"(?:另外|补充(?:一下)?|还有一点|再补充)",
        )
        .unwrap(),
        Regex::new(
            r"(?i)\b(?:please\s+)?(?:help me|can you|could you)\s+(check|analyze|analyse|plan|recommend|figure out|understand|review|compare|advise on|find|pick|choose)\b",
        )
        .unwrap(),
        Regex::new(
            r"(?i)\b(?:my|our)\s+(?:situation|constraints|requirements|background|profile)\s+(?:is|are)\b",
        )
        .unwrap(),
        Regex::new(
            r"(?i)\bi\s+(?:want|need)\s+to\s+(understand|learn about|plan for|figure out|invest in|buy)\b",
        )
        .unwrap(),
    ];
}

/// Fast gate: does this utterance look like a real request or constraint dump?
pub fn has_task_or_domain_signal(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if TASK_CAPTURE_RES.iter().any(|re| re.is_match(trimmed)) {
        return true;
    }
    looks_like_constraint_dump(trimmed)
}

fn looks_like_constraint_dump(text: &str) -> bool {
    let clauses = text
        .split(['；', ';', '，', ','])
        .filter(|c| c.trim().chars().count() >= 4)
        .count();
    if clauses >= 3 {
        return true;
    }
    let has_amount = text.contains('万')
        || text.contains("积蓄")
        || text.contains("预算")
        || text.contains("收入")
        || lower_has_year_horizon(text);
    clauses >= 2 && has_amount
}

fn lower_has_year_horizon(text: &str) -> bool {
    text.contains('年')
        && (text.contains("投资")
            || text.contains("规划")
            || text.contains('期')
            || text.contains("退休"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_task_gate() {
        assert!(has_task_or_domain_signal(
            "帮我看看当前时间上A股行情怎么样了？"
        ));
        assert!(has_task_or_domain_signal(
            "这30万是我的全部积蓄；投资期5年；要稳健点"
        ));
        assert!(has_task_or_domain_signal("请问怎么选一款适合新手的基金？"));
        assert!(has_task_or_domain_signal("最近想买房，预算200万左右"));
        assert!(!has_task_or_domain_signal("ok"));
    }
}
