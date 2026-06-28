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
