use super::*;

fn codex_jwt_with_account(account_id: Option<&str>) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = match account_id {
        Some(account_id) => serde_json::json!({
            "sub": "user-xyz",
            "exp": 9_999_999_999_i64,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": "plus"
            }
        }),
        None => serde_json::json!({
            "sub": "user-xyz",
            "exp": 9_999_999_999_i64
        }),
    };
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    format!("{header}.{payload}.sig")
}

fn header_value<'a>(headers: &'a [(String, String)], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

include!("tests/codex_generic.rs");

include!("tests/profile_sanitization.rs");

include!("tests/openai_parsing.rs");

include!("tests/anthropic_requests.rs");

include!("tests/anthropic_openrouter_parsing.rs");
