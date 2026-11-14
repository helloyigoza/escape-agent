//! Antigravity Hub runtime version resolution from the auto-updater YAML manifest.

use crate::constants::{ANTIGRAVITY_MINIMUM_VERSION, ANTIGRAVITY_VERSION_FALLBACK};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

const VERSION_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_VERSION_LENGTH: usize = 32;

static GLOBAL_VERSION: OnceLock<VersionTracker> = OnceLock::new();

/// Parse a strict `major.minor.patch` string where each component has 1-6 ASCII digits.
#[must_use]
pub fn parse_strict_version(input: &str) -> Option<(u64, u64, u64)> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_VERSION_LENGTH {
        return None;
    }
    let mut parts = trimmed.split('.');
    let major = parse_part(parts.next()?)?;
    let minor = parse_part(parts.next()?)?;
    let patch = parse_part(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn parse_part(part: &str) -> Option<u64> {
    if part.is_empty() || part.len() > 6 || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse::<u64>().ok()
}

/// Validate strict `X.Y.Z` version syntax.
#[must_use]
pub fn is_strict_version(input: &str) -> bool {
    parse_strict_version(input).is_some()
}

/// Compare two strict `X.Y.Z` versions. Returns `None` if either is malformed.
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    Some(parse_strict_version(a)?.cmp(&parse_strict_version(b)?))
}

/// Parse the top-level `version:` entry from an electron-builder YAML manifest.
///
/// Only unindented `version: ...` lines are considered so nested file entries cannot spoof it.
#[must_use]
pub fn parse_manifest_version(yaml_text: &str) -> Option<String> {
    for raw_line in yaml_text.lines() {
        let line = raw_line.trim_end_matches('\r');
        // Top-level YAML key must start at column 0.
        if line.starts_with(' ') || line.starts_with('\t') || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("version:") else {
            continue;
        };
        let mut value = rest.trim();
        // Strip inline YAML comment if preceded by whitespace (safe for X.Y.Z values).
        if let Some((before_comment, _)) = value.split_once('#') {
            value = before_comment.trim();
        }
        let unquoted = value
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(value)
            .trim();
        if is_strict_version(unquoted) {
            return Some(unquoted.to_owned());
        }
        return None;
    }
    None
}

/// Thread-safe Hub version state pinned once on refresh or falling back cleanly.
#[derive(Debug, Clone)]
pub struct VersionTracker {
    inner: Arc<RwLock<VersionState>>,
}

#[derive(Debug, Clone)]
struct VersionState {
    current: String,
    locked: bool,
}

impl Default for VersionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl VersionTracker {
    /// Create a tracker initialized to [`ANTIGRAVITY_VERSION_FALLBACK`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(VersionState {
                current: ANTIGRAVITY_VERSION_FALLBACK.to_owned(),
                locked: false,
            })),
        }
    }

    /// Return the global process-wide tracker.
    #[must_use]
    pub fn global() -> &'static Self {
        GLOBAL_VERSION.get_or_init(Self::new)
    }

    /// Read the active Antigravity Hub version.
    #[must_use]
    pub fn current(&self) -> String {
        self.inner.read().map_or_else(
            |_| ANTIGRAVITY_VERSION_FALLBACK.to_owned(),
            |guard| guard.current.clone(),
        )
    }

    /// Pin the runtime version once if `version` is valid and `>= ANTIGRAVITY_MINIMUM_VERSION`.
    /// Subsequent calls are ignored once locked.
    pub fn set_once(&self, version: &str) -> bool {
        if !is_strict_version(version) {
            return false;
        }
        if compare_versions(version, ANTIGRAVITY_MINIMUM_VERSION)
            .is_none_or(std::cmp::Ordering::is_lt)
        {
            return false;
        }
        if let Ok(mut guard) = self.inner.write() {
            if guard.locked {
                return false;
            }
            version.clone_into(&mut guard.current);
            guard.locked = true;
            return true;
        }
        false
    }

    /// Fetch the remote auto-updater manifest and update the tracked version if newer/valid.
    pub async fn refresh_from_manifest(
        &self,
        client: &reqwest::Client,
        manifest_url: &str,
    ) -> Option<String> {
        let resp = client
            .get(manifest_url)
            .timeout(VERSION_FETCH_TIMEOUT)
            .header("User-Agent", "electron-builder")
            .header("Cache-Control", "no-cache")
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let text = resp.text().await.ok()?;
        let parsed = parse_manifest_version(&text)?;
        if compare_versions(&parsed, ANTIGRAVITY_MINIMUM_VERSION)
            .is_none_or(std::cmp::Ordering::is_lt)
        {
            return None;
        }
        let _ = self.set_once(&parsed);
        Some(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_version_validation_matches_spec() {
        assert!(is_strict_version("2.24.0"));
        assert!(is_strict_version("10.0.123456"));
        assert!(!is_strict_version("2.24"));
        assert!(!is_strict_version("2.24.0-beta.1"));
        assert!(!is_strict_version("v2.24.0"));
        assert!(!is_strict_version("2.24.0.1"));
        assert!(!is_strict_version("2.24.1234567"));
    }

    #[test]
    fn parse_manifest_ignores_indented_keys() {
        let yaml = "\
files:
  - url: app.zip
  - version: 9.9.9
version: '2.25.1' # latest
releaseDate: '2026-01-01'
";
        assert_eq!(parse_manifest_version(yaml).as_deref(), Some("2.25.1"));
    }

    #[test]
    fn version_tracker_sets_once_and_rejects_old_versions() {
        let tracker = VersionTracker::new();
        assert_eq!(tracker.current(), ANTIGRAVITY_VERSION_FALLBACK);
        assert!(!tracker.set_once("2.22.9"));
        assert_eq!(tracker.current(), ANTIGRAVITY_VERSION_FALLBACK);
        assert!(tracker.set_once("2.25.0"));
        assert_eq!(tracker.current(), "2.25.0");
        assert!(!tracker.set_once("2.26.0"));
        assert_eq!(tracker.current(), "2.25.0");
    }
}
