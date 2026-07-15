//! Safe, shell-free handoff to the platform's file browser and URL opener.
//!
//! Agent callers must pass through System-tier authorization before reaching
//! this module. Validation still lives here so malformed or relative targets
//! cannot turn into surprising process launches.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_TARGET_CHARS: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalTarget {
    WebUrl(String),
    Path(PathBuf),
}

/// Accept an HTTP(S) URL or an existing absolute path.
pub fn parse_target(raw: &str) -> Result<ExternalTarget, String> {
    let target = raw.trim();
    if target.is_empty() {
        return Err("external target must not be empty".into());
    }
    if target.chars().count() > MAX_TARGET_CHARS {
        return Err("external target is too long".into());
    }
    if target.chars().any(char::is_control) {
        return Err("external target contains control characters".into());
    }

    if let Some((scheme, rest)) = target.split_once("://") {
        if !matches_ignore_ascii_case(scheme, &["http", "https"]) {
            return Err("external URLs must use http or https".into());
        }
        if rest.is_empty() || rest.chars().any(char::is_whitespace) {
            return Err("external URL is malformed".into());
        }
        return Ok(ExternalTarget::WebUrl(target.to_string()));
    }

    let path = PathBuf::from(target);
    validate_existing_absolute_path(&path)?;
    Ok(ExternalTarget::Path(path))
}

pub fn open_external(target: &ExternalTarget) -> Result<(), String> {
    match target {
        ExternalTarget::WebUrl(url) => spawn(open_command(OsStr::new(url))),
        ExternalTarget::Path(path) => {
            validate_existing_absolute_path(path)?;
            spawn(open_command(path.as_os_str()))
        }
    }
}

pub fn open_web_url(url: &str) -> Result<(), String> {
    let target = parse_target(url)?;
    match &target {
        ExternalTarget::WebUrl(_) => open_external(&target),
        ExternalTarget::Path(_) => Err("browser target must be an http or https URL".into()),
    }
}

/// Reveal an existing absolute path, selecting it where the platform supports
/// that and opening its containing directory otherwise.
pub fn reveal_path(path: &Path) -> Result<(), String> {
    validate_existing_absolute_path(path)?;
    spawn(reveal_command(path))
}

pub(crate) fn validate_existing_absolute_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("external path must be absolute".into());
    }
    if !path.exists() {
        return Err("external path does not exist".into());
    }
    Ok(())
}

fn matches_ignore_ascii_case(value: &str, allowed: &[&str]) -> bool {
    allowed
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchCommand {
    program: &'static str,
    arguments: Vec<OsString>,
}

fn spawn(command: LaunchCommand) -> Result<(), String> {
    Command::new(command.program)
        .args(&command.arguments)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not launch the system opener: {error}"))
}

#[cfg(target_os = "macos")]
fn open_command(target: &OsStr) -> LaunchCommand {
    LaunchCommand {
        program: "open",
        arguments: vec![target.to_os_string()],
    }
}

#[cfg(target_os = "windows")]
fn open_command(target: &OsStr) -> LaunchCommand {
    LaunchCommand {
        program: "explorer.exe",
        arguments: vec![target.to_os_string()],
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_command(target: &OsStr) -> LaunchCommand {
    LaunchCommand {
        program: "xdg-open",
        arguments: vec![target.to_os_string()],
    }
}

#[cfg(target_os = "macos")]
fn reveal_command(path: &Path) -> LaunchCommand {
    LaunchCommand {
        program: "open",
        arguments: vec![OsString::from("-R"), path.as_os_str().to_os_string()],
    }
}

#[cfg(target_os = "windows")]
fn reveal_command(path: &Path) -> LaunchCommand {
    let mut select = OsString::from("/select,");
    select.push(path.as_os_str());
    LaunchCommand {
        program: "explorer.exe",
        arguments: vec![select],
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn reveal_command(path: &Path) -> LaunchCommand {
    let directory = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    LaunchCommand {
        program: "xdg-open",
        arguments: vec![directory.as_os_str().to_os_string()],
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parser_accepts_only_web_urls_and_existing_absolute_paths() {
        assert_eq!(
            parse_target("https://cutlass.sh/docs"),
            Ok(ExternalTarget::WebUrl(
                "https://cutlass.sh/docs".to_string()
            ))
        );
        assert!(matches!(
            parse_target("HTTP://localhost:8080"),
            Ok(ExternalTarget::WebUrl(_))
        ));
        for invalid in [
            "",
            "javascript:alert(1)",
            "file:///tmp",
            "ftp://example.com",
            "https://bad host",
            "relative/file.txt",
        ] {
            assert!(parse_target(invalid).is_err(), "{invalid}");
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("clip.mp4");
        fs::write(&file, b"fixture").expect("write");
        assert_eq!(
            parse_target(file.to_str().expect("UTF-8 fixture")),
            Ok(ExternalTarget::Path(file))
        );
    }

    #[test]
    fn parser_rejects_missing_paths_controls_and_oversized_targets() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(parse_target(dir.path().join("missing").to_str().unwrap()).is_err());
        assert!(parse_target("https://cutlass.sh/\nnext").is_err());
        assert!(parse_target(&format!("https://cutlass.sh/{}", "a".repeat(8_193))).is_err());
    }

    #[test]
    fn command_construction_passes_targets_as_single_arguments() {
        let target = OsStr::new("https://cutlass.sh/?a=1&b=two words");
        let command = open_command(target);
        assert!(!command.program.is_empty());
        assert_eq!(command.arguments, vec![target.to_os_string()]);

        let dir = tempfile::tempdir().expect("tempdir");
        let command = reveal_command(dir.path());
        assert!(!command.program.is_empty());
        assert!(!command.arguments.is_empty());
        assert!(command.arguments.iter().any(|argument| {
            argument
                .to_string_lossy()
                .contains(dir.path().to_str().unwrap())
        }));
    }
}
