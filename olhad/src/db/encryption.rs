//! Field-level encryption for notification content.
//!
//! A 32-byte Data Encryption Key (DEK) is derived from the raw bytes held
//! in `pass` at a configurable entry (default `olha/db-key`) via
//! `SHA-256(ikm)`. The derived DEK is used with XChaCha20-Poly1305 to
//! encrypt individual fields (`summary`, `body`, `hints`). Each field
//! gets a fresh 24-byte random nonce, and the AAD binds the ciphertext
//! to its field name + on-disk format version so ciphertexts cannot be
//! swapped between fields.
//!
//! Stored layout per field: `nonce(24) || ciphertext || tag(16)`.
//!
//! The DEK never leaves memory — see `Dek` / `EncryptionContext` which
//! wrap it in `Zeroizing` so it's wiped on drop.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;
use zeroize::Zeroizing;

/// Current on-disk encryption format version. Rows encrypted under this
/// version use XChaCha20-Poly1305 with AAD = `"olha/v1/{field}"`.
pub const ENC_VERSION_CURRENT: i64 = 1;

/// Length of a DEK in bytes.
pub const DEK_LEN: usize = 32;

/// Length of an XChaCha20-Poly1305 nonce.
pub const NONCE_LEN: usize = 24;

/// Length of the truncated SHA-256 identifier kept alongside each row.
pub const KEY_ID_LEN: usize = 4;

/// Timeout for the `pass show` subprocess. If gpg-agent is wedged with
/// no TTY, we fail fast rather than hanging forever.
const PASS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum EncryptionError {
    #[error("pass subprocess failed: {0}")]
    PassSpawn(#[source] std::io::Error),

    #[error("pass show returned non-zero exit code ({0}); is the entry set up? run `olha encryption init`")]
    PassExit(i32),

    #[error("pass show timed out after {}s — is gpg-agent waiting on pinentry with no TTY?", PASS_TIMEOUT.as_secs())]
    PassTimeout,

    #[error("pass entry is empty")]
    PassEmpty,

    #[error("encryption failed")]
    Encrypt,

    #[error("decryption failed: wrong key, tampered ciphertext, or truncated payload")]
    Decrypt,

    #[error("ciphertext payload too short ({got} bytes, need at least {min})", min = NONCE_LEN + 16)]
    CiphertextTooShort { got: usize },
}

/// Bytes of the data encryption key, auto-zeroized on drop.
pub type Dek = Zeroizing<[u8; DEK_LEN]>;

/// Everything a DB call needs to encrypt or decrypt a field.
///
/// Cheap to share: the inner `Zeroizing` owns the 32 bytes, and
/// `XChaCha20Poly1305` is just a reference to them. We wrap in `Arc`
/// inside `DaemonState` so every DB call borrows the same DEK.
pub struct EncryptionContext {
    cipher: XChaCha20Poly1305,
    key_id: [u8; KEY_ID_LEN],
    // Keep the DEK alive so its Drop runs at daemon shutdown rather than
    // right after cipher construction.
    _dek: Dek,
}

impl EncryptionContext {
    /// Build a context from an already-materialized DEK (tests, rotate).
    pub fn from_dek(dek: Dek) -> Self {
        let cipher = XChaCha20Poly1305::new_from_slice(dek.as_ref())
            .expect("DEK_LEN matches XChaCha20Poly1305 key size");
        let key_id = compute_key_id(dek.as_ref());
        Self {
            cipher,
            key_id,
            _dek: dek,
        }
    }

    /// Resolve and derive a DEK by shelling out to `pass show <entry>`.
    ///
    /// The raw bytes returned by `pass` (trailing whitespace stripped)
    /// are hashed with SHA-256 to produce the 32-byte DEK. This means
    /// any input length works, and the user can put whatever high-entropy
    /// secret they like in the pass entry.
    pub fn load_from_pass(pass_entry: &str) -> Result<Self, EncryptionError> {
        let ikm = run_pass_show(pass_entry)?;
        if ikm.is_empty() {
            return Err(EncryptionError::PassEmpty);
        }
        let dek = derive_dek(&ikm);
        Ok(Self::from_dek(dek))
    }

    pub fn key_id(&self) -> &[u8; KEY_ID_LEN] {
        &self.key_id
    }

    /// Encrypt a UTF-8 field into `nonce || ciphertext || tag`.
    pub fn encrypt_field(&self, field: FieldTag, plaintext: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let aad = field.aad();
        let ct = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| EncryptionError::Encrypt)?;

        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a `nonce || ciphertext || tag` blob back to bytes.
    pub fn decrypt_field(&self, field: FieldTag, blob: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        if blob.len() < NONCE_LEN + 16 {
            return Err(EncryptionError::CiphertextTooShort { got: blob.len() });
        }
        let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
        let nonce = XNonce::from_slice(nonce_bytes);
        let aad = field.aad();
        self.cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ct,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| EncryptionError::Decrypt)
    }
}

/// Which field a ciphertext belongs to. Used as AAD so ciphertexts
/// can't be swapped between columns (e.g. body_enc → summary_enc).
#[derive(Copy, Clone, Debug)]
pub enum FieldTag {
    Summary,
    Body,
    Hints,
}

impl FieldTag {
    fn aad(self) -> &'static str {
        match self {
            FieldTag::Summary => "olha/v1/summary",
            FieldTag::Body => "olha/v1/body",
            FieldTag::Hints => "olha/v1/hints",
        }
    }
}

fn derive_dek(ikm: &[u8]) -> Dek {
    let mut hasher = Sha256::new();
    hasher.update(ikm);
    let digest = hasher.finalize();
    let mut out = Zeroizing::new([0u8; DEK_LEN]);
    out.copy_from_slice(&digest);
    out
}

pub fn compute_key_id(dek: &[u8]) -> [u8; KEY_ID_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(dek);
    let digest = hasher.finalize();
    let mut id = [0u8; KEY_ID_LEN];
    id.copy_from_slice(&digest[..KEY_ID_LEN]);
    id
}

/// Shell out to `pass show <entry>`, capturing stdout and discarding
/// stderr (avoids leaking pinentry / agent diagnostics into our logs).
/// Times out after PASS_TIMEOUT to avoid hangs.
fn run_pass_show(entry: &str) -> Result<Vec<u8>, EncryptionError> {
    let mut child = Command::new("pass")
        .arg("show")
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(EncryptionError::PassSpawn)?;

    let deadline = Instant::now() + PASS_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = Vec::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_end(&mut out);
                }
                if !status.success() {
                    return Err(EncryptionError::PassExit(status.code().unwrap_or(-1)));
                }
                // Strip trailing whitespace — pass often appends a newline.
                while out.last().map_or(false, |b| b.is_ascii_whitespace()) {
                    out.pop();
                }
                return Ok(out);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(EncryptionError::PassTimeout);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(EncryptionError::PassSpawn(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_ctx() -> EncryptionContext {
        // Deterministic DEK for round-trip tests. Never a real key.
        let dek = Zeroizing::new([0xABu8; DEK_LEN]);
        EncryptionContext::from_dek(dek)
    }

    #[test]
    fn roundtrip_summary() {
        let ctx = fixed_ctx();
        let ct = ctx.encrypt_field(FieldTag::Summary, b"hello world").unwrap();
        let pt = ctx.decrypt_field(FieldTag::Summary, &ct).unwrap();
        assert_eq!(pt, b"hello world");
    }

    #[test]
    fn roundtrip_unicode_body() {
        let ctx = fixed_ctx();
        let msg = "héllo — émoji 🔐".as_bytes();
        let ct = ctx.encrypt_field(FieldTag::Body, msg).unwrap();
        let pt = ctx.decrypt_field(FieldTag::Body, &ct).unwrap();
        assert_eq!(pt, msg);
    }

    #[test]
    fn nonces_are_unique_across_calls() {
        let ctx = fixed_ctx();
        let a = ctx.encrypt_field(FieldTag::Summary, b"same").unwrap();
        let b = ctx.encrypt_field(FieldTag::Summary, b"same").unwrap();
        assert_ne!(a, b, "AEAD must randomize nonces to avoid determinism");
    }

    #[test]
    fn aad_field_swap_fails() {
        let ctx = fixed_ctx();
        let ct = ctx.encrypt_field(FieldTag::Summary, b"secret").unwrap();
        // Same ctx, same key, but decrypt as Body → tag check fails.
        assert!(ctx.decrypt_field(FieldTag::Body, &ct).is_err());
    }

    #[test]
    fn tamper_ciphertext_detected() {
        let ctx = fixed_ctx();
        let mut ct = ctx.encrypt_field(FieldTag::Body, b"hello").unwrap();
        // Flip a byte inside the ciphertext region (past the nonce).
        ct[NONCE_LEN + 2] ^= 0x01;
        assert!(ctx.decrypt_field(FieldTag::Body, &ct).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let ctx1 = EncryptionContext::from_dek(Zeroizing::new([0x01u8; DEK_LEN]));
        let ctx2 = EncryptionContext::from_dek(Zeroizing::new([0x02u8; DEK_LEN]));
        let ct = ctx1.encrypt_field(FieldTag::Body, b"hello").unwrap();
        assert!(ctx2.decrypt_field(FieldTag::Body, &ct).is_err());
    }

    #[test]
    fn truncated_payload_rejected() {
        let ctx = fixed_ctx();
        assert!(matches!(
            ctx.decrypt_field(FieldTag::Body, b"short"),
            Err(EncryptionError::CiphertextTooShort { .. })
        ));
    }

    #[test]
    fn key_id_is_stable_and_deterministic() {
        let a = EncryptionContext::from_dek(Zeroizing::new([0x33u8; DEK_LEN]));
        let b = EncryptionContext::from_dek(Zeroizing::new([0x33u8; DEK_LEN]));
        assert_eq!(a.key_id(), b.key_id());
    }

    #[test]
    fn derive_dek_is_sha256() {
        let d = derive_dek(b"hello");
        // Sanity: SHA-256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        let expected = [
            0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
            0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
            0x93, 0x8b, 0x98, 0x24,
        ];
        assert_eq!(d.as_ref(), &expected);
    }
}
