//! Bundled memory provider plugins.
//!
//! Each module implements `MemoryProviderPlugin` for a specific external
//! memory backend. The built-in provider is always registered first.
//! External providers are additive and can run side-by-side.

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

use std::sync::{Arc, Mutex};

use crate::memory_manager::{MemoryManager, MemoryProviderPlugin};

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
        .unwrap_or_else(|| {
            vec![
                "contextlattice".to_string(),
                "honcho".to_string(),
                "supermemory".to_string(),
                "mem0".to_string(),
                "hindsight".to_string(),
                "retaindb".to_string(),
                "openviking".to_string(),
                "byterover".to_string(),
                "holographic".to_string(),
            ]
        })
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

/// Build and initialize a memory manager using all discovered providers.
pub fn build_initialized_memory_manager(
    session_id: &str,
    hermes_home: &str,
    nudge_threshold: u32,
) -> Option<Arc<Mutex<MemoryManager>>> {
    let mut manager = MemoryManager::new().with_nudge_threshold(nudge_threshold.max(1));
    let names = auto_register_providers(&mut manager);
    if names.is_empty() {
        return None;
    }
    for provider in manager.providers() {
        provider.initialize(session_id, hermes_home);
    }
    Some(Arc::new(Mutex::new(manager)))
}
