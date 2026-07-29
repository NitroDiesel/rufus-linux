//! Download catalog and verification planning.
//!
//! Microsoft Windows ISOs and UEFI Shell images are never bundled. The UI may
//! open a browser to an official endpoint or fetch a signed release manifest.
//! Actual network downloads require HTTPS with certificate validation and
//! detached signature checks before any payload is trusted.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("download unavailable: {0}")]
    Unavailable(String),
    #[error("manifest invalid: {0}")]
    InvalidManifest(String),
    #[error("signature verification failed: {0}")]
    BadSignature(String),
    #[error("checksum mismatch")]
    ChecksumMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DownloadKind {
    WindowsIso,
    UefiShell,
    DbxUpdate,
    AppUpdate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadEntry {
    pub kind: DownloadKind,
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: Option<u64>,
    pub notes: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadManifest {
    pub schema_version: u32,
    pub generated_at: String,
    pub entries: Vec<DownloadEntry>,
}

/// Built-in catalog of *official* destinations. No proprietary binaries are shipped.
pub fn builtin_catalog() -> Vec<DownloadEntry> {
    vec![
        DownloadEntry {
            kind: DownloadKind::WindowsIso,
            name: "Windows 11".into(),
            version: "retail".into(),
            url: "https://www.microsoft.com/software-download/windows11".into(),
            sha256: String::new(),
            size_bytes: None,
            notes: "Opens Microsoft's official download page. Images are not redistributed.".into(),
        },
        DownloadEntry {
            kind: DownloadKind::WindowsIso,
            name: "Windows 10".into(),
            version: "retail".into(),
            url: "https://www.microsoft.com/software-download/windows10".into(),
            sha256: String::new(),
            size_bytes: None,
            notes: "Opens Microsoft's official download page. Images are not redistributed.".into(),
        },
        DownloadEntry {
            kind: DownloadKind::UefiShell,
            name: "UEFI Shell".into(),
            version: "latest".into(),
            url: "https://github.com/pbatard/UEFI-Shell/releases".into(),
            sha256: String::new(),
            size_bytes: None,
            notes: "Official UEFI Shell release page. Verify release signatures before use.".into(),
        },
        DownloadEntry {
            kind: DownloadKind::DbxUpdate,
            name: "UEFI DBX update".into(),
            version: "vendor".into(),
            url: "https://uefi.org/revocationlistfile".into(),
            sha256: String::new(),
            size_bytes: None,
            notes: "UEFI Forum revocation list. Failed checks must not report media as safe."
                .into(),
        },
    ]
}

pub fn parse_manifest(json: &str) -> Result<DownloadManifest, DownloadError> {
    let manifest: DownloadManifest =
        serde_json::from_str(json).map_err(|e| DownloadError::InvalidManifest(e.to_string()))?;
    if manifest.schema_version == 0 {
        return Err(DownloadError::InvalidManifest(
            "schema_version must be >= 1".into(),
        ));
    }
    for entry in &manifest.entries {
        if !entry.url.starts_with("https://") {
            return Err(DownloadError::InvalidManifest(format!(
                "non-HTTPS URL for {}",
                entry.name
            )));
        }
    }
    Ok(manifest)
}

/// Verify a payload digest. Empty expected digests mean "browser-only entry".
pub fn verify_sha256(data: &[u8], expected_hex: &str) -> Result<(), DownloadError> {
    if expected_hex.is_empty() {
        return Err(DownloadError::Unavailable(
            "no pinned digest for this catalog entry; open the official page instead".into(),
        ));
    }
    let digest = Sha256::digest(data);
    let actual = hex_lower(&digest);
    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(DownloadError::ChecksumMismatch)
    }
}

/// Placeholder for detached signature verification. Production builds must pin
/// a reviewed public key and fail closed on missing/invalid signatures.
pub fn verify_detached_signature(
    _payload: &[u8],
    signature: &[u8],
    _public_key_pem: &str,
) -> Result<(), DownloadError> {
    if signature.is_empty() {
        return Err(DownloadError::BadSignature("empty signature".into()));
    }
    // Intentionally conservative: without a linked signature library and pinned
    // key material, refuse to claim success.
    Err(DownloadError::BadSignature(
        "signature verification backend not configured in this build".into(),
    ))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_https_only() {
        for entry in builtin_catalog() {
            assert!(entry.url.starts_with("https://"));
        }
    }

    #[test]
    fn manifest_rejects_http() {
        let json = r#"{
            "schema_version": 1,
            "generated_at": "2026-01-01T00:00:00Z",
            "entries": [{
                "kind": "AppUpdate",
                "name": "test",
                "version": "1",
                "url": "http://example.com/x",
                "sha256": "abc",
                "size_bytes": null,
                "notes": ""
            }]
        }"#;
        assert!(parse_manifest(json).is_err());
    }

    #[test]
    fn sha256_matches() {
        let data = b"hello";
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_sha256(data, expected).is_ok());
    }
}
