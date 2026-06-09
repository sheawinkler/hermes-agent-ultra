//! Band metrics for work package payloads.

use hermes_insights::types::WorkMetricsPayload;

pub fn build_work_metrics(
    user_turns: u32,
    tool_failures: u32,
    skill_patch_count: u32,
) -> WorkMetricsPayload {
    WorkMetricsPayload {
        turn_band: band_turns(user_turns),
        duration_band: "unknown".to_string(),
        tool_failure_band: band_tool_failures(tool_failures),
        skill_patch_count_band: band_skill_patches(skill_patch_count),
    }
}

fn band_turns(turns: u32) -> String {
    match turns {
        0..=2 => "1-2".to_string(),
        3..=5 => "3-5".to_string(),
        6..=10 => "6-10".to_string(),
        _ => "11+".to_string(),
    }
}

fn band_tool_failures(failures: u32) -> String {
    match failures {
        0 => "0".to_string(),
        1..=2 => "1-2".to_string(),
        _ => "3+".to_string(),
    }
}

fn band_skill_patches(count: u32) -> String {
    match count {
        0 => "0".to_string(),
        1 => "1".to_string(),
        _ => "2+".to_string(),
    }
}
