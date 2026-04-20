//! Bundled memory provider plugins.
//!
//! Each module implements `MemoryProviderPlugin` for a specific external
//! memory backend. The built-in provider is always registered first;
//! at most ONE of these external providers can be active at a time.

pub mod byterover;
pub mod contextlattice;
pub mod hindsight;
pub mod holographic;
pub mod honcho;
pub mod mem0;
pub mod openviking;
pub mod retaindb;
pub mod supermemory;

use std::sync::Arc;

use crate::memory_manager::MemoryProviderPlugin;

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

    providers
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
