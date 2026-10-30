//! Long-lived daemon identity (self-signed P-256 certificate) and the store
//! of paired iPads (pinned certificate fingerprints).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sidecar_protocol::pairing::{self, Fingerprint};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Identity {
    pub cert_der: Vec<u8>,
    /// PKCS#8 DER.
    pub key_der: Vec<u8>,
}

impl Identity {
    pub fn fingerprint(&self) -> Fingerprint {
        pairing::fingerprint(&self.cert_der)
    }

    /// Fresh P-256 self-signed certificate usable for TLS client and server
    /// auth (the fake iPad reuses this code path).
    pub fn generate(common_name: &str) -> Result<Identity> {
        use rcgen::{
            date_time_ymd, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
            KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
        };
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut params = CertificateParams::new(vec!["sidecar.invalid".to_string()])?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, common_name);
        dn.push(DnType::OrganizationName, "sidecarforlinux");
        params.distinguished_name = dn;
        params.not_before = date_time_ymd(2025, 1, 1);
        params.not_after = date_time_ymd(2045, 1, 1);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ClientAuth,
            ExtendedKeyUsagePurpose::ServerAuth,
        ];
        let cert = params.self_signed(&key)?;
        Ok(Identity {
            cert_der: cert.der().to_vec(),
            key_der: key.serialize_der(),
        })
    }

    /// Loads `identity.cert.der` / `identity.key.der` from `dir`, creating
    /// them (key file mode 0600) on first use.
    pub fn load_or_create(dir: &Path, common_name: &str) -> Result<Identity> {
        let cert_p = dir.join("identity.cert.der");
        let key_p = dir.join("identity.key.der");
        if cert_p.exists() && key_p.exists() {
            return Ok(Identity {
                cert_der: fs::read(&cert_p)
                    .with_context(|| format!("reading {}", cert_p.display()))?,
                key_der: fs::read(&key_p)
                    .with_context(|| format!("reading {}", key_p.display()))?,
            });
        }
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let id = Identity::generate(common_name)?;
        write_private(&key_p, &id.key_der)?;
        fs::write(&cert_p, &id.cert_der)?;
        tracing::info!(
            "created identity {} (fingerprint {})",
            cert_p.display(),
            pairing::format_fingerprint(&id.fingerprint())
        );
        Ok(id)
    }
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrustedDevice {
    pub name: String,
    /// Lower-case hex SHA-256 of the iPad certificate DER.
    pub fingerprint: String,
    /// Unix seconds.
    pub paired_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default, rename = "device")]
    pub devices: Vec<TrustedDevice>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl TrustStore {
    pub fn load(path: &Path) -> Result<TrustStore> {
        let mut t: TrustStore = if path.exists() {
            toml::from_str(&fs::read_to_string(path)?)
                .with_context(|| format!("parsing {}", path.display()))?
        } else {
            TrustStore::default()
        };
        t.path = Some(path.to_path_buf());
        Ok(t)
    }

    pub fn in_memory() -> TrustStore {
        TrustStore::default()
    }

    pub fn find(&self, fp: &Fingerprint) -> Option<&TrustedDevice> {
        let hex = pairing::hex_fingerprint(fp);
        self.devices.iter().find(|d| d.fingerprint == hex)
    }

    pub fn is_trusted(&self, fp: &Fingerprint) -> bool {
        self.find(fp).is_some()
    }

    pub fn add(&mut self, fp: &Fingerprint, name: &str) -> Result<()> {
        let hex = pairing::hex_fingerprint(fp);
        self.devices.retain(|d| d.fingerprint != hex);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.devices.push(TrustedDevice {
            name: name.to_string(),
            fingerprint: hex,
            paired_at: now,
        });
        self.save()
    }

    pub fn remove(&mut self, fp_prefix_or_name: &str) -> Result<usize> {
        let before = self.devices.len();
        let needle = fp_prefix_or_name.to_ascii_lowercase().replace(':', "");
        self.devices.retain(|d| {
            !(d.name == fp_prefix_or_name
                || (!needle.is_empty() && d.fingerprint.starts_with(&needle)))
        });
        self.save()?;
        Ok(before - self.devices.len())
    }

    fn save(&self) -> Result<()> {
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent() {
                fs::create_dir_all(dir)?;
            }
            write_private(p, toml::to_string_pretty(self)?.as_bytes())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists_and_trust_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(dir.path(), "test").unwrap();
        let b = Identity::load_or_create(dir.path(), "test").unwrap();
        assert_eq!(a.cert_der, b.cert_der);
        let mode = std::fs::metadata(dir.path().join("identity.key.der")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);

        let tp = dir.path().join("trusted.toml");
        let mut t = TrustStore::load(&tp).unwrap();
        let fp = a.fingerprint();
        assert!(!t.is_trusted(&fp));
        t.add(&fp, "iPad").unwrap();
        let t2 = TrustStore::load(&tp).unwrap();
        assert!(t2.is_trusted(&fp));
        let mut t3 = TrustStore::load(&tp).unwrap();
        assert_eq!(t3.remove("iPad").unwrap(), 1);
        assert!(!TrustStore::load(&tp).unwrap().is_trusted(&fp));
    }
}
