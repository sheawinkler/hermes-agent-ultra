impl App {
    async fn run_quorum_fanout_turn(
        &mut self,
        run_started_at: Instant,
        policy: QuorumPolicy,
    ) -> Result<bool, AgentError> {
        let quorum_contract = self.load_quorum_agent_contract_text();
        let (voter_models, model_resolution_notes) = self.resolve_quorum_models(&policy).await;
        for note in model_resolution_notes {
            Self::emit_lifecycle_event(&self.stream_handle_shared, note);
        }
        if voter_models.len() < 2 {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "quorum armed but only {} distinct model configured; falling back to normal run",
                    voter_models.len()
                ),
            );
            return Ok(false);
        }

        let (base_messages, reformulated) = self.build_inference_messages();
        if reformulated {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                "runtime prompt reformulation injected (anti-scheming + context + tool routing + contradiction self-check)",
            );
        }
        let original_model = self.current_model.clone();
        let mut outcomes: Vec<QuorumVoterOutcome> = Vec::new();
        let mut succeeded = 0usize;
        let output_char_cap = Self::quorum_output_char_cap();

        Self::emit_phase_event(
            &self.stream_handle_shared,
            "quorum",
            "multi-voter fan-out dispatch",
            30,
        );

        for (idx, model) in voter_models.iter().enumerate() {
            let display_index = idx + 1;
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "quorum voter {}/{} dispatch -> {}",
                    display_index,
                    voter_models.len(),
                    model
                ),
            );
            if self.current_model != *model {
                self.switch_model(model);
            }
            let force_refresh = display_index == 1 || Self::quorum_force_refresh_each_voter();
            self.refresh_runtime_provider_credentials_if_needed(force_refresh)
                .await;

            let started = Instant::now();
            let max_attempts = Self::quorum_voter_retry_limit();
            let voter_passes = Self::quorum_voter_passes();
            let mut pass_errors: Vec<String> = Vec::new();
            let mut combined_output = String::new();
            let mut combined_turns: u32 = 0;
            let mut combined_tool_errors: usize = 0;
            let mut last_err: Option<AgentError> = None;
            let mut toolless_fallback_used = false;
            let voter_tools_enabled = Self::quorum_voter_tools_enabled();

            for pass_idx in 0..voter_passes {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!(
                        "quorum voter {}/{} pass {}/{}",
                        display_index,
                        voter_models.len(),
                        pass_idx + 1,
                        voter_passes
                    ),
                );

                let mut system_sections = Vec::new();
                if let Some((contract_path, contract_text)) = quorum_contract.as_ref() {
                    system_sections.push(format!(
                        "[QUORUM_AGENT_CONTRACT]\npath={}\nApply this contract strictly for this voter pass:\n{}",
                        contract_path.display(),
                        contract_text
                    ));
                }
                system_sections.push(Self::build_quorum_voter_prompt(
                    pass_idx,
                    voter_passes,
                    model,
                ));
                let trailing_user_context = if pass_idx > 0 && !combined_output.trim().is_empty() {
                    Some(format!(
                        "[PRIOR_VOTER_DRAFT]\n{}\n\nCritique and strengthen this prior draft for pass {}/{}.",
                        combined_output,
                        pass_idx + 1,
                        voter_passes
                    ))
                } else {
                    None
                };
                let pass_messages = Self::compose_quorum_messages(
                    system_sections,
                    base_messages.clone(),
                    trailing_user_context,
                );

                let mut attempts = 0usize;
                let mut maybe_result: Option<hermes_core::AgentResult> = None;
                while attempts < max_attempts {
                    attempts += 1;
                    match self
                        .run_messages_with_current_agent_tools(
                            pass_messages.clone(),
                            false,
                            voter_tools_enabled,
                        )
                        .await
                    {
                        Ok(result) => {
                            maybe_result = Some(result);
                            break;
                        }
                        Err(err) => {
                            if Self::is_provider_tool_payload_error(&err)
                                && Self::quorum_toolless_provider_fallback_enabled()
                                && voter_tools_enabled
                                && !toolless_fallback_used
                            {
                                toolless_fallback_used = true;
                                pass_errors.push(format!(
                                    "pass {}: provider rejected tool schema on requested model; retried this voter pass without tool schemas",
                                    pass_idx + 1
                                ));
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "quorum voter {}/{} provider rejected tool schema; retrying this voter pass without tool schemas",
                                        display_index,
                                        voter_models.len()
                                    ),
                                );
                                match self
                                    .run_messages_with_current_agent_tools(
                                        pass_messages.clone(),
                                        false,
                                        false,
                                    )
                                    .await
                                {
                                    Ok(result) => {
                                        maybe_result = Some(result);
                                        break;
                                    }
                                    Err(fallback_err) => {
                                        last_err = Some(fallback_err);
                                        break;
                                    }
                                }
                            }
                            if Self::is_provider_auth_or_session_error(&err)
                                && attempts < max_attempts
                            {
                                let refreshed = self.force_auth_refresh_after_error().await;
                                if refreshed {
                                    continue;
                                }
                            }
                            if Self::is_transient_retryable_error(&err) && attempts < max_attempts {
                                let backoff_ms = (attempts as u64).saturating_mul(750).max(500);
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "quorum voter {}/{} transient error (attempt {}/{}): {} — retrying after {}ms",
                                        display_index,
                                        voter_models.len(),
                                        attempts,
                                        max_attempts,
                                        err,
                                        backoff_ms
                                    ),
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                                    .await;
                                continue;
                            }
                            last_err = Some(err);
                            break;
                        }
                    }
                }

                let Some(result) = maybe_result else {
                    if let Some(err) = &last_err {
                        pass_errors.push(format!("pass {}: {}", pass_idx + 1, err));
                    } else {
                        pass_errors.push(format!("pass {}: unknown error", pass_idx + 1));
                    }
                    break;
                };

                combined_turns = combined_turns.saturating_add(result.total_turns);
                combined_tool_errors =
                    combined_tool_errors.saturating_add(result.tool_errors.len());
                let latest = Self::extract_last_assistant_output(&result.messages);
                if !latest.trim().is_empty() {
                    combined_output = latest;
                } else {
                    pass_errors.push(format!("pass {}: empty assistant output", pass_idx + 1));
                    break;
                }
            }

            if !combined_output.trim().is_empty() {
                let output = Self::truncate_for_quorum(&combined_output, output_char_cap);
                let degraded = Self::quorum_output_is_degraded_non_answer(&output);
                let status = if output.trim().is_empty() {
                    "empty"
                } else if degraded {
                    pass_errors.push("voter returned degraded non-answer".to_string());
                    "degraded"
                } else {
                    succeeded += 1;
                    "ok"
                };
                let error = if !pass_errors.is_empty() {
                    Some(pass_errors.join(" | "))
                } else if output.trim().is_empty() {
                    Some("voter returned empty assistant output".to_string())
                } else {
                    None
                };
                outcomes.push(QuorumVoterOutcome {
                    model: model.clone(),
                    status: status.to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    total_turns: combined_turns,
                    tool_errors: combined_tool_errors,
                    output: output.clone(),
                    error,
                });
                if status == "ok" {
                    Self::emit_moa_reference_event(
                        &self.stream_handle_shared,
                        display_index,
                        voter_models.len(),
                        model,
                        &output,
                    );
                }
            } else {
                let err_text = last_err
                    .as_ref()
                    .map(ToString::to_string)
                    .or_else(|| (!pass_errors.is_empty()).then(|| pass_errors.join(" | ")))
                    .unwrap_or_else(|| "unknown voter error".to_string());
                outcomes.push(QuorumVoterOutcome {
                    model: model.clone(),
                    status: "error".to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    total_turns: combined_turns,
                    tool_errors: combined_tool_errors,
                    output: String::new(),
                    error: Some(err_text),
                });
            }
        }

        if self.current_model != original_model {
            self.switch_model(&original_model);
        }
        let synthesis_model = Self::quorum_synthesis_model_for_original(&original_model);
        let artifact_path = self.persist_quorum_artifact(&policy, &outcomes)?;
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("quorum voter artifact saved: {}", artifact_path.display()),
        );

        let required_success = Self::required_quorum_success(voter_models.len());
        if succeeded < required_success {
            let error_summary = outcomes
                .iter()
                .map(|o| {
                    format!(
                        "{} => {}",
                        o.model,
                        match (o.status.as_str(), o.error.as_deref()) {
                            ("ok", _) => "ok".to_string(),
                            ("empty", Some(e)) => format!("empty ({})", e),
                            (_, Some(e)) => e.to_string(),
                            _ => "unknown error".to_string(),
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(AgentError::LlmApi(format!(
                "Quorum fan-out did not meet success threshold (required={}, got={}): {}",
                required_success, succeeded, error_summary
            )));
        }

        if self.current_model != synthesis_model {
            self.try_switch_model(&synthesis_model)?;
        }
        let synthesis_system = Self::build_quorum_synthesis_prompt(&policy, &outcomes);
        let mut synthesis_system_sections = Vec::new();
        if let Some((contract_path, contract_text)) = quorum_contract.as_ref() {
            synthesis_system_sections.push(format!(
                "[QUORUM_AGENT_CONTRACT]\npath={}\nApply this contract strictly for synthesis:\n{}",
                contract_path.display(),
                contract_text
            ));
        }
        synthesis_system_sections.push(synthesis_system);
        let synthesis_messages =
            Self::compose_quorum_messages(synthesis_system_sections, base_messages, None);

        Self::emit_phase_event(
            &self.stream_handle_shared,
            "synthesis",
            "quorum synthesis from voter outputs",
            75,
        );
        let synthesis_result = self
            .run_messages_with_current_agent_tools(
                synthesis_messages,
                true,
                Self::quorum_synthesis_tools_enabled(),
            )
            .await;
        if self.current_model != original_model {
            if let Err(err) = self.try_switch_model(&original_model) {
                tracing::warn!(
                    model = %original_model,
                    error = %err,
                    "Failed to restore original model after quorum synthesis"
                );
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!(
                        "warning: failed to restore original model after quorum synthesis: {}",
                        err
                    ),
                );
            }
        }
        let result = synthesis_result?;
        let total_turns = result.total_turns;
        let synthesis_text = Self::extract_last_assistant_output(&result.messages);
        if let Err(err) =
            Self::update_quorum_artifact_with_synthesis(&artifact_path, &synthesis_text)
        {
            tracing::warn!("quorum synthesis artifact update skipped: {}", err);
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!("warning: quorum synthesis artifact update skipped: {}", err),
            );
        }
        if let Err(err) = self.apply_agent_result_and_persist(result) {
            tracing::warn!("session autosave skipped: {}", err);
        }
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!(
                "quorum run finished in {:.2}s (voters={} succeeded={} total_turns={})",
                run_started_at.elapsed().as_secs_f64(),
                voter_models.len(),
                succeeded,
                total_turns
            ),
        );
        Self::emit_phase_event(
            &self.stream_handle_shared,
            "finalize",
            "transcript finalization + persistence",
            100,
        );
        if let Some(handle) = &self.stream_handle {
            handle.send_done();
        }
        Ok(true)
    }

    fn required_quorum_success(voter_count: usize) -> usize {
        let n = voter_count.max(1);
        (n / 2) + 1
    }
}
