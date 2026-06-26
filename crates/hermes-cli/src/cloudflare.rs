use hermes_core::AgentError;
use regex::Regex;
use serde::Serialize;
use std::io::Read;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemporaryDeployFacts {
    pub live_url: Option<String>,
    pub claim_url: Option<String>,
    pub account: Option<String>,
    pub account_state: Option<String>,
    pub expires_minutes: Option<u32>,
    pub deployed: bool,
}

fn first_match(pattern: &Regex, text: &str) -> Option<String> {
    pattern.find(text).map(|m| {
        m.as_str()
            .trim_end_matches(&['.', ',', ')', ';', ']'][..])
            .to_string()
    })
}

pub fn parse_temporary_deploy_output(text: &str) -> TemporaryDeployFacts {
    let live_url =
        Regex::new(r"https://[A-Za-z0-9._-]+\.workers\.dev\S*").expect("valid live URL regex");
    let claim_url =
        Regex::new(r"(?i)https://\S*claim\S*claimToken=\S*").expect("valid claim URL regex");
    let account = Regex::new(r"(?i)Account:\s*(?P<name>.+?)\s*\((?P<state>created|reused)\)")
        .expect("valid account regex");
    let claim_within =
        Regex::new(r"(?i)Claim within:\s*(?P<minutes>\d+)\s*minutes?").expect("valid claim regex");
    let deployed = Regex::new(r"(?im)^\s*(Deployed|Uploaded)\b").expect("valid deploy regex");

    let account_match = account.captures(text);
    let expires_minutes = claim_within
        .captures(text)
        .and_then(|caps| caps.name("minutes"))
        .and_then(|m| m.as_str().parse::<u32>().ok());

    TemporaryDeployFacts {
        live_url: first_match(&live_url, text),
        claim_url: first_match(&claim_url, text),
        account: account_match
            .as_ref()
            .and_then(|caps| caps.name("name"))
            .map(|m| m.as_str().trim().to_string()),
        account_state: account_match
            .as_ref()
            .and_then(|caps| caps.name("state"))
            .map(|m| m.as_str().to_ascii_lowercase()),
        expires_minutes,
        deployed: deployed.is_match(text),
    }
}

fn cloudflare_help() -> &'static str {
    "Usage:\n  hermes cloudflare parse-temporary-deploy-output < wrangler.log\n  npx wrangler@latest deploy --temporary 2>&1 | hermes cloudflare parse-temporary-deploy-output\n  hermes cloudflare --selftest\n\nActions:\n  parse-temporary-deploy-output   Parse wrangler temporary deploy output into JSON\n  parse-temporary-deploy          Alias for parse-temporary-deploy-output\n  parse                           Alias for parse-temporary-deploy-output"
}

fn run_selftest() -> Result<(), AgentError> {
    const SAMPLE: &str = r#"
Continuing means you accept Cloudflare's Terms of Service and Privacy Policy.

Temporary account ready:
     Account:        example-name (created)
     Claim within:   60 minutes
     Claim URL:      https://dash.cloudflare.com/claim-preview?claimToken=abc123XYZ

Uploaded example-worker
Deployed example-worker triggers
     https://example-worker.example-name.workers.dev
"#;
    let facts = parse_temporary_deploy_output(SAMPLE);
    if facts.live_url.as_deref() != Some("https://example-worker.example-name.workers.dev")
        || facts.claim_url.as_deref()
            != Some("https://dash.cloudflare.com/claim-preview?claimToken=abc123XYZ")
        || facts.account.as_deref() != Some("example-name")
        || facts.account_state.as_deref() != Some("created")
        || facts.expires_minutes != Some(60)
        || !facts.deployed
    {
        return Err(AgentError::Config(format!(
            "Cloudflare temporary deploy parser selftest failed: {:?}",
            facts
        )));
    }
    println!("selftest: OK");
    Ok(())
}

pub async fn handle_cli_cloudflare(
    action: Option<String>,
    selftest: bool,
) -> Result<(), AgentError> {
    if selftest || action.as_deref() == Some("selftest") {
        return run_selftest();
    }

    match action.as_deref().unwrap_or("help") {
        "parse-temporary-deploy-output" | "parse-temporary-deploy" | "parse" => {
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|e| AgentError::Io(format!("read stdin: {}", e)))?;
            let facts = parse_temporary_deploy_output(&input);
            println!(
                "{}",
                serde_json::to_string_pretty(&facts)
                    .map_err(|e| AgentError::Config(format!("serialize parse result: {}", e)))?
            );
            if facts.live_url.is_some() {
                Ok(())
            } else {
                Err(AgentError::Config(
                    "wrangler output did not contain a workers.dev live URL".to_string(),
                ))
            }
        }
        "help" | "-h" | "--help" => {
            println!("{}", cloudflare_help());
            Ok(())
        }
        other => Err(AgentError::Config(format!(
            "Unknown cloudflare action '{}'.\n{}",
            other,
            cloudflare_help()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CREATED: &str = r#"
Temporary account ready:
     Account:        Serene Temple (created)
     Claim within:   60 minutes
     Claim URL:      https://dash.cloudflare.com/claim-preview?claimToken=abc123XYZ

Uploaded example-worker
Deployed example-worker triggers
     https://example-worker.serene-temple.workers.dev
"#;

    const SAMPLE_REUSED: &str = r#"
Temporary account ready:
     Account:        example-name (reused)
     Claim within:   42 minutes
     Claim URL:      https://dash.cloudflare.com/claim-preview?claimToken=def456
Deployed example-worker triggers
     https://example-worker.example-name.workers.dev
"#;

    const SAMPLE_NO_TEMP: &str = r#"
Error: You are not logged in.
To continue without logging in, rerun this command with `--temporary`.
"#;

    #[test]
    fn parses_created_temporary_deploy_output() {
        let facts = parse_temporary_deploy_output(SAMPLE_CREATED);
        assert_eq!(
            facts.live_url.as_deref(),
            Some("https://example-worker.serene-temple.workers.dev")
        );
        assert_eq!(
            facts.claim_url.as_deref(),
            Some("https://dash.cloudflare.com/claim-preview?claimToken=abc123XYZ")
        );
        assert_eq!(facts.account.as_deref(), Some("Serene Temple"));
        assert_eq!(facts.account_state.as_deref(), Some("created"));
        assert_eq!(facts.expires_minutes, Some(60));
        assert!(facts.deployed);
    }

    #[test]
    fn parses_reused_temporary_deploy_output() {
        let facts = parse_temporary_deploy_output(SAMPLE_REUSED);
        assert_eq!(
            facts.live_url.as_deref(),
            Some("https://example-worker.example-name.workers.dev")
        );
        assert_eq!(facts.account_state.as_deref(), Some("reused"));
        assert_eq!(facts.expires_minutes, Some(42));
        assert!(facts.deployed);
    }

    #[test]
    fn missing_temporary_deploy_facts_stays_structured() {
        let facts = parse_temporary_deploy_output(SAMPLE_NO_TEMP);
        assert_eq!(facts.live_url, None);
        assert_eq!(facts.claim_url, None);
        assert_eq!(facts.account, None);
        assert!(!facts.deployed);
    }

    #[test]
    fn trims_url_trailing_punctuation() {
        let facts = parse_temporary_deploy_output(
            "Deployed demo\nhttps://demo.temp.workers.dev).\nClaim URL: https://dash.cloudflare.com/claim-preview?claimToken=tok];",
        );
        assert_eq!(
            facts.live_url.as_deref(),
            Some("https://demo.temp.workers.dev")
        );
        assert_eq!(
            facts.claim_url.as_deref(),
            Some("https://dash.cloudflare.com/claim-preview?claimToken=tok")
        );
    }
}
