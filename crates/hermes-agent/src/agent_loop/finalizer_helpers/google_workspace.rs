fn google_workspace_auth_blocker_mutation_guard(
    messages: &[Message],
    tool_calls: &[ToolCall],
) -> Option<&'static str> {
    if !detect_google_workspace_intent(messages)
        || !history_includes_google_workspace_auth_blocker(messages)
    {
        return None;
    }
    let attempts_mutation = tool_calls.iter().any(|call| {
        let name = call.function.name.as_str();
        let args = call.function.arguments.to_ascii_lowercase();
        matches!(
            name,
            "write_file" | "patch" | "apply_patch" | "skill_manage"
        ) || args.contains("--client-secret")
            || args.contains("--auth-url")
            || args.contains("--auth-code")
            || args.contains("google_client_secret.json")
            || args.contains("simulated")
            || args.contains("fake")
            || args.contains("dummy")
    });
    attempts_mutation.then_some(
        "[SYSTEM] Google Workspace auth blocker already observed. This request is a read-only Gmail backfill, not a setup flow. \
         Do not create simulated OAuth clients, write credential files, patch skills, run `--client-secret`, run `--auth-url`, or run `--auth-code`. \
         Final answer must be `GOOGLE_WORKSPACE_USED: no` with the exact NOT_AUTHENTICATED/no-token command output and next legitimate setup command for the user.",
    )
}

fn finalizer_google_workspace_requires_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_GOOGLE_WORKSPACE_MAX_RETRIES
        || !detect_google_workspace_intent(messages)
    {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let marker_text = lower.replace('*', "");
    if !marker_text.contains("google_workspace_used: yes")
        && !marker_text.contains("google_workspace_used: no")
        && !marker_text.contains("google_workspace_used=yes")
        && !marker_text.contains("google_workspace_used=no")
    {
        return true;
    }
    let claims_blocked = [
        "blocked",
        "cannot",
        "no viable",
        "no google",
        "no gmail",
        "not authenticated",
        "no credentials",
        "credentials verification",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let claims_absent_despite_skill = history_includes_google_workspace_skill(messages)
        && [
            "no google workspace",
            "no google/gmail",
            "no gmail/email api",
            "no tools",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    if claims_absent_despite_skill {
        return true;
    }
    let claims_success = [
        "google_workspace_used: yes",
        "emails were found",
        "important emails",
        "gmail search and reading were successful",
        "authenticated and working",
        "full message text",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if claims_success {
        return history_includes_google_workspace_auth_blocker(messages)
            || !history_includes_gmail_api_probe(messages);
    }
    if !claims_blocked {
        return false;
    }
    let has_final_evidence = (lower.contains("setup.py")
        || lower.contains("google_api.py")
        || lower.contains("google_token.json"))
        && (lower.contains("cmd=") || lower.contains("command="));
    !(has_final_evidence && history_includes_google_workspace_setup_probe(messages))
}

