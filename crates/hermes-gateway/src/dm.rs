//! DM (Direct Message) pairing mechanism (Requirement 7.9).
//!
//! Handles authorization decisions when an unregistered user sends a
//! direct message to the bot. Supports configurable behaviors:
//! - Pair: Create a session and request admin approval
//! - Ignore: Silently discard the message

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use hermes_config::UnauthorizedDmBehavior;

const DM_GRANT_STORE_VERSION: u32 = 1;
const DM_GRANT_VERSION: u32 = 1;
const PAIRING_CODE_LENGTH: usize = 8;
const PAIRING_CODE_ATTEMPTS: usize = 64;
/// Pairing approvals expire after thirty days unless replaced by a new approval.
pub const DEFAULT_DM_GRANT_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// A clock injected into `DmManager` so expiry behavior is deterministic in tests.
pub type DmClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

// ---------------------------------------------------------------------------
// DmDecision
// ---------------------------------------------------------------------------

/// Decision outcome for a DM from an unregistered/unauthorized user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DmDecision {
    /// Allow the DM through (user is authorized).
    Allow,
    /// Pair the user: create a session and request admin approval.
    Pair {
        /// A message to show the user while awaiting approval.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Deny the DM entirely.
    Deny,
}

/// The only scope a durable pairing grant may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DmGrantScope {
    DirectMessage,
}

/// The only operation a durable pairing grant may authorize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DmGrantOperation {
    DirectMessage,
}

/// The platform-local administrator that issued a durable pairing grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DmGrantAdminOrigin {
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
}

/// A versioned, exact DM authorization grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DmGrant {
    pub version: u32,
    pub pairing_code: String,
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub scope: DmGrantScope,
    pub operation: DmGrantOperation,
    pub admin_origin: DmGrantAdminOrigin,
    pub issued_at_unix_seconds: i64,
    pub expires_at_unix_seconds: i64,
    /// Retained revocation tombstone. Keeping the opaque code bound to this
    /// target prevents delayed commands from ever being rebound to another DM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_unix_seconds: Option<i64>,
}

/// A pending pairing request, indexed by its opaque code and exact DM target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingDmPairing {
    pub code: String,
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
}

/// Errors returned while loading or mutating durable pairing grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmGrantError {
    NotAdmin,
    NotDirectMessage,
    InvalidPairingCode,
    PairingNotFound,
    WrongPlatform,
    ClockUnavailable,
    StoreLoad(String),
    Persistence(String),
}

impl DmGrantError {
    /// Whether this error means the durable snapshot could not be committed.
    pub fn is_persistence_error(&self) -> bool {
        matches!(self, Self::Persistence(_))
    }
}

impl fmt::Display for DmGrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAdmin => write!(f, "administrator privileges are required"),
            Self::NotDirectMessage => {
                write!(f, "pairing approval must be issued from a direct message")
            }
            Self::InvalidPairingCode => write!(f, "invalid pairing code"),
            Self::PairingNotFound => write!(f, "pairing code was not found"),
            Self::WrongPlatform => write!(f, "pairing code belongs to a different platform"),
            Self::ClockUnavailable => write!(f, "pairing clock is unavailable"),
            Self::StoreLoad(message) => write!(f, "could not load DM grant store: {message}"),
            Self::Persistence(message) => write!(f, "could not persist DM grant store: {message}"),
        }
    }
}

impl std::error::Error for DmGrantError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DmPairingKey {
    platform: String,
    chat_id: String,
    user_id: String,
}

#[derive(Default)]
struct PendingPairings {
    by_code: HashMap<String, PendingDmPairing>,
    by_target: HashMap<DmPairingKey, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DmGrantStore {
    version: u32,
    grants: Vec<DmGrant>,
    /// Unresolved code bindings survive restart so an externally shown code
    /// cannot later be reissued for another target.
    #[serde(default)]
    pending: Vec<PendingDmPairing>,
}

#[cfg(test)]
pub(crate) type DmStoreWriteOverride =
    Arc<dyn Fn(&Path, &[DmGrant]) -> Result<(), DmGrantError> + Send + Sync>;

#[cfg(test)]
pub(crate) type DmStorePostReplaceSyncOverride =
    Arc<dyn Fn(&Path) -> Result<(), DmGrantError> + Send + Sync>;

// ---------------------------------------------------------------------------
// DmManager
// ---------------------------------------------------------------------------

/// Manages DM authorization decisions for incoming messages.
pub struct DmManager {
    /// Set of user IDs that are explicitly authorized to DM the bot.
    authorized_users: HashSet<String>,

    /// Set of user IDs that have admin privileges.
    admin_users: HashSet<String>,

    /// Per-platform user IDs from config/env allowlists.
    platform_authorized_users: HashMap<String, HashSet<String>>,

    /// Pairing grants issued by `/approve`, scoped to one exact DM target.
    grants: Vec<DmGrant>,

    /// Optional owner-only durable store for pairing grants.
    grant_store_path: Option<PathBuf>,

    /// Clock used for grant issuance and expiry checks.
    clock: DmClock,

    /// Per-platform admin user IDs from config/env allowlists.
    platform_admin_users: HashMap<String, HashSet<String>>,

    /// Per-platform group sender allowlists. These never authorize DMs.
    platform_group_authorized_users: HashMap<String, HashSet<String>>,

    /// Per-platform group chat allowlists. These never authorize DMs.
    platform_group_authorized_chats: HashMap<String, HashSet<String>>,

    /// Per-platform aliases such as WhatsApp LID <-> phone mappings.
    platform_user_aliases: HashMap<String, HashMap<String, HashSet<String>>>,

    /// How to handle DMs from unauthorized users.
    unauthorized_dm_behavior: UnauthorizedDmBehavior,

    /// Per-platform unauthorized-DM policy. This preserves Python behavior
    /// where strict allowlists default to ignoring strangers instead of pairing.
    platform_unauthorized_dm_behavior: HashMap<String, UnauthorizedDmBehavior>,

    /// Pending pairing requests keyed both by opaque code and exact DM target.
    pairing_codes: Mutex<PendingPairings>,

    /// Source for pending pairing codes. Production uses UUIDs; tests can force
    /// collisions to prove code allocation is safe.
    pairing_code_generator: Arc<dyn Fn() -> String + Send + Sync>,

    /// Users whose pairing requests are currently rate-limited.
    pairing_rate_limited: Mutex<HashSet<DmPairingKey>>,

    #[cfg(test)]
    store_write_override: Option<DmStoreWriteOverride>,
    #[cfg(test)]
    post_replace_directory_sync_override: Option<DmStorePostReplaceSyncOverride>,
}

impl DmManager {
    /// Create a new in-memory `DmManager`.
    pub fn new(
        authorized_users: HashSet<String>,
        admin_users: HashSet<String>,
        unauthorized_dm_behavior: UnauthorizedDmBehavior,
    ) -> Self {
        Self::new_with_grant_store(
            authorized_users,
            admin_users,
            unauthorized_dm_behavior,
            None,
        )
        .expect("an in-memory DM grant manager cannot fail to load")
    }

    /// Create a manager that fail-closes if its durable grant store cannot load.
    pub fn new_with_grant_store(
        authorized_users: HashSet<String>,
        admin_users: HashSet<String>,
        unauthorized_dm_behavior: UnauthorizedDmBehavior,
        grant_store_path: Option<PathBuf>,
    ) -> Result<Self, DmGrantError> {
        let clock: DmClock = Arc::new(SystemTime::now);
        Self::new_with_grant_store_and_clock(
            authorized_users,
            admin_users,
            unauthorized_dm_behavior,
            grant_store_path,
            clock,
        )
    }

    /// Create a manager with an injected durable store and clock.
    pub fn new_with_grant_store_and_clock(
        authorized_users: HashSet<String>,
        admin_users: HashSet<String>,
        unauthorized_dm_behavior: UnauthorizedDmBehavior,
        grant_store_path: Option<PathBuf>,
        clock: DmClock,
    ) -> Result<Self, DmGrantError> {
        #[cfg(windows)]
        if grant_store_path.is_some() {
            return Err(DmGrantError::StoreLoad(
                "durable DM grants require owner-only ACL enforcement, which is not implemented on Windows"
                    .to_string(),
            ));
        }
        let (grants, pairing_codes) = match grant_store_path.as_deref() {
            Some(path) => Self::load_grant_state(path)?,
            None => (Vec::new(), PendingPairings::default()),
        };
        Ok(Self {
            authorized_users,
            admin_users,
            platform_authorized_users: HashMap::new(),
            grants,
            grant_store_path,
            clock,
            platform_admin_users: HashMap::new(),
            platform_group_authorized_users: HashMap::new(),
            platform_group_authorized_chats: HashMap::new(),
            platform_user_aliases: HashMap::new(),
            unauthorized_dm_behavior,
            platform_unauthorized_dm_behavior: HashMap::new(),
            pairing_codes: Mutex::new(pairing_codes),
            pairing_code_generator: Arc::new(Self::new_pairing_code),
            pairing_rate_limited: Mutex::new(HashSet::new()),
            #[cfg(test)]
            store_write_override: None,
            #[cfg(test)]
            post_replace_directory_sync_override: None,
        })
    }

    /// Create an in-memory manager with the Pair behavior and no configured users.
    pub fn with_pair_behavior() -> Self {
        Self::with_pair_behavior_and_grant_store(None)
            .expect("an in-memory DM grant manager cannot fail to load")
    }

    /// Create a Pair manager backed by an optional durable grant store.
    pub fn with_pair_behavior_and_grant_store(
        grant_store_path: Option<PathBuf>,
    ) -> Result<Self, DmGrantError> {
        Self::new_with_grant_store(
            HashSet::new(),
            HashSet::new(),
            UnauthorizedDmBehavior::Pair,
            grant_store_path,
        )
    }

    /// Create a Pair manager with an injected durable store and clock.
    pub fn with_pair_behavior_and_grant_store_and_clock(
        grant_store_path: Option<PathBuf>,
        clock: DmClock,
    ) -> Result<Self, DmGrantError> {
        Self::new_with_grant_store_and_clock(
            HashSet::new(),
            HashSet::new(),
            UnauthorizedDmBehavior::Pair,
            grant_store_path,
            clock,
        )
    }

    /// Create an in-memory manager with the Ignore behavior and no configured users.
    pub fn with_ignore_behavior() -> Self {
        Self::with_ignore_behavior_and_grant_store(None)
            .expect("an in-memory DM grant manager cannot fail to load")
    }

    /// Create an Ignore manager backed by an optional durable grant store.
    pub fn with_ignore_behavior_and_grant_store(
        grant_store_path: Option<PathBuf>,
    ) -> Result<Self, DmGrantError> {
        Self::new_with_grant_store(
            HashSet::new(),
            HashSet::new(),
            UnauthorizedDmBehavior::Ignore,
            grant_store_path,
        )
    }

    /// Create an Ignore manager with an injected durable store and clock.
    pub fn with_ignore_behavior_and_grant_store_and_clock(
        grant_store_path: Option<PathBuf>,
        clock: DmClock,
    ) -> Result<Self, DmGrantError> {
        Self::new_with_grant_store_and_clock(
            HashSet::new(),
            HashSet::new(),
            UnauthorizedDmBehavior::Ignore,
            grant_store_path,
            clock,
        )
    }

    fn platform_key(platform: &str) -> String {
        let key = platform.trim().to_ascii_lowercase();
        match key.as_str() {
            "qq" | "qq_bot" => "qqbot".to_string(),
            _ => key,
        }
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

    fn user_variants_for_platform(platform: &str, user_id: &str) -> HashSet<String> {
        let mut variants = HashSet::new();
        let candidate = user_id.trim();
        if candidate.is_empty() {
            return variants;
        }

        variants.insert(candidate.to_string());
        variants.insert(candidate.strip_prefix('@').unwrap_or(candidate).to_string());

        if Self::platform_key(platform) == "whatsapp" {
            let base = candidate
                .strip_suffix("@lid")
                .or_else(|| candidate.strip_suffix("@s.whatsapp.net"))
                .unwrap_or(candidate)
                .trim();
            if !base.is_empty() {
                variants.insert(base.to_string());
                if let Some(no_plus) = base.strip_prefix('+') {
                    if !no_plus.is_empty() {
                        variants.insert(no_plus.to_string());
                    }
                } else if base.chars().all(|c| c.is_ascii_digit()) {
                    variants.insert(format!("+{base}"));
                }
            }
        }

        variants
    }

    fn alias_key(value: &str) -> String {
        value.trim().to_ascii_lowercase()
    }

    fn insert_alias(
        aliases: &mut HashMap<String, HashSet<String>>,
        platform: &str,
        key: &str,
        alias: &str,
    ) {
        for key_variant in Self::user_variants_for_platform(platform, key) {
            let key_variant = Self::alias_key(&key_variant);
            if key_variant.is_empty() {
                continue;
            }
            let entry = aliases.entry(key_variant).or_default();
            for alias_variant in Self::user_variants_for_platform(platform, alias) {
                let alias_variant = alias_variant.trim();
                if !alias_variant.is_empty() {
                    entry.insert(alias_variant.to_string());
                }
            }
        }
    }

    fn user_matches_platform_set(
        &self,
        platform: &str,
        user_id: &str,
        set: &HashSet<String>,
    ) -> bool {
        if Self::user_matches_any(user_id, set) {
            return true;
        }

        let platform = Self::platform_key(platform);
        let variants = Self::user_variants_for_platform(&platform, user_id);
        if variants
            .iter()
            .any(|variant| Self::user_matches_any(variant, set))
        {
            return true;
        }

        let Some(aliases) = self.platform_user_aliases.get(&platform) else {
            return false;
        };
        variants.iter().any(|variant| {
            aliases
                .get(&Self::alias_key(variant))
                .is_some_and(|mapped| {
                    mapped
                        .iter()
                        .any(|alias| Self::user_matches_any(alias, set))
                })
        })
    }

    fn chat_matches_any(chat_id: &str, set: &HashSet<String>) -> bool {
        let candidate = chat_id.trim();
        if candidate.is_empty() {
            return false;
        }
        set.iter().any(|entry| {
            let allowed = entry.trim();
            allowed == "*" || allowed.eq_ignore_ascii_case(candidate)
        })
    }

    fn pairing_key(platform: &str, chat_id: &str, user_id: &str) -> Option<DmPairingKey> {
        let platform = Self::platform_key(platform);
        let chat_id = chat_id.trim();
        let user_id = user_id.trim();
        if platform.is_empty() || chat_id.is_empty() || user_id.is_empty() {
            return None;
        }
        Some(DmPairingKey {
            platform,
            chat_id: chat_id.to_string(),
            user_id: user_id.to_string(),
        })
    }

    fn normalize_pairing_code(code: &str) -> Option<String> {
        let normalized = code.trim().to_ascii_uppercase();
        (normalized.len() == PAIRING_CODE_LENGTH
            && normalized
                .chars()
                .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit()))
        .then_some(normalized)
    }

    fn new_pairing_code() -> String {
        uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(PAIRING_CODE_LENGTH)
            .collect::<String>()
            .to_ascii_uppercase()
    }

    fn now_unix_seconds(&self) -> Option<i64> {
        (self.clock)()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
    }

    fn nonempty_trimmed(value: &str) -> bool {
        !value.trim().is_empty() && value == value.trim()
    }

    fn validate_loaded_grant(grant: &DmGrant) -> Result<(), DmGrantError> {
        if grant.version != DM_GRANT_VERSION {
            return Err(DmGrantError::StoreLoad(format!(
                "unsupported grant version {}",
                grant.version
            )));
        }
        if grant.platform != Self::platform_key(&grant.platform)
            || !Self::nonempty_trimmed(&grant.platform)
            || !Self::nonempty_trimmed(&grant.chat_id)
            || !Self::nonempty_trimmed(&grant.user_id)
            || Self::normalize_pairing_code(&grant.pairing_code).as_deref()
                != Some(grant.pairing_code.as_str())
            || grant.scope != DmGrantScope::DirectMessage
            || grant.operation != DmGrantOperation::DirectMessage
            || grant.admin_origin.platform != grant.platform
            || !Self::nonempty_trimmed(&grant.admin_origin.chat_id)
            || !Self::nonempty_trimmed(&grant.admin_origin.user_id)
            || grant.expires_at_unix_seconds < grant.issued_at_unix_seconds
            || grant
                .revoked_at_unix_seconds
                .is_some_and(|revoked_at| revoked_at < grant.issued_at_unix_seconds)
        {
            return Err(DmGrantError::StoreLoad(
                "grant has invalid scope, identity, origin, or expiry".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_loaded_pending_pairing(pending: &PendingDmPairing) -> Result<(), DmGrantError> {
        if pending.platform != Self::platform_key(&pending.platform)
            || !Self::nonempty_trimmed(&pending.platform)
            || !Self::nonempty_trimmed(&pending.chat_id)
            || !Self::nonempty_trimmed(&pending.user_id)
            || Self::normalize_pairing_code(&pending.code).as_deref() != Some(pending.code.as_str())
        {
            return Err(DmGrantError::StoreLoad(
                "pending pairing has invalid identity or code".to_string(),
            ));
        }
        Ok(())
    }

    fn load_grant_state(path: &Path) -> Result<(Vec<DmGrant>, PendingPairings), DmGrantError> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((Vec::new(), PendingPairings::default()));
            }
            Err(error) => {
                return Err(DmGrantError::StoreLoad(format!(
                    "{}: {error}",
                    path.display()
                )));
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path)
                .map_err(|error| DmGrantError::StoreLoad(format!("{}: {error}", path.display())))?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                return Err(DmGrantError::StoreLoad(format!(
                    "{} is not owner-only",
                    path.display()
                )));
            }
        }
        let store: DmGrantStore = serde_json::from_str(&raw)
            .map_err(|error| DmGrantError::StoreLoad(format!("{}: {error}", path.display())))?;
        if store.version != DM_GRANT_STORE_VERSION {
            return Err(DmGrantError::StoreLoad(format!(
                "{}: unsupported store version {}",
                path.display(),
                store.version
            )));
        }

        let mut codes = HashSet::new();
        let mut active_targets = HashSet::new();
        for grant in &store.grants {
            Self::validate_loaded_grant(grant)?;
            let target = Self::pairing_key(&grant.platform, &grant.chat_id, &grant.user_id)
                .expect("validated durable grant target");
            if !codes.insert(grant.pairing_code.clone())
                || (grant.revoked_at_unix_seconds.is_none() && !active_targets.insert(target))
            {
                return Err(DmGrantError::StoreLoad(
                    "grant store contains duplicate pairing codes or active targets".to_string(),
                ));
            }
        }

        let mut pairings = PendingPairings::default();
        for pending in store.pending {
            Self::validate_loaded_pending_pairing(&pending)?;
            let target = Self::pairing_key(&pending.platform, &pending.chat_id, &pending.user_id)
                .expect("validated pending pairing target");
            if !codes.insert(pending.code.clone()) || pairings.by_target.contains_key(&target) {
                return Err(DmGrantError::StoreLoad(
                    "grant store contains duplicate pending pairing codes or targets".to_string(),
                ));
            }
            pairings.by_target.insert(target, pending.code.clone());
            pairings.by_code.insert(pending.code.clone(), pending);
        }
        Ok((store.grants, pairings))
    }

    /// Preserve the previously committed snapshot until the replacement's
    /// directory entry is durable. Prefer a hard link for an exact snapshot;
    /// filesystems without hard-link support fall back to an owner-only,
    /// same-directory copied and synced snapshot.
    fn create_rollback_backup(
        path: &Path,
        parent: &Path,
        filename: &str,
    ) -> Result<Option<PathBuf>, DmGrantError> {
        for _ in 0..PAIRING_CODE_ATTEMPTS {
            let backup = parent.join(format!(".{filename}.{}.bak", uuid::Uuid::new_v4()));
            match fs::hard_link(path, &backup) {
                Ok(()) => return Ok(Some(backup)),
                Err(error) if error.kind() == io::ErrorKind::NotFound && !path.exists() => {
                    return Ok(None);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Self::copy_rollback_backup(path, parent, filename),
            }
        }
        Err(DmGrantError::Persistence(format!(
            "could not allocate a rollback snapshot beside {}",
            path.display()
        )))
    }

    /// Copy the prior durable snapshot when hard links are unavailable.
    fn copy_rollback_backup(
        path: &Path,
        parent: &Path,
        filename: &str,
    ) -> Result<Option<PathBuf>, DmGrantError> {
        let mut source = match fs::File::open(path) {
            Ok(source) => source,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(DmGrantError::Persistence(format!(
                    "{}: {error}",
                    path.display()
                )));
            }
        };
        for _ in 0..PAIRING_CODE_ATTEMPTS {
            let backup = parent.join(format!(".{filename}.{}.bak", uuid::Uuid::new_v4()));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut destination = match options.open(&backup) {
                Ok(destination) => destination,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(DmGrantError::Persistence(format!(
                        "{}: {error}",
                        backup.display()
                    )));
                }
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(error) = fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))
                {
                    drop(destination);
                    let _ = fs::remove_file(&backup);
                    return Err(DmGrantError::Persistence(format!(
                        "{}: {error}",
                        backup.display()
                    )));
                }
            }
            if let Err(error) = io::copy(&mut source, &mut destination) {
                drop(destination);
                let _ = fs::remove_file(&backup);
                return Err(DmGrantError::Persistence(format!(
                    "{}: {error}",
                    backup.display()
                )));
            }
            if let Err(error) = destination.sync_all() {
                drop(destination);
                let _ = fs::remove_file(&backup);
                return Err(DmGrantError::Persistence(format!(
                    "{}: {error}",
                    backup.display()
                )));
            }
            drop(destination);
            return Ok(Some(backup));
        }
        Err(DmGrantError::Persistence(format!(
            "could not allocate a copied rollback snapshot beside {}",
            path.display()
        )))
    }

    /// Replace `path` with its same-directory temporary file without deleting
    /// the old state before the replacement is durable.
    fn replace_grant_store(temporary: &Path, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            return fs::rename(temporary, path);
        }
        #[cfg(windows)]
        {
            let _ = (temporary, path);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "durable DM grants require owner-only ACL enforcement",
            ));
        }
        #[cfg(not(any(unix, windows)))]
        {
            fs::rename(temporary, path)
        }
    }

    /// Synchronize a directory entry after a durable replacement. Unix
    /// requires an explicit directory fsync to make the rename survive a
    /// crash.
    fn sync_parent_directory(&self, parent: &Path, post_replace: bool) -> Result<(), DmGrantError> {
        #[cfg(test)]
        if post_replace {
            if let Some(sync) = &self.post_replace_directory_sync_override {
                return sync(parent);
            }
        }
        let _ = post_replace;

        #[cfg(unix)]
        {
            return fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| {
                    DmGrantError::Persistence(format!(
                        "sync directory {}: {error}",
                        parent.display()
                    ))
                });
        }
        #[cfg(not(unix))]
        {
            let _ = parent;
            Ok(())
        }
    }

    /// Restore the prior durable snapshot when the post-replacement directory
    /// sync fails. It is invoked before grant memory can be committed.
    fn rollback_replaced_grant_store(
        &self,
        parent: &Path,
        path: &Path,
        backup: Option<&Path>,
    ) -> Result<(), DmGrantError> {
        match backup {
            Some(backup) => Self::replace_grant_store(backup, path).map_err(|error| {
                DmGrantError::Persistence(format!("rollback {}: {error}", path.display()))
            })?,
            None => match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(DmGrantError::Persistence(format!(
                        "rollback {}: {error}",
                        path.display()
                    )));
                }
            },
        }
        self.sync_parent_directory(parent, false)
    }

    fn write_grant_store(
        &self,
        path: &Path,
        grants: &[DmGrant],
        pending: &[PendingDmPairing],
    ) -> Result<(), DmGrantError> {
        let payload = serde_json::to_vec_pretty(&DmGrantStore {
            version: DM_GRANT_STORE_VERSION,
            grants: grants.to_vec(),
            pending: pending.to_vec(),
        })
        .map_err(|error| DmGrantError::Persistence(error.to_string()))?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| DmGrantError::Persistence(format!("{}: {error}", parent.display())))?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("dm-grants.json");

        for _ in 0..PAIRING_CODE_ATTEMPTS {
            let temporary = parent.join(format!(".{filename}.{}.tmp", uuid::Uuid::new_v4()));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&temporary) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(DmGrantError::Persistence(format!(
                        "{}: {error}",
                        temporary.display()
                    )));
                }
            };
            if let Err(error) = file.write_all(&payload) {
                drop(file);
                let _ = fs::remove_file(&temporary);
                return Err(DmGrantError::Persistence(format!(
                    "{}: {error}",
                    temporary.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(error) =
                    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
                {
                    drop(file);
                    let _ = fs::remove_file(&temporary);
                    return Err(DmGrantError::Persistence(format!(
                        "{}: {error}",
                        temporary.display()
                    )));
                }
            }
            if let Err(error) = file.sync_all() {
                drop(file);
                let _ = fs::remove_file(&temporary);
                return Err(DmGrantError::Persistence(format!(
                    "{}: {error}",
                    temporary.display()
                )));
            }
            drop(file);

            let backup = match Self::create_rollback_backup(path, parent, filename) {
                Ok(backup) => backup,
                Err(error) => {
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
            };
            if backup.is_some() {
                if let Err(error) = self.sync_parent_directory(parent, false) {
                    if let Some(backup) = backup.as_deref() {
                        let _ = fs::remove_file(backup);
                    }
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
            }

            if let Err(error) = Self::replace_grant_store(&temporary, path) {
                let _ = fs::remove_file(&temporary);
                if let Some(backup) = backup.as_deref() {
                    let _ = fs::remove_file(backup);
                }
                return Err(DmGrantError::Persistence(format!(
                    "{}: {error}",
                    path.display()
                )));
            }
            if let Err(sync_error) = self.sync_parent_directory(parent, true) {
                return match self.rollback_replaced_grant_store(parent, path, backup.as_deref()) {
                    Ok(()) => Err(sync_error),
                    Err(rollback_error) => Err(DmGrantError::Persistence(format!(
                        "{sync_error}; rollback failed: {rollback_error}"
                    ))),
                };
            }
            if let Some(backup) = backup {
                let _ = fs::remove_file(backup);
            }
            return Ok(());
        }

        Err(DmGrantError::Persistence(format!(
            "could not allocate an atomic temporary file beside {}",
            path.display()
        )))
    }

    fn persist_state(
        &self,
        grants: &[DmGrant],
        pending: &[PendingDmPairing],
    ) -> Result<(), DmGrantError> {
        let Some(path) = self.grant_store_path.as_deref() else {
            return Ok(());
        };
        #[cfg(test)]
        if let Some(writer) = &self.store_write_override {
            return writer(path, grants);
        }
        self.write_grant_store(path, grants, pending)
    }

    fn grant_is_active_for_dm(&self, platform: &str, chat_id: &str, user_id: &str) -> bool {
        let Some(target) = Self::pairing_key(platform, chat_id, user_id) else {
            return false;
        };
        let Some(now) = self.now_unix_seconds() else {
            return false;
        };
        self.grants.iter().any(|grant| {
            grant.version == DM_GRANT_VERSION
                && grant.scope == DmGrantScope::DirectMessage
                && grant.operation == DmGrantOperation::DirectMessage
                && grant.platform == target.platform
                && grant.chat_id == target.chat_id
                && grant.user_id == target.user_id
                && grant.revoked_at_unix_seconds.is_none()
                && now < grant.expires_at_unix_seconds
        })
    }

    fn next_available_pairing_code(&self, pairings: &PendingPairings) -> Option<String> {
        for _ in 0..PAIRING_CODE_ATTEMPTS {
            let generated = (self.pairing_code_generator)();
            let Some(code) = Self::normalize_pairing_code(&generated) else {
                continue;
            };
            if !pairings.by_code.contains_key(&code)
                && !self.grants.iter().any(|grant| grant.pairing_code == code)
            {
                return Some(code);
            }
        }
        None
    }

    fn pending_snapshot(pairings: &PendingPairings) -> Vec<PendingDmPairing> {
        let mut pending = pairings.by_code.values().cloned().collect::<Vec<_>>();
        pending.sort_by(|left, right| left.code.cmp(&right.code));
        pending
    }

    fn pairing_message(code: &str) -> String {
        format!(
            "Your pairing code is {code}. Send this code to an admin to approve this gateway session."
        )
    }

    fn pending_pairing_message(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
    ) -> Option<String> {
        let key = Self::pairing_key(platform, chat_id, user_id)?;
        if self.pairing_rate_limited.lock().contains(&key) {
            return None;
        }
        let mut pairings = self.pairing_codes.lock();
        if let Some(code) = pairings.by_target.get(&key) {
            return Some(Self::pairing_message(code));
        }
        let code = self.next_available_pairing_code(&pairings)?;
        let pending = PendingDmPairing {
            code: code.clone(),
            platform: key.platform.clone(),
            chat_id: key.chat_id.clone(),
            user_id: key.user_id.clone(),
        };
        let mut staged_pending = Self::pending_snapshot(&pairings);
        staged_pending.push(pending.clone());
        if self.persist_state(&self.grants, &staged_pending).is_err() {
            return None;
        }
        pairings.by_target.insert(key, code.clone());
        pairings.by_code.insert(code.clone(), pending);
        Some(Self::pairing_message(&code))
    }

    /// Inspect the exact target a pending pairing code resolves to.
    pub fn pending_pairing_for_code(&self, code: &str) -> Option<PendingDmPairing> {
        let code = Self::normalize_pairing_code(code)?;
        self.pairing_codes.lock().by_code.get(&code).cloned()
    }

    /// Return the optional durable grant store path owned by this manager.
    pub fn grant_store_path(&self) -> Option<&Path> {
        self.grant_store_path.as_deref()
    }

    /// Handle a DM without chat context.
    ///
    /// This preserves configured Pair/Ignore behavior for legacy callers, but
    /// never issues a code or authorizes a durable scoped grant without the
    /// exact chat identity required by `handle_dm_for_chat`.
    pub async fn handle_dm(&self, user_id: &str, platform: &str) -> DmDecision {
        let platform = Self::platform_key(platform);
        if self.is_authorized_for_platform(&platform, user_id) {
            return DmDecision::Allow;
        }
        match self
            .platform_unauthorized_dm_behavior
            .get(&platform)
            .copied()
            .unwrap_or(self.unauthorized_dm_behavior)
        {
            UnauthorizedDmBehavior::Pair => DmDecision::Pair { message: None },
            UnauthorizedDmBehavior::Ignore => DmDecision::Deny,
        }
    }

    /// Handle a DM using the source context required by durable pairing grants.
    ///
    /// A grant may authorize only a non-slash direct message from its exact
    /// platform/chat/user tuple. Configured allowlists retain their existing
    /// behavior and can still authorize a configured administrator's command.
    pub async fn handle_dm_for_chat(
        &self,
        user_id: &str,
        platform: &str,
        chat_id: &str,
        is_slash_command: bool,
    ) -> DmDecision {
        let platform = Self::platform_key(platform);
        if self.is_authorized_for_platform(&platform, user_id) {
            return DmDecision::Allow;
        }
        if is_slash_command {
            return DmDecision::Deny;
        }
        if self.grant_is_active_for_dm(&platform, chat_id, user_id) {
            return DmDecision::Allow;
        }

        let behavior = self
            .platform_unauthorized_dm_behavior
            .get(&platform)
            .copied()
            .unwrap_or(self.unauthorized_dm_behavior);
        match behavior {
            UnauthorizedDmBehavior::Pair => {
                match self.pending_pairing_message(&platform, chat_id, user_id) {
                    Some(message) => DmDecision::Pair {
                        message: Some(message),
                    },
                    None => DmDecision::Deny,
                }
            }
            UnauthorizedDmBehavior::Ignore => DmDecision::Deny,
        }
    }

    /// Persist an approval for the exact target behind a pending code, then
    /// commit it to memory only after the durable snapshot succeeds.
    pub fn approve_pairing_code(
        &mut self,
        platform: &str,
        admin_user_id: &str,
        admin_chat_id: &str,
        admin_is_dm: bool,
        code: &str,
    ) -> Result<DmGrant, DmGrantError> {
        let platform = Self::platform_key(platform);
        if !admin_is_dm || admin_chat_id.trim().is_empty() {
            return Err(DmGrantError::NotDirectMessage);
        }
        if !self.is_admin_for_platform(&platform, admin_user_id) {
            return Err(DmGrantError::NotAdmin);
        }
        let code = Self::normalize_pairing_code(code).ok_or(DmGrantError::InvalidPairingCode)?;
        if let Some(existing) = self
            .grants
            .iter()
            .find(|grant| grant.pairing_code == code)
            .cloned()
        {
            if existing.platform != platform {
                return Err(DmGrantError::WrongPlatform);
            }
            let now = self
                .now_unix_seconds()
                .ok_or(DmGrantError::ClockUnavailable)?;
            if existing.revoked_at_unix_seconds.is_none()
                && existing.scope == DmGrantScope::DirectMessage
                && existing.operation == DmGrantOperation::DirectMessage
                && now < existing.expires_at_unix_seconds
            {
                return Ok(existing);
            }
        }
        let (pending, staged_pending) = {
            let pairings = self.pairing_codes.lock();
            let pending = pairings
                .by_code
                .get(&code)
                .cloned()
                .ok_or(DmGrantError::PairingNotFound)?;
            let staged_pending = Self::pending_snapshot(&pairings)
                .into_iter()
                .filter(|existing| existing.code != code)
                .collect::<Vec<_>>();
            (pending, staged_pending)
        };
        if pending.platform != platform {
            return Err(DmGrantError::WrongPlatform);
        }
        let issued_at_unix_seconds = self
            .now_unix_seconds()
            .ok_or(DmGrantError::ClockUnavailable)?;
        let expires_at_unix_seconds = issued_at_unix_seconds
            .checked_add(DEFAULT_DM_GRANT_TTL_SECS)
            .ok_or(DmGrantError::ClockUnavailable)?;
        let grant = DmGrant {
            version: DM_GRANT_VERSION,
            pairing_code: pending.code.clone(),
            platform: pending.platform.clone(),
            chat_id: pending.chat_id.clone(),
            user_id: pending.user_id.clone(),
            scope: DmGrantScope::DirectMessage,
            operation: DmGrantOperation::DirectMessage,
            admin_origin: DmGrantAdminOrigin {
                platform: platform.clone(),
                chat_id: admin_chat_id.trim().to_string(),
                user_id: admin_user_id.trim().to_string(),
            },
            issued_at_unix_seconds,
            expires_at_unix_seconds,
            revoked_at_unix_seconds: None,
        };
        let mut staged = self.grants.clone();
        for existing in &mut staged {
            if existing.platform == grant.platform
                && existing.chat_id == grant.chat_id
                && existing.user_id == grant.user_id
                && existing.revoked_at_unix_seconds.is_none()
            {
                existing.revoked_at_unix_seconds =
                    Some(issued_at_unix_seconds.max(existing.issued_at_unix_seconds));
            }
        }
        staged.push(grant.clone());
        self.persist_state(&staged, &staged_pending)?;
        self.grants = staged;

        let target = Self::pairing_key(&pending.platform, &pending.chat_id, &pending.user_id)
            .expect("pending pairing has a validated target");
        let mut pairings = self.pairing_codes.lock();
        pairings.by_code.remove(&pending.code);
        pairings.by_target.remove(&target);
        drop(pairings);
        self.pairing_rate_limited.lock().remove(&target);
        Ok(grant)
    }

    /// Revoke an active grant or persistently deny a pending request by
    /// resolving its opaque code to the actual platform/chat/user target.
    pub fn revoke_pairing_code(
        &mut self,
        platform: &str,
        admin_user_id: &str,
        admin_chat_id: &str,
        admin_is_dm: bool,
        code: &str,
    ) -> Result<PendingDmPairing, DmGrantError> {
        let platform = Self::platform_key(platform);
        if !admin_is_dm || admin_chat_id.trim().is_empty() {
            return Err(DmGrantError::NotDirectMessage);
        }
        if !self.is_admin_for_platform(&platform, admin_user_id) {
            return Err(DmGrantError::NotAdmin);
        }
        let code = Self::normalize_pairing_code(code).ok_or(DmGrantError::InvalidPairingCode)?;

        if let Some(grant) = self
            .grants
            .iter()
            .find(|grant| grant.pairing_code == code)
            .cloned()
        {
            if grant.platform != platform {
                return Err(DmGrantError::WrongPlatform);
            }
            let target = PendingDmPairing {
                code,
                platform: grant.platform,
                chat_id: grant.chat_id,
                user_id: grant.user_id,
            };
            if grant.revoked_at_unix_seconds.is_some() {
                return Ok(target);
            }
            let revoked_at_unix_seconds = self
                .now_unix_seconds()
                .ok_or(DmGrantError::ClockUnavailable)?;
            let staged_pending = {
                let pairings = self.pairing_codes.lock();
                Self::pending_snapshot(&pairings)
            };
            let staged = self
                .grants
                .iter()
                .cloned()
                .map(|mut existing| {
                    if existing.pairing_code == target.code {
                        existing.revoked_at_unix_seconds =
                            Some(revoked_at_unix_seconds.max(existing.issued_at_unix_seconds));
                    }
                    existing
                })
                .collect::<Vec<_>>();
            self.persist_state(&staged, &staged_pending)?;
            self.grants = staged;
            return Ok(target);
        }

        let (pending, staged_pending) = {
            let pairings = self.pairing_codes.lock();
            let pending = pairings
                .by_code
                .get(&code)
                .cloned()
                .ok_or(DmGrantError::PairingNotFound)?;
            let staged_pending = Self::pending_snapshot(&pairings)
                .into_iter()
                .filter(|existing| existing.code != code)
                .collect::<Vec<_>>();
            (pending, staged_pending)
        };
        if pending.platform != platform {
            return Err(DmGrantError::WrongPlatform);
        }
        let revoked_at_unix_seconds = self
            .now_unix_seconds()
            .ok_or(DmGrantError::ClockUnavailable)?;
        let tombstone = DmGrant {
            version: DM_GRANT_VERSION,
            pairing_code: pending.code.clone(),
            platform: pending.platform.clone(),
            chat_id: pending.chat_id.clone(),
            user_id: pending.user_id.clone(),
            scope: DmGrantScope::DirectMessage,
            operation: DmGrantOperation::DirectMessage,
            admin_origin: DmGrantAdminOrigin {
                platform: platform.clone(),
                chat_id: admin_chat_id.trim().to_string(),
                user_id: admin_user_id.trim().to_string(),
            },
            issued_at_unix_seconds: revoked_at_unix_seconds,
            expires_at_unix_seconds: revoked_at_unix_seconds,
            revoked_at_unix_seconds: Some(revoked_at_unix_seconds),
        };
        let mut staged = self.grants.clone();
        staged.push(tombstone);
        self.persist_state(&staged, &staged_pending)?;
        self.grants = staged;

        let target = Self::pairing_key(&pending.platform, &pending.chat_id, &pending.user_id)
            .expect("pending pairing has a validated target");
        let mut pairings = self.pairing_codes.lock();
        pairings.by_code.remove(&pending.code);
        pairings.by_target.remove(&target);
        drop(pairings);
        self.pairing_rate_limited.lock().insert(target);
        Ok(pending)
    }

    #[cfg(test)]
    fn set_pairing_code_generator_for_tests(
        &mut self,
        generator: impl Fn() -> String + Send + Sync + 'static,
    ) {
        self.pairing_code_generator = Arc::new(generator);
    }

    #[cfg(test)]
    pub(crate) fn set_store_write_override_for_tests(&mut self, writer: DmStoreWriteOverride) {
        self.store_write_override = Some(writer);
    }

    #[cfg(test)]
    pub(crate) fn set_post_replace_directory_sync_override_for_tests(
        &mut self,
        sync: DmStorePostReplaceSyncOverride,
    ) {
        self.post_replace_directory_sync_override = Some(sync);
    }

    /// Add a user to the authorized users set.
    pub fn authorize_user(&mut self, user_id: impl Into<String>) {
        self.authorized_users.insert(user_id.into());
    }

    /// Add a platform-scoped user to the authorized users set.
    pub fn authorize_user_for_platform(
        &mut self,
        platform: impl AsRef<str>,
        user_id: impl Into<String>,
    ) {
        self.platform_authorized_users
            .entry(Self::platform_key(platform.as_ref()))
            .or_default()
            .insert(user_id.into());
    }

    /// Add a platform-scoped group sender allowlist entry.
    pub fn authorize_group_user_for_platform(
        &mut self,
        platform: impl AsRef<str>,
        user_id: impl Into<String>,
    ) {
        self.platform_group_authorized_users
            .entry(Self::platform_key(platform.as_ref()))
            .or_default()
            .insert(user_id.into());
    }

    /// Add a platform-scoped group chat allowlist entry.
    pub fn authorize_group_chat_for_platform(
        &mut self,
        platform: impl AsRef<str>,
        chat_id: impl Into<String>,
    ) {
        self.platform_group_authorized_chats
            .entry(Self::platform_key(platform.as_ref()))
            .or_default()
            .insert(chat_id.into());
    }

    /// Add a platform-scoped user alias, for example WhatsApp LID <-> phone ID.
    pub fn add_user_alias_for_platform(
        &mut self,
        platform: impl AsRef<str>,
        user_id: impl AsRef<str>,
        alias: impl AsRef<str>,
    ) {
        let platform = Self::platform_key(platform.as_ref());
        let user_id = user_id.as_ref().trim();
        let alias = alias.as_ref().trim();
        if user_id.is_empty() || alias.is_empty() {
            return;
        }
        let aliases = self
            .platform_user_aliases
            .entry(platform.clone())
            .or_default();
        Self::insert_alias(aliases, &platform, user_id, alias);
        Self::insert_alias(aliases, &platform, alias, user_id);
    }

    /// Load WhatsApp LID mapping files from `$HERMES_HOME/whatsapp/session`.
    pub fn load_whatsapp_lid_mappings_from_home(&mut self, home: impl AsRef<Path>) -> usize {
        let session_dir = home.as_ref().join("whatsapp").join("session");
        let Ok(entries) = std::fs::read_dir(session_dir) else {
            return 0;
        };
        let mut loaded = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(raw_key) = file_name
                .strip_prefix("lid-mapping-")
                .and_then(|s| s.strip_suffix(".json"))
            else {
                continue;
            };
            let key = raw_key.strip_suffix("_reverse").unwrap_or(raw_key);
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<String>(&raw) else {
                continue;
            };
            self.add_user_alias_for_platform("whatsapp", key, value);
            loaded += 1;
        }
        loaded
    }

    /// Remove a user from the authorized users set.
    pub fn deauthorize_user(&mut self, user_id: &str) {
        self.authorized_users.remove(user_id);
    }

    /// Add a user to the admin users set.
    pub fn add_admin(&mut self, user_id: impl Into<String>) {
        self.admin_users.insert(user_id.into());
    }

    /// Add a platform-scoped admin user.
    pub fn add_admin_for_platform(
        &mut self,
        platform: impl AsRef<str>,
        user_id: impl Into<String>,
    ) {
        self.platform_admin_users
            .entry(Self::platform_key(platform.as_ref()))
            .or_default()
            .insert(user_id.into());
    }

    /// Remove a user from the admin users set.
    pub fn remove_admin(&mut self, user_id: &str) {
        self.admin_users.remove(user_id);
    }

    /// Check if a user is authorized.
    pub fn is_authorized(&self, user_id: &str) -> bool {
        Self::user_matches_any(user_id, &self.authorized_users)
            || Self::user_matches_any(user_id, &self.admin_users)
    }

    /// Check configured authorization for the given platform.
    ///
    /// Durable pairing grants are deliberately absent: callers without an
    /// exact DM chat and operation must fail closed rather than reuse a grant.
    pub fn is_authorized_for_platform(&self, platform: &str, user_id: &str) -> bool {
        let platform = Self::platform_key(platform);
        self.platform_authorized_users
            .get(&platform)
            .is_some_and(|users| self.user_matches_platform_set(&platform, user_id, users))
            || self
                .platform_admin_users
                .get(&platform)
                .is_some_and(|admins| self.user_matches_platform_set(&platform, user_id, admins))
            || self.user_matches_platform_set(&platform, user_id, &self.authorized_users)
            || self.user_matches_platform_set(&platform, user_id, &self.admin_users)
    }

    /// Check a configured group source without consulting DM pairing grants.
    ///
    /// Routing uses `PlatformAccessPolicy` directly; this compatibility helper
    /// intentionally cannot turn a DM grant into group authorization.
    pub fn is_authorized_source(
        &self,
        platform: &str,
        user_id: &str,
        chat_id: &str,
        is_dm: bool,
    ) -> bool {
        if is_dm {
            return self.is_authorized_for_platform(platform, user_id);
        }

        let platform = Self::platform_key(platform);
        self.platform_group_authorized_users
            .get(&platform)
            .is_some_and(|users| self.user_matches_platform_set(&platform, user_id, users))
            || self
                .platform_group_authorized_chats
                .get(&platform)
                .is_some_and(|chats| Self::chat_matches_any(chat_id, chats))
    }

    /// Check if a user is an explicitly global admin.
    pub fn is_admin(&self, user_id: &str) -> bool {
        Self::user_matches_any(user_id, &self.admin_users)
    }

    /// Check admin authority for the current platform.
    pub fn is_admin_for_platform(&self, platform: &str, user_id: &str) -> bool {
        let platform = Self::platform_key(platform);
        self.platform_admin_users
            .get(&platform)
            .is_some_and(|admins| self.user_matches_platform_set(&platform, user_id, admins))
            || self.user_matches_platform_set(&platform, user_id, &self.admin_users)
    }

    /// Get the number of authorized users.
    pub fn authorized_user_count(&self) -> usize {
        self.authorized_users.len()
    }

    /// Get the number of admin users.
    pub fn admin_user_count(&self) -> usize {
        self.admin_users.len()
    }

    /// Override unauthorized-DM behavior for a platform.
    pub fn set_platform_unauthorized_behavior(
        &mut self,
        platform: impl AsRef<str>,
        behavior: UnauthorizedDmBehavior,
    ) {
        self.platform_unauthorized_dm_behavior
            .insert(Self::platform_key(platform.as_ref()), behavior);
    }

    /// Mark a pairing flow as rate-limited for one exact direct-message target.
    pub fn record_pairing_rate_limit_for_chat(&self, platform: &str, chat_id: &str, user_id: &str) {
        if let Some(key) = Self::pairing_key(platform, chat_id, user_id) {
            self.pairing_rate_limited.lock().insert(key);
        }
    }

    /// Legacy helper for platforms whose DM chat ID is the user ID.
    pub fn record_pairing_rate_limit(&self, platform: &str, user_id: &str) {
        self.record_pairing_rate_limit_for_chat(platform, user_id, user_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    async fn request_pairing(
        dm: &DmManager,
        platform: &str,
        chat_id: &str,
        user_id: &str,
    ) -> String {
        let DmDecision::Pair {
            message: Some(message),
        } = dm
            .handle_dm_for_chat(user_id, platform, chat_id, false)
            .await
        else {
            panic!("expected pairing decision");
        };
        message
            .strip_prefix("Your pairing code is ")
            .and_then(|message| message.split('.').next())
            .expect("pairing code in response")
            .to_string()
    }

    fn test_clock(seconds: Arc<Mutex<i64>>) -> DmClock {
        Arc::new(move || {
            let seconds = *seconds.lock();
            UNIX_EPOCH + Duration::from_secs(seconds.try_into().expect("non-negative clock"))
        })
    }

    #[tokio::test]
    async fn dm_manager_allows_configured_user_and_admin() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.authorize_user("user1");
        dm.add_admin("admin1");

        assert_eq!(
            dm.handle_dm_for_chat("user1", "telegram", "chat-user1", false)
                .await,
            DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm_for_chat("admin1", "discord", "chat-admin1", true)
                .await,
            DmDecision::Allow
        );
    }

    #[tokio::test]
    async fn dm_manager_pairs_only_when_source_chat_is_available() {
        let dm = DmManager::with_pair_behavior();

        assert!(matches!(
            dm.handle_dm_for_chat("unknown_user", "telegram", "dm-1", false)
                .await,
            DmDecision::Pair { .. }
        ));
        assert_eq!(
            dm.handle_dm("unknown_user", "telegram").await,
            DmDecision::Pair { message: None }
        );
    }

    #[tokio::test]
    async fn dm_manager_ignore_behavior_denies_unknown_user() {
        let dm = DmManager::with_ignore_behavior();
        assert_eq!(
            dm.handle_dm_for_chat("unknown_user", "telegram", "dm-1", false)
                .await,
            DmDecision::Deny
        );
    }

    #[test]
    fn dm_manager_authorize_and_deauthorize() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.authorize_user("user1");
        assert!(dm.is_authorized("user1"));

        dm.deauthorize_user("user1");
        assert!(!dm.is_authorized("user1"));
    }

    #[tokio::test]
    async fn dm_manager_wildcard_authorizes_any_non_empty_user() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.authorize_user("*");

        assert_eq!(
            dm.handle_dm_for_chat("user1", "telegram", "chat-1", false)
                .await,
            DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm_for_chat("999", "discord", "chat-2", false)
                .await,
            DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm_for_chat("", "discord", "chat-3", false).await,
            DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn configured_platform_allowlist_does_not_cross_authorize() {
        let mut dm = DmManager::with_pair_behavior();
        dm.authorize_user_for_platform("telegram", "123");
        dm.set_platform_unauthorized_behavior("telegram", UnauthorizedDmBehavior::Ignore);

        assert_eq!(
            dm.handle_dm_for_chat("123", "telegram", "dm-123", false)
                .await,
            DmDecision::Allow
        );
        assert!(matches!(
            dm.handle_dm_for_chat("123", "discord", "dm-123", false)
                .await,
            DmDecision::Pair { .. }
        ));
        assert_eq!(
            dm.handle_dm_for_chat("999", "telegram", "dm-999", false)
                .await,
            DmDecision::Deny
        );
    }

    #[test]
    fn platform_admin_authority_is_not_cross_platform() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.add_admin_for_platform("telegram", "platform-admin");

        assert!(dm.is_admin_for_platform("telegram", "platform-admin"));
        assert!(!dm.is_admin_for_platform("discord", "platform-admin"));
    }

    #[tokio::test]
    async fn configured_group_allowlists_do_not_authorize_dms() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.authorize_group_user_for_platform("telegram", "999");
        dm.authorize_group_chat_for_platform("telegram", "-1001878443972");

        assert!(dm.is_authorized_source("telegram", "999", "-1001878443972", false));
        assert!(dm.is_authorized_source("telegram", "123", "-1001878443972", false));
        assert!(!dm.is_authorized_source("telegram", "999", "999", true));
    }

    #[tokio::test]
    async fn whatsapp_lid_alias_authorizes_configured_phone_allowlist() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.authorize_user_for_platform("whatsapp", "15550000001");
        dm.add_user_alias_for_platform("whatsapp", "900000000000001", "15550000001");

        assert_eq!(
            dm.handle_dm_for_chat(
                "900000000000001@lid",
                "whatsapp",
                "900000000000001@lid",
                false
            )
            .await,
            DmDecision::Allow
        );
    }

    #[tokio::test]
    async fn rate_limited_pairing_is_bound_to_the_exact_chat() {
        let dm = DmManager::with_pair_behavior();
        assert!(matches!(
            dm.handle_dm_for_chat("user", "whatsapp", "dm-one", false)
                .await,
            DmDecision::Pair { .. }
        ));

        dm.record_pairing_rate_limit_for_chat("whatsapp", "dm-one", "user");
        assert_eq!(
            dm.handle_dm_for_chat("user", "whatsapp", "dm-one", false)
                .await,
            DmDecision::Deny
        );
        assert!(matches!(
            dm.handle_dm_for_chat("user", "whatsapp", "dm-two", false)
                .await,
            DmDecision::Pair { .. }
        ));
    }

    #[tokio::test]
    async fn approval_resolves_code_to_exact_target_and_scopes_the_grant() {
        let mut dm = DmManager::with_pair_behavior();
        dm.add_admin_for_platform("telegram", "admin");
        let code = request_pairing(&dm, "Telegram", "target-dm", "target-user").await;

        let pending = dm.pending_pairing_for_code(&code).expect("pending pairing");
        assert_eq!(pending.platform, "telegram");
        assert_eq!(pending.chat_id, "target-dm");
        assert_eq!(pending.user_id, "target-user");

        let grant = dm
            .approve_pairing_code("telegram", "admin", "admin-dm", true, &code)
            .expect("approval");
        assert_eq!(grant.platform, "telegram");
        assert_eq!(grant.chat_id, "target-dm");
        assert_eq!(grant.user_id, "target-user");
        assert_ne!(grant.user_id, code);
        assert_eq!(grant.admin_origin.platform, "telegram");
        assert_eq!(grant.admin_origin.user_id, "admin");
        assert_eq!(grant.admin_origin.chat_id, "admin-dm");
        assert_eq!(grant.scope, DmGrantScope::DirectMessage);
        assert_eq!(grant.operation, DmGrantOperation::DirectMessage);
        assert_eq!(
            dm.approve_pairing_code("telegram", "admin", "admin-dm", true, &code)
                .expect("replayed approval"),
            grant
        );
        assert_eq!(dm.grants.len(), 1);

        dm.set_platform_unauthorized_behavior("telegram", UnauthorizedDmBehavior::Ignore);
        dm.set_platform_unauthorized_behavior("discord", UnauthorizedDmBehavior::Ignore);
        assert_eq!(
            dm.handle_dm_for_chat("target-user", "telegram", "target-dm", false)
                .await,
            DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm_for_chat("target-user", "telegram", "other-dm", false)
                .await,
            DmDecision::Deny
        );
        assert_eq!(
            dm.handle_dm_for_chat("target-user", "discord", "target-dm", false)
                .await,
            DmDecision::Deny
        );
        assert_eq!(
            dm.handle_dm_for_chat("target-user", "telegram", "target-dm", true)
                .await,
            DmDecision::Deny
        );
        assert!(!dm.is_authorized_source("telegram", "target-user", "group-1", false));
    }

    #[tokio::test]
    async fn pending_codes_are_collision_safe_and_code_addressable() {
        let candidates = Arc::new(Mutex::new(
            vec![
                "AAAAAAAA".to_string(),
                "AAAAAAAA".to_string(),
                "BBBBBBBB".to_string(),
            ]
            .into_iter()
            .collect::<VecDeque<_>>(),
        ));
        let mut dm = DmManager::with_pair_behavior();
        dm.set_pairing_code_generator_for_tests({
            let candidates = candidates.clone();
            move || candidates.lock().pop_front().expect("candidate")
        });

        let first = request_pairing(&dm, "telegram", "dm-one", "user-one").await;
        let second = request_pairing(&dm, "telegram", "dm-two", "user-two").await;
        assert_eq!(first, "AAAAAAAA");
        assert_eq!(second, "BBBBBBBB");
        assert_eq!(
            dm.pending_pairing_for_code(&second)
                .expect("second pending")
                .chat_id,
            "dm-two"
        );
    }

    #[tokio::test]
    async fn revoked_codes_remain_bound_after_restart_and_cannot_be_reassigned() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let mut dm = DmManager::with_pair_behavior_and_grant_store(Some(store.clone()))
            .expect("new durable manager");
        dm.add_admin_for_platform("telegram", "admin");
        dm.set_pairing_code_generator_for_tests(|| "AAAAAAAA".to_string());
        let retired_code = request_pairing(&dm, "telegram", "first-dm", "first-user").await;
        dm.approve_pairing_code("telegram", "admin", "admin-dm", true, &retired_code)
            .expect("approval");
        dm.revoke_pairing_code("telegram", "admin", "admin-dm", true, &retired_code)
            .expect("revoke");

        let snapshot: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&store).expect("grant store")).unwrap();
        assert_eq!(snapshot["grants"][0]["pairing_code"], retired_code.as_str());
        assert!(snapshot["grants"][0]["revoked_at_unix_seconds"].is_number());

        let candidates = Arc::new(Mutex::new(
            vec!["AAAAAAAA".to_string(), "BBBBBBBB".to_string()]
                .into_iter()
                .collect::<VecDeque<_>>(),
        ));
        let mut restarted =
            DmManager::with_pair_behavior_and_grant_store(Some(store)).expect("restart");
        restarted.add_admin_for_platform("telegram", "admin");
        restarted.set_pairing_code_generator_for_tests({
            let candidates = candidates.clone();
            move || candidates.lock().pop_front().expect("candidate")
        });
        let replacement_code =
            request_pairing(&restarted, "telegram", "second-dm", "second-user").await;
        assert_eq!(replacement_code, "BBBBBBBB");
        assert_eq!(
            restarted
                .approve_pairing_code("telegram", "admin", "admin-dm", true, &retired_code)
                .expect_err("retired code cannot approve a later target"),
            DmGrantError::PairingNotFound
        );
        assert_eq!(
            restarted
                .pending_pairing_for_code(&replacement_code)
                .expect("replacement pairing")
                .user_id,
            "second-user"
        );
    }

    #[tokio::test]
    async fn pending_codes_survive_restart_and_denial_reserves_their_binding() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let mut dm = DmManager::with_pair_behavior_and_grant_store(Some(store.clone()))
            .expect("new durable manager");
        dm.set_pairing_code_generator_for_tests(|| "AAAAAAAA".to_string());
        let pending_code = request_pairing(&dm, "telegram", "first-dm", "first-user").await;

        let candidates = Arc::new(Mutex::new(
            vec!["AAAAAAAA".to_string(), "BBBBBBBB".to_string()]
                .into_iter()
                .collect::<VecDeque<_>>(),
        ));
        let mut restarted =
            DmManager::with_pair_behavior_and_grant_store(Some(store.clone())).expect("restart");
        restarted.add_admin_for_platform("telegram", "admin");
        assert_eq!(
            restarted
                .pending_pairing_for_code(&pending_code)
                .expect("persisted pending pairing")
                .user_id,
            "first-user"
        );
        restarted
            .revoke_pairing_code("telegram", "admin", "admin-dm", true, &pending_code)
            .expect("deny persisted pending pairing");

        restarted.set_pairing_code_generator_for_tests({
            let candidates = candidates.clone();
            move || candidates.lock().pop_front().expect("candidate")
        });
        let replacement_code =
            request_pairing(&restarted, "telegram", "second-dm", "second-user").await;
        assert_eq!(replacement_code, "BBBBBBBB");
        assert_eq!(
            restarted
                .approve_pairing_code("telegram", "admin", "admin-dm", true, &pending_code)
                .expect_err("denied code cannot approve a later target"),
            DmGrantError::PairingNotFound
        );
    }

    #[tokio::test]
    async fn durable_grants_survive_restart_and_store_owner_only_snapshot() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("gateway").join("dm-grants.json");
        let mut dm = DmManager::with_pair_behavior_and_grant_store(Some(store.clone()))
            .expect("new durable manager");
        dm.add_admin_for_platform("telegram", "admin");
        let code = request_pairing(&dm, "telegram", "target-dm", "target-user").await;
        dm.approve_pairing_code("telegram", "admin", "admin-dm", true, &code)
            .expect("approval");
        assert!(store.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&store)
                    .expect("grant store metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }

        let mut restarted = DmManager::with_ignore_behavior_and_grant_store(Some(store.clone()))
            .expect("restart loads durable grants");
        restarted.set_platform_unauthorized_behavior("telegram", UnauthorizedDmBehavior::Ignore);
        assert_eq!(
            restarted
                .handle_dm_for_chat("target-user", "telegram", "target-dm", false)
                .await,
            DmDecision::Allow
        );
        assert_eq!(
            restarted
                .handle_dm_for_chat("target-user", "telegram", "wrong-dm", false)
                .await,
            DmDecision::Deny
        );
        let snapshot: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&store).expect("grant store")).unwrap();
        assert_eq!(snapshot["version"], DM_GRANT_STORE_VERSION);
        assert_eq!(snapshot["grants"][0]["user_id"], "target-user");
        assert_eq!(snapshot["grants"][0]["platform"], "telegram");
        assert_eq!(snapshot["grants"][0]["chat_id"], "target-dm");
        assert_eq!(snapshot["grants"][0]["scope"], "direct_message");
        assert_eq!(snapshot["grants"][0]["operation"], "direct_message");
    }

    #[tokio::test]
    async fn expired_durable_grants_deny_after_restart_clock_advance() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let now = Arc::new(Mutex::new(1_000_000_i64));
        let mut dm = DmManager::with_pair_behavior_and_grant_store_and_clock(
            Some(store.clone()),
            test_clock(now.clone()),
        )
        .expect("new manager");
        dm.add_admin_for_platform("telegram", "admin");
        let code = request_pairing(&dm, "telegram", "target-dm", "target-user").await;
        dm.approve_pairing_code("telegram", "admin", "admin-dm", true, &code)
            .expect("approval");

        *now.lock() += DEFAULT_DM_GRANT_TTL_SECS;
        let mut restarted =
            DmManager::with_ignore_behavior_and_grant_store_and_clock(Some(store), test_clock(now))
                .expect("restart");
        restarted.set_platform_unauthorized_behavior("telegram", UnauthorizedDmBehavior::Ignore);
        assert_eq!(
            restarted
                .handle_dm_for_chat("target-user", "telegram", "target-dm", false)
                .await,
            DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn revocation_clamps_a_backward_clock_before_persisting_tombstone() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let now = Arc::new(Mutex::new(2_000_000_i64));
        let mut dm = DmManager::with_pair_behavior_and_grant_store_and_clock(
            Some(store.clone()),
            test_clock(now.clone()),
        )
        .expect("new manager");
        dm.add_admin_for_platform("telegram", "admin");
        let code = request_pairing(&dm, "telegram", "target-dm", "target-user").await;
        dm.approve_pairing_code("telegram", "admin", "admin-dm", true, &code)
            .expect("approval");

        *now.lock() = 1_000_000;
        dm.revoke_pairing_code("telegram", "admin", "admin-dm", true, &code)
            .expect("revocation with a backward clock");

        let restarted =
            DmManager::with_ignore_behavior_and_grant_store_and_clock(Some(store), test_clock(now))
                .expect("tombstone survives restart");
        assert_eq!(
            restarted
                .handle_dm_for_chat("target-user", "telegram", "target-dm", false)
                .await,
            DmDecision::Deny
        );
    }

    #[test]
    fn corrupt_or_unknown_version_store_fails_closed_on_load() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        std::fs::write(&store, r#"{"version":99,"grants":[]}"#).expect("write invalid store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600))
                .expect("make invalid store owner-only");
        }

        assert!(matches!(
            DmManager::with_ignore_behavior_and_grant_store(Some(store)),
            Err(DmGrantError::StoreLoad(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn non_owner_only_grant_store_fails_closed_on_load() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        std::fs::write(&store, r#"{"version":1,"grants":[]}"#).expect("write grant store");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o644))
            .expect("make grant store group-readable");

        assert!(matches!(
            DmManager::with_ignore_behavior_and_grant_store(Some(store)),
            Err(DmGrantError::StoreLoad(_))
        ));
    }

    #[test]
    fn copied_rollback_snapshot_preserves_contents_and_owner_only_mode() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let contents = r#"{"version":1,"grants":[],"pending":[]}"#;
        std::fs::write(&store, contents).expect("write source snapshot");

        let backup = DmManager::copy_rollback_backup(&store, temporary.path(), "dm-grants.json")
            .expect("copy rollback snapshot")
            .expect("source exists");
        assert_eq!(
            std::fs::read_to_string(&backup).expect("read copied rollback snapshot"),
            contents
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&backup)
                    .expect("copied rollback metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn durable_grant_store_fails_closed_without_owner_only_acl_support() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");

        assert!(matches!(
            DmManager::with_ignore_behavior_and_grant_store(Some(store)),
            Err(DmGrantError::StoreLoad(_))
        ));
    }

    #[tokio::test]
    async fn failed_initial_pending_snapshot_write_leaves_no_pairing_binding_or_disk() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let mut dm = DmManager::with_pair_behavior_and_grant_store(Some(store.clone()))
            .expect("new durable manager");
        dm.set_pairing_code_generator_for_tests(|| "AAAAAAAA".to_string());
        dm.set_store_write_override_for_tests(Arc::new(|_, _| {
            Err(DmGrantError::Persistence(
                "injected initial write failure".to_string(),
            ))
        }));

        assert_eq!(
            dm.handle_dm_for_chat("target-user", "telegram", "target-dm", false)
                .await,
            DmDecision::Deny
        );
        assert!(dm.pending_pairing_for_code("AAAAAAAA").is_none());
        assert!(!store.exists());
    }

    #[tokio::test]
    async fn failed_snapshot_write_preserves_memory_disk_and_pending_request() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let mut dm = DmManager::with_pair_behavior_and_grant_store(Some(store.clone()))
            .expect("new durable manager");
        dm.add_admin_for_platform("telegram", "admin");
        let existing_code = request_pairing(&dm, "telegram", "existing-dm", "existing-user").await;
        dm.approve_pairing_code("telegram", "admin", "admin-dm", true, &existing_code)
            .expect("initial approval");
        let pending_code = request_pairing(&dm, "telegram", "target-dm", "target-user").await;
        let disk_before = std::fs::read_to_string(&store).expect("grant store before mutation");
        let memory_before = dm.grants.clone();
        dm.set_store_write_override_for_tests(Arc::new(|_, _| {
            Err(DmGrantError::Persistence(
                "injected write failure".to_string(),
            ))
        }));

        let revoke_error = dm
            .revoke_pairing_code("telegram", "admin", "admin-dm", true, &existing_code)
            .expect_err("revoke write failure");
        assert!(revoke_error.is_persistence_error());
        assert_eq!(dm.grants, memory_before);
        assert_eq!(
            std::fs::read_to_string(&store).expect("unchanged grant store after revoke"),
            disk_before
        );

        let deny_error = dm
            .revoke_pairing_code("telegram", "admin", "admin-dm", true, &pending_code)
            .expect_err("pending denial write failure");
        assert!(deny_error.is_persistence_error());
        assert_eq!(dm.grants, memory_before);
        assert_eq!(
            std::fs::read_to_string(&store).expect("unchanged grant store after pending denial"),
            disk_before
        );
        assert!(dm.pending_pairing_for_code(&pending_code).is_some());

        let error = dm
            .approve_pairing_code("telegram", "admin", "admin-dm", true, &pending_code)
            .expect_err("write failure");
        assert!(error.is_persistence_error());
        assert_eq!(dm.grants, memory_before);
        assert_eq!(
            std::fs::read_to_string(&store).expect("unchanged grant store"),
            disk_before
        );
        assert!(dm.pending_pairing_for_code(&pending_code).is_some());
        dm.set_platform_unauthorized_behavior("telegram", UnauthorizedDmBehavior::Ignore);
        assert_eq!(
            dm.handle_dm_for_chat("existing-user", "telegram", "existing-dm", false)
                .await,
            DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm_for_chat("target-user", "telegram", "target-dm", false)
                .await,
            DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn post_replace_directory_sync_failure_restores_previous_snapshot() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let mut dm = DmManager::with_pair_behavior_and_grant_store(Some(store.clone()))
            .expect("new durable manager");
        dm.add_admin_for_platform("telegram", "admin");
        let existing_code = request_pairing(&dm, "telegram", "existing-dm", "existing-user").await;
        dm.approve_pairing_code("telegram", "admin", "admin-dm", true, &existing_code)
            .expect("initial approval");
        let memory_before = dm.grants.clone();
        let pending_code = request_pairing(&dm, "telegram", "target-dm", "target-user").await;
        let disk_before = std::fs::read_to_string(&store).expect("grant store before mutation");
        dm.set_post_replace_directory_sync_override_for_tests(Arc::new(|_| {
            Err(DmGrantError::Persistence(
                "injected post-replace directory sync failure".to_string(),
            ))
        }));

        let revoke_error = dm
            .revoke_pairing_code("telegram", "admin", "admin-dm", true, &existing_code)
            .expect_err("post-replace revoke failure");
        assert!(revoke_error.is_persistence_error());
        assert_eq!(dm.grants, memory_before);
        assert_eq!(
            std::fs::read_to_string(&store).expect("restored grant store after revoke"),
            disk_before
        );

        let approval_error = dm
            .approve_pairing_code("telegram", "admin", "admin-dm", true, &pending_code)
            .expect_err("post-replace approval failure");
        assert!(approval_error.is_persistence_error());
        assert_eq!(dm.grants, memory_before);
        assert_eq!(
            std::fs::read_to_string(&store).expect("restored grant store after approval"),
            disk_before
        );
        assert!(dm.pending_pairing_for_code(&pending_code).is_some());
        let mut restarted =
            DmManager::with_ignore_behavior_and_grant_store(Some(store)).expect("restart");
        restarted.set_platform_unauthorized_behavior("telegram", UnauthorizedDmBehavior::Ignore);
        assert_eq!(
            restarted
                .handle_dm_for_chat("existing-user", "telegram", "existing-dm", false)
                .await,
            DmDecision::Allow
        );
        assert_eq!(
            restarted
                .handle_dm_for_chat("target-user", "telegram", "target-dm", false)
                .await,
            DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn revoke_resolves_code_and_persists_the_actual_target_removal() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        let mut dm = DmManager::with_pair_behavior_and_grant_store(Some(store.clone()))
            .expect("new durable manager");
        dm.add_admin_for_platform("telegram", "admin");
        let code = request_pairing(&dm, "telegram", "target-dm", "target-user").await;
        dm.approve_pairing_code("telegram", "admin", "admin-dm", true, &code)
            .expect("approval");
        let revoked = dm
            .revoke_pairing_code("telegram", "admin", "admin-dm", true, &code)
            .expect("revoke");
        assert_eq!(revoked.user_id, "target-user");
        assert_ne!(revoked.user_id, code);

        let mut restarted =
            DmManager::with_ignore_behavior_and_grant_store(Some(store)).expect("restart");
        restarted.set_platform_unauthorized_behavior("telegram", UnauthorizedDmBehavior::Ignore);
        assert_eq!(
            restarted
                .handle_dm_for_chat("target-user", "telegram", "target-dm", false)
                .await,
            DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn non_admin_cross_platform_and_group_approvals_fail_closed() {
        let mut dm = DmManager::with_pair_behavior();
        dm.add_admin_for_platform("telegram", "telegram-admin");
        let code = request_pairing(&dm, "telegram", "target-dm", "target-user").await;

        assert_eq!(
            dm.approve_pairing_code("discord", "telegram-admin", "admin-dm", true, &code)
                .expect_err("cross-platform local admin"),
            DmGrantError::NotAdmin
        );
        assert_eq!(
            dm.approve_pairing_code("telegram", "not-admin", "admin-dm", true, &code)
                .expect_err("non-admin"),
            DmGrantError::NotAdmin
        );
        assert_eq!(
            dm.approve_pairing_code("telegram", "telegram-admin", "admin-group", false, &code)
                .expect_err("group command"),
            DmGrantError::NotDirectMessage
        );

        dm.add_admin("global-admin");
        assert_eq!(
            dm.approve_pairing_code("discord", "global-admin", "admin-dm", true, &code)
                .expect_err("wrong platform code"),
            DmGrantError::WrongPlatform
        );
        assert!(dm.grants.is_empty());
        assert!(dm.pending_pairing_for_code(&code).is_some());
    }

    #[test]
    fn dm_decision_serde() {
        let allow = DmDecision::Allow;
        let json = serde_json::to_string(&allow).unwrap();
        assert!(json.contains("allow"));

        let pair = DmDecision::Pair {
            message: Some("pending".to_string()),
        };
        let json = serde_json::to_string(&pair).unwrap();
        assert!(json.contains("pair"));

        let deny = DmDecision::Deny;
        let json = serde_json::to_string(&deny).unwrap();
        assert!(json.contains("deny"));
    }
}
