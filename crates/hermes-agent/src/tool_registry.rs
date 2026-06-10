//! Minimal tool registry for the agent loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use hermes_core::{ToolError, ToolSchema};
use serde_json::Value;

// ---------------------------------------------------------------------------
// ToolEntry
// ---------------------------------------------------------------------------

/// A single tool entry in the registry.
#[derive(Clone)]
pub struct ToolEntry {
    /// The tool's JSON Schema descriptor.
    pub schema: ToolSchema,
    /// A handler function: takes a JSON Value and returns the tool output string.
    pub handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
}

impl std::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("schema", &self.schema)
            .field("handler", &"<function>")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// A simple registry mapping tool names to their schemas and handlers.
///
/// The full-featured implementation lives in `hermes-tools`; this minimal
/// version exists so the agent loop can be tested and used independently.
#[derive(Debug)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
    schemas_cache: RwLock<Option<Arc<[ToolSchema]>>>,
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            schemas_cache: RwLock::new(None),
        }
    }
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            schemas_cache: RwLock::new(None),
        }
    }

    /// Register a tool.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
    ) {
        self.tools
            .insert(name.into(), ToolEntry { schema, handler });
        if let Ok(mut cache) = self.schemas_cache.write() {
            *cache = None;
        }
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.get(name)
    }

    /// Return cached tool schemas (stable name order; shared across calls).
    pub fn schemas(&self) -> Arc<[ToolSchema]> {
        if let Ok(cache) = self.schemas_cache.read() {
            if let Some(ref cached) = *cache {
                return Arc::clone(cached);
            }
        }
        let mut names: Vec<&String> = self.tools.keys().collect();
        names.sort();
        let built: Arc<[ToolSchema]> = names
            .into_iter()
            .map(|name| self.tools[name].schema.clone())
            .collect::<Vec<_>>()
            .into();
        if let Ok(mut cache) = self.schemas_cache.write() {
            *cache = Some(Arc::clone(&built));
        }
        built
    }

    /// Clone all schemas into a vec (legacy callers; prefer [`Self::schemas`]).
    pub fn schemas_vec(&self) -> Vec<ToolSchema> {
        self.schemas().to_vec()
    }

    /// Return all registered tool names.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
