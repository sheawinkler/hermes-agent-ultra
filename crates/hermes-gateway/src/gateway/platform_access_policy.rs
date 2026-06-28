impl PlatformAccessPolicy {
    fn has_allowlist(&self) -> bool {
        !self.allowed_users.is_empty() || !self.admin_users.is_empty()
    }

    fn user_matches_any(user_id: &str, set: &HashSet<String>) -> bool {
        let candidate = user_id.trim();
        if candidate.is_empty() {
            return false;
        }
        let candidate_no_at = candidate.strip_prefix('@').unwrap_or(candidate);
        set.iter().any(|entry| {
            let allowed = entry.trim();
            if allowed.is_empty() {
                return false;
            }
            if allowed == "*" {
                return true;
            }
            let allowed_no_at = allowed.strip_prefix('@').unwrap_or(allowed);
            allowed.eq_ignore_ascii_case(candidate)
                || allowed.eq_ignore_ascii_case(candidate_no_at)
                || allowed_no_at.eq_ignore_ascii_case(candidate)
                || allowed_no_at.eq_ignore_ascii_case(candidate_no_at)
        })
    }

    pub fn is_user_allowed(&self, user_id: &str) -> bool {
        Self::user_matches_any(user_id, &self.admin_users)
            || Self::user_matches_any(user_id, &self.allowed_users)
    }

    fn channel_matches_any(channel_id: &str, set: &HashSet<String>) -> bool {
        let candidate = channel_id.trim();
        if candidate.is_empty() {
            return false;
        }
        set.iter().any(|entry| {
            let allowed = entry.trim();
            allowed == "*" || allowed.eq_ignore_ascii_case(candidate)
        })
    }

    fn is_channel_allowed(&self, channel_id: &str) -> bool {
        self.allowed_channels.is_empty()
            || Self::channel_matches_any(channel_id, &self.allowed_channels)
    }

    fn is_channel_ignored(&self, channel_id: &str) -> bool {
        Self::channel_matches_any(channel_id, &self.ignored_channels)
    }

    pub fn is_group_chat_authorized(&self, channel_id: &str) -> bool {
        Self::channel_matches_any(channel_id, &self.authorized_group_chats)
    }

    fn allows_sender_without_user_allowlist(
        &self,
        incoming: &IncomingMessage,
        sender: IncomingSender,
    ) -> bool {
        incoming.platform.eq_ignore_ascii_case("discord")
            && sender.is_bot
            && self.bot_sender_bypasses_allowlist
    }
}
