impl App {
    const SESSION_OBJECTIVE_PREFIX: &'static str = "[SESSION_OBJECTIVE] ";

    fn ensure_session_stub_snapshot(&self) {
        if let Err(err) = self.persist_session_snapshot(None) {
            tracing::warn!("session startup snapshot skipped: {}", err);
        }
    }

    fn snapshot_file_is_empty_session(path: &Path, session_id: &str) -> bool {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            return false;
        };
        let Some(snapshot_session_id) = value
            .get("session_info")
            .and_then(|info| info.get("session_id"))
            .and_then(|value| value.as_str())
        else {
            return false;
        };
        if snapshot_session_id != session_id {
            return false;
        }
        value
            .get("messages")
            .and_then(|messages| messages.as_array())
            .is_some_and(|messages| messages.is_empty())
    }

    fn remove_empty_snapshot_file(&self, session_id: &str) -> Result<bool, AgentError> {
        let snapshot_path = self
            .state_root
            .join("sessions")
            .join(format!("{session_id}.json"));
        if !Self::snapshot_file_is_empty_session(&snapshot_path, session_id) {
            return Ok(false);
        }
        std::fs::remove_file(&snapshot_path).map_err(|e| {
            AgentError::Io(format!(
                "Failed to remove empty session snapshot {}: {}",
                snapshot_path.display(),
                e
            ))
        })?;
        Ok(true)
    }

    fn discard_session_if_empty(
        &self,
        session_id: &str,
        message_count: usize,
        has_session_objective: bool,
    ) -> bool {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return false;
        }

        let mut discarded = false;
        match SessionPersistence::new(&self.state_root).delete_session_if_empty(session_id) {
            Ok(deleted) => discarded |= deleted,
            Err(err) => tracing::debug!(
                session_id,
                error = %err,
                "failed to delete empty session db row"
            ),
        }

        if message_count == 0 && !has_session_objective {
            match self.remove_empty_snapshot_file(session_id) {
                Ok(removed) => discarded |= removed,
                Err(err) => tracing::debug!(
                    session_id,
                    error = %err,
                    "failed to remove empty session snapshot"
                ),
            }
        }

        discarded
    }

    pub fn discard_current_session_if_empty(&self) -> bool {
        self.discard_session_if_empty(
            &self.session_id,
            self.messages.len(),
            self.session_objective.is_some(),
        )
    }

}
