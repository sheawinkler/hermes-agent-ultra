//! Shared SQLite state.db read/write helpers (`hermes_state.py` parity surface for tools/gateway).

mod billing;
mod columns;
mod conn;
mod content;
mod error;
mod list;
mod search;
mod sessions;
mod telegram;

pub use billing::{TokenCountUpdate, update_token_counts};
pub use conn::StateDb;
pub use content::decode_content_preview;
pub use error::StateDbError;
pub use list::{SessionListRow, get_compression_tip, list_sessions_rich, load_session_messages};
pub use search::{SearchContextMessage, SearchMessageMatch, sanitize_fts5_query, search_messages};
pub use sessions::{insert_session_if_missing, message_insert_sql};
pub use telegram::{
    TelegramTopicBinding, UnlinkedTelegramSession, apply_telegram_topic_migration,
    bind_telegram_topic, disable_telegram_topic_mode, enable_telegram_topic_mode,
    get_telegram_topic_binding, is_telegram_lobby_system_command,
    is_telegram_session_linked_to_topic, is_telegram_topic_mode_enabled,
    list_unlinked_telegram_sessions_for_user,
};

impl StateDb {
    pub fn search_messages(
        &self,
        query: &str,
        source_filter: Option<&[&str]>,
        exclude_sources: Option<&[&str]>,
        role_filter: Option<&[&str]>,
        limit: usize,
        offset: usize,
        sort: Option<&str>,
    ) -> Result<Vec<SearchMessageMatch>, StateDbError> {
        search_messages(
            self.conn(),
            query,
            source_filter,
            exclude_sources,
            role_filter,
            limit,
            offset,
            sort,
        )
    }

    pub fn list_sessions_rich(
        &self,
        source: Option<&str>,
        exclude_sources: &[&str],
        limit: usize,
        offset: usize,
        min_message_count: i64,
        order_by_last_active: bool,
    ) -> Result<Vec<SessionListRow>, StateDbError> {
        list_sessions_rich(
            self.conn(),
            source,
            exclude_sources,
            limit,
            offset,
            min_message_count,
            order_by_last_active,
        )
    }

    pub fn get_compression_tip(&self, session_id: &str) -> Result<String, StateDbError> {
        get_compression_tip(self.conn(), session_id)
    }

    pub fn load_session_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<(String, String, Option<String>)>, StateDbError> {
        load_session_messages(self.conn(), session_id)
    }

    pub fn update_token_counts(
        &self,
        session_id: &str,
        update: &TokenCountUpdate,
    ) -> Result<(), StateDbError> {
        update_token_counts(self.conn(), session_id, update)
    }

    pub fn apply_telegram_topic_migration(&self) -> Result<(), StateDbError> {
        apply_telegram_topic_migration(self.conn())
    }

    pub fn enable_telegram_topic_mode(
        &self,
        chat_id: &str,
        user_id: &str,
        has_topics_enabled: Option<bool>,
        allows_users_to_create_topics: Option<bool>,
    ) -> Result<(), StateDbError> {
        enable_telegram_topic_mode(
            self.conn(),
            chat_id,
            user_id,
            has_topics_enabled,
            allows_users_to_create_topics,
        )
    }

    pub fn disable_telegram_topic_mode(
        &self,
        chat_id: &str,
        clear_bindings: bool,
    ) -> Result<(), StateDbError> {
        disable_telegram_topic_mode(self.conn(), chat_id, clear_bindings)
    }

    pub fn is_telegram_topic_mode_enabled(&self, chat_id: &str, user_id: &str) -> bool {
        is_telegram_topic_mode_enabled(self.conn(), chat_id, user_id)
    }

    pub fn get_telegram_topic_binding(
        &self,
        chat_id: &str,
        thread_id: &str,
    ) -> Result<Option<TelegramTopicBinding>, StateDbError> {
        get_telegram_topic_binding(self.conn(), chat_id, thread_id)
    }

    pub fn bind_telegram_topic(
        &self,
        chat_id: &str,
        thread_id: &str,
        user_id: &str,
        session_key: &str,
        session_id: &str,
        managed_mode: &str,
    ) -> Result<(), StateDbError> {
        bind_telegram_topic(
            self.conn(),
            chat_id,
            thread_id,
            user_id,
            session_key,
            session_id,
            managed_mode,
        )
    }

    pub fn list_unlinked_telegram_sessions_for_user(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<UnlinkedTelegramSession>, StateDbError> {
        list_unlinked_telegram_sessions_for_user(self.conn(), user_id, limit)
    }
}
