impl App {
    /// Attach a streaming handle (used by TUI mode).
    pub fn set_stream_handle(&mut self, handle: Option<StreamHandle>) {
        if let Ok(mut guard) = self.stream_handle_shared.lock() {
            *guard = handle.clone();
        }
        self.stream_handle = handle;
    }

    /// Enable/disable TUI mouse handling.
    pub fn set_mouse_enabled(&mut self, enabled: bool) {
        self.mouse_enabled = enabled;
    }

    /// Current TUI mouse handling state.
    pub fn mouse_enabled(&self) -> bool {
        self.mouse_enabled
    }

    /// Queue a TUI skin/theme change request to be applied in the UI loop.
    pub fn request_theme_change(&mut self, skin: &str) {
        let value = skin.trim();
        if value.is_empty() {
            return;
        }
        self.pending_theme = Some(value.to_string());
    }

    /// Queue an image hint for the next user prompt.
    pub fn set_pending_image_hint(&mut self, path: String) {
        let trimmed = path.trim();
        self.pending_image_hint = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    /// Read queued image hint without consuming it.
    pub fn pending_image_hint(&self) -> Option<&str> {
        self.pending_image_hint.as_deref()
    }

    /// Clear queued image hint.
    pub fn clear_pending_image_hint(&mut self) {
        self.pending_image_hint = None;
    }

    /// Submit text through the normal user-message path and run the agent.
    pub async fn submit_user_message(&mut self, raw: &str) -> Result<(), AgentError> {
        for note in std::mem::take(&mut self.pending_system_notes) {
            self.messages.push(hermes_core::Message::system(note));
        }
        let user_message = self.prepare_user_message(raw);
        self.messages.push(hermes_core::Message::user(user_message));
        self.run_agent().await
    }

    pub(crate) async fn submit_moa_oneshot(&mut self, raw: &str) -> Result<(), AgentError> {
        let prompt = raw.trim();
        if prompt.is_empty() {
            return Err(AgentError::Config("MoA prompt cannot be empty".to_string()));
        }

        let restore_model = self.current_model.clone();
        let moa_model = format!("{MOA_PROVIDER}:{MOA_DEFAULT_PRESET}");
        self.try_switch_model(&moa_model)?;
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("MoA one-shot armed via {moa_model}; prior model will be restored"),
        );

        let run_result = self.submit_user_message(prompt).await;
        let restore_result = if self.current_model != restore_model {
            self.try_switch_model(&restore_model)
        } else {
            Ok(())
        };

        match (run_result, restore_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(run_err), Ok(())) => Err(run_err),
            (Ok(()), Err(restore_err)) => Err(AgentError::Config(format!(
                "MoA one-shot completed but failed to restore prior model `{restore_model}`: {restore_err}"
            ))),
            (Err(run_err), Err(restore_err)) => Err(AgentError::Config(format!(
                "MoA one-shot failed: {run_err}; also failed to restore prior model `{restore_model}`: {restore_err}"
            ))),
        }
    }

    pub fn queue_next_turn_system_note(&mut self, note: String) {
        let trimmed = note.trim();
        if !trimmed.is_empty() {
            self.pending_system_notes.push(trimmed.to_string());
        }
    }

    #[cfg(test)]
    pub fn pending_system_note_count(&self) -> usize {
        self.pending_system_notes.len()
    }

    pub fn take_pending_input_prefill(&mut self) -> Option<String> {
        self.pending_input_prefill.take()
    }

    pub fn queue_pending_agent_seed(&mut self, value: impl Into<String>) {
        let value = value.into();
        if !value.trim().is_empty() {
            self.pending_agent_seed = Some(value);
        }
    }

    pub fn take_pending_agent_seed(&mut self) -> Option<String> {
        self.pending_agent_seed.take()
    }

    fn composer_drafts_path(&self) -> PathBuf {
        self.state_root.join(COMPOSER_DRAFTS_FILE)
    }

    fn composer_draft_key(&self) -> String {
        let trimmed = self.session_id.trim();
        if trimmed.is_empty() {
            "__new__".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn load_composer_draft_store(&self) -> Result<ComposerDraftStore, AgentError> {
        let path = self.composer_drafts_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ComposerDraftStore {
                    version: 1,
                    drafts: Vec::new(),
                });
            }
            Err(err) => {
                return Err(AgentError::Io(format!(
                    "Failed to read composer drafts {}: {}",
                    path.display(),
                    err
                )));
            }
        };
        let mut store: ComposerDraftStore = serde_json::from_str(&raw).map_err(|err| {
            AgentError::Config(format!(
                "Failed to parse composer drafts {}: {}",
                path.display(),
                err
            ))
        })?;
        store.version = 1;
        Ok(store)
    }

    fn write_composer_draft_store(&self, store: &ComposerDraftStore) -> Result<(), AgentError> {
        let path = self.composer_drafts_path();
        if store.drafts.is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(AgentError::Io(format!(
                        "Failed to remove composer drafts {}: {}",
                        path.display(),
                        err
                    )));
                }
            }
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AgentError::Io(format!(
                    "Failed to create composer draft dir {}: {}",
                    parent.display(),
                    err
                ))
            })?;
        }
        let raw = serde_json::to_string_pretty(store).map_err(|err| {
            AgentError::Config(format!("Failed to serialize composer drafts: {err}"))
        })?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, raw).map_err(|err| {
            AgentError::Io(format!(
                "Failed to write composer drafts {}: {}",
                tmp_path.display(),
                err
            ))
        })?;
        std::fs::rename(&tmp_path, &path).map_err(|err| {
            AgentError::Io(format!(
                "Failed to replace composer drafts {}: {}",
                path.display(),
                err
            ))
        })?;
        Ok(())
    }

    /// Load unsent composer text for the active session.
    pub fn load_current_composer_draft(&self) -> Result<Option<String>, AgentError> {
        let key = self.composer_draft_key();
        let store = self.load_composer_draft_store()?;
        Ok(store
            .drafts
            .into_iter()
            .rev()
            .find(|draft| draft.session_id == key && !draft.text.trim().is_empty())
            .map(|draft| draft.text))
    }

    /// Persist unsent composer text for the active session.
    pub fn persist_current_composer_draft(&self, text: &str) -> Result<(), AgentError> {
        let key = self.composer_draft_key();
        let mut store = self.load_composer_draft_store()?;
        store.drafts.retain(|draft| draft.session_id != key);
        if !text.trim().is_empty() {
            store.drafts.push(ComposerDraftRecord {
                session_id: key,
                text: text.to_string(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            });
        }
        if store.drafts.len() > MAX_COMPOSER_DRAFTS {
            let keep_from = store.drafts.len() - MAX_COMPOSER_DRAFTS;
            store.drafts.drain(0..keep_from);
        }
        store.version = 1;
        self.write_composer_draft_store(&store)
    }

    /// Clear unsent composer text for the active session.
    pub fn clear_current_composer_draft(&self) -> Result<(), AgentError> {
        self.persist_current_composer_draft("")
    }

    /// Prepare outbound user text, consuming any queued image hint.
    pub fn prepare_user_message(&mut self, raw: &str) -> String {
        let base = raw.trim();
        if let Some(path) = self
            .pending_image_hint
            .take()
            .filter(|value| !value.trim().is_empty())
        {
            format!("[IMAGE_HINT] path={}\n{}", path, base)
        } else {
            base.to_string()
        }
    }

    /// Drain any queued skin/theme change request.
    pub fn take_pending_theme_change(&mut self) -> Option<String> {
        self.pending_theme.take()
    }

    /// Retrieve current companion pet settings.
    pub fn pet_settings(&self) -> &PetSettings {
        &self.pet_settings
    }

    /// Update and persist companion pet settings.
    pub fn set_pet_settings(&mut self, settings: PetSettings) -> Result<(), AgentError> {
        let normalized = settings.normalized();
        persist_pet_settings(&normalized)?;
        self.pet_settings = normalized;
        Ok(())
    }
}
