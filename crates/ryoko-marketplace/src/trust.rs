//! Verified plugin publisher identity, Ed25519 signature verification,
//! publisher key pinning, revocation, and monotonic anti-rollback checks.
//!
//! Registry metadata is never treated as a source of publisher trust. Every
//! package must carry a canonical Ed25519 signature (`ring::signature::ED25519`)
//! from a locally trusted, non-revoked publisher key that binds the publisher,
//! plugin ID, version, exact artifact SHA-256 digest, declared capabilities,
//! and expiry timestamp.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::MarketplaceError;

pub const TRUST_SCHEMA_VERSION: u32 = 1;
const SIGNING_DOMAIN: &[u8] = b"ryoko-plugin-v1\n";

/// Canonical statement signed by a plugin publisher's Ed25519 key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SignedPackageStatement {
    pub schema_version: u32,
    pub publisher_id: String,
    pub key_id: String,
    pub plugin_id: String,
    pub version: String,
    pub artifact_sha256: String,
    pub capabilities: Vec<String>,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
}

impl SignedPackageStatement {
    /// Deterministic domain-separated bytes covered by the Ed25519 signature.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, MarketplaceError> {
        let mut normalized = self.clone();
        normalized.capabilities.sort();
        normalized.capabilities.dedup();
        let json = serde_json::to_vec(&normalized)
            .map_err(|error| MarketplaceError::Parse(error.to_string()))?;
        let mut out = Vec::with_capacity(SIGNING_DOMAIN.len() + json.len());
        out.extend_from_slice(SIGNING_DOMAIN);
        out.extend_from_slice(&json);
        Ok(out)
    }
}

/// Signed envelope accompanying a downloadable plugin artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PublisherSignatureEnvelope {
    pub statement: SignedPackageStatement,
    /// Lowercase hex-encoded 64-byte Ed25519 signature.
    pub signature_hex: String,
}

/// A locally trusted publisher Ed25519 public key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrustedPublisherKey {
    pub publisher_id: String,
    pub key_id: String,
    /// Lowercase hex-encoded 32-byte Ed25519 public key.
    pub public_key_hex: String,
    #[serde(default)]
    pub revoked: bool,
}

impl TrustedPublisherKey {
    pub fn fingerprint(&self) -> String {
        sha256_hex(self.public_key_hex.to_ascii_lowercase().as_bytes())
    }
}

/// Persistent local pin binding a plugin ID to its verified publisher and
/// monotonic version floor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PinnedPluginTrust {
    pub plugin_id: String,
    pub publisher_id: String,
    pub key_id: String,
    pub key_fingerprint: String,
    pub last_installed_version: String,
    pub last_artifact_sha256: String,
    pub approved_capabilities: Vec<String>,
    pub verified_at_unix: u64,
}

/// Verified installation receipt recorded into the local installation lock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedInstallRecord {
    pub plugin_id: String,
    pub publisher_id: String,
    pub key_id: String,
    pub key_fingerprint: String,
    pub version: String,
    pub artifact_sha256: String,
    pub capabilities: Vec<String>,
    pub verified_at_unix: u64,
}

/// Local operator-controlled publisher trust store and plugin pin database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PublisherTrustStore {
    pub schema_version: u32,
    #[serde(default)]
    pub trusted_keys: BTreeMap<String, TrustedPublisherKey>,
    #[serde(default)]
    pub pinned_plugins: BTreeMap<String, PinnedPluginTrust>,
}

impl Default for PublisherTrustStore {
    fn default() -> Self {
        Self {
            schema_version: TRUST_SCHEMA_VERSION,
            trusted_keys: BTreeMap::new(),
            pinned_plugins: BTreeMap::new(),
        }
    }
}

impl PublisherTrustStore {
    pub fn load(path: &Path) -> Result<Self, MarketplaceError> {
        if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(MarketplaceError::Io(
                "refusing to read symlinked publisher trust store".to_owned(),
            ));
        }
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(MarketplaceError::Io(error.to_string())),
        };
        let store: Self = serde_json::from_str(&text)
            .map_err(|error| MarketplaceError::Parse(error.to_string()))?;
        if store.schema_version != TRUST_SCHEMA_VERSION {
            return Err(MarketplaceError::InvalidManifest(format!(
                "unsupported trust store schema version {}",
                store.schema_version
            )));
        }
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<(), MarketplaceError> {
        if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(MarketplaceError::Io(
                "refusing to overwrite symlinked publisher trust store".to_owned(),
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| MarketplaceError::Io("invalid trust store path".to_owned()))?;
        fs::create_dir_all(parent).map_err(|error| MarketplaceError::Io(error.to_string()))?;
        let encoded = serde_json::to_vec_pretty(self)
            .map_err(|error| MarketplaceError::Parse(error.to_string()))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let temp = parent.join(format!(".trust-store-{}-{stamp}.tmp", std::process::id()));
        let write_result = (|| -> std::io::Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(&encoded)?;
            file.sync_all()
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp);
            return Err(MarketplaceError::Io(error.to_string()));
        }
        if let Err(error) = fs::rename(&temp, path) {
            let _ = fs::remove_file(&temp);
            return Err(MarketplaceError::Io(error.to_string()));
        }
        Ok(())
    }

    pub fn add_trusted_key(&mut self, key: TrustedPublisherKey) -> Result<(), MarketplaceError> {
        validate_id(&key.publisher_id, "publisher_id")?;
        validate_id(&key.key_id, "key_id")?;
        let decoded = decode_hex(&key.public_key_hex)?;
        if decoded.len() != 32 {
            return Err(MarketplaceError::InvalidManifest(
                "Ed25519 public key must be 32 bytes (64 hex chars)".to_owned(),
            ));
        }
        self.trusted_keys.insert(key.key_id.clone(), key);
        Ok(())
    }

    pub fn revoke_key(&mut self, key_id: &str) -> Result<(), MarketplaceError> {
        let key = self
            .trusted_keys
            .get_mut(key_id)
            .ok_or_else(|| MarketplaceError::NotFound(format!("unknown key `{key_id}`")))?;
        key.revoked = true;
        Ok(())
    }

    /// Explicitly approves transferring a pinned plugin ID to a new trusted
    /// publisher identity. Without this call, publisher changes fail closed.
    pub fn transfer_plugin_publisher(
        &mut self,
        plugin_id: &str,
        new_publisher_id: &str,
        new_key_id: &str,
    ) -> Result<(), MarketplaceError> {
        let key = self.trusted_keys.get(new_key_id).ok_or_else(|| {
            MarketplaceError::AuthRequired(format!("untrusted key `{new_key_id}`"))
        })?;
        if key.revoked || key.publisher_id != new_publisher_id {
            return Err(MarketplaceError::AuthRequired(
                "target transfer key is revoked or does not belong to new publisher".to_owned(),
            ));
        }
        let pin = self.pinned_plugins.get_mut(plugin_id).ok_or_else(|| {
            MarketplaceError::NotFound(format!("plugin `{plugin_id}` is not pinned"))
        })?;
        pin.publisher_id = new_publisher_id.to_owned();
        pin.key_id = new_key_id.to_owned();
        pin.key_fingerprint = key.fingerprint();
        Ok(())
    }

    /// Verifies signature, publisher key trust/revocation, expiry, exact
    /// artifact SHA-256 digest, publisher pin, monotonic version, and explicit
    /// capability consent. On success, updates the local plugin pin.
    pub fn verify_and_pin_package(
        &mut self,
        envelope: &PublisherSignatureEnvelope,
        artifact_bytes: &[u8],
        consented_capabilities: &BTreeSet<String>,
        now_unix: u64,
    ) -> Result<VerifiedInstallRecord, MarketplaceError> {
        let stmt = &envelope.statement;
        if stmt.schema_version != TRUST_SCHEMA_VERSION {
            return Err(MarketplaceError::InvalidManifest(format!(
                "unsupported signature schema version {}",
                stmt.schema_version
            )));
        }
        validate_id(&stmt.publisher_id, "publisher_id")?;
        validate_id(&stmt.key_id, "key_id")?;
        validate_id(&stmt.plugin_id, "plugin_id")?;
        let new_version = parse_semver(&stmt.version)?;

        if stmt.issued_at_unix > stmt.expires_at_unix {
            return Err(MarketplaceError::AuthRequired(
                "signature validity window is inverted".to_owned(),
            ));
        }
        if now_unix > stmt.expires_at_unix {
            return Err(MarketplaceError::AuthRequired(format!(
                "publisher signature for `{}` expired at {}",
                stmt.plugin_id, stmt.expires_at_unix
            )));
        }

        let trusted = self.trusted_keys.get(&stmt.key_id).ok_or_else(|| {
            MarketplaceError::AuthRequired(format!(
                "untrusted publisher key `{}` for `{}`",
                stmt.key_id, stmt.plugin_id
            ))
        })?;
        if trusted.revoked {
            return Err(MarketplaceError::AuthRequired(format!(
                "publisher key `{}` has been revoked",
                stmt.key_id
            )));
        }
        if trusted.publisher_id != stmt.publisher_id {
            return Err(MarketplaceError::AuthRequired(format!(
                "publisher impersonation: key `{}` belongs to `{}`, not `{}`",
                stmt.key_id, trusted.publisher_id, stmt.publisher_id
            )));
        }

        // Verify cryptographic Ed25519 signature over canonical statement bytes.
        let public_key_bytes = decode_hex(&trusted.public_key_hex)?;
        let signature_bytes = decode_hex(&envelope.signature_hex)?;
        if signature_bytes.len() != 64 {
            return Err(MarketplaceError::AuthRequired(
                "invalid Ed25519 signature length".to_owned(),
            ));
        }
        let message = stmt.canonical_bytes()?;
        UnparsedPublicKey::new(&ED25519, &public_key_bytes)
            .verify(&message, &signature_bytes)
            .map_err(|_| {
                MarketplaceError::AuthRequired(format!(
                    "Ed25519 signature verification failed for `{}`",
                    stmt.plugin_id
                ))
            })?;

        // Bind exact downloaded artifact bytes to the signed digest.
        let actual_sha256 = sha256_hex(artifact_bytes);
        if !actual_sha256.eq_ignore_ascii_case(&stmt.artifact_sha256) {
            return Err(MarketplaceError::checksum(
                &stmt.artifact_sha256,
                &actual_sha256,
            ));
        }

        // Enforce explicit capability consent.
        let mut normalized_caps = stmt.capabilities.clone();
        normalized_caps.sort();
        normalized_caps.dedup();
        for cap in &normalized_caps {
            if !consented_capabilities.contains(cap) {
                return Err(MarketplaceError::AuthRequired(format!(
                    "plugin `{}` requests unconsented capability `{cap}`",
                    stmt.plugin_id
                )));
            }
        }

        // Enforce publisher identity pinning and monotonic anti-rollback.
        if let Some(pin) = self.pinned_plugins.get(&stmt.plugin_id) {
            if pin.publisher_id != stmt.publisher_id {
                return Err(MarketplaceError::AuthRequired(format!(
                    "publisher pin mismatch for `{}`: pinned to `{}`, got `{}`",
                    stmt.plugin_id, pin.publisher_id, stmt.publisher_id
                )));
            }
            let pinned_version = parse_semver(&pin.last_installed_version)?;
            if new_version < pinned_version {
                return Err(MarketplaceError::VersionConflict(format!(
                    "rollback refused for `{}`: {} < pinned {}",
                    stmt.plugin_id, stmt.version, pin.last_installed_version
                )));
            }
        }

        let key_fingerprint = trusted.fingerprint();
        self.pinned_plugins.insert(
            stmt.plugin_id.clone(),
            PinnedPluginTrust {
                plugin_id: stmt.plugin_id.clone(),
                publisher_id: stmt.publisher_id.clone(),
                key_id: stmt.key_id.clone(),
                key_fingerprint: key_fingerprint.clone(),
                last_installed_version: stmt.version.clone(),
                last_artifact_sha256: actual_sha256.clone(),
                approved_capabilities: normalized_caps.clone(),
                verified_at_unix: now_unix,
            },
        );

        Ok(VerifiedInstallRecord {
            plugin_id: stmt.plugin_id.clone(),
            publisher_id: stmt.publisher_id.clone(),
            key_id: stmt.key_id.clone(),
            key_fingerprint,
            version: stmt.version.clone(),
            artifact_sha256: actual_sha256,
            capabilities: normalized_caps,
            verified_at_unix: now_unix,
        })
    }
}

/// Signs a canonical package statement using an Ed25519 key pair.
pub fn sign_package_statement(
    key_pair: &Ed25519KeyPair,
    statement: SignedPackageStatement,
) -> Result<PublisherSignatureEnvelope, MarketplaceError> {
    let payload = statement.canonical_bytes()?;
    let sig = key_pair.sign(&payload);
    Ok(PublisherSignatureEnvelope {
        statement,
        signature_hex: encode_hex(sig.as_ref()),
    })
}

fn validate_id(value: &str, field: &str) -> Result<(), MarketplaceError> {
    if value.is_empty()
        || value.len() > 80
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(MarketplaceError::InvalidManifest(format!(
            "invalid {field}: `{value}`"
        )));
    }
    Ok(())
}

fn parse_semver(version: &str) -> Result<(u64, u64, u64), MarketplaceError> {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.');
    let major = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .ok_or_else(|| MarketplaceError::InvalidManifest(format!("invalid version `{version}`")))?;
    let minor = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .ok_or_else(|| MarketplaceError::InvalidManifest(format!("invalid version `{version}`")))?;
    let patch = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .ok_or_else(|| MarketplaceError::InvalidManifest(format!("invalid version `{version}`")))?;
    if parts.next().is_some() {
        return Err(MarketplaceError::InvalidManifest(format!(
            "invalid version `{version}`"
        )));
    }
    Ok((major, minor, patch))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

pub fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, MarketplaceError> {
    if hex.len() & 1 != 0 {
        return Err(MarketplaceError::Parse(
            "hex string has odd length".to_owned(),
        ));
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    for idx in (0..bytes.len()).step_by(2) {
        let hi = (bytes[idx] as char)
            .to_digit(16)
            .ok_or_else(|| MarketplaceError::Parse("invalid hex digit".to_owned()))?;
        let lo = (bytes[idx + 1] as char)
            .to_digit(16)
            .ok_or_else(|| MarketplaceError::Parse("invalid hex digit".to_owned()))?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::KeyPair;

    fn generate_key() -> (Ed25519KeyPair, String) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let pub_hex = encode_hex(pair.public_key().as_ref());
        (pair, pub_hex)
    }

    fn make_statement(
        publisher_id: &str,
        key_id: &str,
        plugin_id: &str,
        version: &str,
        artifact: &[u8],
        expires_at_unix: u64,
    ) -> SignedPackageStatement {
        SignedPackageStatement {
            schema_version: TRUST_SCHEMA_VERSION,
            publisher_id: publisher_id.to_owned(),
            key_id: key_id.to_owned(),
            plugin_id: plugin_id.to_owned(),
            version: version.to_owned(),
            artifact_sha256: sha256_hex(artifact),
            capabilities: vec!["lint".to_owned()],
            issued_at_unix: 1_700_000_000,
            expires_at_unix,
        }
    }

    #[test]
    fn verifies_pins_persists_and_blocks_tampering_rollback_revocation_and_impersonation() {
        let dir = tempfile::TempDir::new().unwrap();
        let store_path = dir.path().join("trust-store.json");
        let (key_a, pub_a) = generate_key();
        let (key_b, pub_b) = generate_key();

        let mut store = PublisherTrustStore::default();
        store
            .add_trusted_key(TrustedPublisherKey {
                publisher_id: "acme".to_owned(),
                key_id: "acme-2026".to_owned(),
                public_key_hex: pub_a,
                revoked: false,
            })
            .unwrap();
        store
            .add_trusted_key(TrustedPublisherKey {
                publisher_id: "evil".to_owned(),
                key_id: "evil-2026".to_owned(),
                public_key_hex: pub_b,
                revoked: false,
            })
            .unwrap();

        let artifact = b"\0asm\x01\0\0\0";
        let consent = BTreeSet::from(["lint".to_owned()]);
        let stmt = make_statement(
            "acme",
            "acme-2026",
            "rust-lint",
            "1.2.0",
            artifact,
            2_000_000_000,
        );
        let env = sign_package_statement(&key_a, stmt).unwrap();

        // 1. Valid package succeeds and persists pin across reload.
        let record = store
            .verify_and_pin_package(&env, artifact, &consent, 1_800_000_000)
            .unwrap();
        assert_eq!(record.publisher_id, "acme");
        assert_eq!(record.version, "1.2.0");
        store.save(&store_path).unwrap();
        let mut reloaded = PublisherTrustStore::load(&store_path).unwrap();

        // 2. Altered artifact bytes fail closed.
        assert!(
            reloaded
                .verify_and_pin_package(&env, b"tampered", &consent, 1_800_000_000)
                .is_err()
        );

        // 3. Altered manifest field invalidates Ed25519 signature.
        let mut altered = env.clone();
        altered.statement.version = "1.2.1".to_owned();
        assert!(
            reloaded
                .verify_and_pin_package(&altered, artifact, &consent, 1_800_000_000)
                .is_err()
        );

        // 4. Key substitution (signed by key_b claiming key_a) fails closed.
        let spoofed = sign_package_statement(&key_b, env.statement.clone()).unwrap();
        assert!(
            reloaded
                .verify_and_pin_package(&spoofed, artifact, &consent, 1_800_000_000)
                .is_err()
        );

        // 5. Expired signature fails closed.
        assert!(
            reloaded
                .verify_and_pin_package(&env, artifact, &consent, 2_000_000_001)
                .is_err()
        );

        // 6. Version rollback fails closed.
        let older = sign_package_statement(
            &key_a,
            make_statement(
                "acme",
                "acme-2026",
                "rust-lint",
                "1.1.9",
                artifact,
                2_000_000_000,
            ),
        )
        .unwrap();
        assert!(
            reloaded
                .verify_and_pin_package(&older, artifact, &consent, 1_800_000_000)
                .is_err()
        );

        // 7. Unconsented capability fails closed.
        assert!(
            reloaded
                .verify_and_pin_package(&env, artifact, &BTreeSet::new(), 1_800_000_000)
                .is_err()
        );

        // 8. Different trusted publisher cannot hijack pinned plugin ID without transfer.
        let hijack = sign_package_statement(
            &key_b,
            make_statement(
                "evil",
                "evil-2026",
                "rust-lint",
                "2.0.0",
                artifact,
                2_000_000_000,
            ),
        )
        .unwrap();
        assert!(
            reloaded
                .verify_and_pin_package(&hijack, artifact, &consent, 1_800_000_000)
                .is_err()
        );

        // 9. Explicit verified transfer permits the new publisher.
        reloaded
            .transfer_plugin_publisher("rust-lint", "evil", "evil-2026")
            .unwrap();
        assert!(
            reloaded
                .verify_and_pin_package(&hijack, artifact, &consent, 1_800_000_000)
                .is_ok()
        );

        // 10. Revoked key fails closed immediately.
        reloaded.revoke_key("evil-2026").unwrap();
        assert!(
            reloaded
                .verify_and_pin_package(&hijack, artifact, &consent, 1_800_000_000)
                .is_err()
        );
    }
}
