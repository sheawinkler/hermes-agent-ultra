use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Closed execution postures used by policy and durable runtime evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Manual,
    Reviewed,
    AutonomousLocal,
    Ci,
    RemoteWorker,
}

impl ExecutionMode {
    pub const ALL: [Self; 5] = [
        Self::Manual,
        Self::Reviewed,
        Self::AutonomousLocal,
        Self::Ci,
        Self::RemoteWorker,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Reviewed => "reviewed",
            Self::AutonomousLocal => "autonomous_local",
            Self::Ci => "ci",
            Self::RemoteWorker => "remote_worker",
        }
    }
    /// Closed modes may execute host-sensitive effects only through a
    /// containment certificate issued for the exact execution backend.
    pub const fn requires_containment_certificate(self) -> bool {
        !matches!(self, Self::Manual)
    }
}

impl Default for ExecutionMode {
    fn default() -> Self {
        Self::Manual
    }
}

impl fmt::Display for ExecutionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ExecutionMode {
    type Err = ExecutionModeValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "manual" => Ok(Self::Manual),
            "reviewed" => Ok(Self::Reviewed),
            "autonomous_local" => Ok(Self::AutonomousLocal),
            "ci" => Ok(Self::Ci),
            "remote_worker" => Ok(Self::RemoteWorker),
            other => Err(ExecutionModeValueError {
                value: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid execution mode '{value}' (expected manual, reviewed, autonomous_local, ci, or remote_worker)")]
pub struct ExecutionModeValueError {
    pub value: String,
}

/// Configuration layer that selected the resolved mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionModeSource {
    BuiltInDefault,
    Profile,
    ConfigFile,
    Environment,
    Cli,
}

impl ExecutionModeSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltInDefault => "built_in_default",
            Self::Profile => "profile",
            Self::ConfigFile => "config_file",
            Self::Environment => "environment",
            Self::Cli => "cli",
        }
    }
}

impl fmt::Display for ExecutionModeSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A validated execution mode plus the layer that won precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResolvedExecutionMode {
    pub mode: ExecutionMode,
    pub source: ExecutionModeSource,
}

impl Default for ResolvedExecutionMode {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::default(),
            source: ExecutionModeSource::BuiltInDefault,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {layer} execution mode: {reason}")]
pub struct ExecutionModeResolveError {
    pub layer: ExecutionModeSource,
    pub reason: ExecutionModeValueError,
}

/// Resolve mode precedence without consulting process-global state.
///
/// Every explicitly present layer is validated before precedence is applied.
/// Therefore an empty or unknown lower-precedence value cannot be hidden by a
/// valid higher-precedence override. Callers must distinguish an absent layer
/// (`None`) from an explicitly configured empty value (`Some("")`).
pub fn resolve_execution_mode(
    built_in_default: ExecutionMode,
    profile: Option<&str>,
    config_file: Option<&str>,
    environment: Option<&str>,
    cli: Option<&str>,
) -> Result<ResolvedExecutionMode, ExecutionModeResolveError> {
    let layers = [
        (ExecutionModeSource::Profile, profile),
        (ExecutionModeSource::ConfigFile, config_file),
        (ExecutionModeSource::Environment, environment),
        (ExecutionModeSource::Cli, cli),
    ];
    let mut resolved = ResolvedExecutionMode {
        mode: built_in_default,
        source: ExecutionModeSource::BuiltInDefault,
    };

    for (source, raw) in layers {
        let Some(raw) = raw else {
            continue;
        };
        let mode = raw
            .parse::<ExecutionMode>()
            .map_err(|reason| ExecutionModeResolveError {
                layer: source,
                reason,
            })?;
        resolved = ResolvedExecutionMode { mode, source };
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_overrides_uses_typed_manual_default_and_records_source() {
        assert_eq!(
            resolve_execution_mode(ExecutionMode::Manual, None, None, None, None).unwrap(),
            ResolvedExecutionMode::default()
        );
    }

    #[test]
    fn precedence_is_profile_then_config_then_environment_then_cli() {
        let cases = [
            (
                [Some("reviewed"), None, None, None],
                ExecutionMode::Reviewed,
                ExecutionModeSource::Profile,
            ),
            (
                [Some("reviewed"), Some("ci"), None, None],
                ExecutionMode::Ci,
                ExecutionModeSource::ConfigFile,
            ),
            (
                [Some("reviewed"), Some("ci"), Some("remote_worker"), None],
                ExecutionMode::RemoteWorker,
                ExecutionModeSource::Environment,
            ),
            (
                [
                    Some("reviewed"),
                    Some("ci"),
                    Some("remote_worker"),
                    Some("autonomous_local"),
                ],
                ExecutionMode::AutonomousLocal,
                ExecutionModeSource::Cli,
            ),
        ];
        for (layers, expected_mode, expected_source) in cases {
            let resolved = resolve_execution_mode(
                ExecutionMode::Manual,
                layers[0],
                layers[1],
                layers[2],
                layers[3],
            )
            .unwrap();
            assert_eq!(resolved.mode, expected_mode);
            assert_eq!(resolved.source, expected_source);
        }
    }

    #[test]
    fn cartesian_precedence_matrix_is_exhaustive() {
        let values = [
            None,
            Some("manual"),
            Some("reviewed"),
            Some("autonomous_local"),
            Some("ci"),
            Some("remote_worker"),
        ];
        let sources = [
            ExecutionModeSource::Profile,
            ExecutionModeSource::ConfigFile,
            ExecutionModeSource::Environment,
            ExecutionModeSource::Cli,
        ];
        let mut checked = 0usize;
        for built_in_default in ExecutionMode::ALL {
            for &profile in &values {
                for &config_file in &values {
                    for &environment in &values {
                        for &cli in &values {
                            let layers = [profile, config_file, environment, cli];
                            let mut expected = ResolvedExecutionMode {
                                mode: built_in_default,
                                source: ExecutionModeSource::BuiltInDefault,
                            };
                            for (source, raw) in sources.into_iter().zip(layers) {
                                if let Some(raw) = raw {
                                    expected = ResolvedExecutionMode {
                                        mode: raw.parse().unwrap(),
                                        source,
                                    };
                                }
                            }
                            assert_eq!(
                                resolve_execution_mode(
                                    built_in_default,
                                    profile,
                                    config_file,
                                    environment,
                                    cli,
                                )
                                .unwrap(),
                                expected,
                                "default={built_in_default}, layers={layers:?}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(checked, ExecutionMode::ALL.len() * 6usize.pow(4));
    }

    #[test]
    fn invalid_value_at_every_layer_is_not_hidden_by_higher_valid_overrides() {
        let sources = [
            ExecutionModeSource::Profile,
            ExecutionModeSource::ConfigFile,
            ExecutionModeSource::Environment,
            ExecutionModeSource::Cli,
        ];
        for invalid in ["", "unknown", "autonomous", "MANUAL", "remote-worker"] {
            for invalid_index in 0..sources.len() {
                let mut layers = [Some("reviewed"); 4];
                layers[invalid_index] = Some(invalid);
                for higher in &mut layers[invalid_index + 1..] {
                    *higher = Some("ci");
                }
                let error = resolve_execution_mode(
                    ExecutionMode::Manual,
                    layers[0],
                    layers[1],
                    layers[2],
                    layers[3],
                )
                .unwrap_err();
                assert_eq!(error.layer, sources[invalid_index]);
            }
        }
    }

    #[test]
    fn every_mode_round_trips_through_text_and_json() {
        for mode in ExecutionMode::ALL {
            assert_eq!(mode.as_str().parse::<ExecutionMode>().unwrap(), mode);
            let encoded = serde_json::to_string(&mode).unwrap();
            assert_eq!(
                serde_json::from_str::<ExecutionMode>(&encoded).unwrap(),
                mode
            );
        }
    }

    #[test]
    fn explicit_empty_unknown_or_alias_values_fail_at_their_source() {
        for (source, layers) in [
            (ExecutionModeSource::Profile, [Some(""), None, None, None]),
            (
                ExecutionModeSource::ConfigFile,
                [None, Some("autonomous"), None, None],
            ),
            (
                ExecutionModeSource::Environment,
                [None, None, Some("MANUAL"), None],
            ),
            (
                ExecutionModeSource::Cli,
                [None, None, None, Some("remote-worker")],
            ),
        ] {
            let error = resolve_execution_mode(
                ExecutionMode::Manual,
                layers[0],
                layers[1],
                layers[2],
                layers[3],
            )
            .unwrap_err();
            assert_eq!(error.layer, source);
        }
    }

    #[test]
    fn invalid_lower_layer_is_not_hidden_by_valid_higher_override() {
        let error = resolve_execution_mode(ExecutionMode::Manual, Some(""), None, None, Some("ci"))
            .unwrap_err();
        assert_eq!(error.layer, ExecutionModeSource::Profile);
    }
}
