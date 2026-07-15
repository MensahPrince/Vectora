//! The host-tool registry: tools the embedding app implements
//! (screenshots, app control, cache management, analysis, python, …),
//! offered to the model alongside the closed edit vocabulary.
//!
//! Deliberately separate from [`crate::wire`]: wire commands mutate the
//! timeline through validation, rehearsal, and replay; host tools execute
//! host-side, immediately, and this crate never sees inside them. The
//! loop treats them as opaque calls — specs in, text and images out — so
//! this vocabulary is open (each embedder brings its own) while the edit
//! vocabulary stays closed.

use std::sync::atomic::AtomicBool;

use crate::provider::ImagePart;

/// How much trust a host tool needs. Decides confirmation UX host-side;
/// the loop itself never gates on tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTier {
    /// Pure senses: screenshots, state reads, analysis queries. Never
    /// mutates, never confirms.
    ReadOnly,
    /// Safe, reversible app/project operations (save, playback, panels,
    /// window). No confirmation.
    Workspace,
    /// Destructive or externally visible (clear caches, write outside
    /// managed project storage, run scripts). The host confirms per the
    /// user's autonomy setting.
    System,
}

/// One host-implemented tool. `name` is the full wire name and must be
/// namespace-prefixed as `{namespace}_{tool}`, e.g.
/// "app_set_theme", "media_screenshot_timeline". Namespaces are single
/// words: app, project, system, media, analysis, python, job.
#[derive(Debug, Clone)]
pub struct HostToolSpec {
    /// Owned because analysis plugins and MCP catalogs can discover tools
    /// at runtime rather than compiling them into the app.
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
    pub tier: ToolTier,
}

/// What a host tool returns: text for the model, plus optional images
/// (they ride the multimodal pipeline: budgeted per request, stripped
/// from history).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolOutput {
    pub text: String,
    pub images: Vec<ImagePart>,
}

impl ToolOutput {
    /// Text-only output, the common case.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
    }
}

/// The embedder's tool surface. Implementations live host-side (the
/// desktop app); the loop only sees specs and calls. Blocking is fine —
/// the loop runs on a dedicated agent thread; long calls should poll
/// `cancel`.
pub trait ToolHost {
    fn tools(&self) -> Vec<HostToolSpec>;
    /// Execute one call. `Err` is a model-readable reason; the loop
    /// forwards it as "rejected: {reason}".
    fn call(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
        cancel: &AtomicBool,
    ) -> Result<ToolOutput, String>;
}

/// The empty host: no tools. For tests, examples, and embedders that
/// haven't wired a host yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullToolHost;

impl ToolHost for NullToolHost {
    fn tools(&self) -> Vec<HostToolSpec> {
        Vec::new()
    }

    fn call(
        &mut self,
        name: &str,
        _arguments: &serde_json::Value,
        _cancel: &AtomicBool,
    ) -> Result<ToolOutput, String> {
        // Unreachable through the loop (no specs ⇒ no dispatch), but a
        // model-readable answer beats a panic if called directly.
        Err(format!(
            "unknown tool '{name}': no host tools are available"
        ))
    }
}

/// True when a tool name belongs to the reserved host namespace surface.
/// Enforced before a spec is shown to the model, so embedders cannot
/// accidentally create an unprefixed capability that looks like a
/// validated edit command.
pub fn is_host_tool_name(name: &str) -> bool {
    let Some((prefix, tool)) = name.split_once('_') else {
        return false;
    };
    matches!(
        prefix,
        "app" | "project" | "system" | "media" | "analysis" | "python" | "job"
    ) && !tool.is_empty()
        && tool
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// The `{namespace}` prefix of a tool name — everything before the first
/// `_`, or the whole name if there is none. Grouping for UIs; never used
/// for dispatch.
pub fn namespace(name: &str) -> &str {
    name.split('_').next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_the_prefix_before_the_first_underscore() {
        assert_eq!(namespace("app_set_theme"), "app");
        assert_eq!(namespace("media_screenshot_timeline"), "media");
        assert_eq!(namespace("python"), "python");
        assert_eq!(namespace(""), "");
    }

    #[test]
    fn host_names_require_a_reserved_prefix_and_machine_safe_suffix() {
        for name in [
            "app_set_theme",
            "project_save",
            "system_cache_clear",
            "media_frame",
            "analysis_find_moments",
            "python_run",
            "job_status",
            "job_2",
        ] {
            assert!(is_host_tool_name(name), "{name}");
        }
        for name in [
            "",
            "split_clip",
            "app",
            "app_",
            "unknown_ping",
            "app_SetTheme",
            "app_set-theme",
        ] {
            assert!(!is_host_tool_name(name), "{name}");
        }
    }

    #[test]
    fn text_constructor_carries_no_images() {
        let output = ToolOutput::text("done");
        assert_eq!(output.text, "done");
        assert!(output.images.is_empty());
    }
}
