use super::{App, UiTranscriptMessage};

impl App {
    /// Append a UI-only message anchored to the current conversation size.
    pub fn push_ui_message(&mut self, message: hermes_core::Message) {
        self.session.ui_messages.push(UiTranscriptMessage {
            insert_at: self.session.messages.len(),
            message,
        });
    }

    /// Append a UI-only user transcript line.
    pub fn push_ui_user(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::user(text.into()));
    }

    /// Append a UI-only assistant transcript line.
    pub fn push_ui_assistant(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::assistant(text.into()));
    }

    /// Build the merged transcript for TUI rendering.
    ///
    /// This includes durable conversation history and UI-only events in
    /// chronological order, while preserving model-facing context purity.
    pub fn transcript_messages(&self) -> Vec<hermes_core::Message> {
        let mut merged =
            Vec::with_capacity(self.session.messages.len() + self.session.ui_messages.len());
        for idx in 0..=self.session.messages.len() {
            for ui in self
                .session
                .ui_messages
                .iter()
                .filter(|m| m.insert_at == idx)
            {
                merged.push(ui.message.clone());
            }
            if idx < self.session.messages.len() {
                merged.push(self.session.messages[idx].clone());
            }
        }
        merged
    }

    pub(super) fn prune_ui_after_current_messages(&mut self) {
        let cap = self.session.messages.len();
        self.session.ui_messages.retain(|m| m.insert_at <= cap);
    }
}
