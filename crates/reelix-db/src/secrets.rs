//! Encryption of provider secrets at rest.

use std::io::Write;
use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;
use rand::rngs::OsRng;

use crate::{DbError, Result};

const NONCE_LEN: usize = 12;

/// AES-256-GCM for provider secrets at rest. Output = base64(nonce || ciphertext).
#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox(..)")
    }
}

impl SecretBox {
    pub fn from_key(key: [u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key)),
        }
    }

    /// Reads a 32-byte key (base64 text file); creates it with 0600 perms on unix if missing.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let bytes = B64
                    .decode(text.trim())
                    .map_err(|e| DbError::Crypto(format!("secret key file {}: {e}", path.display())))?;
                let key: [u8; 32] = bytes.try_into().map_err(|_| {
                    DbError::Crypto(format!(
                        "secret key file {} must contain 32 bytes",
                        path.display()
                    ))
                })?;
                Ok(Self::from_key(key))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut key = [0u8; 32];
                OsRng.fill_bytes(&mut key);
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent)?;
                }
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                let mut file = opts.open(path)?;
                file.write_all(B64.encode(key).as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                tracing::info!(path = %path.display(), "generated new secret key");
                Ok(Self::from_key(key))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
            .map_err(|_| DbError::Crypto("encryption failed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(B64.encode(out))
    }

    pub fn decrypt(&self, encoded: &str) -> Result<String> {
        let raw = B64
            .decode(encoded.trim())
            .map_err(|e| DbError::Crypto(format!("invalid base64: {e}")))?;
        if raw.len() < NONCE_LEN + 16 {
            return Err(DbError::Crypto("ciphertext too short".into()));
        }
        let (nonce, ct) = raw.split_at(NONCE_LEN);
        let pt = self
            .cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| DbError::Crypto("decryption failed (wrong key or tampered data)".into()))?;
        String::from_utf8(pt).map_err(|_| DbError::Crypto("decrypted secret is not UTF-8".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_random_nonce() {
        let sb = SecretBox::from_key([7; 32]);
        let a = sb.encrypt("hunter2").unwrap();
        let b = sb.encrypt("hunter2").unwrap();
        assert_ne!(a, b);
        assert_eq!(sb.decrypt(&a).unwrap(), "hunter2");
        assert_eq!(sb.decrypt(&sb.encrypt("").unwrap()).unwrap(), "");
    }

    #[test]
    fn tamper_and_wrong_key_detected() {
        let sb = SecretBox::from_key([1; 32]);
        let enc = sb.encrypt("secret").unwrap();
        let mut raw = B64.decode(&enc).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        assert!(matches!(sb.decrypt(&B64.encode(&raw)), Err(DbError::Crypto(_))));
        assert!(matches!(
            SecretBox::from_key([2; 32]).decrypt(&enc),
            Err(DbError::Crypto(_))
        ));
        assert!(matches!(sb.decrypt("not base64!"), Err(DbError::Crypto(_))));
        assert!(matches!(sb.decrypt("AAAA"), Err(DbError::Crypto(_))));
    }

    #[test]
    fn load_or_create_persists_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("secret.key");
        let a = SecretBox::load_or_create(&path).unwrap();
        let enc = a.encrypt("abc").unwrap();
        let b = SecretBox::load_or_create(&path).unwrap();
        assert_eq!(b.decrypt(&enc).unwrap(), "abc");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn load_rejects_bad_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        std::fs::write(&path, B64.encode([0u8; 16])).unwrap();
        assert!(matches!(
            SecretBox::load_or_create(&path),
            Err(DbError::Crypto(_))
        ));
    }
}
