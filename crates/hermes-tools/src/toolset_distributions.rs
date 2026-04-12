//! Toolset distribution system.
//!
//! A distribution defines probability weights for including each toolset in an
//! agent's tool belt. This allows profiles/modes to bias towards certain tool
//! categories (e.g. "research" mode heavily weights web tools, "safe" mode
//! excludes terminal).

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// ToolsetDistribution
// ---------------------------------------------------------------------------

/// A named distribution of toolset weights.
///
/// Each entry in `toolset_weights` maps a toolset name to a probability weight
/// (0.0 = never included, 1.0 = always included). Weights are not required to
/// sum to 1.0 — they are interpreted as independent inclusion probabilities.
#[derive(Debug, Clone)]
pub struct ToolsetDistribution {
    pub name: String,
    pub description: String,
    pub toolset_weights: HashMap<String, f64>,
}

impl ToolsetDistribution {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            toolset_weights: HashMap::new(),
        }
    }

    pub fn with_weight(mut self, toolset: impl Into<String>, weight: f64) -> Self {
        self.toolset_weights.insert(toolset.into(), weight);
        self
    }
}

// ---------------------------------------------------------------------------
// Built-in distributions
// ---------------------------------------------------------------------------

/// Return all predefined toolset distributions.
pub fn builtin_distributions() -> Vec<ToolsetDistribution> {
    vec![
        make_default(),
        make_image_gen(),
        make_research(),
        make_science(),
        make_development(),
        make_safe(),
        make_balanced(),
        make_minimal(),
        make_terminal_only(),
        make_terminal_web(),
        make_creative(),
        make_reasoning(),
        make_browser_use(),
        make_browser_only(),
        make_browser_tasks(),
        make_terminal_tasks(),
        make_mixed_tasks(),
    ]
}

fn base_weights() -> HashMap<String, f64> {
    let mut w = HashMap::new();
    w.insert("web".into(), 0.8);
    w.insert("terminal".into(), 0.9);
    w.insert("file".into(), 1.0);
    w.insert("browser".into(), 0.3);
    w.insert("vision".into(), 0.5);
    w.insert("image_gen".into(), 0.2);
    w.insert("skills".into(), 0.7);
    w.insert("memory".into(), 0.6);
    w.insert("session_search".into(), 0.4);
    w.insert("todo".into(), 0.8);
    w.insert("clarify".into(), 0.9);
    w.insert("code_execution".into(), 0.7);
    w.insert("delegation".into(), 0.3);
    w.insert("cronjob".into(), 0.2);
    w.insert("messaging".into(), 0.1);
    w.insert("homeassistant".into(), 0.1);
    w.insert("tts".into(), 0.1);
    w.insert("voice_mode".into(), 0.1);
    w
}

fn make_default() -> ToolsetDistribution {
    ToolsetDistribution {
        name: "default".into(),
        description: "Standard distribution with balanced weights for general use".into(),
        toolset_weights: base_weights(),
    }
}

fn make_image_gen() -> ToolsetDistribution {
    let mut w = base_weights();
    w.insert("image_gen".into(), 1.0);
    w.insert("vision".into(), 0.9);
    w.insert("terminal".into(), 0.3);
    ToolsetDistribution {
        name: "image_gen".into(),
        description: "Biased towards image generation and vision tasks".into(),
        toolset_weights: w,
    }
}

fn make_research() -> ToolsetDistribution {
    let mut w = base_weights();
    w.insert("web".into(), 1.0);
    w.insert("browser".into(), 0.9);
    w.insert("memory".into(), 0.9);
    w.insert("session_search".into(), 0.8);
    w.insert("terminal".into(), 0.3);
    w.insert("file".into(), 0.5);
    ToolsetDistribution {
        name: "research".into(),
        description: "Optimized for web research and information gathering".into(),
        toolset_weights: w,
    }
}

fn make_science() -> ToolsetDistribution {
    let mut w = base_weights();
    w.insert("code_execution".into(), 1.0);
    w.insert("web".into(), 0.9);
    w.insert("file".into(), 1.0);
    w.insert("vision".into(), 0.8);
    w.insert("terminal".into(), 0.7);
    ToolsetDistribution {
        name: "science".into(),
        description: "Scientific computing with emphasis on code execution and data".into(),
        toolset_weights: w,
    }
}

fn make_development() -> ToolsetDistribution {
    let mut w = base_weights();
    w.insert("terminal".into(), 1.0);
    w.insert("file".into(), 1.0);
    w.insert("code_execution".into(), 1.0);
    w.insert("web".into(), 0.7);
    w.insert("skills".into(), 0.9);
    w.insert("todo".into(), 1.0);
    w.insert("browser".into(), 0.2);
    ToolsetDistribution {
        name: "development".into(),
        description: "Software development with terminal, file, and code tools".into(),
        toolset_weights: w,
    }
}

fn make_safe() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("web".into(), 0.7);
    w.insert("terminal".into(), 0.0);
    w.insert("file".into(), 0.5);
    w.insert("browser".into(), 0.0);
    w.insert("vision".into(), 0.3);
    w.insert("skills".into(), 0.5);
    w.insert("memory".into(), 0.5);
    w.insert("todo".into(), 0.8);
    w.insert("clarify".into(), 1.0);
    w.insert("code_execution".into(), 0.0);
    ToolsetDistribution {
        name: "safe".into(),
        description: "No terminal or code execution — read-only tools only".into(),
        toolset_weights: w,
    }
}

fn make_balanced() -> ToolsetDistribution {
    let mut w = HashMap::new();
    for key in [
        "web", "terminal", "file", "browser", "vision", "skills",
        "memory", "todo", "clarify", "code_execution",
    ] {
        w.insert(key.into(), 0.5);
    }
    ToolsetDistribution {
        name: "balanced".into(),
        description: "Equal probability for all core toolsets".into(),
        toolset_weights: w,
    }
}

fn make_minimal() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("file".into(), 1.0);
    w.insert("clarify".into(), 1.0);
    ToolsetDistribution {
        name: "minimal".into(),
        description: "Only file and clarify tools — minimal footprint".into(),
        toolset_weights: w,
    }
}

fn make_terminal_only() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("terminal".into(), 1.0);
    w.insert("file".into(), 1.0);
    ToolsetDistribution {
        name: "terminal_only".into(),
        description: "Terminal and file tools only".into(),
        toolset_weights: w,
    }
}

fn make_terminal_web() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("terminal".into(), 1.0);
    w.insert("file".into(), 1.0);
    w.insert("web".into(), 1.0);
    ToolsetDistribution {
        name: "terminal_web".into(),
        description: "Terminal, file, and web search tools".into(),
        toolset_weights: w,
    }
}

fn make_creative() -> ToolsetDistribution {
    let mut w = base_weights();
    w.insert("image_gen".into(), 1.0);
    w.insert("tts".into(), 0.8);
    w.insert("voice_mode".into(), 0.7);
    w.insert("vision".into(), 0.9);
    w.insert("web".into(), 0.9);
    ToolsetDistribution {
        name: "creative".into(),
        description: "Creative mode with image, voice, and vision tools emphasized".into(),
        toolset_weights: w,
    }
}

fn make_reasoning() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("clarify".into(), 1.0);
    w.insert("memory".into(), 0.9);
    w.insert("session_search".into(), 0.8);
    w.insert("todo".into(), 0.9);
    w.insert("web".into(), 0.6);
    w.insert("file".into(), 0.5);
    ToolsetDistribution {
        name: "reasoning".into(),
        description: "Focused on reasoning with memory, clarification, and planning".into(),
        toolset_weights: w,
    }
}

fn make_browser_use() -> ToolsetDistribution {
    let mut w = base_weights();
    w.insert("browser".into(), 1.0);
    w.insert("web".into(), 1.0);
    w.insert("vision".into(), 0.8);
    ToolsetDistribution {
        name: "browser_use".into(),
        description: "Full tool belt with browser emphasis".into(),
        toolset_weights: w,
    }
}

fn make_browser_only() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("browser".into(), 1.0);
    w.insert("vision".into(), 0.8);
    w.insert("web".into(), 0.5);
    ToolsetDistribution {
        name: "browser_only".into(),
        description: "Browser-only interaction mode".into(),
        toolset_weights: w,
    }
}

fn make_browser_tasks() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("browser".into(), 1.0);
    w.insert("web".into(), 1.0);
    w.insert("vision".into(), 0.9);
    w.insert("file".into(), 0.7);
    w.insert("todo".into(), 0.8);
    ToolsetDistribution {
        name: "browser_tasks".into(),
        description: "Browser-driven task automation".into(),
        toolset_weights: w,
    }
}

fn make_terminal_tasks() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("terminal".into(), 1.0);
    w.insert("file".into(), 1.0);
    w.insert("code_execution".into(), 0.9);
    w.insert("todo".into(), 0.9);
    w.insert("skills".into(), 0.7);
    ToolsetDistribution {
        name: "terminal_tasks".into(),
        description: "Terminal-driven task automation".into(),
        toolset_weights: w,
    }
}

fn make_mixed_tasks() -> ToolsetDistribution {
    let mut w = HashMap::new();
    w.insert("terminal".into(), 0.8);
    w.insert("browser".into(), 0.8);
    w.insert("web".into(), 0.9);
    w.insert("file".into(), 1.0);
    w.insert("code_execution".into(), 0.7);
    w.insert("vision".into(), 0.6);
    w.insert("todo".into(), 0.9);
    w.insert("skills".into(), 0.7);
    w.insert("memory".into(), 0.6);
    ToolsetDistribution {
        name: "mixed_tasks".into(),
        description: "Mixed terminal + browser task automation".into(),
        toolset_weights: w,
    }
}

// ---------------------------------------------------------------------------
// Lookup helpers
// ---------------------------------------------------------------------------

/// Get a built-in distribution by name.
pub fn get_distribution(name: &str) -> Option<ToolsetDistribution> {
    builtin_distributions()
        .into_iter()
        .find(|d| d.name == name)
}

/// List all available distribution names.
pub fn list_distributions() -> Vec<String> {
    builtin_distributions()
        .iter()
        .map(|d| d.name.clone())
        .collect()
}

/// Sample toolsets from a distribution: include toolsets whose weight exceeds
/// a random threshold.
pub fn sample_toolsets_from_distribution(dist: &ToolsetDistribution) -> Vec<String> {
    use rand::Rng;
    let mut rng = rand::thread_rng();

    dist.toolset_weights
        .iter()
        .filter(|(_, &weight)| {
            if weight >= 1.0 {
                true
            } else if weight <= 0.0 {
                false
            } else {
                rng.gen::<f64>() < weight
            }
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Deterministically select toolsets: include all with weight > 0.
pub fn select_toolsets_deterministic(dist: &ToolsetDistribution) -> Vec<String> {
    dist.toolset_weights
        .iter()
        .filter(|(_, &weight)| weight > 0.0)
        .map(|(name, _)| name.clone())
        .collect()
}

/// Validate a distribution.
///
/// Checks that all weights are in [0.0, 1.0] and the distribution has a name.
pub fn validate_distribution(dist: &ToolsetDistribution) -> Result<(), String> {
    if dist.name.is_empty() {
        return Err("Distribution name cannot be empty".into());
    }

    for (toolset, &weight) in &dist.toolset_weights {
        if weight < 0.0 || weight > 1.0 {
            return Err(format!(
                "Weight for toolset '{}' is {} — must be between 0.0 and 1.0",
                toolset, weight
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_distributions_count() {
        let dists = builtin_distributions();
        assert_eq!(dists.len(), 17);
    }

    #[test]
    fn test_builtin_distributions_unique_names() {
        let dists = builtin_distributions();
        let mut names: Vec<_> = dists.iter().map(|d| d.name.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), dists.len(), "Duplicate distribution names found");
    }

    #[test]
    fn test_get_distribution_found() {
        assert!(get_distribution("default").is_some());
        assert!(get_distribution("safe").is_some());
        assert!(get_distribution("research").is_some());
    }

    #[test]
    fn test_get_distribution_not_found() {
        assert!(get_distribution("nonexistent").is_none());
    }

    #[test]
    fn test_list_distributions() {
        let names = list_distributions();
        assert!(names.contains(&"default".to_string()));
        assert!(names.contains(&"minimal".to_string()));
    }

    #[test]
    fn test_validate_distribution_ok() {
        let dist = ToolsetDistribution::new("test", "test dist")
            .with_weight("web", 0.5)
            .with_weight("file", 1.0);
        assert!(validate_distribution(&dist).is_ok());
    }

    #[test]
    fn test_validate_distribution_bad_weight() {
        let dist = ToolsetDistribution::new("test", "test")
            .with_weight("web", 1.5);
        assert!(validate_distribution(&dist).is_err());
    }

    #[test]
    fn test_validate_distribution_negative_weight() {
        let dist = ToolsetDistribution::new("test", "test")
            .with_weight("web", -0.1);
        assert!(validate_distribution(&dist).is_err());
    }

    #[test]
    fn test_validate_distribution_empty_name() {
        let dist = ToolsetDistribution::new("", "test");
        assert!(validate_distribution(&dist).is_err());
    }

    #[test]
    fn test_validate_all_builtins() {
        for dist in builtin_distributions() {
            assert!(
                validate_distribution(&dist).is_ok(),
                "Built-in distribution '{}' failed validation",
                dist.name
            );
        }
    }

    #[test]
    fn test_sample_always_includes_weight_1() {
        let dist = ToolsetDistribution::new("test", "test")
            .with_weight("file", 1.0)
            .with_weight("never", 0.0);

        for _ in 0..10 {
            let selected = sample_toolsets_from_distribution(&dist);
            assert!(selected.contains(&"file".to_string()));
            assert!(!selected.contains(&"never".to_string()));
        }
    }

    #[test]
    fn test_deterministic_select() {
        let dist = ToolsetDistribution::new("test", "test")
            .with_weight("file", 1.0)
            .with_weight("web", 0.5)
            .with_weight("never", 0.0);

        let selected = select_toolsets_deterministic(&dist);
        assert!(selected.contains(&"file".to_string()));
        assert!(selected.contains(&"web".to_string()));
        assert!(!selected.contains(&"never".to_string()));
    }

    #[test]
    fn test_safe_distribution_no_terminal() {
        let dist = get_distribution("safe").unwrap();
        assert_eq!(*dist.toolset_weights.get("terminal").unwrap(), 0.0);
        assert_eq!(*dist.toolset_weights.get("code_execution").unwrap(), 0.0);
    }

    #[test]
    fn test_builder_pattern() {
        let dist = ToolsetDistribution::new("custom", "My custom distribution")
            .with_weight("web", 0.9)
            .with_weight("terminal", 0.1);
        assert_eq!(dist.name, "custom");
        assert_eq!(dist.toolset_weights.len(), 2);
        assert_eq!(*dist.toolset_weights.get("web").unwrap(), 0.9);
    }
}
