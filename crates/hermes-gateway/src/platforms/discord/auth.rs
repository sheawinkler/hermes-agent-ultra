//! Discord-specific user/role authorization (P1-9).

use super::config::ChannelIdSet;

#[derive(Debug, Clone, Default)]
pub struct DiscordAuthConfig {
    pub allowed_users: ChannelIdSet,
    pub allowed_roles: ChannelIdSet,
    /// When set, DM role checks are scoped to this guild only.
    pub dm_role_auth_guild: Option<String>,
}

impl DiscordAuthConfig {
    pub fn has_restrictions(&self) -> bool {
        self.allowed_users.is_restrictive() || self.allowed_roles.is_restrictive()
    }
}

/// Whether the sender is authorized for this Discord message.
pub fn is_discord_user_authorized(
    user_id: &str,
    role_ids: &[String],
    guild_id: Option<&str>,
    is_dm: bool,
    cfg: &DiscordAuthConfig,
) -> bool {
    if !cfg.has_restrictions() {
        return true;
    }
    if cfg.allowed_users.contains(user_id) {
        return true;
    }
    if role_ids
        .iter()
        .any(|role| cfg.allowed_roles.contains(role))
    {
        if is_dm {
            if let Some(trusted) = cfg.dm_role_auth_guild.as_deref() {
                return guild_id == Some(trusted);
            }
            return false;
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn users(ids: &str) -> ChannelIdSet {
        ChannelIdSet::parse(Some(ids))
    }

    fn roles(ids: &str) -> ChannelIdSet {
        ChannelIdSet::parse(Some(ids))
    }

    #[test]
    fn f16_guild_role_authorized() {
        let cfg = DiscordAuthConfig {
            allowed_users: ChannelIdSet::new(),
            allowed_roles: roles("5555"),
            dm_role_auth_guild: None,
        };
        assert!(is_discord_user_authorized(
            "user1",
            &["5555".into()],
            Some("g1"),
            false,
            &cfg
        ));
    }

    #[test]
    fn f17_dm_rejects_role_from_other_guild() {
        let cfg = DiscordAuthConfig {
            allowed_users: ChannelIdSet::new(),
            allowed_roles: roles("5555"),
            dm_role_auth_guild: None,
        };
        assert!(!is_discord_user_authorized(
            "user1",
            &["5555".into()],
            Some("other-guild"),
            true,
            &cfg
        ));
    }

    #[test]
    fn f18_dm_role_with_trusted_guild() {
        let cfg = DiscordAuthConfig {
            allowed_users: ChannelIdSet::new(),
            allowed_roles: roles("5555"),
            dm_role_auth_guild: Some("trusted".into()),
        };
        assert!(is_discord_user_authorized(
            "user1",
            &["5555".into()],
            Some("trusted"),
            true,
            &cfg
        ));
    }

    #[test]
    fn f19_user_allowlist_without_role() {
        let cfg = DiscordAuthConfig {
            allowed_users: users("user42"),
            allowed_roles: ChannelIdSet::new(),
            dm_role_auth_guild: None,
        };
        assert!(is_discord_user_authorized("user42", &[], None, true, &cfg));
    }

    #[test]
    fn f20_guild_denied_without_match() {
        let cfg = DiscordAuthConfig {
            allowed_users: users("other"),
            allowed_roles: roles("999"),
            dm_role_auth_guild: None,
        };
        assert!(!is_discord_user_authorized(
            "user1",
            &["5555".into()],
            Some("g1"),
            false,
            &cfg
        ));
    }
}
