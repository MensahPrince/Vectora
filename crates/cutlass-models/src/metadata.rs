use serde::{Deserialize, Serialize};

/// User-facing metadata attached to a [`Project`](crate::Project).
///
/// Kept separate from timeline/media state so saves and agent edits can update
/// notes without touching the edit graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMetadata {
    /// Free-form description or notes about this edit.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Optional creator / author label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Per-project AI agent rules, injected into the assistant's system
    /// prompt alongside the user's `~/.cutlass/agent/rules`. Prompt-level
    /// only — rules can shape proposals but never bypass command
    /// validation. Travels with an exported `.cutlass`, so the desktop UI
    /// must show (never silently apply) rules arriving with an imported
    /// project.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_rules: String,
}
