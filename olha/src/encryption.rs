//! CLI subcommands for managing olha's at-rest encryption state.
//!
//! These operate directly on SQLite + `pass` so they work without
//! the daemon. Commands that touch row content (`rotate-key`,
//! `disable --rekey-to-plaintext`) require the daemon to be stopped
//! first — we detect a live daemon on the session bus and bail.

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::rngs::OsRng as RandOsRng;
use rand::RngCore;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

// ---- constants matching olhad/src/db/encryption.rs ----
const DEFAULT_PASS_ENTRY: &str = "olha/db-key";
const DEK_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const KEY_ID_LEN: usize = 4;
const X25519_KEY_LEN: usize = 32;

const ENC_VERSION: i64 = 1;

const VERSION_BYTE: u8 = 0x01;
const WRAPPED_SK_VERSION: u8 = 0x01;
const WRAPPED_SK_AAD: &[u8] = b"olha/wrapped-sk";

const META_ENC_PUBLIC_KEY: &str = "enc_public_key";
const META_ENC_WRAPPED_SECRET: &str = "enc_wrapped_secret";
const META_ENC_KEY_ID: &str = "enc_key_id";
const META_ENC_DEK_KID: &str = "enc_dek_kid";

pub type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

// -----------------------------------------------------------------------------
// `olha encryption init`
// -----------------------------------------------------------------------------

/// Generate X25519 keypair + seed a DEK in `pass`, and stash the
/// wrapped secret + public key in the DB's `meta` table.
pub fn init(
    pass_entry: &str,
    force: bool,
    config_path: Option<&Path>,
    db_path: Option<&Path>,
) -> CliResult<()> {
    let config_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path)?,
    };

    // Refuse to overwrite existing state unless --force.
    let preexisting_pk = match Connection::open(&db_path) {
        Ok(conn) => get_meta(&conn, META_ENC_PUBLIC_KEY)
            .ok()
            .flatten()
            .is_some(),
        Err(_) => false,
    };
    if preexisting_pk && !force {
        return Err(format!(
            "encryption material already present in {}. Pass --force to overwrite (will make existing encrypted rows unreadable).",
            db_path.display()
        )
        .into());
    }
    if pass_exists(pass_entry)? && !force {
        return Err(format!(
            "pass entry '{}' already exists. Pass --force to overwrite.",
            pass_entry
        )
        .into());
    }

    // 1. Seed pass entry with a fresh 32-byte IKM (base64).
    let mut ikm = [0u8; DEK_LEN];
    RandOsRng.fill_bytes(&mut ikm);
    let ikm_b64 = base64::engine::general_purpose::STANDARD.encode(ikm);
    pass_insert_force(pass_entry, &ikm_b64, force)?;

    // 2. Read it back through `pass show` (that path is what olhad and
    // `olha unlock` will use, so exercising it catches gpg-agent
    // misconfig early).
    let ikm_readback = run_pass_show(pass_entry)?;
    if ikm_readback.is_empty() {
        return Err("pass show returned empty output after init; check gpg-agent setup".into());
    }
    let dek = derive_dek(&ikm_readback);

    // 3. Generate X25519 keypair; wrap sk under the DEK.
    let sk_static = StaticSecret::random_from_rng(RandOsRng);
    let pk = PublicKey::from(&sk_static);
    let sk_bytes: [u8; X25519_KEY_LEN] = sk_static.to_bytes();
    let sk_zero = Zeroizing::new(sk_bytes);
    let wrapped = wrap_sk(&dek, &sk_zero)?;

    let key_id = compute_pk_key_id(&pk);
    let dek_kid = compute_bytes_key_id(dek.as_ref());

    // 4. Persist to meta.
    let conn = Connection::open(&db_path)?;
    ensure_meta_table(&conn)?;
    set_meta(
        &conn,
        META_ENC_PUBLIC_KEY,
        &base64::engine::general_purpose::STANDARD.encode(pk.as_bytes()),
    )?;
    set_meta(
        &conn,
        META_ENC_WRAPPED_SECRET,
        &base64::engine::general_purpose::STANDARD.encode(&wrapped),
    )?;
    set_meta(&conn, META_ENC_KEY_ID, &hex_bytes(&key_id))?;
    set_meta(&conn, META_ENC_DEK_KID, &hex_bytes(&dek_kid))?;

    // dek and sk_zero zero out on drop.
    drop(dek);
    drop(sk_zero);

    println!("Initialized encryption (key_id={}).", hex_bytes(&key_id));
    println!(
        "Wrote pass entry '{}' and meta keys to {}.",
        pass_entry,
        db_path.display()
    );
    println!();
    println!("Next steps:");
    println!(
        "  1. Back up ~/.password-store/{}.gpg alongside your DB.",
        pass_entry
    );
    println!("  2. Run `olha encryption enable` to flip the config flag.");
    println!("  3. Start olhad, then `olha unlock` to read history.");
    Ok(())
}

// -----------------------------------------------------------------------------
// `olha encryption enable`
// -----------------------------------------------------------------------------

pub fn enable(
    pass_entry: &str,
    config_path: Option<&Path>,
    db_path: Option<&Path>,
    assume_yes: bool,
) -> CliResult<()> {
    let ikm = run_pass_show(pass_entry)?;
    if ikm.is_empty() {
        return Err(format!("pass entry '{}' exists but is empty", pass_entry).into());
    }

    let config_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path)?,
    };

    let conn = Connection::open(&db_path).ok();

    // Sanity-check: keypair must exist so the daemon can seal writes.
    let pk_present = match &conn {
        Some(c) => get_meta(c, META_ENC_PUBLIC_KEY).ok().flatten().is_some(),
        None => false,
    };
    if !pk_present {
        return Err("no key material in meta. Run `olha encryption init` before enabling.".into());
    }

    if let Some(ref conn) = conn {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE enc_version = 0",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if count > 0 {
            println!(
                "Enabling encryption will DELETE all {} plaintext notifications currently in {}.",
                count,
                db_path.display()
            );
            if !assume_yes && !confirm_yes("Proceed? [y/N] ")? {
                return Err("aborted by user".into());
            }
            conn.execute("DELETE FROM notifications WHERE enc_version = 0", [])?;
            println!("Wiped {} plaintext rows.", count);
        }
    }

    set_encryption_enabled_in_config(&config_path, true, pass_entry)?;
    println!("Encryption enabled in {}", config_path.display());
    println!("Restart olhad to pick up the change.");
    Ok(())
}

// -----------------------------------------------------------------------------
// `olha encryption disable`
// -----------------------------------------------------------------------------

pub async fn disable(
    pass_entry: &str,
    config_path: Option<&Path>,
    db_path: Option<&Path>,
    assume_yes: bool,
    rekey_to_plaintext: bool,
) -> CliResult<()> {
    let config_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path)?,
    };

    let conn = Connection::open(&db_path)?;
    let enc_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notifications WHERE enc_version > 0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if enc_rows > 0 && !rekey_to_plaintext {
        return Err(format!(
            "{} encrypted row(s) exist in {}. Pass `--rekey-to-plaintext` to decrypt them \
             back to plaintext columns before disabling encryption (explicit downgrade).",
            enc_rows,
            db_path.display()
        )
        .into());
    }

    if enc_rows > 0 {
        refuse_if_daemon_running("disable --rekey-to-plaintext").await?;

        println!(
            "About to DECRYPT {} row(s) and store them as plaintext in {}.",
            enc_rows,
            db_path.display()
        );
        if !assume_yes
            && !confirm_yes("This is an intentional plaintext downgrade. Proceed? [y/N] ")?
        {
            return Err("aborted by user".into());
        }

        // Load sk the same way `olha unlock` does.
        let ikm = run_pass_show(pass_entry)?;
        if ikm.is_empty() {
            return Err(format!("pass entry '{}' is empty", pass_entry).into());
        }
        let dek = derive_dek(&ikm);
        let wrapped = decode_meta_b64(&conn, META_ENC_WRAPPED_SECRET)?;
        let sk = unwrap_sk(&dek, &wrapped)?;
        drop(dek);
        let pk_bytes = decode_meta_b64(&conn, META_ENC_PUBLIC_KEY)?;
        let pk = public_key_from_bytes(&pk_bytes)?;

        let downgraded = downgrade_encrypted_rows_to_plaintext(&conn, &pk, &sk)?;
        println!("Downgraded {} row(s) to plaintext.", downgraded);
    }

    set_encryption_enabled_in_config(&config_path, false, pass_entry)?;
    println!("Encryption disabled in {}", config_path.display());
    println!("Restart olhad to pick up the change.");
    Ok(())
}

// -----------------------------------------------------------------------------
// `olha encryption status`
// -----------------------------------------------------------------------------

pub async fn status(
    pass_entry: &str,
    config_path: Option<&Path>,
    db_path: Option<&Path>,
) -> CliResult<()> {
    let config_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let (enabled, configured_entry) =
        read_encryption_config(&config_path).unwrap_or((false, DEFAULT_PASS_ENTRY.to_string()));
    let entry = if pass_entry.is_empty() {
        configured_entry.as_str()
    } else {
        pass_entry
    };

    println!("config file         : {}", config_path.display());
    println!("encryption.enabled  : {}", enabled);
    println!("pass_entry          : {}", entry);

    match run_pass_show(entry) {
        Ok(ikm) if ikm.is_empty() => println!("pass entry          : EXISTS but EMPTY (broken)"),
        Ok(ikm) => {
            let kid = compute_bytes_key_id(derive_dek(&ikm).as_ref());
            println!("pass entry          : OK (dek_kid = {})", hex_bytes(&kid));
        }
        Err(e) => println!("pass entry          : NOT AVAILABLE ({e})"),
    }

    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path).unwrap_or_else(|_| default_db_path()),
    };
    match Connection::open(&db_path) {
        Ok(conn) => {
            println!("db                  : {}", db_path.display());
            match get_meta(&conn, META_ENC_KEY_ID).ok().flatten() {
                Some(kid) => println!("keypair             : present (key_id = {})", kid),
                None => println!("keypair             : ABSENT — run `olha encryption init`"),
            }
            let plain: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM notifications WHERE enc_version = 0",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let enc: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM notifications WHERE enc_version > 0",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            println!("rows (plaintext)    : {}", plain);
            println!("rows (encrypted)    : {}", enc);
        }
        Err(e) => println!(
            "db                  : {} (unreadable: {})",
            db_path.display(),
            e
        ),
    }

    match probe_daemon_unlocked().await {
        Ok(Some(true)) => println!("daemon              : running, unlocked"),
        Ok(Some(false)) => println!("daemon              : running, locked"),
        Ok(None) => println!("daemon              : not running"),
        Err(e) => println!("daemon              : probe failed ({e})"),
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// `olha encryption rewrap`  (rotate DEK only)
// -----------------------------------------------------------------------------

pub fn rewrap(
    old_pass_entry: &str,
    new_pass_entry: &str,
    config_path: Option<&Path>,
    db_path: Option<&Path>,
) -> CliResult<()> {
    let config_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path)?,
    };

    let conn = Connection::open(&db_path)?;
    let wrapped = decode_meta_b64(&conn, META_ENC_WRAPPED_SECRET)?;

    // Unwrap under the old DEK.
    let old_ikm = run_pass_show(old_pass_entry)?;
    if old_ikm.is_empty() {
        return Err(format!("pass entry '{}' is empty", old_pass_entry).into());
    }
    let old_dek = derive_dek(&old_ikm);
    let sk = unwrap_sk(&old_dek, &wrapped)?;
    drop(old_dek);

    // Seed a new IKM if the target entry is the same (or simply reuse
    // what's already there if the user just wants to re-wrap without
    // touching the pass entry — in that case old == new).
    let new_ikm = if old_pass_entry == new_pass_entry {
        // Regenerate the entry contents so an attacker who saw the
        // old wrapped blob no longer has a matching DEK.
        let mut bytes = [0u8; DEK_LEN];
        RandOsRng.fill_bytes(&mut bytes);
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        pass_insert_force(new_pass_entry, &b64, true)?;
        run_pass_show(new_pass_entry)?
    } else {
        let raw = run_pass_show(new_pass_entry);
        match raw {
            Ok(x) if !x.is_empty() => x,
            _ => {
                let mut bytes = [0u8; DEK_LEN];
                RandOsRng.fill_bytes(&mut bytes);
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                pass_insert_force(new_pass_entry, &b64, true)?;
                run_pass_show(new_pass_entry)?
            }
        }
    };
    let new_dek = derive_dek(&new_ikm);
    let new_dek_kid = compute_bytes_key_id(new_dek.as_ref());

    let sk_arr: [u8; X25519_KEY_LEN] = *sk;
    let sk_zero = Zeroizing::new(sk_arr);
    let new_wrapped = wrap_sk(&new_dek, &sk_zero)?;
    drop(new_dek);

    set_meta(
        &conn,
        META_ENC_WRAPPED_SECRET,
        &base64::engine::general_purpose::STANDARD.encode(&new_wrapped),
    )?;
    set_meta(&conn, META_ENC_DEK_KID, &hex_bytes(&new_dek_kid))?;

    if old_pass_entry != new_pass_entry {
        set_encryption_enabled_in_config(&config_path, true, new_pass_entry)?;
        println!(
            "Pass entry updated in config: {} -> {}",
            old_pass_entry, new_pass_entry
        );
    }
    println!(
        "Re-wrapped secret under new DEK (dek_kid={}).",
        hex_bytes(&new_dek_kid)
    );
    Ok(())
}

// -----------------------------------------------------------------------------
// `olha encryption rotate-key`  (rotate X25519 keypair, reseal rows)
// -----------------------------------------------------------------------------

pub async fn rotate_key(
    pass_entry: &str,
    config_path: Option<&Path>,
    db_path: Option<&Path>,
    assume_yes: bool,
) -> CliResult<()> {
    refuse_if_daemon_running("rotate-key").await?;

    let config_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path)?,
    };

    let mut conn = Connection::open(&db_path)?;
    let enc_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notifications WHERE enc_version > 0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    println!(
        "About to generate a new X25519 keypair and re-seal {} encrypted row(s). Daemon must be stopped.",
        enc_rows
    );
    if !assume_yes && !confirm_yes("Proceed? [y/N] ")? {
        return Err("aborted by user".into());
    }

    // Unwrap old sk to decrypt existing rows.
    let ikm = run_pass_show(pass_entry)?;
    if ikm.is_empty() {
        return Err(format!("pass entry '{}' is empty", pass_entry).into());
    }
    let dek = derive_dek(&ikm);
    let wrapped = decode_meta_b64(&conn, META_ENC_WRAPPED_SECRET)?;
    let old_sk = unwrap_sk(&dek, &wrapped)?;
    let old_pk_bytes = decode_meta_b64(&conn, META_ENC_PUBLIC_KEY)?;
    let old_pk = public_key_from_bytes(&old_pk_bytes)?;

    // Generate new keypair.
    let new_sk_static = StaticSecret::random_from_rng(RandOsRng);
    let new_pk = PublicKey::from(&new_sk_static);
    let new_sk_bytes: [u8; X25519_KEY_LEN] = new_sk_static.to_bytes();
    let new_sk_zero = Zeroizing::new(new_sk_bytes);
    let new_key_id = compute_pk_key_id(&new_pk);

    let tx = conn.transaction()?;

    let rows: Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>)> = {
        let mut stmt = tx.prepare(
            "SELECT id, summary_enc, body_enc, hints_enc FROM notifications WHERE enc_version > 0",
        )?;
        let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let collected: Result<Vec<_>, _> = mapped.collect();
        collected?
    };

    let mut resealed = 0usize;
    let old_sk_arr: &[u8; X25519_KEY_LEN] = &**&old_sk;
    for (id, s, b, h) in rows {
        let s_new = reseal_blob(
            old_sk_arr,
            &old_pk,
            &new_pk,
            FieldTag::Summary,
            s.as_deref(),
        )?;
        let b_new = reseal_blob(old_sk_arr, &old_pk, &new_pk, FieldTag::Body, b.as_deref())?;
        let h_new = reseal_blob(old_sk_arr, &old_pk, &new_pk, FieldTag::Hints, h.as_deref())?;
        tx.execute(
            "UPDATE notifications SET summary_enc = ?1, body_enc = ?2, hints_enc = ?3, key_id = ?4 WHERE id = ?5",
            params![s_new, b_new, h_new, new_key_id.to_vec(), id],
        )?;
        resealed += 1;
    }
    tx.commit()?;

    // Persist new public key + wrapped new secret.
    let new_wrapped = wrap_sk(&dek, &new_sk_zero)?;
    set_meta(
        &conn,
        META_ENC_PUBLIC_KEY,
        &base64::engine::general_purpose::STANDARD.encode(new_pk.as_bytes()),
    )?;
    set_meta(
        &conn,
        META_ENC_WRAPPED_SECRET,
        &base64::engine::general_purpose::STANDARD.encode(&new_wrapped),
    )?;
    set_meta(&conn, META_ENC_KEY_ID, &hex_bytes(&new_key_id))?;

    drop(dek);
    drop(old_sk);
    drop(new_sk_zero);

    println!(
        "Re-sealed {} row(s) under new key_id={}.",
        resealed,
        hex_bytes(&new_key_id)
    );
    Ok(())
}

// -----------------------------------------------------------------------------
// helpers: crypto
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug)]
enum FieldTag {
    Summary,
    Body,
    Hints,
}

impl FieldTag {
    fn byte(self) -> u8 {
        match self {
            FieldTag::Summary => 0x01,
            FieldTag::Body => 0x02,
            FieldTag::Hints => 0x03,
        }
    }
    fn aad(self) -> &'static [u8] {
        match self {
            FieldTag::Summary => b"olha/summary",
            FieldTag::Body => b"olha/body",
            FieldTag::Hints => b"olha/hints",
        }
    }
}

fn derive_dek(ikm: &[u8]) -> Zeroizing<[u8; DEK_LEN]> {
    let mut hasher = Sha256::new();
    hasher.update(ikm);
    let digest = hasher.finalize();
    let mut out = Zeroizing::new([0u8; DEK_LEN]);
    out.copy_from_slice(&digest);
    out
}

fn compute_pk_key_id(pk: &PublicKey) -> [u8; KEY_ID_LEN] {
    compute_bytes_key_id(pk.as_bytes())
}

fn compute_bytes_key_id(bytes: &[u8]) -> [u8; KEY_ID_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut id = [0u8; KEY_ID_LEN];
    id.copy_from_slice(&digest[..KEY_ID_LEN]);
    id
}

fn wrap_sk(dek: &[u8; DEK_LEN], sk_bytes: &[u8; X25519_KEY_LEN]) -> CliResult<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    RandOsRng.fill_bytes(&mut nonce);
    let cipher = XChaCha20Poly1305::new_from_slice(dek).map_err(|_| "DEK length")?;
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: sk_bytes,
                aad: WRAPPED_SK_AAD,
            },
        )
        .map_err(|_| "wrap_sk encrypt")?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
    out.push(WRAPPED_SK_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn unwrap_sk(dek: &[u8; DEK_LEN], blob: &[u8]) -> CliResult<Zeroizing<[u8; X25519_KEY_LEN]>> {
    if blob.len() < 1 + NONCE_LEN + 16 + X25519_KEY_LEN {
        return Err("wrapped-sk blob too short".into());
    }
    if blob[0] != WRAPPED_SK_VERSION {
        return Err(format!("unknown wrapped-sk version byte 0x{:02x}", blob[0]).into());
    }
    let nonce = &blob[1..1 + NONCE_LEN];
    let ct = &blob[1 + NONCE_LEN..];
    let cipher = XChaCha20Poly1305::new_from_slice(dek).map_err(|_| "DEK length")?;
    let pt = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: WRAPPED_SK_AAD,
            },
        )
        .map_err(|_| "unwrap_sk decrypt (wrong pass entry?)")?;
    if pt.len() != X25519_KEY_LEN {
        return Err("unwrap_sk: decrypted sk has wrong length".into());
    }
    let mut out = Zeroizing::new([0u8; X25519_KEY_LEN]);
    out.copy_from_slice(&pt);
    Ok(out)
}

/// Derive the symmetric key from the shared secret + both pks +
/// a domain prefix. Mirrors olhad/src/db/encryption.rs.
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

/// Seal plaintext under `pk`. Byte-for-byte compatible with olhad's
/// `seal_field`.
fn seal_field(pk: &PublicKey, tag: FieldTag, plaintext: &[u8]) -> CliResult<Vec<u8>> {
    let esk = StaticSecret::random_from_rng(RandOsRng);
    let epk = PublicKey::from(&esk);
    let shared = esk.diffie_hellman(pk);
    let key = derive_sym_key(shared.as_bytes(), epk.as_bytes(), pk.as_bytes());

    let mut nonce = [0u8; NONCE_LEN];
    RandOsRng.fill_bytes(&mut nonce);
    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|_| "sym key length")?;
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: tag.aad(),
            },
        )
        .map_err(|_| "seal_field encrypt")?;

    let mut out = Vec::with_capacity(2 + X25519_KEY_LEN + NONCE_LEN + ct.len());
    out.push(VERSION_BYTE);
    out.push(tag.byte());
    out.extend_from_slice(epk.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open a field sealed with `seal_field`.
fn open_field(
    sk: &[u8; X25519_KEY_LEN],
    pk: &PublicKey,
    tag: FieldTag,
    blob: &[u8],
) -> CliResult<Vec<u8>> {
    let header_len = 2 + X25519_KEY_LEN + NONCE_LEN + 16;
    if blob.len() < header_len {
        return Err("sealed blob too short".into());
    }
    if blob[0] != VERSION_BYTE {
        return Err(format!("unknown outer version 0x{:02x}", blob[0]).into());
    }
    if blob[1] != tag.byte() {
        return Err("field tag mismatch".into());
    }
    let epk_bytes: [u8; X25519_KEY_LEN] = blob[2..2 + X25519_KEY_LEN].try_into().unwrap();
    let nonce_bytes: [u8; NONCE_LEN] = blob[2 + X25519_KEY_LEN..2 + X25519_KEY_LEN + NONCE_LEN]
        .try_into()
        .unwrap();
    let ct = &blob[2 + X25519_KEY_LEN + NONCE_LEN..];

    let epk = PublicKey::from(epk_bytes);
    let sk_static = StaticSecret::from(*sk);
    let shared = sk_static.diffie_hellman(&epk);
    let key = derive_sym_key(shared.as_bytes(), epk.as_bytes(), pk.as_bytes());

    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|_| "sym key length")?;
    let pt = cipher
        .decrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: ct,
                aad: tag.aad(),
            },
        )
        .map_err(|_| "open_field decrypt")?;
    Ok(pt)
}

fn reseal_blob(
    old_sk: &[u8; X25519_KEY_LEN],
    old_pk: &PublicKey,
    new_pk: &PublicKey,
    tag: FieldTag,
    blob: Option<&[u8]>,
) -> CliResult<Option<Vec<u8>>> {
    let Some(blob) = blob else { return Ok(None) };
    let pt = open_field(old_sk, old_pk, tag, blob)?;
    let sealed = seal_field(new_pk, tag, &pt)?;
    Ok(Some(sealed))
}

fn downgrade_encrypted_rows_to_plaintext(
    conn: &Connection,
    pk: &PublicKey,
    sk: &Zeroizing<[u8; X25519_KEY_LEN]>,
) -> CliResult<usize> {
    let sk_arr: &[u8; X25519_KEY_LEN] = &**sk;

    let rows: Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, summary_enc, body_enc, hints_enc FROM notifications WHERE enc_version > 0",
        )?;
        let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let collected: Result<Vec<_>, _> = mapped.collect();
        collected?
    };

    let mut count = 0;
    for (id, s, b, h) in rows {
        let summary = match s.as_deref() {
            Some(blob) => String::from_utf8(open_field(sk_arr, pk, FieldTag::Summary, blob)?)
                .map_err(|_| "summary: decrypted bytes are not valid UTF-8")?,
            None => String::new(),
        };
        let body = match b.as_deref() {
            Some(blob) => String::from_utf8(open_field(sk_arr, pk, FieldTag::Body, blob)?)
                .map_err(|_| "body: decrypted bytes are not valid UTF-8")?,
            None => String::new(),
        };
        let hints = match h.as_deref() {
            Some(blob) => String::from_utf8(open_field(sk_arr, pk, FieldTag::Hints, blob)?)
                .unwrap_or_else(|_| "{}".into()),
            None => "{}".into(),
        };
        conn.execute(
            "UPDATE notifications SET summary = ?1, body = ?2, hints = ?3,
                summary_enc = NULL, body_enc = NULL, hints_enc = NULL,
                enc_version = 0, key_id = NULL WHERE id = ?4",
            params![summary, body, hints, id],
        )?;
        count += 1;
    }
    Ok(count)
}

// -----------------------------------------------------------------------------
// helpers: process / file / meta
// -----------------------------------------------------------------------------

fn run_pass_show(entry: &str) -> CliResult<Vec<u8>> {
    let output = Command::new("pass")
        .arg("show")
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn pass: {e}"))?;

    if !output.status.success() {
        return Err(format!("pass show exited with {}", output.status).into());
    }
    let mut out = output.stdout;
    while out.last().map_or(false, |b| b.is_ascii_whitespace()) {
        out.pop();
    }
    Ok(out)
}

fn pass_exists(entry: &str) -> CliResult<bool> {
    let status = Command::new("pass")
        .arg("show")
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn pass_insert_force(entry: &str, secret: &str, force: bool) -> CliResult<()> {
    let mut cmd = Command::new("pass");
    cmd.arg("insert").arg("-e");
    if force {
        cmd.arg("--force");
    }
    let mut child = cmd
        .arg(entry)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to spawn `pass insert`: {e}"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or("pass insert: stdin not available")?;
        stdin.write_all(secret.as_bytes())?;
        stdin.write_all(b"\n")?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(format!("pass insert exited with {}", status).into());
    }
    Ok(())
}

fn confirm_yes(prompt: &str) -> CliResult<bool> {
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .expect("XDG config dir")
        .join("olha")
        .join("config.toml")
}

fn default_db_path() -> PathBuf {
    dirs::data_local_dir()
        .expect("XDG data dir")
        .join("olha")
        .join("notifications.db")
}

fn db_path_from_config(config_path: &Path) -> CliResult<PathBuf> {
    if !config_path.exists() {
        return Ok(default_db_path());
    }
    let text = std::fs::read_to_string(config_path)?;
    let doc: toml::Value = toml::from_str(&text)?;
    if let Some(p) = doc
        .get("general")
        .and_then(|g| g.get("db_path"))
        .and_then(|v| v.as_str())
    {
        return Ok(PathBuf::from(shellexpand_home(p)));
    }
    Ok(default_db_path())
}

fn shellexpand_home(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    s.to_string()
}

fn read_encryption_config(path: &Path) -> CliResult<(bool, String)> {
    if !path.exists() {
        return Ok((false, DEFAULT_PASS_ENTRY.to_string()));
    }
    let text = std::fs::read_to_string(path)?;
    let doc: toml::Value = toml::from_str(&text)?;
    let sect = doc.get("encryption");
    let enabled = sect
        .and_then(|s| s.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let entry = sect
        .and_then(|s| s.get("pass_entry"))
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PASS_ENTRY)
        .to_string();
    Ok((enabled, entry))
}

fn set_encryption_enabled_in_config(path: &Path, enabled: bool, pass_entry: &str) -> CliResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = if existing.is_empty() {
        "".parse()?
    } else {
        existing.parse()?
    };

    let enc_tbl = doc
        .entry("encryption")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or("[encryption] is not a table")?;

    enc_tbl.insert("enabled", toml_edit::value(enabled));
    enc_tbl.insert("pass_entry", toml_edit::value(pass_entry));

    std::fs::write(path, doc.to_string())?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> CliResult<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    match stmt.query_row(params![key], |row| row.get::<_, String>(0)) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> CliResult<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn ensure_meta_table(conn: &Connection) -> CliResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    Ok(())
}

fn decode_meta_b64(conn: &Connection, key: &str) -> CliResult<Vec<u8>> {
    let val = get_meta(conn, key)?.ok_or_else(|| format!("meta.{} missing", key))?;
    Ok(base64::engine::general_purpose::STANDARD.decode(val.trim())?)
}

fn public_key_from_bytes(bytes: &[u8]) -> CliResult<PublicKey> {
    if bytes.len() != X25519_KEY_LEN {
        return Err(format!("pk is {} bytes, expected {}", bytes.len(), X25519_KEY_LEN).into());
    }
    let mut arr = [0u8; X25519_KEY_LEN];
    arr.copy_from_slice(bytes);
    Ok(PublicKey::from(arr))
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

async fn probe_daemon_unlocked() -> Result<Option<bool>, String> {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => return Err(format!("session bus: {e}")),
    };
    let proxy = match crate::client::ControlDaemonProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            if is_service_unknown(&e) {
                return Ok(None);
            }
            return Err(format!("proxy: {e}"));
        }
    };
    match proxy.is_unlocked().await {
        Ok(b) => Ok(Some(b)),
        Err(e) => {
            if is_service_unknown(&e) {
                Ok(None)
            } else {
                Err(format!("is_unlocked: {e}"))
            }
        }
    }
}

fn is_service_unknown(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::FDO(fdo) => matches!(**fdo, zbus::fdo::Error::ServiceUnknown(_)),
        zbus::Error::MethodError(name, _, _) => {
            name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner"
        }
        _ => false,
    }
}

async fn refuse_if_daemon_running(action: &str) -> CliResult<()> {
    match probe_daemon_unlocked().await {
        Ok(Some(_)) => Err(format!(
            "olhad is running; stop it before `olha encryption {action}` to avoid concurrent writes."
        )
        .into()),
        Ok(None) => Ok(()),
        Err(e) => {
            tracing::warn!("daemon probe failed ({e}); proceeding without the running-daemon guard");
            Ok(())
        }
    }
}

const _: i64 = ENC_VERSION;
