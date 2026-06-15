//! Inference configuration: `~/.cutlass/config.toml`, `[ml]` table.
//!
//! Local-first, so the feature works with *no* config — an absent `[ml]` table
//! means "use the local defaults" (the base.en whisper model), never an error.
//! The table only exists to pick a different local model or route a capability
//! to a cloud provider. Keys never live in project files; cloud credentials
//! resolve from the environment, mirroring the `[ai]` table in `cutlass-ai`.
//!
//! ```toml
//! [ml]
//! transcribe_model = "base.en"      # which local whisper model to run
//! transcribe_provider = "local"     # "local" (default) or "cloud"
//! # Cloud transcribe (OpenAI-compatible), used when provider = "cloud":
//! # base_url = "https://api.openai.com/v1"
//! # cloud_model = "whisper-1"
//! # api_key_env = "OPENAI_API_KEY"
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where a capability runs. Local-first: the default is always the local
/// runtime, and cloud is opt-in per capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscribeProvider {
    #[default]
    Local,
    Cloud,
}

/// The `[ml]` table of `config.toml`. Every field is defaulted, so an empty
/// table — or no table at all — yields a usable local configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct MlSection {
    /// Local whisper model name, resolved against the model registry, e.g.
    /// `"base.en"` or `"small"`.
    pub transcribe_model: String,
    /// Whether transcription runs locally or against a cloud provider.
    pub transcribe_provider: TranscribeProvider,
    /// OpenAI-compatible endpoint root for cloud transcribe.
    pub base_url: Option<String>,
    /// Cloud transcribe model name, e.g. `"whisper-1"`.
    pub cloud_model: Option<String>,
    /// Literal API key for the cloud provider.
    pub api_key: Option<String>,
    /// Name of an environment variable holding the key (preferred over a
    /// literal).
    pub api_key_env: Option<String>,
}

fn default_transcribe_model() -> String {
    "base.en".to_string()
}

impl Default for MlSection {
    fn default() -> Self {
        Self {
            transcribe_model: default_transcribe_model(),
            transcribe_provider: TranscribeProvider::Local,
            base_url: None,
            cloud_model: None,
            api_key: None,
            api_key_env: None,
        }
    }
}

impl MlSection {
    /// The cloud key to send, resolving `api_key_env` if set. `Ok(None)` means
    /// no key; `Err` names what is missing. (Only relevant when
    /// `transcribe_provider = "cloud"`.)
    pub fn resolve_api_key(&self) -> Result<Option<String>, String> {
        if let Some(var) = &self.api_key_env {
            return match std::env::var(var) {
                Ok(key) if !key.is_empty() => Ok(Some(key)),
                _ => Err(format!(
                    "api_key_env points at '{var}' but that environment variable is unset"
                )),
            };
        }
        Ok(self.api_key.clone())
    }
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    ml: Option<MlSection>,
}

/// `~/.cutlass/config.toml` (HOME-relative; falls back to the working
/// directory when HOME is unset, mirroring `recent.json`, autosave, and the
/// `[ai]` config).
pub fn default_config_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cutlass")
        .join("config.toml")
}

/// Load the `[ml]` section from `path`. `Ok(None)` = no `[ml]` table (use
/// [`MlSection::default`] — local defaults); `Err` = the file exists but is
/// broken, with a message naming the problem.
pub fn load_ml_config(path: &Path) -> Result<Option<MlSection>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let parsed: ConfigFile =
        toml::from_str(&raw).map_err(|e| format!("could not parse {}: {e}", path.display()))?;
    Ok(parsed.ml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_no_table() {
        assert_eq!(
            load_ml_config(Path::new("/nonexistent/config.toml")),
            Ok(None)
        );
    }

    #[test]
    fn default_config_path_lives_under_dot_cutlass() {
        assert!(default_config_path().ends_with(PathBuf::from(".cutlass").join("config.toml")));
    }

    #[test]
    fn empty_ml_table_yields_local_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[ml]\n").unwrap();

        let section = load_ml_config(&path).unwrap().unwrap();
        assert_eq!(section, MlSection::default());
        assert_eq!(section.transcribe_model, "base.en");
        assert_eq!(section.transcribe_provider, TranscribeProvider::Local);
    }

    #[test]
    fn parses_overrides_and_tolerates_unknown_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[ai]
base_url = "http://localhost:11434/v1"
model = "qwen3:14b"

[ml]
transcribe_model = "small"
transcribe_provider = "cloud"
base_url = "https://api.openai.com/v1"
cloud_model = "whisper-1"
api_key_env = "OPENAI_API_KEY"
"#,
        )
        .unwrap();

        let section = load_ml_config(&path).unwrap().unwrap();
        assert_eq!(section.transcribe_model, "small");
        assert_eq!(section.transcribe_provider, TranscribeProvider::Cloud);
        assert_eq!(section.cloud_model.as_deref(), Some("whisper-1"));
    }

    #[test]
    fn missing_ml_table_is_none_and_broken_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        std::fs::write(&path, "[ai]\nmodel = \"x\"\n").unwrap();
        assert_eq!(load_ml_config(&path), Ok(None));

        std::fs::write(&path, "[ml]\ntranscribe_provider = \n").unwrap();
        assert!(
            load_ml_config(&path)
                .unwrap_err()
                .contains("could not parse")
        );
    }

    #[test]
    fn unknown_provider_value_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[ml]\ntranscribe_provider = \"telepathy\"\n").unwrap();
        assert!(load_ml_config(&path).is_err());
    }

    #[test]
    fn api_key_env_resolution() {
        let section = MlSection {
            api_key: Some("literal".into()),
            api_key_env: Some("CUTLASS_ML_TEST_KEY_UNSET".into()),
            ..MlSection::default()
        };
        assert!(section.resolve_api_key().unwrap_err().contains("unset"));

        let literal = MlSection {
            api_key_env: None,
            ..section
        };
        assert_eq!(literal.resolve_api_key(), Ok(Some("literal".into())));
    }
}
