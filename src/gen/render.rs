//! Renderer: [`Config`] → YAML or JSON string.

use crate::config::Config;

/// Serialize a [`Config`] to YAML.
pub fn to_yaml(config: &Config) -> anyhow::Result<String> {
    serde_norway::to_string(config).map_err(|e| anyhow::anyhow!("YAML serialization failed: {e}"))
}

/// Serialize a [`Config`] to pretty-printed JSON.
pub fn to_json(config: &Config) -> anyhow::Result<String> {
    serde_json::to_string_pretty(config)
        .map_err(|e| anyhow::anyhow!("JSON serialization failed: {e}"))
}
