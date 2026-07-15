#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

//! Identity capsule for Unicity AOS.
//!
//! Owns the agent's identity (spark config) as persistent state. Builds
//! the system prompt on `spark.v1.request.build` requests. On first
//! boot, injects an onboarding instruction so the agent walks the user
//! through identity setup. Provides `/identity-export` and
//! `/identity-import` CLI commands.

use astrid_sdk::prelude::*;
use astrid_sdk::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// Default agent name when no spark config exists.
const DEFAULT_CALLSIGN: &str = "AOS";

/// VFS path to the spark identity configuration file.
const SPARK_CONFIG_PATH: &str = "home://.config/spark.toml";
/// Default agent class/role.
const DEFAULT_CLASS: &str = "a secure coding assistant";

/// Onboarding instruction appended to the system prompt when the user
/// hasn't configured their agent identity yet.
const ONBOARDING_PROMPT: &str = "\
# Important: Identity Setup Required

This is your first session. You have no name or identity yet. Introduce
yourself briefly, then ask the user one open question about how they'd like
to work together. Let the conversation flow naturally. From it, derive a name,
personality, and focus that feel right — then surface what you came up with
and let the user react. Adjust from there. Once you've landed on something,
call `save_identity` to save it. Always call it — if the user wants to skip,
derive something fitting from the exchange and confirm it casually before saving.";

/// Agent identity configuration.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SparkConfig {
    /// Agent name/identifier.
    #[serde(default)]
    pub callsign: String,
    /// Agent role description.
    #[serde(default)]
    pub class: String,
    /// Personality traits.
    #[serde(default)]
    pub aura: String,
    /// Communication style preferences.
    #[serde(default)]
    pub signal: String,
    /// Core directives and constraints.
    #[serde(default)]
    pub core: String,
}

impl Default for SparkConfig {
    fn default() -> Self {
        Self {
            callsign: DEFAULT_CALLSIGN.into(),
            class: DEFAULT_CLASS.into(),
            aura: String::new(),
            signal: String::new(),
            core: String::new(),
        }
    }
}

impl SparkConfig {
    /// Build the identity preamble from spark fields.
    fn build_preamble(&self) -> String {
        let callsign = if self.callsign.is_empty() {
            DEFAULT_CALLSIGN
        } else {
            &self.callsign
        };

        let mut parts = vec![];
        if !self.class.is_empty() {
            parts.push(format!("You are {callsign}, {class}.", class = self.class));
        } else {
            parts.push(format!("You are {callsign}."));
        }

        if !self.aura.is_empty() {
            parts.push(format!("# Personality\n{}", self.aura));
        }
        if !self.signal.is_empty() {
            parts.push(format!("# Communication Style\n{}", self.signal));
        }
        if !self.core.is_empty() {
            parts.push(format!("# Core Directives\n{}", self.core));
        }

        parts.join("\n\n")
    }

    /// Serialize to TOML for export.
    fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }
}

/// Request payload for building the system prompt.
#[derive(Debug, Deserialize)]
pub struct BuildRequest {
    /// Absolute path to the workspace root directory.
    pub workspace_root: String,
    /// Session ID for correlation.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Response payload containing the assembled system prompt.
#[derive(Debug, Serialize)]
struct BuildResponse {
    /// The fully assembled system prompt string.
    prompt: String,
    /// Session ID echoed from the request for correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

/// Identity capsule state — persisted to KV via `#[capsule(state)]`.
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct IdentityBuilder {
    /// The spark identity configuration.
    spark: SparkConfig,
    /// Whether the user has completed identity onboarding.
    onboarded: bool,
}

#[capsule(state)]
impl IdentityBuilder {
    /// Builds the system prompt from the spark identity.
    #[astrid::interceptor("handle_build_request")]
    pub fn build_system_prompt(&mut self, req: BuildRequest) -> Result<(), SysError> {
        let workspace_root = req.workspace_root.trim_end_matches('/');
        let prompt = self.build_prompt_text(workspace_root);

        let response = BuildResponse {
            prompt,
            session_id: req.session_id,
        };
        ipc::publish_json("spark.v1.response.ready", &response)?;

        Ok(())
    }

    fn build_prompt_text(&mut self, workspace_root: &str) -> String {
        self.build_prompt_text_with_spark_loader(workspace_root, || {
            fs::read_to_string(SPARK_CONFIG_PATH).ok()
        })
    }

    fn build_prompt_text_with_spark_loader<F>(
        &mut self,
        workspace_root: &str,
        load_spark: F,
    ) -> String
    where
        F: FnOnce() -> Option<String>,
    {
        // TODO: Move to a new capsule which handles env details. Time would be good too.
        let mut prompt = format!(
            "# Environment\n\
             - Current working directory: {workspace_root}\n\
             - Platform: Unicity AOS"
        );

        // Auto-detect an existing spark.toml when KV state says not yet onboarded.
        // This makes the capsule resilient to KV resets: if the file exists and
        // parses successfully we treat the user as onboarded without requiring
        // an explicit `identity-import`.
        if !self.onboarded
            && let Some(content) = load_spark()
        {
            // Parse directly instead of going through parse_spark_toml (which
            // falls back to a default with a non-empty callsign on error).
            match toml::from_str::<SparkConfig>(&content) {
                Ok(config) if !config.callsign.is_empty() => {
                    self.spark = config;
                    self.onboarded = true;
                }
                Ok(_) => {} // Empty callsign — treat as stub, don't onboard.
                Err(e) => {
                    log::warn(format!(
                        "Failed to parse {SPARK_CONFIG_PATH} during auto-detect: {e}"
                    ));
                }
            }
        }

        if self.onboarded {
            // Prepend the established identity preamble.
            let opening = self.spark.build_preamble();
            prompt = format!("{opening}\n\n{prompt}");
        } else {
            // No preamble — don't anchor the model to a name before onboarding.
            prompt.push_str("\n\n");
            prompt.push_str(ONBOARDING_PROMPT);
        }

        prompt
    }

    /// Handles `/identity-export` and `/identity-import` CLI commands.
    #[astrid::interceptor("handle_command")]
    pub fn handle_command(&mut self, payload: serde_json::Value) -> Result<(), SysError> {
        let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        let spark_path = SPARK_CONFIG_PATH;

        match text.trim() {
            "identity-export" => {
                let toml = self.spark.to_toml();
                fs::write(spark_path, toml.as_bytes())?;

                ipc::publish_json(
                    "agent.v1.response",
                    &serde_json::json!({
                        "type": "agent_response",
                        "text": format!("Identity exported to {spark_path} ({} bytes)", toml.len()),
                        "is_final": true,
                        "session_id": session_id,
                    }),
                )?;
            }
            "identity-import" => {
                let content = fs::read_to_string(spark_path)?;
                self.spark = parse_spark_toml(&content);
                self.onboarded = true;

                ipc::publish_json(
                    "agent.v1.response",
                    &serde_json::json!({
                        "type": "agent_response",
                        "text": format!("Identity imported from {spark_path} (callsign: {})", self.spark.callsign),
                        "is_final": true,
                        "session_id": session_id,
                    }),
                )?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Save the agent's identity. Called by the LLM after onboarding to
    /// persist the chosen callsign, personality, and style. Writes both
    /// KV state (for immediate use) and spark.toml (for persistence
    /// across KV resets).
    #[astrid::tool("save_identity")]
    pub fn save_identity(&mut self, args: SparkConfig) -> Result<serde_json::Value, SysError> {
        self.spark = args;
        self.onboarded = true;

        // Persist to spark.toml so identity survives KV resets.
        let toml = self.spark.to_toml();
        fs::write(SPARK_CONFIG_PATH, toml.as_bytes())?;

        Ok(serde_json::json!({
            "status": "ok",
            "callsign": self.spark.callsign,
        }))
    }
}

/// Parse spark.toml into a `SparkConfig`.
fn parse_spark_toml(content: &str) -> SparkConfig {
    toml::from_str(content).unwrap_or_else(|e| {
        log::warn(format!("Failed to parse spark.toml, using defaults: {e}"));
        SparkConfig::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_identity() -> SparkConfig {
        SparkConfig {
            callsign: "Lyra".into(),
            class: "a precise concierge agent".into(),
            aura: "Calm, direct, and context aware.".into(),
            signal: "Use short answers unless detail is needed.".into(),
            core: "Preserve user boundaries.".into(),
        }
    }

    #[test]
    fn prompt_requests_onboarding_until_identity_is_saved() {
        let mut builder = IdentityBuilder::default();

        let prompt = builder.build_prompt_text_with_spark_loader("/tmp/workspace", || None);

        assert!(prompt.contains("# Important: Identity Setup Required"));
        assert!(prompt.contains("- Current working directory: /tmp/workspace"));
        assert!(!prompt.contains("You are Lyra"));
    }

    #[test]
    fn saved_identity_is_used_without_repeating_onboarding() {
        let mut builder = IdentityBuilder {
            spark: configured_identity(),
            onboarded: true,
        };

        let prompt = builder.build_prompt_text_with_spark_loader("/tmp/workspace", || None);

        assert!(prompt.contains("You are Lyra, a precise concierge agent."));
        assert!(prompt.contains("# Personality\nCalm, direct, and context aware."));
        assert!(
            prompt.contains("# Communication Style\nUse short answers unless detail is needed.")
        );
        assert!(prompt.contains("# Core Directives\nPreserve user boundaries."));
        assert!(!prompt.contains("# Important: Identity Setup Required"));
    }

    #[test]
    fn spark_file_loader_restores_identity_when_state_is_empty() {
        let mut builder = IdentityBuilder::default();
        let spark_toml = configured_identity().to_toml();

        let prompt =
            builder.build_prompt_text_with_spark_loader("/tmp/workspace", || Some(spark_toml));

        assert!(prompt.contains("You are Lyra, a precise concierge agent."));
        assert!(!prompt.contains("# Important: Identity Setup Required"));
    }
}
