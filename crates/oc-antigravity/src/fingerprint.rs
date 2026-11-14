//! Per-account device fingerprint generation and header snapshots.

use crate::constants::{
    GEMINI_CLI_API_CLIENT, GEMINI_CLI_CLIENT_METADATA, GEMINI_CLI_USER_AGENT,
    antigravity_user_agent,
};
use crate::version::VersionTracker;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum historical fingerprint entries kept per account.
pub const MAX_FINGERPRINT_HISTORY: usize = 5;

const SDK_CLIENTS: &[&str] = &[
    "google-cloud-sdk vscode_cloudshelleditor/0.1",
    "google-cloud-sdk vscode/1.86.0",
    "google-cloud-sdk jetbrains/2024.1",
];

const IDE_TYPES: &[&str] = &["IDE_UNSPECIFIED", "VSCODE", "JETBRAINS", "ANDROID_STUDIO"];

const PLATFORMS: &[&str] = &["WINDOWS", "MACOS", "LINUX"];

/// Client metadata embedded inside a [`Fingerprint`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientMetadataPayload {
    /// Reported IDE type.
    pub ide_type: String,
    /// Reported OS platform.
    pub platform: String,
    /// Reported plugin type.
    pub plugin_type: String,
}

/// Stable pseudo-device identity bound to an Antigravity account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    /// Device UUID v4 identifier.
    pub device_id: String,
    /// Client session token (32-char hex string).
    pub session_token: String,
    /// User-Agent string snapshot.
    pub user_agent: String,
    /// API client descriptor.
    pub api_client: String,
    /// Client metadata payload.
    pub client_metadata: ClientMetadataPayload,
    /// `X-Goog-QuotaUser` header value (`device-<hex>`).
    pub quota_user: String,
    /// Timestamp when this fingerprint was created (epoch ms).
    pub created_at: i64,
}

/// Archived fingerprint entry saved prior to regeneration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintVersion {
    /// Archived fingerprint payload.
    pub fingerprint: Fingerprint,
    /// Archive timestamp (epoch ms).
    pub timestamp: i64,
    /// Reason for regenerating (e.g. `"rate_limit"` or `"manual"`).
    pub reason: String,
}

/// Generate a fresh randomized [`Fingerprint`] at `now_ms`.
#[must_use]
pub fn generate_fingerprint(now_ms: i64, antigravity_version: &str) -> Fingerprint {
    let mut rng = rand::rng();
    let device_bytes: [u8; 16] = rng.random();
    let session_bytes: [u8; 16] = rng.random();
    let device_id = format_uuid_v4(&device_bytes);
    let session_token = hex::encode(session_bytes);

    let sdk_idx = rng.random_range(0..SDK_CLIENTS.len());
    let ide_idx = rng.random_range(0..IDE_TYPES.len());
    let plat_idx = rng.random_range(0..PLATFORMS.len());

    let mut hasher = Sha256::new();
    hasher.update(device_id.as_bytes());
    let digest = hasher.finalize();
    let short_hex = hex::encode(&digest[..8]);

    Fingerprint {
        device_id,
        session_token,
        user_agent: antigravity_user_agent(antigravity_version),
        api_client: SDK_CLIENTS[sdk_idx].to_owned(),
        client_metadata: ClientMetadataPayload {
            ide_type: IDE_TYPES[ide_idx].to_owned(),
            platform: PLATFORMS[plat_idx].to_owned(),
            plugin_type: "GEMINI".to_owned(),
        },
        quota_user: format!("device-{short_hex}"),
        created_at: now_ms,
    }
}

/// Generate a fresh fingerprint using the global [`VersionTracker`].
#[must_use]
pub fn generate_default_fingerprint(now_ms: i64) -> Fingerprint {
    generate_fingerprint(now_ms, &VersionTracker::global().current())
}

/// Build Gemini-CLI style metadata header pairs supplemented by the account's quota user.
#[must_use]
pub fn fingerprint_headers(fp: &Fingerprint) -> Vec<(&'static str, String)> {
    vec![
        ("User-Agent", GEMINI_CLI_USER_AGENT.to_owned()),
        ("X-Goog-Api-Client", GEMINI_CLI_API_CLIENT.to_owned()),
        ("Client-Metadata", GEMINI_CLI_CLIENT_METADATA.to_owned()),
        ("X-Goog-QuotaUser", fp.quota_user.clone()),
        ("X-Client-Device-Id", fp.device_id.clone()),
    ]
}

fn format_uuid_v4(raw: &[u8; 16]) -> String {
    let mut b = *raw;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&b[0..4]),
        hex::encode(&b[4..6]),
        hex::encode(&b[6..8]),
        hex::encode(&b[8..10]),
        hex::encode(&b[10..16]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_fingerprint_has_valid_uuid_and_quota_user() {
        let fp = generate_fingerprint(1_700_000_000_000, "2.24.0");
        assert_eq!(fp.device_id.len(), 36);
        assert!(fp.quota_user.starts_with("device-"));
        assert_eq!(fp.session_token.len(), 32);
        assert!(fp.user_agent.contains("antigravity/hub/2.24.0"));
    }
}
