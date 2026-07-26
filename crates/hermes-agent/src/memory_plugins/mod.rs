//! Bundled memory provider plugins.
//!
//! Each module implements `MemoryProviderPlugin` for a specific external
//! memory backend. The built-in provider is always registered first.
//! External providers are additive and can run side-by-side.

pub mod builtin;
pub mod byterover;
mod config_io;
pub mod contextlattice;
pub mod hindsight;
pub mod holographic;
pub mod honcho;
pub mod mem0;
pub mod openviking;
pub mod retaindb;
pub mod supermemory;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::memory_manager::{MemoryManager, MemoryManagerError, MemoryProviderPlugin};
use hermes_core::MemorySubject;

fn default_preferred_provider_order() -> Vec<String> {
    vec![
        "honcho".to_string(),
        "contextlattice".to_string(),
        "supermemory".to_string(),
        "mem0".to_string(),
        "hindsight".to_string(),
        "retaindb".to_string(),
        "openviking".to_string(),
        "byterover".to_string(),
        "holographic".to_string(),
    ]
}

fn preferred_provider_order() -> Vec<String> {
    std::env::var("HERMES_MEMORY_PROVIDER_ORDER")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|tok| tok.trim().to_ascii_lowercase())
                .filter(|tok| !tok.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_preferred_provider_order)
}

fn sort_by_preferred_order(
    mut providers: Vec<Arc<dyn MemoryProviderPlugin>>,
) -> Vec<Arc<dyn MemoryProviderPlugin>> {
    let order = preferred_provider_order();
    let mut rank = std::collections::HashMap::new();
    for (idx, name) in order.iter().enumerate() {
        rank.insert(name.as_str(), idx);
    }
    providers.sort_by(|a, b| {
        let a_name = a.name().to_ascii_lowercase();
        let b_name = b.name().to_ascii_lowercase();
        let ar = rank.get(a_name.as_str()).copied().unwrap_or(usize::MAX);
        let br = rank.get(b_name.as_str()).copied().unwrap_or(usize::MAX);
        ar.cmp(&br).then_with(|| a_name.cmp(&b_name))
    });
    providers
}

/// Discover and return all available bundled memory providers.
///
/// Checks each provider's `is_available()` without making network calls.
/// Returns them in priority order (API-first, then CLI/local).
pub fn discover_available_providers() -> Vec<Arc<dyn MemoryProviderPlugin>> {
    let mut providers: Vec<Arc<dyn MemoryProviderPlugin>> = Vec::new();

    let honcho = honcho::HonchoMemoryPlugin::new();
    if honcho.is_available() {
        tracing::info!("Discovered memory provider: honcho");
        providers.push(Arc::new(honcho));
    }

    let contextlattice = contextlattice::ContextLatticeMemoryPlugin::new();
    if contextlattice.is_available() {
        tracing::info!("Discovered memory provider: contextlattice");
        providers.push(Arc::new(contextlattice));
    }

    let hindsight = hindsight::HindsightPlugin::new();
    if hindsight.is_available() {
        tracing::info!("Discovered memory provider: hindsight");
        providers.push(Arc::new(hindsight));
    }

    let retaindb = retaindb::RetainDbMemoryPlugin::new();
    if retaindb.is_available() {
        tracing::info!("Discovered memory provider: retaindb");
        providers.push(Arc::new(retaindb));
    }

    let openviking = openviking::OpenVikingMemoryPlugin::new();
    if openviking.is_available() {
        tracing::info!("Discovered memory provider: openviking");
        providers.push(Arc::new(openviking));
    }

    let brv = byterover::ByteRoverPlugin::new();
    if brv.is_available() {
        tracing::info!("Discovered memory provider: byterover");
        providers.push(Arc::new(brv));
    }

    let mem0 = mem0::Mem0MemoryPlugin::new();
    if mem0.is_available() {
        tracing::info!("Discovered memory provider: mem0");
        providers.push(Arc::new(mem0));
    }

    let sm = supermemory::SupermemoryMemoryPlugin::new();
    if sm.is_available() {
        tracing::info!("Discovered memory provider: supermemory");
        providers.push(Arc::new(sm));
    }

    let holo = holographic::HolographicMemoryPlugin::new();
    if holo.is_available() {
        tracing::info!("Discovered memory provider: holographic");
        providers.push(Arc::new(holo));
    }

    sort_by_preferred_order(providers)
}

/// Auto-register the first available external memory provider into the
/// given MemoryManager. Returns the name of the registered provider, if any.
pub fn auto_register_provider(
    manager: &mut crate::memory_manager::MemoryManager,
) -> Option<String> {
    auto_register_providers(manager).into_iter().next()
}

/// Auto-register all available external memory providers into the given
/// MemoryManager. Returns registered provider names in discovery order.
pub fn auto_register_providers(manager: &mut crate::memory_manager::MemoryManager) -> Vec<String> {
    let providers = discover_available_providers();
    let mut names = Vec::new();
    for provider in providers {
        let name = provider.name().to_string();
        manager.add_provider(provider);
        names.push(name);
    }
    names
}

/// Build and initialize a subject-bound memory manager using all discovered providers.
pub fn build_initialized_memory_manager(
    subject: MemorySubject,
    hermes_home: &str,
    nudge_threshold: u32,
) -> Result<Option<Arc<Mutex<MemoryManager>>>, MemoryManagerError> {
    let mut manager =
        MemoryManager::new(subject, hermes_home)?.with_nudge_threshold(nudge_threshold.max(1));
    register_builtin_then_externals(&mut manager, hermes_home)?;
    manager.initialize_all()?;
    Ok(Some(Arc::new(Mutex::new(manager))))
}

/// Register the mandatory local file-memory provider first, then the sorted
/// configured external providers. The explicit home is bound into the builtin
/// adapter before initialization, so it cannot fall back to process state.
pub fn register_builtin_then_externals(
    manager: &mut MemoryManager,
    hermes_home: &str,
) -> Result<(), MemoryManagerError> {
    register_builtin_then_providers(manager, hermes_home, discover_available_providers())
}

fn register_builtin_then_providers(
    manager: &mut MemoryManager,
    hermes_home: &str,
    external_providers: Vec<Arc<dyn MemoryProviderPlugin>>,
) -> Result<(), MemoryManagerError> {
    let builtin = Arc::new(
        builtin::BuiltinMemoryPlugin::with_home(PathBuf::from(hermes_home)).map_err(|_| {
            MemoryManagerError::ProviderInitialization {
                provider: "builtin".to_string(),
            }
        })?,
    );
    manager.add_builtin_provider(builtin);
    for provider in external_providers {
        manager.add_provider(provider);
    }
    Ok(())
}

#[cfg(test)]
fn build_initialized_memory_manager_with_providers(
    subject: MemorySubject,
    hermes_home: &str,
    nudge_threshold: u32,
    external_providers: Vec<Arc<dyn MemoryProviderPlugin>>,
) -> Result<Arc<Mutex<MemoryManager>>, MemoryManagerError> {
    let mut manager =
        MemoryManager::new(subject, hermes_home)?.with_nudge_threshold(nudge_threshold.max(1));
    register_builtin_then_providers(&mut manager, hermes_home, external_providers)?;
    manager.initialize_all()?;
    Ok(Arc::new(Mutex::new(manager)))
}

#[cfg(test)]
mod provider_order_tests {
    use super::default_preferred_provider_order;

    #[test]
    fn default_provider_order_prioritizes_honcho() {
        assert_eq!(default_preferred_provider_order().first().map(String::as_str), Some("honcho"));
    }
}

#[cfg(test)]
mod builtin_manager_tests {
    use super::build_initialized_memory_manager_with_providers;
    use hermes_core::{MemorySubject, MemorySurface};
    use hermes_tools::{FileMemoryBackend, MemoryBackend};
    use tempfile::tempdir;

    #[tokio::test]
    async fn builtin_only_manager_uses_explicit_home_across_restart_export_and_forget() {
        let home = tempdir().expect("temp Hermes home");
        let subject = MemorySubject::local_owner(MemorySurface::Cli, "builtin-restart-session")
            .expect("valid local subject");
        let manager = build_initialized_memory_manager_with_providers(
            subject.clone(),
            home.path().to_str().expect("UTF-8 temp home"),
            1,
            Vec::new(),
        )
        .expect("initialize builtin-only manager");

        let filter = {
            let manager = manager.lock().expect("manager lock");
            assert_eq!(manager.providers().len(), 1);
            assert_eq!(manager.providers()[0].name(), "builtin");
            manager.subject_filter_key().clone()
        };
        let tool_backend = FileMemoryBackend::with_home(home.path().to_path_buf());
        tool_backend
            .add(&filter, "memory", "builtin-private-memory")
            .await
            .expect("write through explicit-home memory tool backend");
        let subject_path = home
            .path()
            .join("memories")
            .join("subjects")
            .join(filter.as_str())
            .join("MEMORY.md");
        assert!(subject_path.exists());

        {
            let manager = manager.lock().expect("manager lock");
            manager.shutdown_all();
        }
        let restarted = build_initialized_memory_manager_with_providers(
            subject.clone(),
            home.path().to_str().expect("UTF-8 temp home"),
            1,
            Vec::new(),
        )
        .expect("restart builtin-only manager");
        let mut manager = restarted.lock().expect("restarted manager lock");
        let export = manager
            .export_subject(&subject, manager.audit_actor())
            .expect("export explicit-home tool memory");
        assert!(export.is_complete());
        assert_eq!(
            export
                .artifact
                .as_ref()
                .expect("committed artifact")
                .record_count,
            1
        );

        let forget = manager
            .forget_subject(&subject, manager.audit_actor())
            .expect("forget explicit-home tool memory");
        assert!(forget.is_erased());
        assert!(!subject_path.exists());
        manager.shutdown_all();
    }
}
