impl PlatformAccessPolicy {
    fn has_allowlist(&self) -> bool {
        !self.allowed_users.is_empty()
            || !self.admin_users.is_empty()
            || !self.allowed_roles.is_empty()
    }

    fn has_any_access_configuration(&self) -> bool {
        self.has_allowlist()
            || !self.allowed_channels.is_empty()
            || !self.authorized_group_chats.is_empty()
            || !self.ignored_channels.is_empty()
    }

    fn fail_closed_default_warning(&self, platform: &str) -> Option<&'static str> {
        if !platform.eq_ignore_ascii_case("discord")
            || self.group_mode != GroupAccessMode::Allowlist
            || self.has_any_access_configuration()
        {
            return None;
        }

        Some(
            "Discord messages are being denied because no allowlist is configured. Set \
             DISCORD_ALLOWED_USERS, DISCORD_ALLOWED_ROLES, or DISCORD_ALLOWED_CHANNELS, \
             or set DISCORD_ALLOW_ALL_USERS=true for open access.",
        )
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

    /// Evaluate identity admission with the platform's verified sender
    /// envelope. Discord delegates role matching and guild scoping to the
    /// existing interaction policy; other platforms cannot satisfy a Discord
    /// role allowlist.
    fn is_identity_allowed(&self, incoming: &IncomingMessage, sender: &IncomingSender) -> bool {
        #[cfg(feature = "discord")]
        {
            if incoming.platform.eq_ignore_ascii_case("discord") {
                return self.is_verified_discord_identity_allowed(incoming, sender);
            }
        }

        self.is_user_allowed(&incoming.user_id)
    }

    #[cfg(feature = "discord")]
    fn is_verified_discord_identity_allowed(
        &self,
        incoming: &IncomingMessage,
        sender: &IncomingSender,
    ) -> bool {
        use crate::platforms::discord::{
            DiscordAuthDecision, DiscordInteractionAuthPolicy, DiscordInteractionSubject,
        };

        let allowed_user_ids = self
            .allowed_users
            .iter()
            .chain(self.admin_users.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        let allowed_role_ids = self.allowed_roles.iter().cloned().collect::<BTreeSet<_>>();
        let subject = match sender.verified_discord_member.as_ref() {
            Some(member) => DiscordInteractionSubject {
                user_id: Some(incoming.user_id.clone()),
                role_ids: member.role_ids.clone(),
                role_guild_id: member.guild_id.clone(),
            },
            None => DiscordInteractionSubject::user(incoming.user_id.clone()),
        };
        let role_guild_id = sender
            .verified_discord_member
            .as_ref()
            .and_then(|member| member.guild_id.as_deref());
        let policy = DiscordInteractionAuthPolicy {
            allowed_user_ids,
            allowed_role_ids,
            allowed_channels: BTreeSet::new(),
            ignored_channels: BTreeSet::new(),
        };

        matches!(
            policy.authorize_slash(&subject, None, role_guild_id, None),
            DiscordAuthDecision::Allow
        )
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
        sender: &IncomingSender,
    ) -> bool {
        incoming.platform.eq_ignore_ascii_case("discord")
            && sender.is_bot()
            && self.bot_sender_bypasses_allowlist
    }
}
