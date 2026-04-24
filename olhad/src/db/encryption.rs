//! At-rest encryption for notification content.
//!
//! The daemon holds a long-lived X25519 public key loaded from the DB's
//! `meta` table at startup. Incoming notifications are sealed against
//! that public key, so writes succeed even when nobody has unlocked the
//! daemon. Reading back encrypted rows requires the matching X25519
//! secret key, which lives in memory only between `Unlock` and `Lock`
//! (or the idle auto-lock timer).
//!
//! The secret key is itself AEAD-wrapped with a Data Encryption Key
//! (DEK) derived from `pass show <entry>` via `SHA-256(ikm)`. The DEK
//! only exists in memory during the brief unwrap-at-unlock step —
//! the ongoing read capability is the X25519 secret, not the DEK.
//!
//! On-disk field layout (`enc_version = 1`):
//!     version(1)=0x01 || field_tag(1) || epk(32) || nonce(24) || ciphertext_with_tag
//!
//! Wrapped-sk layout stored in `meta.enc_wrapped_secret` (base64 of):
//!     version(1)=0x01 || nonce(24) || ciphertext(32) || tag(16)
//! AAD for the wrap: `b"olha/wrapped-sk"`.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use parking_lot::RwLock;
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// On-disk format version for encrypted row fields. Rows at this
/// version use the sealed-box scheme; reads require an unlocked
/// X25519 secret key.
pub const ENC_VERSION: i64 = 1;

pub const DEK_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const KEY_ID_LEN: usize = 4;
pub const X25519_KEY_LEN: usize = 32;

/// Outer header bytes: [version_byte, field_tag_byte].
const VERSION_BYTE: u8 = 0x01;
/// Wrapped-sk version byte.
const WRAPPED_SK_VERSION: u8 = 0x01;
const WRAPPED_SK_AAD: &[u8] = b"olha/wrapped-sk";

/// Timeout for `pass show` — gpg-agent hangs with no TTY fail fast.
const PASS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum EncryptionError {
    #[error("pass subprocess failed: {0}")]
    PassSpawn(#[source] std::io::Error),

    #[error("pass show returned non-zero exit code ({0}); is the entry set up? run `olha encryption init`")]
    PassExit(i32),

    #[error("pass show timed out after {}s — is gpg-agent waiting on pinentry with no TTY?", PASS_TIMEOUT.as_secs())]
    PassTimeout,

    #[error("encryption failed")]
    Encrypt,

    #[error("decryption failed: wrong key, tampered ciphertext, or truncated payload")]
    Decrypt,

    #[error("sealed blob too short ({got} bytes, need at least {min})")]
    BlobTooShort { got: usize, min: usize },

    #[error("unknown on-disk encryption version byte: 0x{0:02x}")]
    BadVersion(u8),

    #[error("field tag mismatch: stored 0x{stored:02x}, expected 0x{expected:02x}")]
    FieldTagMismatch { stored: u8, expected: u8 },
}

/// Bytes of a DEK, auto-zeroized on drop.
pub type Dek = Zeroizing<[u8; DEK_LEN]>;

/// Bytes of a StaticSecret, auto-zeroized on drop.
pub type SkBytes = Zeroizing<[u8; X25519_KEY_LEN]>;

/// Which field a ciphertext belongs to. The byte value is the outer
/// `field_tag` header; the AAD string binds the decryption to the
/// field name + format version.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FieldTag {
    Summary,
    Body,
    Hints,
}

impl FieldTag {
    pub fn byte(self) -> u8 {
        match self {
            FieldTag::Summary => 0x01,
            FieldTag::Body => 0x02,
            FieldTag::Hints => 0x03,
        }
    }

    pub fn aad(self) -> &'static [u8] {
        match self {
            FieldTag::Summary => b"olha/summary",
            FieldTag::Body => b"olha/body",
            FieldTag::Hints => b"olha/hints",
        }
    }
}

/// Mode passed into every DB read/write. The daemon's `DaemonState`
/// produces a fresh `EncMode` per operation; the `Unlocked` variant
/// owns a Zeroizing copy of the X25519 secret so DB code can decrypt
/// without holding the state-level lock.
pub enum EncMode {
    /// No encryption configured. Rows go to TEXT columns as-is.
    Plaintext,
    /// Encryption is configured; writes seal under `pk`, reads of
    /// encrypted rows return placeholders.
    Locked {
        pk: PublicKey,
        key_id: [u8; KEY_ID_LEN],
    },
    /// Secret is available — full read + write.
    Unlocked {
        pk: PublicKey,
        key_id: [u8; KEY_ID_LEN],
        sk: SkBytes,
        /// Shared with `EncryptionState.last_activity`; bumped on
        /// every successful decrypt so the idle auto-lock timer
        /// resets.
        activity: Arc<AtomicI64>,
    },
}

impl EncMode {
    pub fn pk(&self) -> Option<&PublicKey> {
        match self {
            EncMode::Plaintext => None,
            EncMode::Locked { pk, .. } => Some(pk),
            EncMode::Unlocked { pk, .. } => Some(pk),
        }
    }

    pub fn key_id(&self) -> Option<&[u8; KEY_ID_LEN]> {
        match self {
            EncMode::Plaintext => None,
            EncMode::Locked { key_id, .. } => Some(key_id),
            EncMode::Unlocked { key_id, .. } => Some(key_id),
        }
    }

    pub fn is_encrypted(&self) -> bool {
        !matches!(self, EncMode::Plaintext)
    }

    pub fn is_unlocked(&self) -> bool {
        matches!(self, EncMode::Unlocked { .. })
    }

    /// Bump the shared activity counter if this mode is Unlocked.
    /// No-op for Plaintext / Locked.
    pub fn record_decrypt_activity(&self) {
        if let EncMode::Unlocked { activity, .. } = self {
            activity.store(unix_now(), Ordering::Relaxed);
        }
    }
}

/// Daemon-wide encryption state, wrapped in `Arc` and shared across
/// D-Bus handlers.
pub struct EncryptionState {
    /// Public key for sealing. Present iff encryption is enabled and
    /// key material exists in `meta`. `None` keeps the daemon in
    /// plaintext mode.
    pub public_key: Option<PublicKey>,
    /// SHA-256(pk)[..4]. Zeroed when `public_key` is None.
    pub key_id: [u8; KEY_ID_LEN],
    /// Populated between `Unlock` and `Lock` / auto-lock.
    secret_key: RwLock<Option<SkBytes>>,
    /// Seconds since unix epoch of the last unlock or successful
    /// decrypt. Drives the idle auto-lock task. Zero = no activity.
    /// Shared via `Arc` so `EncMode::Unlocked` instances can bump it
    /// without routing through `EncryptionState`.
    pub(crate) last_activity: Arc<AtomicI64>,
    /// Idle threshold; 0 disables auto-lock.
    auto_lock_secs: u64,
}

impl EncryptionState {
    /// Plaintext mode (encryption disabled in config).
    pub fn plaintext() -> Self {
        Self {
            public_key: None,
            key_id: [0u8; KEY_ID_LEN],
            secret_key: RwLock::new(None),
            last_activity: Arc::new(AtomicI64::new(0)),
            auto_lock_secs: 0,
        }
    }

    /// Encrypted mode: `pk` loaded from `meta`, daemon starts locked.
    pub fn with_public_key(pk: PublicKey, auto_lock_secs: u64) -> Self {
        let key_id = compute_pk_key_id(&pk);
        Self {
            public_key: Some(pk),
            key_id,
            secret_key: RwLock::new(None),
            last_activity: Arc::new(AtomicI64::new(0)),
            auto_lock_secs,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.public_key.is_some()
    }

    pub fn is_unlocked(&self) -> bool {
        self.secret_key.read().is_some()
    }

    pub fn auto_lock_secs(&self) -> u64 {
        self.auto_lock_secs
    }

    /// Populate `sk`. Zeroes any previously held value. Idempotent:
    /// calling with the same secret is a no-op beyond the activity
    /// bump.
    pub fn unlock(&self, sk: SkBytes) {
        *self.secret_key.write() = Some(sk);
        self.record_decrypt_activity();
    }

    /// Zeroize the stored `sk` (the Zeroizing wrapper does the wipe on
    /// drop). Returns whether a secret was actually present.
    pub fn lock(&self) -> bool {
        let mut guard = self.secret_key.write();
        guard.take().is_some()
    }

    /// Timestamp a successful decrypt or unlock. Seed for the
    /// auto-lock idle timer. Cheap — just an atomic write.
    pub fn record_decrypt_activity(&self) {
        let now = unix_now();
        self.last_activity.store(now, Ordering::Relaxed);
    }

    /// How many seconds until the auto-lock timer will fire, given
    /// the current activity clock. `None` when auto-lock is disabled
    /// or the daemon is already locked.
    pub fn idle_until_lock_secs(&self) -> Option<u64> {
        if self.auto_lock_secs == 0 || !self.is_unlocked() {
            return None;
        }
        let now = unix_now();
        let last = self.last_activity.load(Ordering::Relaxed);
        let elapsed = (now.saturating_sub(last)).max(0) as u64;
        Some(self.auto_lock_secs.saturating_sub(elapsed))
    }

    /// True iff auto-lock is enabled and the idle threshold has
    /// elapsed since the last recorded activity. Called from the
    /// background task; the transition to locked is performed via
    /// `lock()` so the caller can emit `locked_changed(false)`.
    pub fn should_auto_lock(&self) -> bool {
        if self.auto_lock_secs == 0 || !self.is_unlocked() {
            return false;
        }
        let now = unix_now();
        let last = self.last_activity.load(Ordering::Relaxed);
        now.saturating_sub(last) as u64 >= self.auto_lock_secs
    }

    /// Build a fresh `EncMode` suitable for a single DB operation.
    /// Clones the sk bytes (if any) into a `Zeroizing` so the caller
    /// doesn't hold the read-lock while doing I/O.
    pub fn enc_mode(&self) -> EncMode {
        let Some(pk) = self.public_key else {
            return EncMode::Plaintext;
        };
        let sk_clone = self.secret_key.read().as_ref().map(|sk| sk.clone());
        match sk_clone {
            Some(sk) => EncMode::Unlocked {
                pk,
                key_id: self.key_id,
                sk,
                activity: Arc::clone(&self.last_activity),
            },
            None => EncMode::Locked {
                pk,
                key_id: self.key_id,
            },
        }
    }
}

impl std::fmt::Debug for EncryptionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionState")
            .field("enabled", &self.public_key.is_some())
            .field("unlocked", &self.is_unlocked())
            .field("key_id", &self.key_id)
            .field("auto_lock_secs", &self.auto_lock_secs)
            .finish()
    }
}

fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------- sealed-box primitives ----------

/// Seal a field under `pk`. Fresh ephemeral keypair per call; the
/// shared secret + both public keys are fed through SHA-256 into an
/// XChaCha20-Poly1305 key, and the AAD binds the field tag.
pub fn seal_field(
    pk: &PublicKey,
    tag: FieldTag,
    plaintext: &[u8],
) -> Result<Vec<u8>, EncryptionError> {
    let esk = StaticSecret::random_from_rng(OsRng);
    let epk = PublicKey::from(&esk);
    let shared = esk.diffie_hellman(pk);
    let key = derive_sym_key(shared.as_bytes(), epk.as_bytes(), pk.as_bytes());

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let cipher = XChaCha20Poly1305::new_from_slice(&key).expect("32-byte key");
    let ct = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: tag.aad(),
            },
        )
        .map_err(|_| EncryptionError::Encrypt)?;

    // 2 (hdr) + 32 (epk) + 24 (nonce) + ct(+tag)
    let mut out = Vec::with_capacity(2 + X25519_KEY_LEN + NONCE_LEN + ct.len());
    out.push(VERSION_BYTE);
    out.push(tag.byte());
    out.extend_from_slice(epk.as_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open a field sealed with [`seal_field`]. Verifies version and
/// field-tag headers before touching the crypto.
pub fn open_field(
    sk: &[u8; X25519_KEY_LEN],
    pk: &PublicKey,
    tag: FieldTag,
    blob: &[u8],
) -> Result<Vec<u8>, EncryptionError> {
    let header_len = 2 + X25519_KEY_LEN + NONCE_LEN + 16;
    if blob.len() < header_len {
        return Err(EncryptionError::BlobTooShort {
            got: blob.len(),
            min: header_len,
        });
    }
    if blob[0] != VERSION_BYTE {
        return Err(EncryptionError::BadVersion(blob[0]));
    }
    if blob[1] != tag.byte() {
        return Err(EncryptionError::FieldTagMismatch {
            stored: blob[1],
            expected: tag.byte(),
        });
    }
    let epk_bytes: [u8; X25519_KEY_LEN] = blob[2..2 + X25519_KEY_LEN]
        .try_into()
        .expect("slice len is X25519_KEY_LEN");
    let nonce_bytes: [u8; NONCE_LEN] = blob[2 + X25519_KEY_LEN..2 + X25519_KEY_LEN + NONCE_LEN]
        .try_into()
        .expect("slice len is NONCE_LEN");
    let ct = &blob[2 + X25519_KEY_LEN + NONCE_LEN..];

    let epk = PublicKey::from(epk_bytes);
    let sk_static = StaticSecret::from(*sk);
    let shared = sk_static.diffie_hellman(&epk);
    let key = derive_sym_key(shared.as_bytes(), epk.as_bytes(), pk.as_bytes());

    let cipher = XChaCha20Poly1305::new_from_slice(&key).expect("32-byte key");
    let nonce = XNonce::from_slice(&nonce_bytes);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ct,
                aad: tag.aad(),
            },
        )
        .map_err(|_| EncryptionError::Decrypt)
}

/// Derive a 32-byte symmetric key from (shared, epk, pk). Binds both
/// endpoints so a ciphertext tied to one (pk, epk) pair can't be
/// relinked by swapping epk.
fn derive_sym_key(shared: &[u8; 32], epk: &[u8; 32], pk: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"olha/kdf");
    hasher.update(shared);
    hasher.update(epk);
    hasher.update(pk);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

// ---------- DEK-based sk wrapping ----------

/// Wrap 32 bytes of X25519 secret under the DEK for persistence.
/// Daemon itself never produces wrapped secrets at runtime — only
/// the CLI `olha encryption init` / `rewrap` do — but the symmetric
/// helper lives here so the daemon's tests can exercise the exact
/// code path `unwrap_sk` consumes.
#[allow(dead_code)]
pub fn wrap_sk(
    dek: &[u8; DEK_LEN],
    sk_bytes: &[u8; X25519_KEY_LEN],
) -> Result<Vec<u8>, EncryptionError> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let cipher = XChaCha20Poly1305::new_from_slice(dek).expect("DEK_LEN is 32");
    let ct = cipher
        .encrypt(
            nonce,
            Payload {
                msg: sk_bytes,
                aad: WRAPPED_SK_AAD,
            },
        )
        .map_err(|_| EncryptionError::Encrypt)?;

    let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
    out.push(WRAPPED_SK_VERSION);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Reverse of `wrap_sk`. Returns the 32-byte secret in `Zeroizing`.
pub fn unwrap_sk(dek: &[u8; DEK_LEN], blob: &[u8]) -> Result<SkBytes, EncryptionError> {
    let header_len = 1 + NONCE_LEN + 16;
    if blob.len() < header_len + X25519_KEY_LEN {
        return Err(EncryptionError::BlobTooShort {
            got: blob.len(),
            min: header_len + X25519_KEY_LEN,
        });
    }
    if blob[0] != WRAPPED_SK_VERSION {
        return Err(EncryptionError::BadVersion(blob[0]));
    }
    let nonce_bytes: [u8; NONCE_LEN] = blob[1..1 + NONCE_LEN]
        .try_into()
        .expect("slice len is NONCE_LEN");
    let ct = &blob[1 + NONCE_LEN..];

    let cipher = XChaCha20Poly1305::new_from_slice(dek).expect("DEK_LEN is 32");
    let nonce = XNonce::from_slice(&nonce_bytes);
    let pt = cipher
        .decrypt(
            nonce,
            Payload {
                msg: ct,
                aad: WRAPPED_SK_AAD,
            },
        )
        .map_err(|_| EncryptionError::Decrypt)?;

    if pt.len() != X25519_KEY_LEN {
        return Err(EncryptionError::Decrypt);
    }
    let mut out = Zeroizing::new([0u8; X25519_KEY_LEN]);
    out.copy_from_slice(&pt);
    Ok(out)
}

// ---------- DEK and key-id helpers ----------

/// Derive a 32-byte DEK from the raw bytes held in `pass`.
pub fn derive_dek(ikm: &[u8]) -> Dek {
    let mut hasher = Sha256::new();
    hasher.update(ikm);
    let digest = hasher.finalize();
    let mut out = Zeroizing::new([0u8; DEK_LEN]);
    out.copy_from_slice(&digest);
    out
}

/// Four-byte fingerprint of an X25519 public key. Stable across
/// restarts as long as the keypair is unchanged.
pub fn compute_pk_key_id(pk: &PublicKey) -> [u8; KEY_ID_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(pk.as_bytes());
    let digest = hasher.finalize();
    let mut id = [0u8; KEY_ID_LEN];
    id.copy_from_slice(&digest[..KEY_ID_LEN]);
    id
}

/// Shell out to `pass show <entry>`. Stdout is captured, stderr
/// discarded so pinentry diagnostics don't end up in our logs.
pub fn run_pass_show(entry: &str) -> Result<Vec<u8>, EncryptionError> {
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

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_keypair() -> (SkBytes, PublicKey) {
        let sk_static = StaticSecret::random_from_rng(OsRng);
        let pk = PublicKey::from(&sk_static);
        let sk_bytes = Zeroizing::new(sk_static.to_bytes());
        (sk_bytes, pk)
    }

    #[test]
    fn seal_open_roundtrip() {
        let (sk, pk) = test_keypair();
        for tag in [FieldTag::Summary, FieldTag::Body, FieldTag::Hints] {
            let msg = format!("hello {:?}", tag);
            let blob = seal_field(&pk, tag, msg.as_bytes()).unwrap();
            let out = open_field(&sk, &pk, tag, &blob).unwrap();
            assert_eq!(out, msg.as_bytes());
        }
    }

    #[test]
    fn wrong_pk_rejected() {
        let (sk1, pk1) = test_keypair();
        let (_, pk2) = test_keypair();
        let blob = seal_field(&pk1, FieldTag::Body, b"secret").unwrap();
        // Using the right sk but claiming a different pk → derived key
        // differs → auth tag mismatch.
        assert!(open_field(&sk1, &pk2, FieldTag::Body, &blob).is_err());
    }

    #[test]
    fn wrong_sk_rejected() {
        let (_, pk) = test_keypair();
        let (sk2, _) = test_keypair();
        let blob = seal_field(&pk, FieldTag::Body, b"secret").unwrap();
        assert!(open_field(&sk2, &pk, FieldTag::Body, &blob).is_err());
    }

    #[test]
    fn field_tag_binding_wrong_tag_fails() {
        let (sk, pk) = test_keypair();
        let blob = seal_field(&pk, FieldTag::Summary, b"topic").unwrap();
        // Reading as Body: outer tag mismatch trips first.
        let err = open_field(&sk, &pk, FieldTag::Body, &blob).unwrap_err();
        assert!(matches!(err, EncryptionError::FieldTagMismatch { .. }));
    }

    #[test]
    fn inner_tag_binding_via_aad() {
        let (sk, pk) = test_keypair();
        let mut blob = seal_field(&pk, FieldTag::Summary, b"topic").unwrap();
        // Flip only the outer tag byte — open() now asks with Body AAD.
        blob[1] = FieldTag::Body.byte();
        let err = open_field(&sk, &pk, FieldTag::Body, &blob).unwrap_err();
        // Outer tag matches now, but AAD differs from what was sealed → auth fail.
        assert!(matches!(err, EncryptionError::Decrypt));
    }

    #[test]
    fn outer_header_version_mismatch_rejected() {
        let (sk, pk) = test_keypair();
        let mut blob = seal_field(&pk, FieldTag::Body, b"x").unwrap();
        blob[0] = 0x03;
        let err = open_field(&sk, &pk, FieldTag::Body, &blob).unwrap_err();
        assert!(matches!(err, EncryptionError::BadVersion(0x03)));
    }

    #[test]
    fn truncated_blob_rejected() {
        let (sk, pk) = test_keypair();
        let blob = seal_field(&pk, FieldTag::Body, b"x").unwrap();
        let short = &blob[..10];
        assert!(matches!(
            open_field(&sk, &pk, FieldTag::Body, short),
            Err(EncryptionError::BlobTooShort { .. })
        ));
    }

    #[test]
    fn nonces_are_unique_across_calls() {
        let (_, pk) = test_keypair();
        let a = seal_field(&pk, FieldTag::Body, b"same").unwrap();
        let b = seal_field(&pk, FieldTag::Body, b"same").unwrap();
        assert_ne!(a, b, "ephemeral keys must randomize per-call");
    }

    #[test]
    fn wrapped_sk_roundtrip() {
        let dek = [0xABu8; DEK_LEN];
        let sk = [0x33u8; X25519_KEY_LEN];
        let blob = wrap_sk(&dek, &sk).unwrap();
        let unwrapped = unwrap_sk(&dek, &blob).unwrap();
        assert_eq!(unwrapped.as_ref(), &sk);
    }

    #[test]
    fn wrapped_sk_rejects_wrong_dek() {
        let dek1 = [0x01u8; DEK_LEN];
        let dek2 = [0x02u8; DEK_LEN];
        let sk = [0x33u8; X25519_KEY_LEN];
        let blob = wrap_sk(&dek1, &sk).unwrap();
        assert!(unwrap_sk(&dek2, &blob).is_err());
    }

    #[test]
    fn wrapped_sk_rejects_tampered_ciphertext() {
        let dek = [0xABu8; DEK_LEN];
        let sk = [0x33u8; X25519_KEY_LEN];
        let mut blob = wrap_sk(&dek, &sk).unwrap();
        // Flip a byte inside the ct region.
        let idx = 1 + NONCE_LEN + 2;
        blob[idx] ^= 0x01;
        assert!(unwrap_sk(&dek, &blob).is_err());
    }

    #[test]
    fn wrapped_sk_rejects_tampered_version_byte() {
        let dek = [0xABu8; DEK_LEN];
        let sk = [0x33u8; X25519_KEY_LEN];
        let mut blob = wrap_sk(&dek, &sk).unwrap();
        blob[0] = 0x02;
        assert!(matches!(
            unwrap_sk(&dek, &blob),
            Err(EncryptionError::BadVersion(0x02))
        ));
    }

    #[test]
    fn key_id_matches_sha256_of_pk_prefix() {
        let (_, pk) = test_keypair();
        let kid1 = compute_pk_key_id(&pk);
        let kid2 = compute_pk_key_id(&pk);
        assert_eq!(kid1, kid2);

        let mut hasher = Sha256::new();
        hasher.update(pk.as_bytes());
        let digest = hasher.finalize();
        assert_eq!(&kid1[..], &digest[..KEY_ID_LEN]);
    }

    #[test]
    fn derive_dek_is_sha256() {
        let d = derive_dek(b"hello");
        let expected = [
            0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
            0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
            0x93, 0x8b, 0x98, 0x24,
        ];
        assert_eq!(d.as_ref(), &expected);
    }

    #[test]
    fn encryption_state_lock_unlock_toggle() {
        let (sk, pk) = test_keypair();
        let state = EncryptionState::with_public_key(pk, 0);
        assert!(!state.is_unlocked());
        state.unlock(sk);
        assert!(state.is_unlocked());
        assert!(state.lock());
        assert!(!state.is_unlocked());
        // Idempotent lock returns false second time.
        assert!(!state.lock());
    }

    #[test]
    fn encryption_state_enc_mode_transitions() {
        let (sk, pk) = test_keypair();
        let state = EncryptionState::with_public_key(pk, 0);
        assert!(matches!(state.enc_mode(), EncMode::Locked { .. }));
        state.unlock(sk);
        assert!(matches!(state.enc_mode(), EncMode::Unlocked { .. }));

        let plain = EncryptionState::plaintext();
        assert!(matches!(plain.enc_mode(), EncMode::Plaintext));
    }

    #[test]
    fn auto_lock_zero_disables_timer() {
        let (sk, pk) = test_keypair();
        let state = EncryptionState::with_public_key(pk, 0);
        state.unlock(sk);
        // Even with ancient activity, auto-lock is off.
        state.last_activity.store(0, Ordering::Relaxed);
        assert!(!state.should_auto_lock());
        assert_eq!(state.idle_until_lock_secs(), None);
    }

    #[test]
    fn auto_lock_fires_after_threshold() {
        let (sk, pk) = test_keypair();
        let state = EncryptionState::with_public_key(pk, 1);
        state.unlock(sk);
        // Pretend the last activity was 10 seconds ago.
        let backdate = unix_now() - 10;
        state.last_activity.store(backdate, Ordering::Relaxed);
        assert!(state.should_auto_lock());
    }
}
